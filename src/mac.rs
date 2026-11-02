//! Mac bağlantısı: aynı ağdaki Mac'i koduyla bulur, Mac'teki Ayna Ajan'dan güven onayı alır ve
//! uzak masaüstünü RustDesk istemcisiyle açar.
//!
//! Akış:
//! 1. Bul: yerel ağa UDP yayını `AYNA1 FIND <kod>`; kod onunsa Ayna Ajan `AYNA1 HERE <kod> <port> <ad>`
//!    ile yanıt verir (sunucu yok, yalnız aynı ağ).
//! 2. Güven: Ajan'a TCP `AYNA1 HELLO <cihaz-kimliği> <bilgisayar-adı>`. Bu bilgisayar Mac'te daha önce
//!    güvenilen listesinde değilse Mac'te "İzin ver / Reddet" sorulur. Ajan izin verilen bilgisayarın
//!    adresini RustDesk'in IP beyaz listesine ekler; listede olmayan adresten RustDesk bağlantısı
//!    kabul edilmez.
//! 3. Bağlan: RustDesk istemcisi Mac'e doğrudan IP ile bağlanır; parolayı (Mac'te Ayna Ajan'da
//!    belirlenen kalıcı parola) RustDesk'in kendi penceresi sorar. Parola bu uygulamadan geçmez.
//!
//! Oturum penceresi RustDesk'e aittir; görev çubuğundaki düğmesi kaldırılır, Mac ikonu onu
//! gösterip gizler.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Security::Cryptography::{
    BCryptGenRandom, BCryptHash, BCRYPT_ALG_HANDLE, BCRYPT_HMAC_SHA256_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::taskbar::{self, Device};
use crate::util;

/// Ajan'dan gelen durum değişikliği (iş parçacığı → ana pencere).
pub const WM_APP_MAC: u32 = WM_APP + 23;
/// Ayna Ajan'ın UDP (bulma) ve TCP (güven) bağlantı noktası.
pub const AGENT_PORT: u16 = 21130;
/// RustDesk'in doğrudan IP erişimi için varsayılan bağlantı noktası.
const RUSTDESK_PORT: u16 = 21118;
const DEVICE_SETTING: PCWSTR = w!("CihazKimligi");
/// Mac'teki onay sorusuna yanıt için bekleme süresi.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(90);
/// RustDesk penceresi bu sürede açılmazsa bağlantı başarısız sayılır.
const WINDOW_TIMEOUT: Duration = Duration::from_secs(45);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// RustDesk'in (Flutter) oturum penceresi sınıfı; ana penceresi `FLUTTER_RUNNER_WIN32_WINDOW`.
const SESSION_CLASS: &str = "RustdeskMultiWindow";

#[derive(Clone, PartialEq, Debug)]
pub enum State {
    Idle,
    Searching,
    Waiting(String),
    Starting(String),
    Connected(String),
    Failed(String),
}

