//! Mac bağlantısı (AYNA2): aynı ağdaki Mac'i koduyla bulur, Mac'teki Ayna Ajan'la şifreli bir oturum
//! açar; Mac'in ekran görüntüsünü (H.264) alır, fareyi ve klavyeyi gönderir. Başka program gerekmez.
//!
//! 1. Bul (UDP 21130): `AYNA1 FIND <kod> <cihaz> <nonce>` →
//!    `AYNA1 HERE <kod> <port> <imza|-> <ad>`. Eşleşilmiş Mac imzalı yanıt verir.
//! 2. El sıkışma (TCP 21130, satırlar):
//!    Mac: `AYNA2 HI <nonce> <mac-açık-anahtar>`;
//!    Windows: `AYNA2 HELLO <cihaz> <imza|-> <win-açık-anahtar> <ad>`.
//!    İki taraf geçici ECDH P-256 anahtarlarından ortak sır çıkarır;
//!    T = `AYNA2|nonce|mac-anahtar|win-anahtar|cihaz` bağlamıyla HKDF-SHA256 yön başına AES-256-GCM
//!    anahtarlarını, ilk eşleşme için 6 haneli doğrulama kodunu (iki ekranda aynı görünür) ve yeni
//!    eşleşme anahtarını verir. Eşleşilmiş cihazlar T'yi anahtarla imzalar (HMAC): aradaki biri iki
//!    tarafla ayrı ayrı anahtar değişse de imzayı üretemez.
//!    Mac: `AYNA2 WAIT` (onay soruluyor) … `AYNA2 OK NEW|SAME <imza>` | `DENY` | `OFF` | `BUSY`.
//! 3. Şifreli çerçeveler: `u32 uzunluk || AES-GCM(şifreli metin || etiket)`, nonce = 4 sıfır bayt ||
//!    64 bit sayaç (yön başına). Düz metnin ilk baytı mesaj türü (bkz. `MSG_*`).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::crypto::{self, ct_eq, hex, unhex, Aead, EcdhKey};
use crate::gst::Player;
use crate::util;

/// Durum değişti (iş parçacığı → Mac penceresi).
pub const WM_APP_MAC: u32 = WM_APP + 23;
/// Ayna Ajan'ın UDP (bulma) ve TCP (oturum) bağlantı noktası.
pub const AGENT_PORT: u16 = 21130;
const DEVICE_SETTING: PCWSTR = w!("CihazKimligi");
/// Son bağlanılan Mac'in kodu.
pub const CODE_SETTING: PCWSTR = w!("MacKodu");
/// Mac'teki onay sorusuna yanıt için bekleme süresi.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(90);
/// Bu süre hiç veri gelmezse (Mac'ten yanıt yok) bağlantı kopmuş sayılır.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(2);
/// Bir çerçevenin en büyük boyu (4K anahtar kare bile bunun çok altında).
const MAX_FRAME: usize = 16 << 20;

// Mac → Windows
const MSG_VIDEO: u8 = 0x01;
const MSG_SCREEN: u8 = 0x02;
const MSG_PONG: u8 = 0x03;
const MSG_BYE: u8 = 0x04;
// Windows → Mac
pub const MSG_MOUSE_MOVE: u8 = 0x10;
pub const MSG_MOUSE_BUTTON: u8 = 0x11;
pub const MSG_SCROLL: u8 = 0x12;
pub const MSG_KEY: u8 = 0x13;
const MSG_KEYFRAME: u8 = 0x14;
const MSG_PING: u8 = 0x15;
const MSG_PAUSE: u8 = 0x16;
const MSG_LEAVE: u8 = 0x17;

#[derive(Clone, PartialEq, Debug)]
pub enum State {
    Idle,
    Searching,
    /// Mac'te onay bekleniyor; iki ekranda aynı görünmesi gereken doğrulama kodu.
    Waiting { name: String, sas: String },
    Connecting(String),
    Connected(String),
    Failed(String),
}

