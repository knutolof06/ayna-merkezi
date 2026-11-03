//! Mac bağlantısı (AYNA2): aynı ağdaki Mac'i koduyla bulur, Mac'teki Ayna Ajan'la şifreli bir oturum
//! açar; Mac'in ekran görüntüsünü (H.264) alır, fareyi ve klavyeyi gönderir. Başka program gerekmez.
//!
//! 1. Bul (UDP 21130): `AYNA1 FIND <kod> <cihaz> <nonce>` →
//!    `AYNA1 HERE <kod> <port> <imza|-> <ad>`. Eşleşilmiş Mac imzalı yanıt verir.
//! 2. El sıkışma (TCP 21130, satırlar):
//!    Windows: `AYNA2 COMMIT <cihaz> <SHA256(win-açık-anahtar|cihaz)>` (anahtara önceden bağlanma);
//!    Mac: `AYNA2 HI <nonce> <mac-açık-anahtar>`;
//!    Windows: `AYNA2 HELLO <cihaz> <imza|-> <win-açık-anahtar> <ad>` (Mac özeti doğrular).
//!    İki taraf geçici ECDH P-256 anahtarlarından ortak sır çıkarır;
//!    T = `AYNA2|nonce|mac-anahtar|win-anahtar|cihaz|ad` bağlamıyla HKDF-SHA256 yön başına AES-256-GCM
//!    anahtarlarını, ilk eşleşme için 6 haneli doğrulama kodunu ve yeni eşleşme anahtarını verir.
//!    Windows anahtarını Mac'inkini görmeden önce bağladığı için aradaki biri iki ekranda aynı kod
//!    çıkana dek anahtar deneyemez. İlk eşleşmede kodu iki tarafta da kullanıcı onaylar.
//!    Eşleşilmiş cihazlar T'yi anahtarla imzalar (HMAC).
//!    Mac: `AYNA2 WAIT` (onay soruluyor) … `AYNA2 OK NEW|SAME <imza>` | `DENY` | `OFF` | `BUSY`.
//! 3. Şifreli çerçeveler: `u32 uzunluk || AES-GCM(şifreli metin || etiket)`, nonce = 4 sıfır bayt ||
//!    64 bit sayaç (yön başına). Düz metnin ilk baytı mesaj türü (bkz. `MSG_*`).
//! 4. Dosyalar aynı şifreli oturumdan geçer. Windows → Mac: `PUT_BEGIN` (toplu iş, dosya no, boyut,
//!    bırakılan nokta, göreli ad) · `PUT_DATA` · `PUT_END`; Mac her dosyadan sonra `PUT_DONE`.
//!    Mac → Windows: Windows `DRAG_QUERY` sorar (Mac'te dosya sürüklenip pencere dışına çıkıldı),
//!    Mac `DRAG_LIST` ile sürüklenen dosyaları bildirir, Windows her birini `GET` ile ister, Mac
//!    `GET_BEGIN` · `GET_DATA` · `GET_END` ile gönderir.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
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
/// Mac → Windows: kip durumu [kip][başarılı][metin].
const MSG_STATE: u8 = 0x05;
// Windows → Mac
pub const MSG_MOUSE_MOVE: u8 = 0x10;
pub const MSG_MOUSE_BUTTON: u8 = 0x11;
pub const MSG_SCROLL: u8 = 0x12;
pub const MSG_KEY: u8 = 0x13;
const MSG_KEYFRAME: u8 = 0x14;
const MSG_PING: u8 = 0x15;
const MSG_PAUSE: u8 = 0x16;
const MSG_LEAVE: u8 = 0x17;
/// Windows → Mac: [0 Mac ekranı | 1 iPhone (Mac'teki iPhone Mirroring)].
const MSG_MODE: u8 = 0x18;
const MSG_PUT_BEGIN: u8 = 0x20;
const MSG_PUT_DATA: u8 = 0x21;
const MSG_PUT_END: u8 = 0x22;
const MSG_DRAG_QUERY: u8 = 0x23;
const MSG_GET: u8 = 0x24;
// Mac → Windows (dosyalar)
const MSG_DRAG_LIST: u8 = 0x30;
const MSG_GET_BEGIN: u8 = 0x31;
const MSG_GET_DATA: u8 = 0x32;
const MSG_GET_END: u8 = 0x33;
const MSG_GET_ERROR: u8 = 0x34;
const MSG_PUT_DONE: u8 = 0x35;