impl State {
    pub fn text(&self) -> String {
        match self {
            State::Idle => "Bağlı değil".into(),
            State::Searching => "Ağda aranıyor…".into(),
            State::Waiting(name) => format!("{name}: Mac'te onay bekleniyor…"),
            State::Starting(name) => format!("{name}: uzak masaüstü açılıyor…"),
            State::Connected(name) => format!("Bağlı: {name}"),
            State::Failed(why) => why.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct Peer {
    ip: Ipv4Addr,
    port: u16,
    name: String,
}

struct Inner {
    state: State,
    /// Güven onayı alınan Mac; ana iş parçacığı RustDesk'i açar.
    approved: Option<Peer>,
    /// Her bağlanma denemesi bir numara alır; vazgeçilen eski denemenin sonucu yok sayılır.
    /// Durumla aynı kilit altında: kontrol ile yazım arasına vazgeçme giremez.
    attempt: u32,
}

struct Shared {
    inner: Mutex<Inner>,
}

impl Shared {
    /// Deneme hâlâ geçerliyse durumu günceller.
    fn publish(&self, attempt: u32, f: impl FnOnce(&mut Inner)) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.attempt != attempt {
            return false;
        }
        f(&mut inner);
        true
    }

    fn alive(&self, attempt: u32) -> bool {
        self.inner.lock().unwrap().attempt == attempt
    }
}

struct Session {
    peer: Peer,
    started: Instant,
    window: Option<HWND>,
    /// Oturumdan önce açık olan RustDesk pencereleri (oturum penceresi yeni açılandır).
    before: Vec<isize>,
    /// Pencere açılmadan vazgeçildi: açılınca kapatılacak.
    cancelled: bool,
}

pub struct Mac {
    notify: HWND,
    shared: Arc<Shared>,
    session: Option<Session>,
}

impl Mac {
    pub fn new(notify: HWND) -> Mac {
        Mac {
            notify,
            shared: Arc::new(Shared { inner: Mutex::new(Inner { state: State::Idle, approved: None, attempt: 0 }) }),
            session: None,
        }
    }

    pub fn state(&self) -> State {
        self.shared.inner.lock().unwrap().state.clone()
    }

    fn set(&self, s: State) {
        self.shared.inner.lock().unwrap().state = s;
    }

    pub fn fail(&self, why: &str) {
        self.set(State::Failed(why.into()));
    }

    pub fn connect(&mut self, code: &str) {
        self.cancel();
        let attempt = {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.attempt = inner.attempt.wrapping_add(1);
            inner.state = State::Searching;
            inner.attempt
        };
        util::log(&format!("Mac aranıyor (kod {code}), yerel adresler: {:?}", local_ipv4()));
        let (shared, notify, code) = (self.shared.clone(), self.notify.0 as isize, code.to_string());
        std::thread::spawn(move || {
            let result = find_and_trust(&code, &shared, attempt, notify);
            let applied = shared.publish(attempt, |inner| match result {
                Ok(peer) => {
                    inner.state = State::Starting(peer.name.clone());
                    inner.approved = Some(peer);
                }
                Err(why) => {
                    util::log(&format!("Mac bağlantısı başarısız: {why}"));
                    inner.state = State::Failed(why);
                }
            });
            if applied {
                post(notify);
            }
        });
    }

    /// Süren denemeden vazgeçer ya da açık oturumu kapatır.
    pub fn cancel(&mut self) {
        {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.attempt = inner.attempt.wrapping_add(1);
            inner.approved = None;
            inner.state = State::Idle;
        }
        if let Some(mut s) = self.session.take() {
            match s.window {
                Some(w) => unsafe {
                    let _ = PostMessageW(w, WM_CLOSE, WPARAM(0), LPARAM(0));
                },
                // RustDesk başladı ama penceresi henüz yok: açılınca kapatılmak üzere izlenir.
                None => {
                    s.cancelled = true;
                    self.session = Some(s);
                }
            }
        }
        taskbar::set_enabled(Device::Mac, false);
        taskbar::set_view(Device::Mac, HWND::default());
    }

    /// Uygulama kapanırken: gizlenmiş oturum penceresi görünür ve görev çubuğu düğmesiyle
    /// bırakılır (yoksa erişilemez kalırdı).
    pub fn release(&mut self) {
        if let Some(w) = self.window() {
            unsafe {
                let _ = ShowWindow(w, SW_SHOW);
                taskbar_button(w, true);
            }
        }
    }

    /// Ana iş parçacığında, zamanlayıcıyla: onaylanan Mac için RustDesk'i başlatır, oturum
    /// penceresini izler (açıldı, kapandı).
    pub fn poll(&mut self) {
        let approved = self.shared.inner.lock().unwrap().approved.take();
        if let Some(peer) = approved {
            match launch(&peer) {
                Ok(before) => {
                    self.session =
                        Some(Session { peer, started: Instant::now(), window: None, before, cancelled: false });
                }
                Err(why) => self.set(State::Failed(why)),
            }
        }
        let Some(s) = self.session.as_mut() else { return };
        match s.window {
            Some(w) if unsafe { IsWindow(w).as_bool() } => {}
            Some(_) => {
                // Pencere kapandı: oturum bitti.
                util::log(&format!("Mac oturumu kapandı: {}", s.peer.name));
                self.session = None;
                self.set(State::Idle);
                taskbar::set_enabled(Device::Mac, false);
                taskbar::set_view(Device::Mac, HWND::default());
            }
            None => {
                if let Some(w) = unsafe { session_window(&s.before, &s.peer.ip.to_string()) } {
                    if s.cancelled {
                        unsafe {
                            let _ = PostMessageW(w, WM_CLOSE, WPARAM(0), LPARAM(0));
                        }
                        self.session = None;
                        return;
                    }
                    s.window = Some(w);
                    unsafe { taskbar_button(w, false) };
                    let name = s.peer.name.clone();
                    util::log(&format!("Mac oturum penceresi açıldı: {name}"));
                    self.set(State::Connected(name));
                    taskbar::set_view(Device::Mac, w);
                    taskbar::set_enabled(Device::Mac, true);
                } else if s.started.elapsed() > WINDOW_TIMEOUT {
                    let cancelled = s.cancelled;
                    self.session = None;
                    if !cancelled {
                        self.set(State::Failed("RustDesk penceresi açılmadı.".into()));
                    }
                }
            }
        }
    }

    fn window(&self) -> Option<HWND> {
        self.session.as_ref().and_then(|s| s.window).filter(|w| unsafe { IsWindow(*w).as_bool() })
    }

    /// Mac ikonuna tıklandı: önde ise gizler, değilse öne getirir.
    pub fn toggle(&mut self) {
        let Some(w) = self.window() else { return };
        unsafe {
            if IsWindowVisible(w).as_bool() && !IsIconic(w).as_bool() && GetForegroundWindow() == w {
                let _ = ShowWindow(w, SW_HIDE);
                taskbar::refresh();
            } else {
                self.show();
            }
        }
    }

    pub fn show(&mut self) {
        let Some(w) = self.window() else { return };
        unsafe {
            let cmd = if IsIconic(w).as_bool() { SW_RESTORE } else { SW_SHOW };
            let _ = ShowWindow(w, cmd);
            let _ = SetForegroundWindow(w);
            taskbar_button(w, false);
        }
        taskbar::refresh();
    }
}

fn post(notify: isize) {
    unsafe {
        let _ = PostMessageW(HWND(notify as _), WM_APP_MAC, WPARAM(0), LPARAM(0));
    }
}

fn find_and_trust(code: &str, shared: &Shared, attempt: u32, notify: isize) -> Result<Peer, String> {
    let not_found = "Mac bulunamadı. Mac'te Ayna Ajan açık mı, iki cihaz aynı ağda mı?";
    let peer = match discover(code)? {
        Found::Signed(peer) | Found::FirstTime(peer) => peer,
        Found::UnsignedOnly(peer) => {
            // Eşleşilmiş Mac imzasız yanıt verdi: Mac'te güven kaldırılmış ya da Ayna Ajan yeniden
            // kurulmuş olabilir; aynı ağdaki başka bir cihazın kendini Mac gibi göstermesi de olabilir.
            // Karar kullanıcının.
            if !shared.alive(attempt) || !ask_repair(notify, &peer) {
                return Err("Mac'in kimliği doğrulanamadı; eşleşme yenilenmedi.".into());
            }
            util::delete_setting(PCWSTR(util::wide(&key_setting(code)).as_ptr()));
            util::log(&format!("Mac kodu {code} için eşleşme kullanıcı onayıyla sıfırlandı."));
            peer
        }
        Found::Nothing => return Err(not_found.into()),
    };
    util::log(&format!("Mac bulundu: {} ({}:{})", peer.name, peer.ip, peer.port));
    if !shared.publish(attempt, |inner| inner.state = State::Waiting(peer.name.clone())) {
        return Err(String::new());
    }
    post(notify);
    trust(code, &peer, &|| shared.alive(attempt))?;
    Ok(peer)
}

unsafe fn message_box(owner: isize, text: &str, flags: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    let text = util::wide(text);
    MessageBoxW(HWND(owner as _), PCWSTR(text.as_ptr()), w!("Ayna Merkezi"), flags)
}

fn ask_repair(owner: isize, peer: &Peer) -> bool {
    let text = format!(
        "“{}” ({}) yanıt verdi ama bu bilgisayarla yapılmış eşleşmeyi kanıtlayamadı.\n\n\
         Mac'te bu bilgisayarın güvenini kaldırdıysanız ya da Ayna Ajan'ı yeniden kurduysanız \
         \"Evet\" deyin; Mac'te yeniden onay istenir.\n\n\
         Böyle bir şey yapmadıysanız \"Hayır\" deyin: ağdaki başka bir cihaz kendini bu Mac gibi \
         gösteriyor olabilir.\n\nYeniden eşleşilsin mi?",
        peer.name, peer.ip
    );
    unsafe { message_box(owner, &text, MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND) == IDYES }
}

enum Found {
    /// Eşleşilmiş Mac, anahtarıyla imzalı yanıt verdi.
    Signed(Peer),
    /// Bu kod için anahtar yok: ilk eşleşme.
    FirstTime(Peer),
    /// Anahtar var ama yalnız imzasız (ya da yanlış imzalı) yanıt geldi.
    UnsignedOnly(Peer),
    Nothing,
}

/// Yerel ağa (her ağ bağdaştırıcısından ayrı) kodu yayınlar.
///
/// Bu Mac'le daha önce eşleşildiyse (anahtar saklıysa) yanıt anahtarla imzalı olmalıdır (bağlantı
/// noktası ve ad dahil): aynı ağdaki başka bir cihaz kodu bilse de kendini o Mac gibi gösteremez.
fn discover(code: &str) -> Result<Found, String> {
    let key = mac_key(code);
    let nonce = hex(&random::<16>().ok_or("Rastgele sayı üretilemedi.")?);
    let query = format!("AYNA1 FIND {code} {} {nonce}", device_id()?);
    let mut sockets = Vec::new();
    for ip in local_ipv4() {
        if let Ok(s) = UdpSocket::bind(SocketAddr::new(IpAddr::V4(ip), 0)) {
            let _ = s.set_broadcast(true);
            let _ = s.set_nonblocking(true);
            sockets.push(s);
        }
    }
    if sockets.is_empty() {
        let s = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("Ağ kullanılamıyor ({e})."))?;
        let _ = s.set_broadcast(true);
        let _ = s.set_nonblocking(true);
        sockets.push(s);
    }
    let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), AGENT_PORT);
    let mut buf = [0u8; 512];
    let mut unsigned: Option<Peer> = None;
    for _ in 0..4 {
        for s in &sockets {
            let _ = s.send_to(query.as_bytes(), target);
        }
        let until = Instant::now() + Duration::from_millis(900);
        while Instant::now() < until {
            for s in &sockets {
                while let Ok((n, from)) = s.recv_from(&mut buf) {
                    // Yalnız yerel (özel) ağ adreslerinden gelen yanıtlar.
                    let IpAddr::V4(ip) = from.ip() else { continue };
                    if !is_private(ip) {
                        continue;
                    }
                    let msg = String::from_utf8_lossy(&buf[..n]);
                    let Some(here) = parse_here(&msg, code, ip) else { continue };
                    let Some(k) = &key else { return Ok(Found::FirstTime(here.peer)) };
                    let signed = format!("here|{code}|{nonce}|{}|{}", here.peer.port, here.raw_name);
                    if proof_matches(k, &signed, &here.proof) {
                        return Ok(Found::Signed(here.peer));
                    }
                    util::log(&format!("Mac kodu {code} için imzasız/yanlış yanıt ({ip})."));
                    unsigned.get_or_insert(here.peer);
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    Ok(unsigned.map_or(Found::Nothing, Found::UnsignedOnly))
}

fn is_private(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local()
}

struct Here {
    peer: Peer,
    proof: String,
    /// İmzalanan ad (temizlenmeden önceki hâli).
    raw_name: String,
}

/// `AYNA1 HERE <kod> <rustdesk-port> <imza|-> <ad…>`
fn parse_here(msg: &str, code: &str, ip: Ipv4Addr) -> Option<Here> {
    let mut parts = msg.trim().splitn(6, ' ');
    if parts.next()? != "AYNA1" || parts.next()? != "HERE" || parts.next()? != code {
        return None;
    }
    let port = parts.next()?.parse().ok()?;
    let proof = parts.next()?.to_string();
    let raw_name = parts.next().unwrap_or("Mac").to_string();
    let name = clean_name(&raw_name);
    let name = if name.trim().is_empty() { "Mac".into() } else { name };
    Some(Here { peer: Peer { ip, port, name }, proof, raw_name })
}

/// Ekranda gösterilen adlardan denetim, görünmez ve yön değiştiren karakterler atılır (ad
/// sahteciliği: yön değiştiren karakterle ters yazılmış bir ad gibi).
pub(crate) fn clean_name(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            !c.is_control()
                && !matches!(c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2069 | 0xFEFF | 0x2028 | 0x2029)
        })
        .take(64)
        .collect()
}

/// Bu bilgisayarın IPv4 adresleri (bağlantı-yerel ve geri döngü hariç).
fn local_ipv4() -> Vec<Ipv4Addr> {
    let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "localhost".into());
    let mut out: Vec<Ipv4Addr> = (host.as_str(), 0)
        .to_socket_addrs()
        .map(|it| {
            it.filter_map(|a| match a.ip() {
                IpAddr::V4(v) if !v.is_loopback() && !v.is_link_local() => Some(v),
                _ => None,
            })
            .collect()
        })
        .unwrap_or_default();
    out.dedup();
    out
}