impl State {
    pub fn text(&self) -> String {
        match self {
            State::Idle => "Bağlı değil".into(),
            State::Searching => "Ağda aranıyor…".into(),
            State::Waiting { name, .. } => format!("{name}: Mac'te onay bekleniyor…"),
            State::Connecting(name) => format!("{name}: bağlanılıyor…"),
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
    /// Her deneme bir numara alır; vazgeçilen eski denemenin sonucu yok sayılır. Durumla aynı kilit
    /// altında: kontrol ile yazım arasına vazgeçme giremez.
    attempt: u32,
    /// Açık oturuma giden mesajlar.
    tx: Option<Sender<Vec<u8>>>,
    /// Açık oturumun soketi (vazgeçince hemen kapatmak için).
    socket: Option<TcpStream>,
    /// Mac ekranının görüntü boyutu.
    screen: Option<(u16, u16)>,
    /// Kopma kullanıcıdan değil ağdan: yeniden bağlanılabilir.
    dropped: bool,
}

struct Shared {
    inner: Mutex<Inner>,
    player: Mutex<Option<Player>>,
}

impl Shared {
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

pub struct Mac {
    notify: HWND,
    shared: Arc<Shared>,
}

impl Mac {
    pub fn new(notify: HWND) -> Mac {
        Mac {
            notify,
            shared: Arc::new(Shared {
                inner: Mutex::new(Inner { state: State::Idle, attempt: 0, tx: None, socket: None, screen: None, dropped: false }),
                player: Mutex::new(None),
            }),
        }
    }

    pub fn state(&self) -> State {
        self.shared.inner.lock().unwrap().state.clone()
    }

    pub fn connected(&self) -> bool {
        matches!(self.state(), State::Connected(_))
    }

    /// Bağlantı ağ yüzünden koptu (kullanıcı kesmedi): yeniden denenebilir.
    pub fn dropped(&self) -> bool {
        self.shared.inner.lock().unwrap().dropped
    }

    pub fn screen(&self) -> Option<(u16, u16)> {
        self.shared.inner.lock().unwrap().screen
    }

    pub fn fail(&self, why: &str) {
        self.shared.inner.lock().unwrap().state = State::Failed(why.into());
    }

    /// Görüntünün çizileceği oynatıcı (pencere açılınca kurulur, kapanınca bırakılır).
    pub fn set_player(&self, player: Option<Player>) {
        let fresh = player.is_some();
        *self.shared.player.lock().unwrap() = player;
        if fresh {
            self.send(vec![MSG_KEYFRAME]);
        }
    }

    pub fn has_player(&self) -> bool {
        self.shared.player.lock().unwrap().is_some()
    }

    pub fn connect(&mut self, code: &str) {
        self.disconnect();
        let attempt = {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.attempt = inner.attempt.wrapping_add(1);
            inner.state = State::Searching;
            inner.attempt
        };
        let _ = util::write_text_setting(CODE_SETTING, code);
        util::log(&format!("Mac aranıyor (kod {code}), yerel adresler: {:?}", local_ipv4()));
        let (shared, notify, code) = (self.shared.clone(), self.notify.0 as isize, code.to_string());
        std::thread::spawn(move || {
            let result = session(&code, &shared, attempt, notify);
            let applied = shared.publish(attempt, |inner| {
                inner.tx = None;
                inner.socket = None;
                match &result {
                    Ok(()) => {
                        // Mac oturumu kapattı ya da bağlantı koptu.
                        inner.dropped = true;
                        inner.state = State::Failed("Mac bağlantısı koptu; yeniden bağlanılıyor…".into());
                    }
                    Err(why) => {
                        util::log(&format!("Mac bağlantısı başarısız: {why}"));
                        // Mac uykuda/ağ geçici olarak yoksa eşleşilmiş Mac'e yeniden denenir.
                        inner.dropped = transient(why) && Mac::paired(&code);
                        inner.state = State::Failed(why.clone());
                    }
                }
            });
            if applied {
                post(notify);
            }
        });
    }

    /// Süren denemeden vazgeçer ya da açık oturumu kapatır.
    pub fn disconnect(&mut self) {
        let (tx, socket) = {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.attempt = inner.attempt.wrapping_add(1);
            inner.state = State::Idle;
            inner.dropped = false;
            inner.screen = None;
            (inner.tx.take(), inner.socket.take())
        };
        if let Some(tx) = tx {
            let _ = tx.send(vec![MSG_LEAVE]);
        }
        if let Some(s) = socket {
            // Yazıcı LEAVE'i gönderebilsin diye kısa bir an tanınır, sonra soket kapanır.
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                let _ = s.shutdown(Shutdown::Both);
            });
        }
    }

    /// Açık oturuma mesaj (fare, klavye…). Oturum yoksa yok sayılır.
    pub fn send(&self, msg: Vec<u8>) {
        if let Some(tx) = &self.shared.inner.lock().unwrap().tx {
            let _ = tx.send(msg);
        }
    }

    /// Pencere gizlenince görüntü durur (bağlantı açık kalır); açılınca anahtar kareyle sürer.
    pub fn pause(&self, paused: bool) {
        self.send(vec![MSG_PAUSE, u8::from(paused)]);
        if !paused {
            self.send(vec![MSG_KEYFRAME]);
        }
    }

    /// Bu kodla daha önce eşleşildi mi (açılışta kendiliğinden bağlanmak için).
    pub fn paired(code: &str) -> bool {
        mac_key(code).is_some()
    }

    /// Eşleşmeyi bu bilgisayardan siler (Mac'te de güven kaldırılmalı).
    pub fn forget(code: &str) {
        util::delete_setting(PCWSTR(util::wide(&key_setting(code)).as_ptr()));
    }
}

/// Ağdan kaynaklanan, yeniden denemeye değer hatalar (ret ya da kimlik hatası değil).
fn transient(why: &str) -> bool {
    why.starts_with("Mac bulunamadı") || why.starts_with("Mac'teki Ayna Ajan'a bağlanılamadı") || why.starts_with("Mac bağlantıyı kapattı")
}

fn post(notify: isize) {
    unsafe {
        let _ = PostMessageW(HWND(notify as _), WM_APP_MAC, WPARAM(0), LPARAM(0));
    }
}

// ---------------------------------------------------------------------------------------------
// Bulma

enum Found {
    Signed(Peer),
    FirstTime(Peer),
    /// Eşleşilmiş Mac'ten yalnız imzasız (ya da yanlış imzalı) yanıt geldi.
    UnsignedOnly(Peer),
    Nothing,
}

/// Yerel ağa (her ağ bağdaştırıcısından ayrı) kodu yayınlar. Bu Mac'le daha önce eşleşildiyse yanıt
/// anahtarla imzalı olmalıdır (bağlantı noktası ve ad dahil).
fn discover(code: &str) -> Result<Found, String> {
    let key = mac_key(code);
    let nonce = hex(&crypto::random::<16>().ok_or("Rastgele sayı üretilemedi.")?);
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
                    let IpAddr::V4(ip) = from.ip() else { continue };
                    if !(ip.is_private() || ip.is_link_local()) {
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

struct Here {
    peer: Peer,
    proof: String,
    raw_name: String,
}

/// `AYNA1 HERE <kod> <port> <imza|-> <ad…>`
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

/// Ekranda gösterilen adlardan denetim, görünmez ve yön değiştiren karakterler atılır.
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

// ---------------------------------------------------------------------------------------------
// Oturum

/// Bulur, el sıkışır ve oturum bitene dek görüntüyü alır. `Ok(())`: oturum kurulmuştu ve sona erdi.
fn session(code: &str, shared: &Arc<Shared>, attempt: u32, notify: isize) -> Result<(), String> {
    let peer = match discover(code)? {
        Found::Signed(peer) | Found::FirstTime(peer) => peer,
        Found::UnsignedOnly(peer) => {
            if !shared.alive(attempt) || !ask_repair(notify, &peer) {
                return Err("Mac'in kimliği doğrulanamadı; eşleşme yenilenmedi.".into());
            }
            Mac::forget(code);
            util::log(&format!("Mac kodu {code} için eşleşme kullanıcı onayıyla sıfırlandı."));
            peer
        }
        Found::Nothing => return Err("Mac bulunamadı. Mac'te Ayna Ajan açık mı, iki cihaz aynı ağda mı?".into()),
    };
    util::log(&format!("Mac bulundu: {} ({}:{})", peer.name, peer.ip, peer.port));
    if !shared.publish(attempt, |i| i.state = State::Connecting(peer.name.clone())) {
        return Err(String::new());
    }
    post(notify);

    let addr = SocketAddr::new(IpAddr::V4(peer.ip), peer.port);
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(4))
        .map_err(|e| format!("Mac'teki Ayna Ajan'a bağlanılamadı ({e})."))?;
    let _ = stream.set_nodelay(true);
    // Kısa okuma süresi: vazgeçme ve süre aşımı her saniye denetlenir.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let socket = stream.try_clone().map_err(|e| e.to_string())?;
    if !shared.publish(attempt, |i| i.socket = Some(socket)) {
        return Err(String::new());
    }
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    let alive = || shared.alive(attempt);

    // --- El sıkışma
    let started = Instant::now();
    let mut pending = String::new();
    let first = read_line_limited(&mut reader, &mut pending, started, &alive)?;
    let mut parts = first.split(' ');
    let (Some("AYNA2"), Some("HI"), Some(nonce), Some(mac_pub), None) =
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(format!("Mac'ten beklenmeyen yanıt: {}", clean_name(&first)));
    };
    if nonce.len() != 32 || !nonce.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Mac'ten geçersiz el sıkışma.".into());
    }
    let mac_pub_bytes = unhex(mac_pub).filter(|b| b.len() == 65).ok_or("Mac'ten geçersiz anahtar.")?;
    let ecdh = EcdhKey::generate().ok_or("Anahtar üretilemedi.")?;
    let win_pub = hex(&ecdh.public_x963().ok_or("Anahtar üretilemedi.")?);
    let shared_secret = ecdh.agree(&mac_pub_bytes).ok_or("Mac'in anahtarı geçersiz.")?;
    let id = device_id()?;
    let t = format!("AYNA2|{nonce}|{mac_pub}|{win_pub}|{id}");
    let key = mac_key(code);
    let proof = match &key {
        Some(k) => hex(&crypto::hmac(k, format!("hello|{t}").as_bytes()).ok_or("İmza hesaplanamadı.")?),
        None => "-".to_string(),
    };
    let name = clean_name(&std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".into()));
    writer
        .write_all(format!("AYNA2 HELLO {id} {proof} {win_pub} {name}\n").as_bytes())
        .map_err(|e| format!("Mac'e ulaşılamadı ({e})."))?;

    let derive = |info: &str, n: usize| crypto::hkdf(&shared_secret, &[], format!("{info}|{t}").as_bytes(), n);
    let keys = derive("keys", 64).ok_or("Anahtar türetilemedi.")?;
    let sas_bytes = derive("sas", 4).ok_or("Anahtar türetilemedi.")?;
    let sas_n = u32::from_be_bytes([sas_bytes[0], sas_bytes[1], sas_bytes[2], sas_bytes[3]]) % 1_000_000;
    let sas = format!("{:03} {:03}", sas_n / 1000, sas_n % 1000);

    loop {
        let line = read_line_limited(&mut reader, &mut pending, started, &alive)?;
        let parts: Vec<&str> = line.split(' ').collect();
        match parts.as_slice() {
            ["AYNA2", "WAIT"] => {
                let (n, s) = (peer.name.clone(), sas.clone());
                if !shared.publish(attempt, |i| i.state = State::Waiting { name: n, sas: s }) {
                    return Err(String::new());
                }
                post(notify);
            }
            ["AYNA2", "OK", kind, proof] => {
                let ok_msg = format!("ok|{t}");
                match (&key, *kind) {
                    // Eşleşilmiş Mac onayını kayıtlı anahtarla imzalar; anahtar değiştirilemez.
                    (Some(k), "SAME") if proof_matches(k, &ok_msg, proof) => {}
                    (None, "NEW") => {
                        let token = derive("token", 32).ok_or("Anahtar türetilemedi.")?;
                        if !proof_matches(&token, &ok_msg, proof) {
                            return Err("Mac'in onayı doğrulanamadı.".into());
                        }
                        if !util::write_text_setting(PCWSTR(util::wide(&key_setting(code)).as_ptr()), &hex(&token)) {
                            util::log("Mac anahtarı kaydedilemedi; sonraki bağlantıda yeniden onay istenecek.");
                        }
                    }
                    _ => return Err("Mac'in onayı doğrulanamadı.".into()),
                }
                break;
            }
            ["AYNA2", "DENY"] => return Err("Mac bu bilgisayarın bağlanmasını reddetti.".into()),
            ["AYNA2", "OFF"] => return Err("Mac'te Ayna Ajan bağlantılara kapalı.".into()),
            ["AYNA2", "BUSY"] => return Err("Mac'e şu an başka bir bilgisayar bağlı.".into()),
            _ => return Err(format!("Mac'ten beklenmeyen yanıt: {}", clean_name(&line))),
        }
    }

    // --- Şifreli oturum
    let mut rx_aead = Aead::new(&keys[32..64]).ok_or("Şifreleme başlatılamadı.")?;
    let tx_aead = Aead::new(&keys[0..32]).ok_or("Şifreleme başlatılamadı.")?;
    let (tx, out) = mpsc::channel::<Vec<u8>>();
    let pname = peer.name.clone();
    if !shared.publish(attempt, |i| {
        i.tx = Some(tx.clone());
        i.dropped = false;
        i.state = State::Connected(pname);
    }) {
        return Err(String::new());
    }
    util::log(&format!("Mac oturumu açıldı: {}", peer.name));
    post(notify);
    if shared.player.lock().unwrap().is_some() {
        let _ = tx.send(vec![MSG_KEYFRAME]);
    }
    drop(tx);
    std::thread::spawn(move || write_loop(writer, tx_aead, out));

    let mut last_data = Instant::now();
    loop {
        let mut len = [0u8; 4];
        if !read_exact_patient(&mut reader, &mut len, &alive, &mut last_data) {
            return Ok(());
        }
        let n = u32::from_be_bytes(len) as usize;
        if !(crypto::TAG_LEN + 1..=MAX_FRAME).contains(&n) {
            return Err("Mac'ten geçersiz veri geldi.".into());
        }
        let mut frame = vec![0u8; n];
        if !read_exact_patient(&mut reader, &mut frame, &alive, &mut last_data) {
            return Ok(());
        }
        let plain = rx_aead.open(&frame).ok_or("Mac'ten gelen veri doğrulanamadı.")?;
        match plain[0] {
            MSG_VIDEO if plain.len() > 2 => {
                if let Some(p) = shared.player.lock().unwrap().as_ref() {
                    p.push(&plain[2..]);
                }
            }
            MSG_SCREEN if plain.len() >= 5 => {
                let size = (u16::from_be_bytes([plain[1], plain[2]]), u16::from_be_bytes([plain[3], plain[4]]));
                if size.0 > 0 && size.1 > 0 {
                    shared.publish(attempt, |i| i.screen = Some(size));
                    post(notify);
                }
            }
            MSG_PONG => {}
            MSG_BYE => return Ok(()),
            _ => {}
        }
    }
}

/// Giden mesajları şifreleyip yazar; arada bir canlılık yoklaması gönderir.
fn write_loop(mut writer: TcpStream, mut aead: Aead, out: Receiver<Vec<u8>>) {
    let started = Instant::now();
    let mut last_ping = Instant::now();
    loop {
        let mut batch = Vec::new();
        match out.recv_timeout(PING_EVERY) {
            Ok(m) => batch.push(m),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        // Birikmiş fare hareketlerinden yalnız sonuncusu gönderilir.
        while let Ok(m) = out.try_recv() {
            if m.first() == Some(&MSG_MOUSE_MOVE) && batch.last().and_then(|l: &Vec<u8>| l.first()) == Some(&MSG_MOUSE_MOVE) {
                batch.pop();
            }
            batch.push(m);
        }
        if last_ping.elapsed() >= PING_EVERY {
            last_ping = Instant::now();
            let mut ping = vec![MSG_PING];
            ping.extend_from_slice(&(started.elapsed().as_millis() as u64).to_be_bytes());
            batch.push(ping);
        }
        let mut bytes = Vec::new();
        for m in &batch {
            let Some(sealed) = aead.seal(m) else { return };
            bytes.extend_from_slice(&(sealed.len() as u32).to_be_bytes());
            bytes.extend_from_slice(&sealed);
        }
        if writer.write_all(&bytes).is_err() {
            return;
        }
        if batch.iter().any(|m| m.first() == Some(&MSG_LEAVE)) {
            return;
        }
    }
}

/// `buf`'ı tamamen doldurur; okuma zaman aşımlarında (1 sn) vazgeçmeyi ve sessizliği denetler.
/// `false`: bağlantı kapandı, sustu ya da vazgeçildi.
fn read_exact_patient(reader: &mut BufReader<TcpStream>, buf: &mut [u8], alive: &dyn Fn() -> bool, last_data: &mut Instant) -> bool {
    use std::io::ErrorKind::{Interrupted, TimedOut, WouldBlock};
    let mut filled = 0;
    while filled < buf.len() {
        if !alive() {
            return false;
        }
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return false,
            Ok(n) => {
                filled += n;
                *last_data = Instant::now();
            }
            Err(e) if matches!(e.kind(), WouldBlock | TimedOut | Interrupted) => {
                if last_data.elapsed() > IDLE_TIMEOUT {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    true
}

/// Tek satır okur: en fazla 512 bayt, toplam onay süresi ve vazgeçme her saniye denetlenir.
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

// ---------------------------------------------------------------------------------------------
// Anahtarlar

fn key_setting(code: &str) -> String {
    format!("MacAnahtari{code}")
}

/// Bu Mac'le eşleşmenin anahtarı (yoksa daha önce eşleşilmemiştir).
fn mac_key(code: &str) -> Option<Vec<u8>> {
    let name = util::wide(&key_setting(code));
    let text = util::read_text_setting(PCWSTR(name.as_ptr()))?;
    unhex(&text).filter(|k| k.len() == 32)
}

/// Sabit sürede karşılaştırma; HMAC hesaplanamazsa eşleşme yok sayılır.
fn proof_matches(key: &[u8], msg: &str, proof: &str) -> bool {
    match (unhex(proof), crypto::hmac(key, msg.as_bytes())) {
        (Some(given), Some(expected)) => ct_eq(&given, &expected),
        _ => false,
    }
}

/// Bu bilgisayarın Mac'teki güvenilenler listesinde tanındığı kalıcı rastgele kimlik.
fn device_id() -> Result<String, String> {
    if let Some(id) = util::read_text_setting(DEVICE_SETTING).filter(|s| s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit())) {
        return Ok(id);
    }
    let id = hex(&crypto::random::<16>().ok_or("Rastgele sayı üretilemedi.")?);
    let _ = util::write_text_setting(DEVICE_SETTING, &id);
    Ok(id)
}

// ---------------------------------------------------------------------------------------------
// Girdi mesajları

/// 0..=65535 aralığına ölçeklenmiş konum.
pub fn mouse_move(x: u16, y: u16) -> Vec<u8> {
    let mut m = vec![MSG_MOUSE_MOVE];
    m.extend_from_slice(&x.to_be_bytes());
    m.extend_from_slice(&y.to_be_bytes());
    m
}

/// `button`: 0 sol, 1 sağ, 2 orta.
pub fn mouse_button(button: u8, down: bool, x: u16, y: u16) -> Vec<u8> {
    let mut m = vec![MSG_MOUSE_BUTTON, button, u8::from(down)];
    m.extend_from_slice(&x.to_be_bytes());
    m.extend_from_slice(&y.to_be_bytes());
    m
}

/// Piksel cinsinden kaydırma (pozitif dy: yukarı, pozitif dx: sağa).
pub fn scroll(dx: i16, dy: i16) -> Vec<u8> {
    let mut m = vec![MSG_SCROLL];
    m.extend_from_slice(&dx.to_be_bytes());
    m.extend_from_slice(&dy.to_be_bytes());
    m
}

/// macOS sanal tuş kodu (kVK_*).
pub fn key(code: u16, down: bool) -> Vec<u8> {
    let mut m = vec![MSG_KEY];
    m.extend_from_slice(&code.to_be_bytes());
    m.push(u8::from(down));
    m
}

/// Windows tarama kodu (genişletilmiş tuşlarda 0xE000 eklenir) → macOS sanal tuş kodu. Konuma
/// göre eşlenir: iki tarafta aynı klavye düzeni (ör. Türkçe Q) seçiliyse karakterler aynı çıkar.
/// Ctrl → Command, Alt → Option, Windows tuşu → Control.
pub fn mac_keycode(scan: u16) -> Option<u16> {
    Some(match scan {
        0x01 => 0x35,
        0x02 => 0x12,
        0x03 => 0x13,
        0x04 => 0x14,
        0x05 => 0x15,
        0x06 => 0x17,
        0x07 => 0x16,
        0x08 => 0x1A,
        0x09 => 0x1C,
        0x0A => 0x19,
        0x0B => 0x1D,
        0x0C => 0x1B,
        0x0D => 0x18,
        0x0E => 0x33,
        0x0F => 0x30,
        0x10 => 0x0C,
        0x11 => 0x0D,
        0x12 => 0x0E,
        0x13 => 0x0F,
        0x14 => 0x11,
        0x15 => 0x10,
        0x16 => 0x20,
        0x17 => 0x22,
        0x18 => 0x1F,
        0x19 => 0x23,
        0x1A => 0x21,
        0x1B => 0x1E,
        0x1C => 0x24,
        0x1D => 0x37,
        0x1E => 0x00,
        0x1F => 0x01,
        0x20 => 0x02,
        0x21 => 0x03,
        0x22 => 0x05,
        0x23 => 0x04,
        0x24 => 0x26,
        0x25 => 0x28,
        0x26 => 0x25,
        0x27 => 0x29,
        0x28 => 0x27,
        0x29 => 0x32,
        0x2A => 0x38,
        0x2B => 0x2A,
        0x2C => 0x06,
        0x2D => 0x07,
        0x2E => 0x08,
        0x2F => 0x09,
        0x30 => 0x0B,
        0x31 => 0x2D,
        0x32 => 0x2E,
        0x33 => 0x2B,
        0x34 => 0x2F,
        0x35 => 0x2C,
        0x36 => 0x3C,
        0x37 => 0x43,
        0x38 => 0x3A,
        0x39 => 0x31,
        0x3A => 0x39,
        0x3B => 0x7A,
        0x3C => 0x78,
        0x3D => 0x63,
        0x3E => 0x76,
        0x3F => 0x60,
        0x40 => 0x61,
        0x41 => 0x62,
        0x42 => 0x64,
        0x43 => 0x65,
        0x44 => 0x6D,
        0x45 => 0x47,
        0x47 => 0x59,
        0x48 => 0x5B,
        0x49 => 0x5C,
        0x4A => 0x4E,
        0x4B => 0x56,
        0x4C => 0x57,
        0x4D => 0x58,
        0x4E => 0x45,
        0x4F => 0x53,
        0x50 => 0x54,
        0x51 => 0x55,
        0x52 => 0x52,
        0x53 => 0x41,
        0x56 => 0x0A,
        0x57 => 0x67,
        0x58 => 0x6F,
        0xE01C => 0x4C,
        0xE01D => 0x36,
        0xE035 => 0x4B,
        0xE038 => 0x3D,
        0xE047 => 0x73,
        0xE048 => 0x7E,
        0xE049 => 0x74,
        0xE04B => 0x7B,
        0xE04D => 0x7C,
        0xE04F => 0x77,
        0xE050 => 0x7D,
        0xE051 => 0x79,
        0xE052 => 0x72,
        0xE053 => 0x75,
        0xE05B => 0x3B,
        0xE05C => 0x3E,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_reply() {
        let ip = Ipv4Addr::new(192, 168, 1, 20);
        let here = parse_here("AYNA1 HERE 123456789 21130 - Burhan's Mac mini\n", "123456789", ip).unwrap();
        assert_eq!(here.peer.port, 21130);
        assert_eq!(here.proof, "-");
        assert_eq!(here.peer.name, "Burhan's Mac mini");
        assert!(parse_here("AYNA1 HERE 999 21130 - x", "123456789", ip).is_none());
        assert_eq!(clean_name("a\u{202E}b\u{200B}c"), "abc");
    }

    #[test]
    fn maps_keys_by_position() {
        assert_eq!(mac_keycode(0x1E), Some(0x00)); // A
        assert_eq!(mac_keycode(0x1D), Some(0x37)); // Ctrl → Command
        assert_eq!(mac_keycode(0xE048), Some(0x7E)); // yukarı ok
        assert_eq!(mac_keycode(0x5D), None);
        assert_eq!(mouse_button(0, true, 1, 2), vec![MSG_MOUSE_BUTTON, 0, 1, 0, 1, 0, 2]);
    }
}