/// Dosya olayı (iş parçacığı → Mac penceresi); WPARAM `FILES_*`.
pub const WM_APP_FILES: u32 = WM_APP + 28;
/// Mac'te sürüklenen dosyalar bildirildi; alınmaya başlanabilir.
pub const FILES_LISTED: usize = 1;
/// Mac'ten sürüklenen dosyaların hepsi geldi.
pub const FILES_READY: usize = 2;
/// Mac'te dosya sürüklenmiyordu.
pub const FILES_NONE: usize = 3;
/// Kullanıcıya gösterilecek not var (`Mac::take_note`).
pub const FILES_NOTE: usize = 4;
/// Gönderim parçası ve gönderilmeyi bekleyen en çok dosya verisi.
const CHUNK: usize = 60_000;
const QUEUE_LIMIT: usize = 4 << 20;
const MAX_ITEMS: usize = 2000;

#[derive(Clone, PartialEq, Debug)]
pub enum State {
    Idle,
    Searching,
    /// Mac'te onay bekleniyor; iki ekranda aynı görünmesi gereken doğrulama kodu.
    Waiting { name: String, sas: String },
    /// Mac onayladı; ilk eşleşmede Windows kullanıcısı da kodun aynı olduğunu onaylamalı.
    Confirm { name: String, sas: String },
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
            State::Confirm { name, .. } => format!("{name}: doğrulama kodunu onaylayın"),
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
    /// İlk eşleşmede Windows kullanıcısının doğrulama kodu kararı (beklerken de verilebilir).
    decision: Option<bool>,
}

struct Shared {
    inner: Mutex<Inner>,
    player: Mutex<Option<Player>>,
    /// Yazıcı kuyruğundaki dosya verisi (bayt): gönderen bunu aşınca bekler. Oturum başına yenidir;
    /// yazıcı biterken sıfırlanır (bekleyen gönderici uyanır, kapanmış kanalı görüp durur).
    queued: Mutex<Arc<AtomicUsize>>,
    /// Mac'e gönderilen toplu işler: beklenen ve tamamlanan dosya sayısı.
    batches: Mutex<HashMap<u32, (usize, usize)>>,
    /// Mac'ten gelen (sürüklenen) dosyalar.
    incoming: Mutex<Option<DragIn>>,
    /// Mac'e "dosya sürükleniyor mu?" soruldu; Mac'in listesi yalnız o zaman kabul edilir.
    drag_asked: AtomicBool,
    /// iPhone kipi istendi (Mac'teki iPhone Mirroring gösterilir); ağ kopup yeniden bağlanınca da sürer.
    phone: AtomicBool,
    /// Kip istendi, Mac henüz yanıt vermedi (eski Ayna Ajan isteği tanımaz, hiç yanıt gelmez).
    phone_asked: Mutex<Option<Instant>>,
    /// Yeniden bağlanınca kendiliğinden yinelenen istek: yalnız hata bildirilir (bildirim yağmuru olmasın).
    phone_quiet: AtomicBool,
    note: Mutex<Option<String>>,
}

/// Mac'ten sürüklenip alınan dosyalar.
struct DragIn {
    dir: PathBuf,
    /// Doğrulanmış göreli yollar (`dir` altında kalır).
    names: Vec<PathBuf>,
    /// Mac'in bildirdiği boyut: fazlası yazılmaz.
    left: Vec<u64>,
    files: Vec<Option<std::fs::File>>,
    /// Sıra başına: bitti mi (bitiş iletisi yinelenirse bir kez sayılır).
    finished: Vec<bool>,
    done: usize,
}