/// Ajan'dan güven onayı ister. Mac'teki kullanıcı "İzin ver" diyene (ya da bu bilgisayar zaten
/// güvenilenler listesindeyse ve anahtarıyla kanıtlarsa hemen) `OK` gelir. İlk onayda Ajan bu
/// bilgisayara özel bir anahtar verir; sonraki bağlantılarda iki taraf da kimliğini bu anahtarla
/// kanıtlar (`OK` satırı da imzalıdır).
fn trust(code: &str, peer: &Peer, alive: &dyn Fn() -> bool) -> Result<(), String> {
    let addr = SocketAddr::new(IpAddr::V4(peer.ip), AGENT_PORT);
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(4))
        .map_err(|e| format!("Mac'teki Ayna Ajan'a bağlanılamadı ({e})."))?;
    let started = Instant::now();
    // Kısa okuma süresi: vazgeçme ve toplam süre her saniye denetlenir.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut pending = String::new();
    let mut read = |reader: &mut BufReader<TcpStream>| read_line_limited(reader, &mut pending, started, alive);
    let first = read(&mut reader)?;
    let nonce = first.strip_prefix("AYNA1 NONCE ").filter(|n| n.len() == 32 && n.chars().all(|c| c.is_ascii_hexdigit()));
    let Some(nonce) = nonce else { return Err(format!("Mac'ten beklenmeyen yanıt: {}", clean_name(&first))) };
    let key = mac_key(code);
    let id = device_id()?;
    let proof = match &key {
        Some(k) => hex(&hmac(k, &format!("hello|{nonce}")).ok_or("İmza hesaplanamadı.")?),
        None => "-".to_string(),
    };
    let name = clean_name(&std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".into()));
    let hello = format!("AYNA1 HELLO {id} {proof} {name}\n");
    stream.write_all(hello.as_bytes()).map_err(|e| format!("Mac'e ulaşılamadı ({e})."))?;
    loop {
        let line = read(&mut reader)?;
        let parts: Vec<&str> = line.split(' ').collect();
        match parts.as_slice() {
            ["AYNA1", "OK", token, proof] => {
                let signed = format!("ok|{nonce}|{id}");
                match &key {
                    // Eşleşilmiş Mac onayını eski anahtarla imzalar; anahtar değiştirilemez.
                    Some(k) => {
                        if !proof_matches(k, &signed, proof) {
                            return Err("Mac'in onayı doğrulanamadı.".into());
                        }
                    }
                    // İlk eşleşme: yeni anahtar ve onunla imzalı onay.
                    None => {
                        let Some(new) = unhex(token).filter(|t| t.len() == 32) else {
                            return Err("Mac anahtar vermedi.".into());
                        };
                        if !proof_matches(&new, &signed, proof) {
                            return Err("Mac'in onayı doğrulanamadı.".into());
                        }
                        if !util::write_text_setting(PCWSTR(util::wide(&key_setting(code)).as_ptr()), token) {
                            util::log("Mac anahtarı kaydedilemedi; sonraki bağlantıda yeniden onay istenecek.");
                        }
                    }
                }
                return Ok(());
            }
            ["AYNA1", "WAIT"] => continue,
            ["AYNA1", "DENY"] => return Err("Mac bu bilgisayarın bağlanmasını reddetti.".into()),
            ["AYNA1", "OFF"] => return Err("Mac'te Ayna Ajan bağlantılara kapalı.".into()),
            _ => return Err(format!("Mac'ten beklenmeyen yanıt: {}", clean_name(&line))),
        }
    }
}