/// Dosya numaraları tüm toplu işlerde tektir (Mac dosyaları numarayla ayırır; üst üste iki
/// bırakma karışmasın).
static NEXT_FILE_ID: AtomicU32 = AtomicU32::new(0);
/// Mac'ten sürüklenen her parti kendi geçici klasörüne.
static NEXT_DRAG_DIR: AtomicU32 = AtomicU32::new(0);

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
                inner: Mutex::new(Inner {
                    state: State::Idle,
                    attempt: 0,
                    tx: None,
                    socket: None,
                    screen: None,
                    dropped: false,
                    decision: None,
                }),
                player: Mutex::new(None),
                queued: Mutex::new(Arc::new(AtomicUsize::new(0))),
                batches: Mutex::new(HashMap::new()),
                incoming: Mutex::new(None),
                drag_asked: AtomicBool::new(false),
                phone: AtomicBool::new(false),
                phone_asked: Mutex::new(None),
                phone_quiet: AtomicBool::new(false),
                note: Mutex::new(None),
            }),
        }
    }

    /// Dosya ve klasörleri Mac'e gönderir; (x, y) Mac penceresinde bırakılan nokta (0..65535).
    /// Mac, o noktadaki Finder klasörüne (yoksa Masaüstü'ne) kaydeder. Bağlı değilse false.
    pub fn send_files(&self, paths: Vec<PathBuf>, x: u16, y: u16) -> bool {
        let Some(tx) = self.shared.inner.lock().unwrap().tx.clone() else { return false };
        let queued = self.shared.queued.lock().unwrap().clone();
        let (shared, notify) = (self.shared.clone(), self.notify.0 as isize);
        std::thread::spawn(move || {
            let mut files = Vec::new();
            for p in &paths {
                let base = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                collect(p, &base, &mut files, 0);
            }
            files.truncate(MAX_ITEMS);
            if files.is_empty() {
                return;
            }
            let batch = crypto::random::<4>().map_or(1, u32::from_be_bytes);
            shared.batches.lock().unwrap().insert(batch, (files.len(), 0));
            for (path, rel) in files {
                let id = NEXT_FILE_ID.fetch_add(1, Ordering::SeqCst);
                match send_file(&tx, &queued, batch, id, &path, &rel, x, y) {
                    Sent::Done => {}
                    Sent::Skipped => {
                        let mut batches = shared.batches.lock().unwrap();
                        if let Some(entry) = batches.get_mut(&batch) {
                            entry.0 -= 1;
                        }
                        finish_batch(&shared, &mut batches, batch, None, notify);
                    }
                    Sent::Closed => break,
                }
            }
        });
        true
    }

    /// Mac'te dosya sürükleniyor mu? Yanıt `WM_APP_FILES` ile gelir.
    pub fn query_drag(&self) {
        self.shared.drag_asked.store(true, Ordering::SeqCst);
        self.send(vec![MSG_DRAG_QUERY]);
    }

    /// Bildirilen sürüklenen dosyaları ister.
    pub fn request_dragged(&self) {
        let count = self.shared.incoming.lock().unwrap().as_ref().map_or(0, |d| d.names.len());
        for i in 0..count as u32 {
            let mut m = vec![MSG_GET];
            m.extend_from_slice(&i.to_be_bytes());
            self.send(m);
        }
    }

    pub fn dragged_count(&self) -> usize {
        self.shared.incoming.lock().unwrap().as_ref().map_or(0, |d| d.names.len())
    }

    /// Alınmış sürüklenen dosyalar: en üst düzey öğeler (dosya ya da klasör), hepsi aynı klasörde.
    pub fn take_dragged(&self) -> Vec<PathBuf> {
        let Some(d) = self.shared.incoming.lock().unwrap().take() else { return Vec::new() };
        let mut top: Vec<PathBuf> = Vec::new();
        for n in &d.names {
            if let Some(first) = n.components().next() {
                let p = d.dir.join(first);
                if p.parent() == Some(d.dir.as_path()) && !top.contains(&p) && p.exists() {
                    top.push(p);
                }
            }
        }
        top
    }

    /// iPhone kipi: Mac penceresinde Mac ekranı yerine Mac'teki iPhone Mirroring görünür.
    pub fn set_phone(&self, on: bool) {
        self.shared.phone.store(on, Ordering::SeqCst);
        self.shared.phone_quiet.store(false, Ordering::SeqCst);
        *self.shared.phone_asked.lock().unwrap() = on.then(Instant::now);
        self.send(vec![MSG_MODE, u8::from(on)]);
    }

    /// Kullanıcı bağlantıyı kesti: bir sonraki bağlanış Mac ekranıyla başlar.
    pub fn clear_phone(&self) {
        self.shared.phone.store(false, Ordering::SeqCst);
        *self.shared.phone_asked.lock().unwrap() = None;
    }

    /// Zamanlayıcıdan: istek birkaç saniyede yanıtlanmadıysa Mac'teki ajan bu kipi bilmiyordur.
    pub fn check_phone_answer(&self) {
        let mut asked = self.shared.phone_asked.lock().unwrap();
        if asked.is_some_and(|t| t.elapsed() > Duration::from_secs(5)) && self.connected() {
            *asked = None;
            self.shared.phone.store(false, Ordering::SeqCst);
            *self.shared.note.lock().unwrap() =
                Some("Mac'teki Ayna Ajan bu özelliği bilmiyor: Mac'te menüden \"Güncellemeleri denetle\".".into());
            post_files(self.notify.0 as isize, FILES_NOTE);
        }
    }

    pub fn phone(&self) -> bool {
        self.shared.phone.load(Ordering::SeqCst)
    }

    pub fn take_note(&self) -> Option<String> {
        self.shared.note.lock().unwrap().take()
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
        let old = std::mem::replace(&mut *self.shared.player.lock().unwrap(), player);
        // Eski hat kilit dışında ve ayrı iş parçacığında kapatılır (kapanırken ağ ve arayüz beklemesin).
        if let Some(old) = old {
            std::thread::spawn(move || drop(old));
        }
        if fresh {
            self.send(vec![MSG_KEYFRAME]);
        }
    }

    /// İlk eşleşmede: iki ekrandaki doğrulama kodu aynı mı?
    pub fn confirm(&self, same: bool) {
        self.shared.inner.lock().unwrap().decision = Some(same);
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
            inner.decision = None;
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
                    // Mac'te bağlantı kesildi: kendiliğinden yeniden bağlanılmaz.
                    Ok(true) => {
                        inner.dropped = false;
                        inner.state = State::Failed("Mac'te bağlantı kesildi.".into());
                    }
                    Ok(false) => {
                        // Bağlantı koptu (ağ, uyku).
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
        util::delete_setting(PCWSTR(util::wide(&address_setting(code)).as_ptr()));
    }
}

/// Ağdan kaynaklanan, yeniden denemeye değer hatalar (ret ya da kimlik hatası değil).
fn transient(why: &str) -> bool {
    why.starts_with("Mac bulunamadı")
        || why.starts_with("Mac'teki Ayna Ajan'a bağlanılamadı")
        || why.starts_with("Mac bağlantıyı kapattı")
        || why.starts_with("Mac'e şu an başka")
        // Son bilinen adreste artık başka bir cihaz olabilir (Mac'in adresi değişti): yeniden denenir.
        || why.starts_with("Mac'ten beklenmeyen yanıt")
        || why.starts_with("Mac'ten geçersiz el sıkışma")
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

/// Bulur, el sıkışır ve oturum bitene dek görüntüyü alır. `Ok(true)`: Mac oturumu kapattı;
/// `Ok(false)`: bağlantı koptu ya da vazgeçildi.
fn session(code: &str, shared: &Arc<Shared>, attempt: u32, notify: isize) -> Result<bool, String> {
    let peer = match discover(code)? {
        Found::Signed(peer) | Found::FirstTime(peer) => peer,
        Found::UnsignedOnly(peer) => {
            // Soru açıkken vazgeçilmiş olabilir: eşleşme ancak deneme hâlâ geçerliyse silinir.
            if !shared.alive(attempt) || !ask_repair(notify, &peer) || !shared.alive(attempt) {
                return Err("Mac'in kimliği doğrulanamadı; eşleşme yenilenmedi.".into());
            }
            Mac::forget(code);
            util::log(&format!("Mac kodu {code} için eşleşme kullanıcı onayıyla sıfırlandı."));
            peer
        }
        // Bulma yanıtı gelmedi: eşleşilmiş Mac'in son bilinen adresi doğrudan denenir. macOS'un
        // "Yerel Ağ" izni güncelleme sonrası sıfırlanınca Mac UDP ile yanıt veremez ama gelen TCP
        // bağlantısını kabul eder. Kimlik yine el sıkışmada kayıtlı anahtarla doğrulanır (OK SAME).
        Found::Nothing => match last_address(code).filter(|_| mac_key(code).is_some()) {
            Some(peer) => {
                util::log(&format!("Bulma yanıtı yok; son bilinen adres deneniyor: {}:{}", peer.ip, peer.port));
                peer
            }
            None => return Err("Mac bulunamadı. Mac'te Ayna Ajan açık mı, iki cihaz aynı ağda mı?".into()),
        },
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
    let id = device_id()?;
    let ecdh = EcdhKey::generate().ok_or("Anahtar üretilemedi.")?;
    let win_pub = hex(&ecdh.public_x963().ok_or("Anahtar üretilemedi.")?);
    // Açık anahtar, Mac'inki görülmeden önce özetiyle bağlanır (doğrulama kodu denenerek eşitlenemesin).
    let commit = hex(&crypto::sha256(format!("{win_pub}|{id}").as_bytes()).ok_or("Özet hesaplanamadı.")?);
    writer
        .write_all(format!("AYNA2 COMMIT {id} {commit}\n").as_bytes())
        .map_err(|e| format!("Mac'e ulaşılamadı ({e})."))?;
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
    let shared_secret = ecdh.agree(&mac_pub_bytes).ok_or("Mac'in anahtarı geçersiz.")?;
    let name = clean_name(&std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".into())).replace(' ', "_");
    let name = if name.is_empty() { "Windows".to_string() } else { name };
    let t = format!("AYNA2|{nonce}|{mac_pub}|{win_pub}|{id}|{name}");
    let key = mac_key(code);
    let proof = match &key {
        Some(k) => hex(&crypto::hmac(k, format!("hello|{t}").as_bytes()).ok_or("İmza hesaplanamadı.")?),
        None => "-".to_string(),
    };
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
                        // Sahte bir Mac de "OK" diyebilir: eşleşme, Windows kullanıcısı Mac'in
                        // ekranında aynı kodu gördüğünü onaylamadan kurulmaz.
                        wait_for_decision(shared, attempt, notify, &peer.name, &sas, &alive)?;
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
    let saved = format!("{}:{} {}", peer.ip, peer.port, peer.name);
    let _ = util::write_text_setting(PCWSTR(util::wide(&address_setting(code)).as_ptr()), &saved);
    post(notify);
    if shared.player.lock().unwrap().is_some() {
        let _ = tx.send(vec![MSG_KEYFRAME]);
    }
    if shared.phone.load(Ordering::SeqCst) {
        shared.phone_quiet.store(true, Ordering::SeqCst);
        *shared.phone_asked.lock().unwrap() = Some(Instant::now());
        let _ = tx.send(vec![MSG_MODE, 1]);
    }
    drop(tx);
    // Ortak zaman başlangıcı: yoklama (ping) zaman damgası yanıtla (pong) geri gelir, gidiş-dönüş
    // süresi ölçülür (Mac'te biriken gönderim kuyruğu da bunda görünür).
    let epoch = Instant::now();
    let queued = Arc::new(AtomicUsize::new(0));
    *shared.queued.lock().unwrap() = queued.clone();
    shared.batches.lock().unwrap().clear();
    std::thread::spawn(move || {
        write_loop(writer, tx_aead, out, epoch, &queued);
        queued.store(0, Ordering::SeqCst);
    });

    let mut stats = Stats::new();
    let mut last_data = Instant::now();
    loop {
        let mut len = [0u8; 4];
        if !read_exact_patient(&mut reader, &mut len, &alive, &mut last_data) {
            return Ok(false);
        }
        let n = u32::from_be_bytes(len) as usize;
        if !(crypto::TAG_LEN + 1..=MAX_FRAME).contains(&n) {
            return Err("Mac'ten geçersiz veri geldi.".into());
        }
        let mut frame = vec![0u8; n];
        if !read_exact_patient(&mut reader, &mut frame, &alive, &mut last_data) {
            return Ok(false);
        }
        let plain = rx_aead.open(&frame).ok_or("Mac'ten gelen veri doğrulanamadı.")?;
        stats.report(&peer.name);
        match plain[0] {
            MSG_VIDEO if plain.len() > 2 => {
                stats.frame(plain.len(), plain[1] & 1 == 1);
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
            MSG_PONG if plain.len() >= 9 => {
                let sent = u64::from_be_bytes(plain[1..9].try_into().unwrap_or([0; 8]));
                stats.rtt(epoch.elapsed().as_millis() as u64 - sent.min(epoch.elapsed().as_millis() as u64));
            }
            MSG_BYE => return Ok(true),
            MSG_STATE if plain.len() >= 3 => {
                let (mode, ok) = (plain[1], plain[2] == 1);
                *shared.phone_asked.lock().unwrap() = None;
                // Mac iPhone kipini açamadıysa ya da kipten çıktıysa istek düşer.
                if !ok || mode == 0 {
                    shared.phone.store(false, Ordering::SeqCst);
                }
                let text: String = String::from_utf8_lossy(&plain[3..]).chars().take(200).collect();
                let quiet = ok && shared.phone_quiet.load(Ordering::SeqCst);
                if !text.is_empty() && !quiet {
                    util::log(&format!("Mac iPhone kipi: {text}"));
                    *shared.note.lock().unwrap() = Some(text);
                    post_files(notify, FILES_NOTE);
                }
            }
            MSG_DRAG_LIST..=MSG_PUT_DONE => receive_file_message(shared, &plain, notify),
            _ => {}
        }
    }
}

/// İlk eşleşmede kullanıcının "kodlar aynı" onayını bekler.
fn wait_for_decision(
    shared: &Shared,
    attempt: u32,
    notify: isize,
    name: &str,
    sas: &str,
    alive: &dyn Fn() -> bool,
) -> Result<(), String> {
    let (n, s) = (name.to_string(), sas.to_string());
    if !shared.publish(attempt, |i| {
        if i.decision.is_none() {
            i.state = State::Confirm { name: n, sas: s };
        }
    }) {
        return Err(String::new());
    }
    post(notify);
    let started = Instant::now();
    loop {
        if !alive() {
            return Err(String::new());
        }
        match shared.inner.lock().unwrap().decision {
            Some(true) => return Ok(()),
            Some(false) => return Err("Doğrulama kodu onaylanmadı; eşleşme kurulmadı.".into()),
            None => {}
        }
        if started.elapsed() > APPROVAL_TIMEOUT {
            return Err("Doğrulama kodu onaylanmadı (süre doldu).".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------------------------
// Dosyalar

/// Bir dosyayı ya da klasörü (içiyle) göreli adlarla listeler; bağlantılar ve çok derin yapılar atlanır.
fn collect(path: &Path, rel: &str, out: &mut Vec<(PathBuf, String)>, depth: usize) {
    if out.len() >= MAX_ITEMS || depth > 32 {
        return;
    }
    let Ok(meta) = std::fs::symlink_metadata(path) else { return };
    if meta.file_type().is_symlink() {
        return;
    }
    if meta.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            collect(&e.path(), &format!("{rel}/{name}"), out, depth + 1);
        }
    } else if meta.is_file() {
        out.push((path.to_path_buf(), rel.to_string()));
    }
}

enum Sent {
    Done,
    /// Açılamadı (kilitli, silinmiş): Mac'e hiç gitmedi.
    Skipped,
    /// Bağlantı kapandı.
    Closed,
}

#[allow(clippy::too_many_arguments)]
fn send_file(tx: &Sender<Vec<u8>>, queued: &AtomicUsize, batch: u32, id: u32, path: &Path, rel: &str, x: u16, y: u16) -> Sent {
    use std::io::Read as _;
    let Ok(mut f) = std::fs::File::open(path) else { return Sent::Skipped };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let name = rel.as_bytes();
    let mut m = vec![MSG_PUT_BEGIN];
    m.extend_from_slice(&batch.to_be_bytes());
    m.extend_from_slice(&id.to_be_bytes());
    m.extend_from_slice(&size.to_be_bytes());
    m.extend_from_slice(&x.to_be_bytes());
    m.extend_from_slice(&y.to_be_bytes());
    m.extend_from_slice(&(name.len().min(u16::MAX as usize) as u16).to_be_bytes());
    m.extend_from_slice(&name[..name.len().min(u16::MAX as usize)]);
    if tx.send(m).is_err() {
        return Sent::Closed;
    }
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        // Geri basınç: yazıcı yetişene dek beklenir (bellek şişmez, görüntü ve fare de akar).
        while queued.load(Ordering::SeqCst) > QUEUE_LIMIT {
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut d = Vec::with_capacity(5 + n);
        d.push(MSG_PUT_DATA);
        d.extend_from_slice(&id.to_be_bytes());
        d.extend_from_slice(&buf[..n]);
        queued.fetch_add(d.len(), Ordering::SeqCst);
        if tx.send(d).is_err() {
            return Sent::Closed;
        }
    }
    let mut e = vec![MSG_PUT_END];
    e.extend_from_slice(&id.to_be_bytes());
    if tx.send(e).is_ok() { Sent::Done } else { Sent::Closed }
}

/// Toplu işte beklenen dosyaların hepsi Mac'e ulaştıysa bildirim yazılır.
fn finish_batch(shared: &Shared, batches: &mut HashMap<u32, (usize, usize)>, batch: u32, dest: Option<&str>, notify: isize) {
    let Some(&(expected, done)) = batches.get(&batch) else { return };
    if done < expected && expected > 0 {
        return;
    }
    batches.remove(&batch);
    if done == 0 {
        return;
    }
    let folder = dest.map(|d| d.trim_end_matches('/').rsplit('/').next().unwrap_or("Mac").to_string());
    *shared.note.lock().unwrap() = Some(match folder {
        Some(f) => format!("Mac'e kopyalandı: {done} dosya → {f}"),
        None => format!("Mac'e kopyalandı: {done} dosya"),
    });
    post_files(notify, FILES_NOTE);
}

/// Mac'ten gelen göreli adı güvenli bir Windows yoluna çevirir (`..`, mutlak yol, sürücü yok).
fn safe_relative(name: &str) -> Option<PathBuf> {
    let mut p = PathBuf::new();
    for part in name.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains(['\\', ':', '<', '>', '"', '|', '?', '*']) || part.chars().any(|c| c.is_control()) {
            return None;
        }
        // CON, NUL, COM1… ve sonda nokta/boşluk Windows'ta başka bir şeye (aygıta, kırpılmış ada) gider.
        let stem = part.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit());
        if reserved || part.ends_with(['.', ' ']) {
            return None;
        }
        p.push(part);
    }
    (!p.as_os_str().is_empty()).then_some(p)
}

/// Önceki sürüklemelerin bir saatten eski geçici klasörleri (bu sürüklemeninki hariç).
fn clean_old_drags(root: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for e in entries.flatten() {
        let path = e.path();
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > Duration::from_secs(3600));
        if path != keep && old {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn post_files(notify: isize, what: usize) {
    unsafe {
        let _ = PostMessageW(HWND(notify as _), WM_APP_FILES, WPARAM(what), LPARAM(0));
    }
}

fn receive_file_message(shared: &Shared, m: &[u8], notify: isize) {
    use std::io::Write as _;
    match m[0] {
        MSG_DRAG_LIST => {
            // Yalnız bizim sorumuza gelen yanıt; Mac kendiliğinden dosya dayatamaz.
            if !shared.drag_asked.swap(false, Ordering::SeqCst) {
                return;
            }
            let count = m.get(1..3).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]) as usize);
            let mut names = Vec::new();
            let mut left = Vec::new();
            let mut at = 3;
            for _ in 0..count.min(MAX_ITEMS) {
                let (Some(size), Some(len)) = (be64(m, at), m.get(at + 8..at + 10)) else { break };
                let len = u16::from_be_bytes([len[0], len[1]]) as usize;
                let Some(name) = m.get(at + 10..at + 10 + len) else { break };
                // Uygunsuz bir ad (mutlak yol, "..", sürücü, ağ yolu) varsa liste bütünüyle reddedilir.
                let Some(rel) = std::str::from_utf8(name).ok().and_then(safe_relative) else {
                    util::log("Mac'ten gelen sürükleme listesinde uygunsuz ad: reddedildi.");
                    post_files(notify, FILES_NONE);
                    return;
                };
                names.push(rel);
                left.push(size);
                at += 10 + len;
            }
            if names.is_empty() {
                post_files(notify, FILES_NONE);
                return;
            }
            // Her sürükleme kendi geçici klasörüne. Kabuk bırakmadan sonra da arka planda
            // kopyalayabildiği için önceki klasörler hemen silinmez; bir saatten eskiler temizlenir.
            let root = std::env::temp_dir().join("AynaMerkezi-Mac");
            let n = NEXT_DRAG_DIR.fetch_add(1, Ordering::SeqCst);
            let dir = root.join(format!("{}-{n}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let keep = dir.clone();
            std::thread::spawn(move || clean_old_drags(&root, &keep));
            let files = names.iter().map(|_| None).collect();
            let finished = vec![false; names.len()];
            *shared.incoming.lock().unwrap() = Some(DragIn { dir, names, left, files, finished, done: 0 });
            post_files(notify, FILES_LISTED);
        }
        MSG_GET_BEGIN | MSG_GET_DATA | MSG_GET_END | MSG_GET_ERROR => {
            let Some(index) = be32(m, 1).map(|i| i as usize) else { return };
            let mut guard = shared.incoming.lock().unwrap();
            let Some(d) = guard.as_mut() else { return };
            if index >= d.names.len() {
                return;
            }
            match m[0] {
                MSG_GET_BEGIN => {
                    if !d.finished[index] && d.files[index].is_none() {
                        let path = d.dir.join(&d.names[index]);
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        d.files[index] = std::fs::File::create(path).ok();
                    }
                }
                MSG_GET_DATA => {
                    let data = &m[5..];
                    let n = (data.len() as u64).min(d.left[index]) as usize;
                    d.left[index] -= n as u64;
                    if let Some(f) = d.files[index].as_mut() {
                        let _ = f.write_all(&data[..n]);
                    }
                }
                _ => {
                    d.files[index] = None;
                    if std::mem::replace(&mut d.finished[index], true) {
                        return;
                    }
                    d.done += 1;
                    if d.done == d.names.len() {
                        drop(guard);
                        post_files(notify, FILES_READY);
                    }
                }
            }
        }
        MSG_PUT_DONE => {
            let (Some(batch), Some(count)) = (be32(m, 1), m.get(5..7)) else { return };
            let count = u16::from_be_bytes([count[0], count[1]]) as usize;
            let dest = String::from_utf8_lossy(m.get(7..).unwrap_or_default()).to_string();
            let mut batches = shared.batches.lock().unwrap();
            if let Some(entry) = batches.get_mut(&batch) {
                entry.1 = count;
                finish_batch(shared, &mut batches, batch, Some(&dest), notify);
            }
        }
        _ => {}
    }
}

/// Oturum ölçümleri: 10 sn'de bir günlüğe kare hızı, bant genişliği ve gidiş-dönüş süresi.
struct Stats {
    since: Instant,
    frames: u32,
    keys: u32,
    bytes: usize,
    rtts: Vec<u64>,
}

impl Stats {
    fn new() -> Stats {
        Stats { since: Instant::now(), frames: 0, keys: 0, bytes: 0, rtts: Vec::new() }
    }

    fn frame(&mut self, len: usize, key: bool) {
        self.frames += 1;
        self.keys += u32::from(key);
        self.bytes += len;
    }

    fn rtt(&mut self, ms: u64) {
        self.rtts.push(ms);
    }

    fn report(&mut self, name: &str) {
        let secs = self.since.elapsed().as_secs_f32();
        if secs < 10.0 {
            return;
        }
        let avg = if self.rtts.is_empty() { 0 } else { self.rtts.iter().sum::<u64>() / self.rtts.len() as u64 };
        let max = self.rtts.iter().copied().max().unwrap_or(0);
        util::log(&format!(
            "Mac {name}: {:.0} kare/sn ({} anahtar), {:.1} Mbit/sn, gidiş-dönüş ort {avg} ms en çok {max} ms",
            self.frames as f32 / secs,
            self.keys,
            self.bytes as f32 * 8.0 / secs / 1_000_000.0,
        ));
        *self = Stats::new();
    }
}

/// Giden mesajları şifreleyip yazar; arada bir canlılık yoklaması gönderir.
fn write_loop(mut writer: TcpStream, mut aead: Aead, out: Receiver<Vec<u8>>, started: Instant, queued: &AtomicUsize) {
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
        let sent: usize = batch.iter().filter(|m| m.first() == Some(&MSG_PUT_DATA)).map(|m| m.len()).sum();
        if sent > 0 {
            queued.fetch_sub(sent.min(queued.load(Ordering::SeqCst)), Ordering::SeqCst);
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

fn address_setting(code: &str) -> String {
    format!("MacAdresi{code}")
}

/// Bu Mac'le son kurulan oturumun adresi ve adı ("ip:port ad").
fn last_address(code: &str) -> Option<Peer> {
    let text = util::read_text_setting(PCWSTR(util::wide(&address_setting(code)).as_ptr()))?;
    let (addr, name) = text.split_once(' ').unwrap_or((text.as_str(), "Mac"));
    let (ip, port) = addr.split_once(':')?;
    let ip: Ipv4Addr = ip.parse().ok()?;
    // Yalnız bu bilgisayar o ağdayken (aynı /24): başka bir ağda aynı adres başka bir cihazdır.
    let same_net = local_ipv4().iter().any(|l| l.octets()[..3] == ip.octets()[..3]);
    if !(ip.is_private() || ip.is_link_local()) || !same_net {
        return None;
    }
    Some(Peer { ip, port: port.parse().ok()?, name: clean_name(name) })
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
        let here = parse_here("AYNA1 HERE 123456789 21130 - Ev Mac mini\n", "123456789", ip).unwrap();
        assert_eq!(here.peer.port, 21130);
        assert_eq!(here.proof, "-");
        assert_eq!(here.peer.name, "Ev Mac mini");
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