/// Tek satır okur: en fazla 512 bayt (satır sonu göndermeyen karşı taraf belleği şişiremez),
/// toplam onay süresi ve vazgeçme her saniye denetlenir.
fn read_line_limited(
    reader: &mut BufReader<TcpStream>,
    pending: &mut String,
    started: Instant,
    alive: &dyn Fn() -> bool,
) -> Result<String, String> {
    use std::io::ErrorKind::{TimedOut, WouldBlock};
    loop {
        if !alive() {
            return Err(String::new());
        }
        if started.elapsed() > APPROVAL_TIMEOUT {
            return Err("Mac'te onay verilmedi (süre doldu).".into());
        }
        let room = 512u64.saturating_sub(pending.len() as u64);
        if room == 0 {
            return Err("Mac'ten çok uzun yanıt geldi.".into());
        }
        match reader.by_ref().take(room).read_line(pending) {
            Ok(0) => return Err("Mac bağlantıyı kapattı.".into()),
            Ok(_) if pending.ends_with('\n') => return Ok(std::mem::take(pending).trim().to_string()),
            Ok(_) => continue,
            Err(e) if matches!(e.kind(), WouldBlock | TimedOut) => continue,
            Err(_) => return Err("Mac'ten geçersiz yanıt geldi.".into()),
        }
    }
}

fn key_setting(code: &str) -> String {
    format!("MacAnahtari{code}")
}

/// Bu Mac'in ilk onayda verdiği anahtar (yoksa daha önce eşleşilmemiştir).
fn mac_key(code: &str) -> Option<Vec<u8>> {
    let name = util::wide(&key_setting(code));
    let text = util::read_text_setting(PCWSTR(name.as_ptr()))?;
    unhex(&text).filter(|k| k.len() == 32)
}

/// Sistemin güvenli rastgele kaynağı; çalışmıyorsa tahmin edilebilir bir değerle devam edilmez.
fn random<const N: usize>() -> Option<[u8; N]> {
    let mut bytes = [0u8; N];
    let status = unsafe { BCryptGenRandom(BCRYPT_ALG_HANDLE::default(), &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    status.is_ok().then_some(bytes)
}

fn hmac(key: &[u8], msg: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    let status = unsafe { BCryptHash(BCRYPT_HMAC_SHA256_ALG_HANDLE, Some(key), msg.as_bytes(), &mut out) };
    status.is_ok().then_some(out)
}

/// Sabit sürede karşılaştırma; HMAC hesaplanamazsa eşleşme yok sayılır.
fn proof_matches(key: &[u8], msg: &str, proof: &str) -> bool {
    let (Some(given), Some(expected)) = (unhex(proof), hmac(key, msg)) else { return false };
    given.len() == expected.len() && given.iter().zip(expected.iter()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Bu bilgisayarın Mac'teki güvenilenler listesinde tanındığı kalıcı rastgele kimlik.
fn device_id() -> Result<String, String> {
    if let Some(id) = util::read_text_setting(DEVICE_SETTING).filter(|s| s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit())) {
        return Ok(id);
    }
    let id = hex(&random::<16>().ok_or("Rastgele sayı üretilemedi.")?);
    util::write_text_setting(DEVICE_SETTING, &id);
    Ok(id)
}

/// Uygulamanın yanındaki `bin\rustdesk\rustdesk.exe` ya da kurulu RustDesk.
fn rustdesk_exe() -> Option<PathBuf> {
    let mut dir = util::exe_path().parent()?.to_path_buf();
    // Exe'nin yanı ya da geliştirmede proje kökü (target\release'in üstü); sürücü köküne inilmez
    // (C:\bin\… herkesin yazabildiği bir yer olabilir).
    for _ in 0..3 {
        let Some(up) = dir.parent() else { break };
        let candidate = dir.join("bin").join("rustdesk").join("rustdesk.exe");
        if candidate.exists() {
            return Some(candidate);
        }
        if up.parent().is_none() {
            break;
        }
        dir = up.to_path_buf();
    }
    for var in ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"] {
        if let Ok(base) = std::env::var(var) {
            let candidate = PathBuf::from(base).join("RustDesk").join("rustdesk.exe");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

fn launch(peer: &Peer) -> Result<Vec<isize>, String> {
    let exe = rustdesk_exe().ok_or_else(|| {
        "Bu bilgisayarda RustDesk bulunamadı (bin\\rustdesk\\rustdesk.exe).".to_string()
    })?;
    let before = unsafe { rustdesk_windows(&peer.ip.to_string()) };
    let target = if peer.port == RUSTDESK_PORT { peer.ip.to_string() } else { format!("{}:{}", peer.ip, peer.port) };
    util::log(&format!("RustDesk başlatılıyor: {} --connect {target}", exe.display()));
    std::process::Command::new(&exe)
        .arg("--connect")
        .arg(&target)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("RustDesk başlatılamadı: {e}"))?;
    Ok(before)
}

/// RustDesk süreçlerine ait görünür oturum pencereleri.
unsafe fn rustdesk_windows(title_part: &str) -> Vec<isize> {
    struct Found {
        pids: Vec<u32>,
        title: Vec<u16>,
        out: Vec<isize>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        let f = &mut *(lp.0 as *mut Found);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if f.pids.contains(&pid) && IsWindowVisible(hwnd).as_bool() {
            // Uzak masaüstü (ve dosya aktarımı) pencereleri; ana RustDesk penceresi hariç.
            let mut class = [0u16; 64];
            let n = GetClassNameW(hwnd, &mut class) as usize;
            let mut title = [0u16; 256];
            let t = GetWindowTextW(hwnd, &mut title) as usize;
            // Başlık "<adres> - Remote Desktop - RustDesk": başka bir RustDesk oturumu karışmasın.
            let matches = f.title.is_empty() || title[..t].windows(f.title.len()).any(|w| w == f.title.as_slice());
            if String::from_utf16_lossy(&class[..n]) == SESSION_CLASS && matches {
                f.out.push(hwnd.0 as isize);
            }
        }
        TRUE
    }
    let mut f = Found {
        pids: crate::uxplay::processes_named("rustdesk.exe"),
        title: title_part.encode_utf16().collect(),
        out: Vec::new(),
    };
    if f.pids.is_empty() {
        return Vec::new();
    }
    let _ = EnumWindows(Some(each), LPARAM(&mut f as *mut Found as isize));
    f.out
}

/// Oturum için yeni açılan RustDesk penceresi. `--connect` ile başlatılan süreç işi çalışan bir
/// RustDesk'e (WM_COPYDATA ile) devredip kapanabildiği için süreç kimliğine değil "önceden
/// yoktu"ya bakılır.
unsafe fn session_window(before: &[isize], ip: &str) -> Option<HWND> {
    rustdesk_windows(ip).into_iter().find(|h| !before.contains(h)).map(|h| HWND(h as _))
}

/// Oturum penceresinin kendi görev çubuğu düğmesi kalkar (giriş Başlat'ın solundaki Mac ikonu);
/// uygulama kapanırken geri verilir.
unsafe fn taskbar_button(hwnd: HWND, show: bool) {
    let list: windows::core::Result<ITaskbarList> = CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER);
    if let Ok(list) = list {
        if list.HrInit().is_ok() {
            let _ = if show { list.AddTab(hwnd) } else { list.DeleteTab(hwnd) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_reply() {
        let ip = Ipv4Addr::new(192, 168, 1, 20);
        let here = parse_here("AYNA1 HERE 123456789 21118 - Burhan's Mac mini\n", "123456789", ip).unwrap();
        assert_eq!(here.peer.port, 21118);
        assert_eq!(here.proof, "-");
        assert_eq!(here.peer.name, "Burhan's Mac mini");
        assert_eq!(here.raw_name, "Burhan's Mac mini");
        assert!(parse_here("AYNA1 HERE 999 21118 - x", "123456789", ip).is_none());
        assert!(parse_here("OTHER HERE 123456789 21118 - x", "123456789", ip).is_none());
    }

    #[test]
    fn hmac_matches_rfc4231_case_2() {
        // RFC 4231, 2. test: anahtar "Jefe".
        let mac = hmac(b"Jefe", "what do ya want for nothing?").unwrap();
        assert_eq!(hex(&mac), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        assert!(proof_matches(b"Jefe", "what do ya want for nothing?", &hex(&mac)));
        assert!(!proof_matches(b"Jefe", "x", &hex(&mac)));
        assert_eq!(unhex("0aff"), Some(vec![0x0a, 0xff]));
        assert_eq!(clean_name("a\u{202E}b\u{200B}c"), "abc");
    }
}
