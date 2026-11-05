//! iPhone ekranının gösterildiği pencere.
//!
//! Çerçevesiz, köşeleri iPhone gibi yuvarlatılmış bir açılır pencere; görev çubuğundaki ikonun
//! tam üstünde, ekran yüksekliğinde açılır. UxPlay'in Direct3D video penceresi bunun çocuğu
//! yapılır ve devre dışı bırakılır: devre dışı çocuk pencereye gelen fare mesajları ebeveyne
//! (buraya) gider. Görüntü üzerindeki tıklama ve sürüklemeler parmak dokunuşuna çevrilip
//! Bluetooth ile iPhone'a gönderilir.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{DragAcceptFiles, ShellExecuteW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::ble::{self, Ble, Status, Step};
use crate::buttons;
use crate::corners;
use crate::hid;
use crate::i18n::{self, tf};
use crate::macview;
use crate::phoneview;
use crate::taskbar;
use crate::toast;
use crate::transfer::{self, Target};
use crate::util::{self, contains, height, rgb, width, wide};
use crate::uxplay::{self, Engine, Geometry};

pub const WM_APP_TOGGLE: u32 = WM_APP + 1;
/// Görev çubuğu ikonu yer değiştirdi; pencere yeniden ikonun üstüne yerleşir.
pub const WM_APP_PLACE: u32 = WM_APP + 3;
/// İkinci kez başlatılan kopya: pencere açıksa da gizlemez, öne getirir.
pub const WM_APP_SHOW: u32 = WM_APP + 6;
/// Arka plandaki kopyalama bitti; metin `SENT` kuyruğundadır (mesaj işaretçi taşımaz).
const WM_APP_SENT: u32 = WM_APP + 5;

/// Kopyalama iş parçacığının sonuç metinleri.
static SENT: Mutex<Vec<String>> = Mutex::new(Vec::new());
pub const CLASS: PCWSTR = w!("iPhoneAyna.Ayna");
const TIMER_POLL: usize = 1;
const MENU_HIDE: usize = 1;
const MENU_EXIT: usize = 2;

/// iPhone 15/16 dikey ekran oranı; video gelince gerçek orana güncellenir.
const DEFAULT_ASPECT: f32 = 1179.0 / 2556.0;
/// iPhone ekran köşesi yarıçapının kısa kenara oranı (≈ 55 pt / 393 pt).
const CORNER_RATIO: f32 = 0.14;
/// Pencerenin çalışma alanı kenarlarına uzaklığı.
const MARGIN: f32 = 10.0;
/// Pencere boyu, çalışma alanı yüksekliğinin yüzdesi; Ctrl + tekerlek ile değişir, hatırlanır.
const SIZE_DEFAULT: u32 = 78;
pub const SIZE_MIN: u32 = 40;
pub const SIZE_MAX: u32 = 100;
/// iPhone ana uygulamada açık mı (kapalıyken UxPlay çalışmaz, ikon görünmez).
const ENABLED_SETTING: PCWSTR = w!("iPhoneAcik");
const SIZE_SETTING: PCWSTR = w!("BoyutYuzde");
/// Yansıtma sürerken iPhone'un otomatik kilitlenmemesi için zararsız Shift vuruşu aralığı
/// (iOS'ta en kısa otomatik kilit 30 sn).
const KEEP_AWAKE: Duration = Duration::from_secs(20);
/// Bluetooth (yeniden) bağlandıktan sonra Büyütme'nin klavye kısayoluyla yenilenmesi için bekleme.
const ZOOM_REARM_DELAY: Duration = Duration::from_millis(1500);
/// 0 yapılırsa Büyütme kendiliğinden yenilenmez.
const ZOOM_REARM_SETTING: PCWSTR = w!("BuyutmeYenile");
/// Panodan bir seferde yazılacak en fazla karakter (iPhone'a ~30 karakter/sn gider).
/// iOS güç tuşunu hangi HID kodundan tanıyor, iPhone modeline ve sürümüne göre değişiyor
/// (Consumer 0x30 kullanıcının iPhone'unda işe yaramadı). Yöntemler sırayla denenir; kilitlenen
/// iPhone yansıtmayı kestiği için işe yarayan anlaşılır ve kayıt defterinde hatırlanır.
#[derive(Clone, Copy)]
enum LockMethod {
    Consumer(u16),
    Key(u8, u8),
}

const LOCK_METHODS: [LockMethod; 6] = [
    LockMethod::Consumer(hid::CONSUMER_LOCK),
    LockMethod::Key(0, hid::USAGE_KEYBOARD_POWER),
    LockMethod::Consumer(hid::CONSUMER_SLEEP_MODE),
    LockMethod::Consumer(hid::CONSUMER_SLEEP),
    LockMethod::Key(hid::MOD_LEFT_CTRL | hid::MOD_LEFT_GUI, hid::USAGE_Q),
    LockMethod::Consumer(hid::CONSUMER_POWER),
];
const LOCK_SETTING: PCWSTR = w!("GucYontemi");
/// Kayıtlı yöntem en az bir kez iPhone'u kilitledi; tek bir başarısızlıkla bırakılmaz.
const LOCK_VERIFIED_SETTING: PCWSTR = w!("GucYontemiDogrulandi");
/// Kilitlenen iPhone yansıtmayı bu süre içinde keser; kesmezse yöntem işe yaramamıştır.
const LOCK_PROBE: Duration = Duration::from_secs(10);

const PASTE_LIMIT: usize = 5000;
const CF_UNICODETEXT: u32 = 13;

/// Ödünç (borrow) bırakıldıktan sonra yapılacak işler: bunlar aynı pencereye eşzamanlı mesaj
/// gönderip pencere yordamına yeniden girebilir.
enum After {
    ReleaseCapture,
    Embed(HWND),
    Place,
    EnsureIcon,
    RecreateIcon,
    RefreshIcon,
    Toggle,
    Hide,
    Destroy,
    Menu(POINT),
    Send(Vec<PathBuf>),
    Toast(String),
    Paste,
    Show,
}

struct Mirror {
    hwnd: HWND,
    engine: Engine,
    ble: Ble,
    video: Option<HWND>,
    video_size: (i32, i32),
    ignore_resize_until: Instant,
    aspect: f32,
    content: RECT,
    touching: bool,
    last_touch: (u16, u16),
    wheel_accum: i32,
    /// iPhone'un gönderdiği karenin geometrisi (UxPlay günlüğünden); siyah dolguyu kırpmak için.
    geometry: Option<Geometry>,
    keyboard: hid::Keyboard,
    /// AltGr'nin önünden gelen sahte sol Ctrl yutuldu; bırakılışı da yutulacak.
    fake_ctrl: bool,
    last_nudge: Instant,
    last_wake: Option<Instant>,
    keyboard_was_ready: bool,
    /// Bluetooth yeniden bağlandı; yansıtma açıkken Büyütme yenilenecek.
    zoom_rearm_pending: bool,
    zoom_rearm_at: Option<Instant>,
    /// Aç/kapa çiftinin ikinci vuruşu (BLE iş parçacığı beklemesin diye burada zamanlanır).
    zoom_second_at: Option<Instant>,
    /// Denenen kilitleme yöntemi ve deneme zamanı; yansıtma kesilirse yöntem doğrulanır.
    lock_probe: Option<(Instant, usize)>,
    /// Kayıtlı yöntemin art arda başarısızlık sayısı.
    lock_failures: u8,
    scale: f32,
    font: HFONT,
    font_title: HFONT,
    taskbar_created: u32,
}

thread_local! {
    static STATE: RefCell<Option<Mirror>> = const { RefCell::new(None) };
    /// Açık bir dosya gönderme iletişim kutusu varken ikinci bir bırakma yeni kutu açmasın.
    static DIALOG_OPEN: Cell<bool> = const { Cell::new(false) };
    /// Son kurulan yuvarlak pencere bölgesinin boyutu; aynıysa bölge yeniden kurulmaz.
    static REGION_SIZE: Cell<(i32, i32)> = const { Cell::new((0, 0)) };
    /// Şu an işlenen sol Ctrl basışının ardından aynı anda sağ Alt geliyor (AltGr).
    static ALTGR_NEXT: Cell<bool> = const { Cell::new(false) };
    static MIRROR: Cell<isize> = const { Cell::new(0) };
}

pub fn create(ble: Ble, engine: Engine) -> Option<HWND> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None).ok()?.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hIcon: taskbar::app_icon(32),
            hIconSm: taskbar::app_icon(16),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        // Sahibi olan pencerenin görev çubuğu düğmesi olmaz: tek giriş Başlat'ın solundaki ikon.
        let owner = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            HWND::default(),
            HMENU::default(),
            instance,
            None,
        )
        .unwrap_or_default();

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            CLASS,
            w!("iPhone"),
            WS_POPUP | WS_CLIPCHILDREN,
            0,
            0,
            400,
            800,
            owner,
            HMENU::default(),
            instance,
            None,
        )
        .ok()?;

        // Köşeleri pencere bölgesi (region) yuvarlatır; Windows 11'in kendi 8 px köşesi kapatılır.
        let corner = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, 4);
        // Üzerine bırakılan dosyalar iCloud Drive üzerinden iPhone'a gider.
        DragAcceptFiles(hwnd, true);

        let scale = util::scale(hwnd);
        let (font, font_title) = fonts(scale);
        ble.set_notify_window(hwnd);
        let m = Mirror {
            hwnd,
            engine,
            ble,
            video: None,
            video_size: (0, 0),
            ignore_resize_until: Instant::now(),
            aspect: DEFAULT_ASPECT,
            content: RECT::default(),
            touching: false,
            last_touch: (0, 0),
            wheel_accum: 0,
            geometry: None,
            keyboard: hid::Keyboard::default(),
            fake_ctrl: false,
            last_nudge: Instant::now(),
            last_wake: None,
            keyboard_was_ready: false,
            zoom_rearm_pending: false,
            zoom_rearm_at: None,
            zoom_second_at: None,
            lock_probe: None,
            lock_failures: 0,
            scale,
            font,
            font_title,
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
        };
        STATE.with(|c| *c.borrow_mut() = Some(m));

        MIRROR.with(|c| c.set(hwnd.0 as isize));
        SetTimer(hwnd, TIMER_POLL, 150, None);
        taskbar::set_enabled(taskbar::Device::IPhone, enabled());
        if enabled() {
            with(|m| m.engine.ensure_running());
        }
        Some(hwnd)
    }
}

pub fn show(hwnd: HWND) {
    unsafe {
        place(hwnd, true);
        let cmd = if IsIconic(hwnd).as_bool() { SW_RESTORE } else { SW_SHOW };
        let _ = ShowWindow(hwnd, cmd);
        let _ = SetForegroundWindow(hwnd);
        // Köşe katmanı pencere göründükten sonra: önce gösterilirse masaüstünde bir an dört siyah
        // köşe asılı kalır.
        place(hwnd, false);
    }
    taskbar::refresh();
}

pub fn enabled() -> bool {
    util::read_setting(ENABLED_SETTING) != Some(0)
}

fn mirror_hwnd() -> HWND {
    HWND(MIRROR.with(|c| c.get()) as _)
}

/// iPhone görüntüsü bundan sonra Mac üzerinden gelecek: açık pencere gizlenir. Açıktıysa true.
pub fn hide_for_mac() -> bool {
    let hwnd = mirror_hwnd();
    unsafe {
        let open = IsWindowVisible(hwnd).as_bool();
        if open {
            let _ = ShowWindow(hwnd, SW_HIDE);
            corners::hide();
            buttons::hide();
        }
        open
    }
}

/// iPhone yansıtmasını keser: pencere gizlenir, alıcı (UxPlay) kapanıp yeniden başlar; iPhone yeniden
/// bağlanabilir.
pub fn disconnect() {
    unsafe {
        let _ = ShowWindow(mirror_hwnd(), SW_HIDE);
    }
    corners::hide();
    buttons::hide();
    with(|m| {
        m.ble.cancel_typing();
        m.engine.shutdown();
    });
}

/// iPhone'u ana uygulamadan açar/kapatır: kapalıyken yansıtma alıcısı (UxPlay) durur, pencere
/// ve görev çubuğu ikonu kalkar.
pub fn set_enabled(on: bool) {
    util::write_setting(ENABLED_SETTING, u32::from(on));
    taskbar::set_enabled(taskbar::Device::IPhone, on);
    let hwnd = mirror_hwnd();
    if on {
        with(|m| m.engine.ensure_running());
    } else {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        corners::hide();
        buttons::hide();
        with(|m| {
            m.ble.cancel_typing();
            m.engine.shutdown();
            // Kapalıyken süren bir güç tuşu denemesi yeniden açılınca yanlış sonuçlanmasın.
            m.lock_probe = None;
        });
    }
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

pub fn set_size_percent(pct: u32) {
    util::write_setting(SIZE_SETTING, pct.clamp(SIZE_MIN, SIZE_MAX));
    phoneview::replace();
    let hwnd = mirror_hwnd();
    if unsafe { IsWindowVisible(hwnd).as_bool() } {
        unsafe {
            let _ = PostMessageW(hwnd, WM_APP_PLACE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Ana uygulamada gösterilen kısa durum.
pub fn status() -> String {
    if !enabled() {
        return i18n::t("Kapalı", "Off").into();
    }
    with(|m| {
        if m.engine.problem().is_some() {
            return i18n::t("Yansıtma motoru bulunamadı (bin\\uxplay)", "Mirroring engine not found (bin\\uxplay)").to_string();
        }
        match (m.video.is_some(), m.ble.connected()) {
            (true, true) => i18n::t("Yansıtılıyor · kontrol açık", "Mirroring · control on").into(),
            (true, false) => i18n::t("Yansıtılıyor · kontrol için Bluetooth bekleniyor", "Mirroring · waiting for Bluetooth for control").into(),
            (false, true) => i18n::t("Bluetooth bağlı · yansıtma bekleniyor", "Bluetooth connected · waiting for mirroring").into(),
            (false, false) => i18n::t("Yansıtma bekleniyor", "Waiting for mirroring").into(),
        }
    })
    .unwrap_or_default()
}

/// Görev çubuğu ikonuna bırakılan dosyalar (aynı UI iş parçacığından çağrılır).
pub fn send_dropped(hwnd: HWND, paths: Vec<PathBuf>) {
    unsafe { send_files(hwnd, paths) }
}

fn with<R>(f: impl FnOnce(&mut Mirror) -> R) -> Option<R> {
    STATE.with(|c| c.try_borrow_mut().ok().and_then(|mut o| o.as_mut().map(f)))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // PeekMessage gönderilmiş mesajları işleyip pencere yordamına yeniden girebilir; bu yüzden
    // durum ödünç alınmadan önce yapılır.
    if msg == WM_KEYDOWN && ((lp.0 >> 16) & 0x1FF) == 0x1D {
        ALTGR_NEXT.with(|a| a.set(altgr_follows(hwnd)));
    }
    let mut after = Vec::new();
    let result = with(|m| m.handle(msg, wp, lp, &mut after)).flatten();
    for a in after {
        run_after(hwnd, a);
    }
    match result {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn run_after(hwnd: HWND, a: After) {
    match a {
        After::ReleaseCapture => {
            let _ = ReleaseCapture();
        }
        After::Embed(video) => {
            embed(hwnd, video);
            with(|m| m.video = Some(video));
            place(hwnd, false);
            with(|m| m.layout());
            let _ = InvalidateRect(hwnd, None, false);
        }
        After::Place => place(hwnd, false),
        After::EnsureIcon => taskbar::ensure(hwnd),
        After::RecreateIcon => taskbar::recreate(hwnd),
        After::RefreshIcon => taskbar::refresh(),
        // Mac üzerinden kip açıkken iPhone ikonu görüntüyü oradan gösterir.
        After::Toggle if macview::phone_on() => phoneview::toggle(),
        After::Show if macview::phone_on() => phoneview::show(),
        After::Toggle | After::Show if !enabled() => {}
        After::Toggle => {
            if IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() && GetForegroundWindow() == hwnd {
                let _ = ShowWindow(hwnd, SW_HIDE);
                corners::hide();
                buttons::hide();
                taskbar::refresh();
            } else {
                show(hwnd);
            }
        }
        After::Hide => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            corners::hide();
                buttons::hide();
            taskbar::refresh();
        }
        After::Destroy => {
            let _ = DestroyWindow(hwnd);
        }
        After::Menu(pt) => show_menu(hwnd, pt),
        After::Send(paths) => send_files(hwnd, paths),
        After::Toast(text) => toast::show(hwnd, &text),
        After::Show => show(hwnd),
        After::Paste => {
            // Pano okuma (OpenClipboard) mesaj pompalayabilir: durum ödünç alınmadan yapılır.
            let note = match clipboard_text() {
                Clipboard::Busy => i18n::t("Pano şu an başka bir uygulamada açık; tekrar deneyin.", "The clipboard is in use by another app; try again.").to_string(),
                Clipboard::Empty => i18n::t("Panoda metin yok.", "No text on the clipboard.").to_string(),
                Clipboard::Text(text) => with(|m| m.start_paste(&text)).unwrap_or_default(),
            };
            if !note.is_empty() {
                toast::show(hwnd, &note);
            }
        }
    }
}

/// Dosyaları iCloud Drive'a kopyalar (arka planda); iCloud hazır değilse ne yapılacağını
/// anlatır ve Microsoft Store'daki iCloud sayfasını açmayı önerir.
unsafe fn send_files(hwnd: HWND, paths: Vec<PathBuf>) {
    if paths.is_empty() || DIALOG_OPEN.with(|d| d.get()) {
        return;
    }
    let dest = match transfer::target() {
        Target::Ready(dest) => dest,
        Target::DriveOff => {
            let text = i18n::t(
                "iCloud kurulu ama iCloud Drive açık değil.\n\n\
                 iCloud uygulamasını açıp Apple kimliğinizle giriş yapın ve \"iCloud Drive\" \
                 kutusunu işaretleyin. Eşitleme başladıktan sonra dosyayı tekrar bırakın.\n\n\
                 Microsoft Store'daki iCloud sayfası açılsın mı? (Oradaki \"Aç\" düğmesi iCloud'u başlatır.)",
                "iCloud is installed but iCloud Drive is turned off.\n\n\
                 Open the iCloud app, sign in with your Apple ID and tick \"iCloud Drive\". \
                 Once syncing starts, drop the file again.\n\n\
                 Open the iCloud page in the Microsoft Store? (Its \"Open\" button launches iCloud.)",
            );
            if ask(hwnd, text) {
                open_store(hwnd);
            }
            return;
        }
        Target::NotInstalled => {
            let text = i18n::t(
                "iPhone, Windows'tan Bluetooth ile dosya almıyor; dosyalar iCloud Drive üzerinden \
                 gönderilir. Bunun için bu bilgisayara ücretsiz \"iCloud for Windows\" gerekli.\n\n\
                 Kurulumdan sonra:\n\
                 1. Apple kimliğinizle giriş yapın.\n\
                 2. \"iCloud Drive\" kutusunu işaretleyin.\n\
                 3. Dosyayı tekrar bırakın; iPhone'da Dosyalar > iCloud Drive > iPhone Ayna'da görünür.\n\n\
                 Microsoft Store'da iCloud sayfası açılsın mı?",
                "iPhone does not accept files from Windows over Bluetooth; files are sent through \
                 iCloud Drive. This needs the free \"iCloud for Windows\" on this computer.\n\n\
                 After installing:\n\
                 1. Sign in with your Apple ID.\n\
                 2. Tick \"iCloud Drive\".\n\
                 3. Drop the file again; it will appear on the iPhone in Files > iCloud Drive > iPhone Ayna.\n\n\
                 Open the iCloud page in the Microsoft Store?",
            );
            if ask(hwnd, text) {
                open_store(hwnd);
            }
            return;
        }
    };

    let (files, bytes) = transfer::measure(&paths);
    if files > transfer::LARGE_FILES || bytes > transfer::LARGE_BYTES {
        let text = tf(
            "Bırakılanlar büyük: {}{} dosya, {}{} GB.\n\n\
             Hepsi iCloud'a yüklenecek ve iCloud kotanızdan düşecek. Devam edilsin mi?",
            "What you dropped is large: {}{} files, {}{} GB.\n\n\
             All of it will be uploaded to iCloud and count against your iCloud storage. Continue?",
            &[
                &if files > transfer::LARGE_FILES { i18n::t("en az ", "at least ") } else { "" },
                &files,
                &if bytes > transfer::LARGE_BYTES { i18n::t("en az ", "at least ") } else { "" },
                &format!("{:.1}", bytes as f64 / (1u64 << 30) as f64),
            ],
        );
        if !ask(hwnd, &text) {
            return;
        }
    }

    toast::show(hwnd, &tf("{} öğe iPhone'a gönderiliyor…", "Sending {} items to iPhone…", &[&paths.len()]));
    let target = hwnd.0 as isize;
    std::thread::spawn(move || {
        let notify = |text: String| {
            util::log(&text);
            SENT.lock().unwrap().push(text);
            unsafe {
                let _ = PostMessageW(HWND(target as _), WM_APP_SENT, WPARAM(0), LPARAM(0));
            }
        };
        let report = transfer::copy_all(&paths, &dest);
        notify(summary(&report));
        if !report.files.is_empty() {
            watch_upload(&report.files, notify);
        }
    });
}

/// Dosya iCloud klasörüne kondu diye iPhone'a ulaşmış olmaz: iCloud onu ayrıca buluta yükler
/// (ilk kurulumda dakikalar sürebilir). Yükleme bitene kadar izlenir, sonuç bildirilir.
fn watch_upload(files: &[PathBuf], notify: impl Fn(String)) {
    const POLL: Duration = Duration::from_secs(3);
    const SLOW: Duration = Duration::from_secs(90);
    const GIVE_UP: Duration = Duration::from_secs(20 * 60);
    let start = Instant::now();
    let mut warned = false;
    loop {
        std::thread::sleep(POLL);
        let done = files.iter().filter(|f| transfer::uploaded(f) == Some(true)).count();
        if done == files.len() {
            notify(tf(
                "{} dosya iCloud'a yüklendi.\niPhone'da Dosyalar > iCloud Drive > {} klasöründe.",
                "{} files uploaded to iCloud.\nOn the iPhone: Files > iCloud Drive > {}.",
                &[&done, &transfer::FOLDER_NAME],
            ));
            return;
        }
        if !warned && start.elapsed() > SLOW {
            warned = true;
            notify(tf(
                "iCloud yüklemesi sürüyor ({}/{}).\nDurumu iCloud uygulamasındaki iCloud Drive satırında görebilirsiniz.",
                "iCloud upload in progress ({}/{}).\nYou can see the status on the iCloud Drive row in the iCloud app.",
                &[&done, &files.len()],
            ));
        }
        if start.elapsed() > GIVE_UP {
            notify(tf(
                "iCloud {} dosyadan {} tanesini yükledi; kalanlar iCloud uygulaması üzerinden yüklenmeye devam edecek.",
                "Of {} files, iCloud has uploaded {}; the rest will keep uploading through the iCloud app.",
                &[&files.len(), &done],
            ));
            return;
        }
    }
}

fn summary(r: &transfer::Report) -> String {
    let place = tf(
        "Yüklenince iPhone'da Dosyalar > iCloud Drive > {} klasöründe görünecek.",
        "Once uploaded it will appear on the iPhone in Files > iCloud Drive > {}.",
        &[&transfer::FOLDER_NAME],
    );
    match (r.copied, r.failed.first()) {
        (0, None) => i18n::t("Gönderilecek dosya yoktu (klasör boş).", "There were no files to send (empty folder).").into(),
        (n, None) => tf("{} dosya iCloud'a yükleniyor…\n{}", "Uploading {} files to iCloud…\n{}", &[&n, &place]),
        (n, Some((path, err))) => {
            let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
            let more = if r.failed.len() > 1 { tf(" (+{} öğe daha)", " (+{} more)", &[&(r.failed.len() - 1)]) } else { String::new() };
            if n == 0 {
                tf("Gönderilemedi: {}: {}{}", "Could not send: {}: {}{}", &[&name, &err, &more])
            } else {
                tf("{} dosya gönderildi, {} öğe gönderilemedi.\n{}: {}{}", "{} files sent, {} items could not be sent.\n{}: {}{}", &[&n, &r.failed.len(), &name, &err, &more])
            }
        }
    }
}

unsafe fn ask(hwnd: HWND, text: &str) -> bool {
    DIALOG_OPEN.with(|d| d.set(true));
    let text = HSTRING::from(text);
    let caption = wide(i18n::t("iPhone'a dosya gönder", "Send file to iPhone"));
    let yes = MessageBoxW(hwnd, &text, PCWSTR(caption.as_ptr()), MB_YESNO | MB_ICONINFORMATION | MB_SETFOREGROUND) == IDYES;
    DIALOG_OPEN.with(|d| d.set(false));
    yes
}

unsafe fn open_store(hwnd: HWND) {
    let url = HSTRING::from(transfer::STORE_URL);
    ShellExecuteW(hwnd, w!("open"), &url, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
}

/// Pencereyi görev çubuğu ikonunun tam üstüne, ekran yüksekliğinde ve telefon oranında koyar;
/// köşeleri yuvarlatır. `raise` yalnız kullanıcı pencereyi açtığında: görev çubuğu kayınca
/// yapılan yeniden yerleştirme, başka uygulamada çalışan kullanıcının önüne pencere çıkarmaz.
unsafe fn place(hwnd: HWND, raise: bool) {
    let Some(aspect) = with(|m| m.aspect) else { return };
    let Some((rect, radius)) = frame(hwnd, aspect) else { return };
    let (x, y, w, h) = (rect.left, rect.top, width(&rect), height(&rect));
    let scale = util::scale(hwnd);
    let flags = if raise { SWP_NOACTIVATE } else { SWP_NOACTIVATE | SWP_NOZORDER };
    let _ = SetWindowPos(hwnd, HWND_TOP, x, y, w, h, flags);
    if REGION_SIZE.with(|r| r.get()) != (w, h) {
        let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, radius * 2, radius * 2);
        // Başarılı olursa bölgenin sahibi sistemdir; başarısızsa biz sileriz.
        if SetWindowRgn(hwnd, rgn, true) == 0 {
            let _ = DeleteObject(rgn);
        } else {
            REGION_SIZE.with(|r| r.set((w, h)));
        }
    }
    if IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() {
        corners::show(hwnd, rect, radius, scale);
        buttons::show(hwnd, rect, scale, aspect > 1.0);
    }
}

/// iPhone penceresinin yeri ve köşe yarıçapı: ikonun üstünde, ayarlı boyda, verilen oranda.
pub unsafe fn frame(hwnd: HWND, aspect: f32) -> Option<(RECT, i32)> {
    let anchor = taskbar::icon_rect();
    let probe = anchor.unwrap_or_default();
    let mon = MonitorFromRect(&probe, MONITOR_DEFAULTTOPRIMARY);
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !GetMonitorInfoW(mon, &mut info).as_bool() {
        return None;
    }
    let work = info.rcWork;
    let scale = util::scale(hwnd);
    let margin = (MARGIN * scale) as i32;

    let pct = size_percent() as i32;
    let max_w = (width(&work) - 2 * margin).max(100);
    let max_h = ((height(&work) - 2 * margin) * pct / 100).max(100);
    let (mut w, mut h) = ((max_h as f32 * aspect) as i32, max_h);
    if w > max_w {
        w = max_w;
        h = (w as f32 / aspect) as i32;
    }
    let center = anchor.map_or(work.left + width(&work) / 2, |a| (a.left + a.right) / 2);
    let lo = work.left + margin;
    let x = (center - w / 2).clamp(lo, (work.right - margin - w).max(lo));
    let y = work.bottom - margin - h;
    let radius = ((w.min(h) as f32 * CORNER_RATIO) as i32).max(1);
    Some((RECT { left: x, top: y, right: x + w, bottom: y + h }, radius))
}

pub fn size_percent() -> u32 {
    util::read_setting(SIZE_SETTING).unwrap_or(SIZE_DEFAULT).clamp(SIZE_MIN, SIZE_MAX)
}

unsafe fn show_menu(hwnd: HWND, pt: POINT) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let hide = wide(i18n::t("Gizle", "Hide"));
    let exit = wide(i18n::t("Çıkış", "Exit"));
    let _ = AppendMenuW(menu, MF_STRING, MENU_HIDE, PCWSTR(hide.as_ptr()));
    let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, PCWSTR(exit.as_ptr()));
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
    let _ = DestroyMenu(menu);
    match cmd.0 as usize {
        MENU_HIDE => run_after(hwnd, After::Hide),
        MENU_EXIT => run_after(hwnd, After::Destroy),
        _ => {}
    }
}

/// Video penceresini başlıksız çocuk pencereye çevirip buraya yerleştirir.
unsafe fn embed(host: HWND, video: HWND) {
    let style = GetWindowLongPtrW(video, GWL_STYLE) as u32;
    let style = (style & !(WS_POPUP.0 | WS_CAPTION.0 | WS_THICKFRAME.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0))
        | WS_CHILD.0
        | WS_VISIBLE.0;
    SetWindowLongPtrW(video, GWL_STYLE, style as isize);
    let ex = GetWindowLongPtrW(video, GWL_EXSTYLE) as u32;
    let ex = ex & !(WS_EX_APPWINDOW.0 | WS_EX_WINDOWEDGE.0 | WS_EX_CLIENTEDGE.0 | WS_EX_DLGMODALFRAME.0);
    SetWindowLongPtrW(video, GWL_EXSTYLE, ex as isize);
    let _ = SetParent(video, host);
    let _ = EnableWindow(video, false);
    util::detach_input(video);
}

impl Mirror {
    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM, after: &mut Vec<After>) -> Option<LRESULT> {
        if msg == self.taskbar_created && msg != 0 {
            after.push(After::RecreateIcon);
            return Some(LRESULT(0));
        }
        match msg {
            WM_TIMER if wp.0 == TIMER_POLL => {
                self.poll(after);
                Some(LRESULT(0))
            }
            WM_APP_TOGGLE => {
                after.push(After::Toggle);
                Some(LRESULT(0))
            }
            WM_APP_PLACE => {
                after.push(After::Place);
                Some(LRESULT(0))
            }
            WM_APP_SHOW => {
                after.push(After::Show);
                Some(LRESULT(0))
            }
            // Pencere kenarındaki ses/güç tuşları.
            buttons::WM_APP_BUTTON => {
                // Yalnız bu üç tuş: mesajı başka bir süreç de gönderebilir.
                let usage = match wp.0 {
                    0x30 => hid::CONSUMER_POWER,
                    0xE9 => hid::CONSUMER_VOLUME_UP,
                    0xEA => hid::CONSUMER_VOLUME_DOWN,
                    _ => return Some(LRESULT(0)),
                };
                let note = if !self.ble.connected() {
                    Some(i18n::t("Tuşlar için iPhone'un Bluetooth ile bağlı olması gerekiyor.", "The iPhone must be connected over Bluetooth for the buttons to work."))
                } else if !self.ble.consumer_ready() {
                    Some(i18n::t(
                        "iPhone tuşları henüz tanımıyor: Ayarlar > Bluetooth'ta bu bilgisayarı \"Bu Aygıtı Unut\" deyip yeniden eşleyin.",
                        "The iPhone does not recognise the buttons yet: in Settings > Bluetooth, tap \"Forget This Device\" for this computer and pair again.",
                    ))
                } else if self.ble.typing() {
                    Some(i18n::t("Yapıştırma sürerken tuşlar kapalı (Esc durdurur).", "Buttons are off while pasting (Esc stops)."))
                } else if usage == hid::CONSUMER_POWER {
                    self.power();
                    None
                } else {
                    self.ble.consumer(usage);
                    None
                };
                if let Some(n) = note {
                    after.push(After::Toast(n.into()));
                }
                Some(LRESULT(0))
            }
            WM_DROPFILES => {
                after.push(After::Send(transfer::dropped_paths(HDROP(wp.0 as _))));
                Some(LRESULT(0))
            }
            WM_APP_SENT => {
                let texts = std::mem::take(&mut *SENT.lock().unwrap());
                if !texts.is_empty() {
                    after.push(After::Toast(texts.join("\n")));
                }
                Some(LRESULT(0))
            }
            ble::WM_APP_BLE => {
                if !self.ble.connected() {
                    self.release_all(after);
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_SIZE => {
                self.layout();
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                self.scale = (wp.0 & 0xFFFF) as f32 / 96.0;
                unsafe {
                    let _ = DeleteObject(self.font);
                    let _ = DeleteObject(self.font_title);
                }
                (self.font, self.font_title) = fonts(self.scale);
                after.push(After::Place);
                Some(LRESULT(0))
            }
            WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
                after.push(After::Place);
                None
            }
            WM_ACTIVATE => {
                if wp.0 & 0xFFFF == 0 {
                    self.release_all(after);
                }
                after.push(After::RefreshIcon);
                None
            }
            WM_KILLFOCUS => {
                self.release_all(after);
                None
            }
            WM_CLOSE => {
                self.release_all(after);
                after.push(After::Hide);
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                self.engine.shutdown();
                unsafe { PostQuitMessage(0) };
                Some(LRESULT(0))
            }
            WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
                self.ble.cancel_typing();
                self.release_all(after);
                after.push(After::Hide);
                Some(LRESULT(0))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => self.key(msg, wp, lp, true, after),
            WM_KEYUP | WM_SYSKEYUP => self.key(msg, wp, lp, false, after),
            // Tuşlar iPhone'a gidince Alt+harf bip sesi ve menü kısayolu üretmesin.
            WM_CHAR | WM_SYSCHAR | WM_DEADCHAR | WM_SYSDEADCHAR if self.ready() => Some(LRESULT(0)),
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_PAINT => {
                self.paint();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                util::note_input_lag("iPhone");
                // Dokunmatik ekranda üzerinde gezinme yoktur: parmak yalnız basılıyken hareket eder.
                // Bırakma mesajı kaçtıysa (capture başka yere geçti) tuş durumundan (MK_LBUTTON) anlaşılır.
                if self.touching && wp.0 & 0x0001 == 0 {
                    self.release_all(after);
                } else if self.touching {
                    let (x, y) = point(lp);
                    self.touch(true, x, y);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                let (x, y) = point(lp);
                // Görüntü yokken (iPhone kilitli ya da ekranı kapalı) tıklama iPhone'u uyandırır.
                if self.video.is_none() && self.ble.connected() && self.ble.keyboard_ready() {
                    // Art arda tıklamalar çok sayıda Shift göndermesin (5 Shift iOS'ta Yapışkan
                    // Tuşlar kısayolu olabilir).
                    if self.last_wake.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
                        return Some(LRESULT(0));
                    }
                    self.last_wake = Some(Instant::now());
                    self.ble.key_sequence(vec![(hid::MOD_LEFT_SHIFT, 0, 0)]);
                    after.push(After::Toast(
                        i18n::t(
                            "iPhone uyandırıldı. Kilidi açıp Denetim Merkezi > Ekran Yansıtma'dan \"Ayna\"yı seçin.",
                            "iPhone woken up. Unlock it, then pick \"Ayna\" in Control Center > Screen Mirroring.",
                        )
                        .into(),
                    ));
                    return Some(LRESULT(0));
                }
                if contains(&self.content, x, y) && self.ready() {
                    unsafe { SetCapture(self.hwnd) };
                    self.touching = true;
                    self.touch(true, x, y);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if self.touching {
                    let (x, y) = point(lp);
                    self.last_touch = self.to_touch(x, y);
                    self.lift();
                    after.push(After::ReleaseCapture);
                }
                Some(LRESULT(0))
            }
            // Sağ tık = uzun basma (bağlam menüleri, ikonları düzenleme).
            WM_RBUTTONDOWN => {
                let (x, y) = point(lp);
                if contains(&self.content, x, y) && self.ready() && !self.touching {
                    let (tx, ty) = self.to_touch(x, y);
                    self.ble.gesture(long_press(tx, ty));
                }
                Some(LRESULT(0))
            }
            // Kontrol yokken uygulamanın kendi menüsü (Gizle / Çıkış): çerçevesiz pencerenin
            // tek kapısı. Bırakışta açılır, basılı tuşun bırakılması bir öğeyi seçmesin.
            WM_RBUTTONUP => {
                let (x, y) = point(lp);
                if !(contains(&self.content, x, y) && self.ready()) {
                    let mut pt = POINT { x, y };
                    unsafe {
                        let _ = ClientToScreen(self.hwnd, &mut pt);
                    }
                    after.push(After::Menu(pt));
                }
                Some(LRESULT(0))
            }
            // Orta tık (tekerleğe basmak) = ana ekran.
            WM_MBUTTONDOWN => {
                if self.ready() && !self.touching {
                    self.ble.gesture(home_swipe());
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.lift();
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                if wp.0 & 0x0008 != 0 {
                    // Ctrl + tekerlek: pencere boyu (MK_CONTROL).
                    let step = if delta > 0 { 5 } else { -5 };
                    let pct = (size_percent() as i32 + step).clamp(SIZE_MIN as i32, SIZE_MAX as i32) as u32;
                    util::write_setting(SIZE_SETTING, pct);
                    after.push(After::Place);
                } else if contains(&self.content, pt.x, pt.y) && self.ready() && !self.touching {
                    // Dokunmatik yüzeylerin küçük adımları birikir; her 120 birim (bir çentik)
                    // kısa bir parmak sürüklemesi olur.
                    self.wheel_accum += delta;
                    let notches = self.wheel_accum / 120;
                    self.wheel_accum -= notches * 120;
                    if notches != 0 {
                        let (tx, ty) = self.to_touch(pt.x, pt.y);
                        self.ble.gesture(scroll(tx, ty, notches));
                    }
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    fn ready(&self) -> bool {
        self.video.is_some() && self.ble.connected()
    }

    /// Güç tuşu: görüntü yoksa iPhone'u uyandırır, varsa kilitler.
    fn power(&mut self) {
        if self.video.is_none() {
            if self.ble.keyboard_ready() {
                self.ble.key_sequence(vec![(hid::MOD_LEFT_SHIFT, 0, 0)]);
            }
            return;
        }
        if self.lock_probe.is_some() {
            return;
        }
        let saved = util::read_setting(LOCK_SETTING).unwrap_or(0) as usize % LOCK_METHODS.len();
        // Klavye raporu henüz dinlenmiyorsa klavye yöntemleri atlanır (sıradaki tüketici yöntemi).
        let keyboard = self.ble.keyboard_ready();
        let Some(index) = (0..LOCK_METHODS.len())
            .map(|i| (saved + i) % LOCK_METHODS.len())
            .find(|&i| keyboard || matches!(LOCK_METHODS[i], LockMethod::Consumer(_)))
        else {
            return;
        };
        match LOCK_METHODS[index] {
            LockMethod::Consumer(usage) => self.ble.consumer(usage),
            LockMethod::Key(modifiers, usage) => self.ble.key_sequence(vec![(modifiers, usage, 0)]),
        }
        self.lock_probe = Some((Instant::now(), index));
    }

    /// Kilitleme denemesinin sonucu: yansıtma kesildiyse yöntem doğru, kesilmediyse sıradaki
    /// yöntem bir sonraki basışta denenir.
    fn check_lock_probe(&mut self, after: &mut Vec<After>) {
        let Some((at, index)) = self.lock_probe else { return };
        if self.video.is_none() {
            self.lock_probe = None;
            self.lock_failures = 0;
            util::write_setting(LOCK_SETTING, index as u32);
            util::write_setting(LOCK_VERIFIED_SETTING, 1);
            util::log(&format!("Güç tuşu: {index}. yöntem iPhone'u kilitledi."));
        } else if at.elapsed() >= LOCK_PROBE {
            self.lock_probe = None;
            self.lock_failures = self.lock_failures.saturating_add(1);
            // Doğrulanmış yöntem (arama, kilit ekranı gibi anlık bir durumda tutmamış olabilir) ancak
            // iki kez üst üste tutmazsa bırakılır.
            let verified = util::read_setting(LOCK_VERIFIED_SETTING) == Some(1);
            if verified && self.lock_failures < 2 {
                util::log(&format!("Güç tuşu: doğrulanmış {index}. yöntem bu kez kilitlemedi."));
                return;
            }
            self.lock_failures = 0;
            let next = (index + 1) % LOCK_METHODS.len();
            util::write_setting(LOCK_SETTING, next as u32);
            util::write_setting(LOCK_VERIFIED_SETTING, 0);
            util::log(&format!("Güç tuşu: {index}. yöntem kilitlemedi, sıradaki {next}."));
            after.push(After::Toast(
                i18n::t(
                    "iPhone bu yöntemle kilitlenmedi. Güç tuşuna yeniden basın; başka bir yöntem denenecek.",
                    "The iPhone did not lock with this method. Press the power button again; another method will be tried.",
                )
                .into(),
            ));
        }
    }

    /// Bluetooth klavyesiyle iki iş: yansıtma sürerken iPhone'un uyumasını engellemek (uyuyan
    /// iPhone yansıtmayı birkaç saniye içinde keser) ve bağlantı yenilenince Büyütme'yi klavye
    /// kısayoluyla kapatıp açmak (iOS harici dokunmatik ekranı, Büyütme açılırken bağlı olanlara
    /// uygular; sonradan bağlananlar için elle aç-kapa gerekiyordu).
    fn keep_iphone_awake(&mut self) {
        let ready = self.ble.connected() && self.ble.keyboard_ready();
        if ready && !self.keyboard_was_ready && util::read_setting(ZOOM_REARM_SETTING) != Some(0) {
            self.zoom_rearm_pending = true;
        }
        self.keyboard_was_ready = ready;
        if !ready {
            self.zoom_rearm_pending = false;
            self.zoom_rearm_at = None;
            self.zoom_second_at = None;
            return;
        }
        // Dokunma, yazma ya da basılı tuş sürerken araya klavye vuruşu girmez.
        if self.ble.typing() || !self.keyboard.is_empty() || self.touching {
            return;
        }
        // Büyütme yalnız yansıtma açıkken yenilenir: kilitli iPhone'u uyandırmasın, kullanıcı
        // ekranı görmeden bir ayarı değiştirmesin.
        if self.zoom_rearm_pending && self.video.is_some() {
            self.zoom_rearm_pending = false;
            self.zoom_rearm_at = Some(Instant::now() + ZOOM_REARM_DELAY);
        }
        // ⌥⌘8: Büyütme aç/kapa (Ayarlar > Erişilebilirlik > Büyütme > Klavye Kısayolları).
        let toggle = (hid::MOD_LEFT_ALT | hid::MOD_LEFT_GUI, hid::USAGE_8, 0);
        if self.zoom_rearm_at.is_some_and(|t| Instant::now() >= t) {
            self.zoom_rearm_at = None;
            self.zoom_second_at = Some(Instant::now() + Duration::from_millis(800));
            self.ble.key_sequence(vec![toggle]);
        }
        if self.zoom_second_at.is_some_and(|t| Instant::now() >= t) {
            self.zoom_second_at = None;
            self.ble.key_sequence(vec![toggle]);
            util::log("Büyütme klavye kısayoluyla yenilendi (⌥⌘8 iki kez).");
        }
        if self.video.is_some() && self.last_nudge.elapsed() >= KEEP_AWAKE {
            self.last_nudge = Instant::now();
            self.ble.key_sequence(vec![(hid::MOD_LEFT_SHIFT, 0, 0)]);
        }
    }

    fn poll(&mut self, after: &mut Vec<After>) {
        after.push(After::EnsureIcon);
        if !enabled() {
            return;
        }
        self.engine.ensure_running();
        // UxPlay'in (uxplay-windows) bildirim simgesi gizlenir; yerinde uygulamanın kendi simgesi.
        if let Some(pid) = self.engine.pid() {
            crate::tray::hide_foreign(pid);
        }
        self.keep_iphone_awake();
        self.check_lock_probe(after);

        // iPhone'un gerçek görüntü oranı ve karedeki siyah dolgu (yansıtma başladı, döndü).
        if let Some(g) = self.engine.poll_geometry() {
            self.geometry = Some(g);
            self.aspect = g.width / g.height;
            self.layout();
            after.push(After::Place);
        }

        match self.video {
            Some(v) if unsafe { !IsWindow(v).as_bool() } => {
                self.video = None;
                self.release_all(after);
                self.invalidate();
            }
            Some(v) => {
                // Geometri bilinmiyorsa: video kaynağı (ör. iPhone döndürüldü) kendi penceresini
                // yeniden boyutlandırırsa yeni oran oradan okunur.
                if self.geometry.is_some() || Instant::now() < self.ignore_resize_until {
                    return;
                }
                let mut r = RECT::default();
                unsafe {
                    let _ = GetWindowRect(v, &mut r);
                }
                let size = (width(&r), height(&r));
                if (size.0 - self.video_size.0).abs() > 2 || (size.1 - self.video_size.1).abs() > 2 {
                    if size.0 > 32 && size.1 > 32 {
                        let a = size.0 as f32 / size.1 as f32;
                        if (a - self.aspect).abs() > 0.02 {
                            self.aspect = a;
                            after.push(After::Place);
                        }
                    }
                    self.layout();
                }
            }
            None => {
                if let Some(v) = self.engine.find_video_window() {
                    let mut c = RECT::default();
                    unsafe {
                        let _ = GetClientRect(v, &mut c);
                    }
                    if self.geometry.is_none() && width(&c) > 0 && height(&c) > 0 {
                        self.aspect = width(&c) as f32 / height(&c) as f32;
                    }
                    after.push(After::Embed(v));
                }
            }
        }
    }

    fn layout(&mut self) {
        let mut c = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut c);
        }
        if width(&c) == 0 || height(&c) == 0 {
            return;
        }
        self.content = fit(&c, self.aspect);

        if let Some(v) = self.video {
            // Kare, telefon görüntüsünün çevresinde siyah dolgu taşıyorsa video penceresi o oranda
            // büyütülüp ortalanır: dolgu bu pencerenin (yuvarlak bölgenin) dışında kalıp kırpılır,
            // görünen alan tam olarak iPhone ekranıdır.
            let c = match self.geometry {
                Some(g) => {
                    let (cw, ch) = (width(&self.content) as f32, height(&self.content) as f32);
                    let (vw, vh) = (cw * g.source_w / g.width, ch * g.source_h / g.height);
                    let left = self.content.left - ((vw - cw) / 2.0).round() as i32;
                    let top = self.content.top - ((vh - ch) / 2.0).round() as i32;
                    RECT { left, top, right: left + vw.round() as i32, bottom: top + vh.round() as i32 }
                }
                None => self.content,
            };
            self.video_size = (width(&c), height(&c));
            self.ignore_resize_until = Instant::now() + Duration::from_millis(600);
            unsafe {
                let _ = SetWindowPos(
                    v,
                    HWND_TOP,
                    c.left,
                    c.top,
                    width(&c),
                    height(&c),
                    SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS,
                );
            }
        }
    }

    fn to_touch(&self, x: i32, y: i32) -> (u16, u16) {
        let c = &self.content;
        let fx = ((x - c.left) as f32 / (width(c) - 1).max(1) as f32).clamp(0.0, 1.0);
        let fy = ((y - c.top) as f32 / (height(c) - 1).max(1) as f32).clamp(0.0, 1.0);
        let m = hid::TOUCH_MAX as f32;
        ((fx * m).round() as u16, (fy * m).round() as u16)
    }

    fn touch(&mut self, down: bool, x: i32, y: i32) {
        if !self.ble.connected() {
            return;
        }
        let (tx, ty) = self.to_touch(x, y);
        self.last_touch = (tx, ty);
        self.ble.touch(down, tx, ty);
    }

    /// Basılı parmağı son konumunda kaldırır (odak/bağlantı/görüntü kaybında).
    fn lift(&mut self) -> bool {
        if !self.touching {
            return false;
        }
        self.touching = false;
        let (x, y) = self.last_touch;
        self.ble.touch(false, x, y);
        true
    }

    fn release_all(&mut self, after: &mut Vec<After>) {
        if self.lift() {
            after.push(After::ReleaseCapture);
        }
        self.fake_ctrl = false;
        if self.keyboard.clear() && self.ble.connected() {
            self.ble.keys(self.keyboard.report());
        }
    }

    fn key(&mut self, msg: u32, wp: WPARAM, lp: LPARAM, down: bool, after: &mut Vec<After>) -> Option<LRESULT> {
        // Alt+F4 pencereyi gizler; iPhone'a gitmez.
        if msg == WM_SYSKEYDOWN && wp.0 == VK_F4.0 as usize {
            return None;
        }
        if !self.ble.connected() {
            return None;
        }
        // Ctrl+V: Windows panosundaki metin iPhone'da imlecin olduğu yere yazılır. Basılı tutunca
        // tekrar eden mesaj (bit 30) yeniden yapıştırmaz.
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let alt = unsafe { GetKeyState(VK_MENU.0 as i32) } < 0;
        if down && ctrl && !alt && wp.0 == b'V' as usize {
            if lp.0 & (1 << 30) == 0 {
                after.push(After::Paste);
            }
            return Some(LRESULT(0));
        }
        // Yazma sürerken tuşlar iPhone'a gitmez (araya karışırdı); iPhone klavyeyi dinlemiyorsa
        // tuşlar Windows'a bırakılır.
        if self.ble.typing() {
            return Some(LRESULT(0));
        }
        if !self.ble.keyboard_ready() {
            return None;
        }

        let scancode = ((lp.0 >> 16) & 0xFF) as u32;
        let extended = (lp.0 >> 24) & 1 == 1;
        let Some(mut usage) = hid::scancode_to_usage(scancode, extended) else {
            return Some(LRESULT(0));
        };

        // AltGr, aynı zaman damgalı sahte bir sol Ctrl ile başlar. O Ctrl iPhone'a Cmd olarak
        // gitmemeli, yoksa @ gibi karakterler kısayola dönüşür; kuyruktaki sonraki tuşa bakılır.
        if usage == hid::USAGE_LEFT_CTRL {
            if down && ALTGR_NEXT.with(|a| a.replace(false)) {
                self.fake_ctrl = true;
                return Some(LRESULT(0));
            }
            if !down && self.fake_ctrl {
                self.fake_ctrl = false;
                return Some(LRESULT(0));
            }
        }
        // Windows'taki Ctrl+C/X/A/Z alışkanlığı iPhone'da Cmd ile çalışır.
        if usage == hid::USAGE_LEFT_CTRL {
            usage = hid::USAGE_LEFT_GUI;
        } else if usage == hid::USAGE_RIGHT_CTRL {
            usage = hid::USAGE_RIGHT_GUI;
        }
        let changed = if down { self.keyboard.press(usage) } else { self.keyboard.release(usage) };
        if changed {
            self.ble.keys(self.keyboard.report());
        }
        Some(LRESULT(0))
    }

    /// Panodan okunan metni tuş vuruşlarına çevirip yazdırır; kullanıcıya gösterilecek notu döner.
    fn start_paste(&mut self, text: &str) -> String {
        if !self.ble.keyboard_ready() {
            return i18n::t(
                "iPhone klavyeyi henüz tanımıyor: iPhone'da Ayarlar > Bluetooth'ta bu bilgisayarı \
                 \"Bu Aygıtı Unut\" deyip yeniden eşleyin.",
                "The iPhone does not recognise the keyboard yet: in Settings > Bluetooth on the iPhone, tap \
                 \"Forget This Device\" for this computer and pair again.",
            )
            .into();
        }
        if self.ble.typing() {
            return i18n::t("Önceki yapıştırma sürüyor (Esc durdurur).", "The previous paste is still running (Esc stops).").into();
        }
        let (strokes, skipped) = text_to_strokes(text);
        if strokes.is_empty() {
            return i18n::t("Panodaki metin bu klavye düzeniyle yazılamıyor.", "The clipboard text cannot be typed with this keyboard layout.").into();
        }
        let total = text.chars().filter(|&c| c != '\r').count();
        let mut note = tf("{} karakter iPhone'a yazılıyor… (Esc durdurur)", "Typing {} characters on the iPhone… (Esc stops)", &[&strokes.len()]);
        if skipped > 0 {
            note.push_str(&tf("\n{} karakter yazılamadı (klavye düzeninde yok).", "\n{} characters could not be typed (not in the keyboard layout).", &[&skipped]));
        }
        if total > PASTE_LIMIT {
            note.push_str(&tf("\nYalnız ilk {} karakter yazılıyor.", "\nOnly the first {} characters are typed.", &[&PASTE_LIMIT]));
        }
        // Basılı Ctrl (iPhone'da Cmd) sıfırlanmaz: yazılan her vuruş kendi değiştiricileriyle
        // gider, yazma bitince Ctrl hâlâ basılıysa sonraki tuş yine Cmd ile birleşir.
        self.ble.type_keys(strokes);
        note
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    fn paint(&mut self) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            let mut c = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut c);
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, width(&c).max(1), height(&c).max(1));
            let old = SelectObject(mem, bmp);

            fill_rect(mem, &c, rgb(0, 0, 0));
            if self.video.is_none() {
                self.paint_placeholder(mem, &c);
            }

            let _ = BitBlt(hdc, 0, 0, width(&c), height(&c), mem, 0, 0, SRCCOPY);
            SelectObject(mem, old);
            let _ = DeleteObject(bmp);
            let _ = DeleteDC(mem);
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    unsafe fn paint_placeholder(&self, dc: HDC, client: &RECT) {
        let s = self.scale;
        // Yuvarlak köşelerin içinde kalacak kadar iç boşluk.
        let pad = ((width(client).min(height(client)) as f32 * CORNER_RATIO * 0.6) as i32).max((24.0 * s) as i32);
        let area = RECT { left: pad, top: pad, right: client.right - pad, bottom: client.bottom - pad };
        let pc = std::env::var("COMPUTERNAME").unwrap_or_else(|_| i18n::t("bu bilgisayar", "this computer").into());

        let problem = self.engine.problem();
        let title = if problem.is_none() {
            i18n::t("iPhone ekranı bekleniyor", "Waiting for the iPhone screen")
        } else {
            i18n::t("Yansıtma başlatılamıyor", "Cannot start mirroring")
        };
        let mut body = if let Some(p) = problem {
            p
        } else {
            tf(
                "iPhone'da Denetim Merkezi'ni açın, Ekran Yansıtma'ya dokunun ve listeden \"{}\" seçin. \
                 iPhone ile bu bilgisayar aynı Wi-Fi ağında olmalı.\n\n\
                 Fareyle kontrol (bir kez yapılır):\n\
                 1. iPhone Ayarlar > Bluetooth'tan \"{}\" cihazını eşleyin. Önceden eşlediyseniz \
                 önce \"Bu Aygıtı Unut\" deyip yeniden eşleyin.\n\
                 2. Ayarlar > Erişilebilirlik > Büyütme'yi açın. Ekran büyürse üç parmakla çift \
                 dokunarak normale döndürün; Büyütme açık kalmalı.\n\n\
                 Dosya göndermek için dosyayı bu pencereye ya da görev çubuğundaki telefon \
                 ikonuna sürükleyip bırakın (iCloud Drive üzerinden).\n\n\
                 Klavye: yazdıklarınız iPhone'a gider; Ctrl+V Windows panosundaki metni yazar.\n\
                 iPhone uyuyorsa bu pencereye tıklamak onu uyandırır. Büyütme'nin her bağlantıda kendiliğinden \
                 yenilenmesi için Ayarlar > Erişilebilirlik > Büyütme > Klavye Kısayolları'nı açın.\n\
                 Kısayollar: orta tık ana ekran, sağ tık uzun basma, Ctrl + tekerlek pencere boyu, \
                 Esc gizler.",
                "On the iPhone, open Control Center, tap Screen Mirroring and pick \"{}\". \
                 The iPhone and this computer must be on the same Wi-Fi network.\n\n\
                 Mouse control (one-time setup):\n\
                 1. In iPhone Settings > Bluetooth, pair \"{}\". If you paired before, \
                 tap \"Forget This Device\" first and pair again.\n\
                 2. Turn on Settings > Accessibility > Zoom. If the screen zooms in, double-tap with \
                 three fingers to return to normal; Zoom must stay on.\n\n\
                 To send files, drag and drop them onto this window or the phone icon in the \
                 taskbar (through iCloud Drive).\n\n\
                 Keyboard: what you type goes to the iPhone; Ctrl+V types the text on the Windows clipboard.\n\
                 If the iPhone is asleep, clicking this window wakes it. To refresh Zoom automatically on every \
                 connection, turn on Settings > Accessibility > Zoom > Keyboard Shortcuts.\n\
                 Shortcuts: middle click is Home, right click is long press, Ctrl + wheel resizes the window, \
                 Esc hides it.",
                &[&uxplay::RECEIVER_NAME, &pc],
            )
        };
        body.push_str(&format!("\n\nBluetooth: {}", status_text(self.ble.status())));
        if self.ble.status() == Status::Failed {
            body.push_str(&format!(" ({})", self.ble.error()));
        }

        SetBkMode(dc, TRANSPARENT);
        let mut t = wide_nonull(title);
        let mut b = wide_nonull(&body);
        let mut tr = area;
        let mut br = area;
        SelectObject(dc, self.font_title);
        DrawTextW(dc, &mut t, &mut tr, DT_CALCRECT | DT_WORDBREAK | DT_CENTER);
        SelectObject(dc, self.font);
        DrawTextW(dc, &mut b, &mut br, DT_CALCRECT | DT_WORDBREAK);
        let gap = (14.0 * s) as i32;
        let total = height(&tr) + gap + height(&br);
        let top = area.top + ((height(&area) - total) / 2).max(0);

        let mut tr = RECT { left: area.left, top, right: area.right, bottom: top + height(&tr) };
        SelectObject(dc, self.font_title);
        SetTextColor(dc, rgb(0xF2, 0xF2, 0xF7));
        DrawTextW(dc, &mut t, &mut tr, DT_WORDBREAK | DT_CENTER);

        let mut br = RECT { left: area.left, top: tr.bottom + gap, right: area.right, bottom: area.bottom };
        SelectObject(dc, self.font);
        SetTextColor(dc, rgb(0x98, 0x98, 0x9F));
        DrawTextW(dc, &mut b, &mut br, DT_WORDBREAK);
    }
}

fn status_text(s: Status) -> &'static str {
    match s {
        Status::Starting => i18n::t("başlatılıyor", "starting"),
        Status::Advertising => i18n::t("eşleşme bekleniyor", "waiting to pair"),
        Status::Connected => i18n::t("bağlı", "connected"),
        Status::Failed => i18n::t("kullanılamıyor", "unavailable"),
    }
}

enum Clipboard {
    Text(String),
    Empty,
    /// Pano başka bir süreçte açık (pano geçmişi, uzak masaüstü…).
    Busy,
}

fn clipboard_text() -> Clipboard {
    unsafe {
        let mut opened = false;
        for _ in 0..5 {
            if OpenClipboard(HWND::default()).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !opened {
            return Clipboard::Busy;
        }
        let text = (|| {
            let data = GetClipboardData(CF_UNICODETEXT).ok()?;
            let global = windows::Win32::Foundation::HGLOBAL(data.0);
            let ptr = GlobalLock(global) as *const u16;
            if ptr.is_null() {
                return None;
            }
            // NUL'a kadar, ama bloğun boyutunu ve yazılacak sınırı aşmadan: hatalı bir uygulamanın
            // NUL'suz bıraktığı veri bellek sonrasını okutmasın, dev metin boşuna çevrilmesin.
            let cap = (GlobalSize(global) / 2).min(PASTE_LIMIT * 2 + 2);
            let len = (0..cap).take_while(|&i| *ptr.add(i) != 0).count();
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(global);
            Some(text)
        })();
        let _ = CloseClipboard();
        match text {
            Some(t) if !t.is_empty() => Clipboard::Text(t),
            _ => Clipboard::Empty,
        }
    }
}

/// Kuyruktaki sonraki tuş, şu anki mesajla aynı anda basılmış sağ Alt mı?
fn altgr_follows(hwnd: HWND) -> bool {
    let mut next = MSG::default();
    unsafe {
        let time = GetMessageTime();
        if !PeekMessageW(&mut next, hwnd, WM_KEYFIRST, WM_KEYLAST, PM_NOREMOVE | PM_NOYIELD).as_bool() {
            return false;
        }
        let scancode = (next.lParam.0 >> 16) & 0xFF;
        let extended = (next.lParam.0 >> 24) & 1 == 1;
        (next.message == WM_KEYDOWN || next.message == WM_SYSKEYDOWN)
            && scancode == 0x38
            && extended
            && next.time as i32 == time
    }
}

/// Metni, bu bilgisayarın etkin klavye düzenine göre (değiştirici, usage) vuruşlarına çevirir.
/// iPhone'un donanım klavyesi düzeni Windows'takiyle aynı seçilmelidir (ör. ikisi de Türkçe Q).
/// Düzende tek tuşla yazılamayan karakterler (ölü tuş gerektirenler) atlanır.
fn text_to_strokes(text: &str) -> (Vec<(u8, u8)>, usize) {
    let map = char_map(unsafe { GetKeyboardLayout(0) });
    let mut strokes = Vec::new();
    let mut skipped = 0;
    for c in text.chars().filter(|&c| c != '\r').take(PASTE_LIMIT) {
        match c {
            '\n' => strokes.push((0, hid::USAGE_ENTER)),
            '\t' => strokes.push((0, hid::USAGE_TAB)),
            _ => match map.get(&c) {
                Some(&s) => strokes.push(s),
                None => skipped += 1,
            },
        }
    }
    (strokes, skipped)
}

/// Klavye düzeninden karakter → (değiştirici, usage) tablosu. Her fiziksel tuş Shift ve AltGr
/// birleşimleriyle `ToUnicodeEx`'e sorulur; bir karakter birden çok yoldan yazılabiliyorsa en az
/// değiştirici gerektiren alınır. (`VkKeyScanEx` bunu garanti etmez: Türkçe Q'da "i" için asıl
/// tuş yerine AltGr+I döndürür.)
fn char_map(layout: HKL) -> std::collections::HashMap<char, (u8, u8)> {
    const DOWN: u8 = 0x80;
    let states: [(u8, &[VIRTUAL_KEY]); 4] = [
        (0, &[]),
        (hid::MOD_LEFT_SHIFT, &[VK_SHIFT]),
        (hid::MOD_RIGHT_ALT, &[VK_CONTROL, VK_MENU]), // AltGr; iPhone'da Option
        (hid::MOD_LEFT_SHIFT | hid::MOD_RIGHT_ALT, &[VK_SHIFT, VK_CONTROL, VK_MENU]),
    ];
    let mut map = std::collections::HashMap::new();
    for (modifiers, keys) in states {
        let mut state = [0u8; 256];
        for k in keys {
            state[k.0 as usize] = DOWN;
        }
        for sc in 0x01u32..=0x58 {
            let Some(usage) = hid::scancode_to_usage(sc, false) else { continue };
            let vk = unsafe { MapVirtualKeyExW(sc, MAPVK_VSC_TO_VK_EX, layout) };
            if vk == 0 {
                continue;
            }
            let mut buf = [0u16; 8];
            // 0x4: çekirdekteki klavye durumunu (ölü tuş beklemesi) değiştirme.
            let n = unsafe { ToUnicodeEx(vk, sc, &state, &mut buf, 0x4, layout) };
            if n != 1 {
                continue; // ölü tuş (-1), karakter yok (0) ya da birden çok karakter
            }
            if let Some(c) = char::from_u32(buf[0] as u32).filter(|c| !c.is_control()) {
                map.entry(c).or_insert((modifiers, usage));
            }
        }
    }
    map
}

fn step(down: bool, x: u16, y: u16, wait_ms: u64) -> Step {
    Step { down, x, y, wait_ms }
}

/// Alt kenardan hızlı ve kısa yukarı kaydırma: ana ekrana döner.
fn home_swipe() -> Vec<Step> {
    let x = hid::TOUCH_MAX / 2;
    let (start, end): (u16, u16) = (hid::TOUCH_MAX - 10, 6500);
    let n = 10u16;
    let mut steps = vec![step(true, x, start, 12)];
    for i in 1..=n {
        let y = start - (start - end) * i / n;
        steps.push(step(true, x, y, 10));
    }
    steps.push(step(false, x, end, 0));
    steps
}

fn long_press(x: u16, y: u16) -> Vec<Step> {
    vec![step(true, x, y, 800), step(false, x, y, 0)]
}

/// Tekerlek çentiği başına ekranın %6'sı kadar parmak sürüklemesi. Sonda kısa bir bekleme
/// fırlatma (momentum) etkisini azaltır, kaydırma kontrollü kalır.
///
/// Jest kenarlardan uzak bir bantta tutulur: alt kenardan yukarı sürükleme ana ekrana döner,
/// üst kenardan aşağı Bildirim Merkezi'ni, yan kenardan sürükleme "geri"yi açar. Tam mesafe
/// sığmazsa başlangıç kaydırılır, mesafe kısaltılmaz (kısa sürükleme dokunma sayılırdı).
fn scroll(x: u16, y: u16, notches: i32) -> Vec<Step> {
    const LO: i32 = 800;
    const HI: i32 = hid::TOUCH_MAX as i32 - 800;
    let dy = (notches * 600).clamp(-6000, 6000);
    let x = (x as i32).clamp(LO, HI) as u16;
    let mut start = (y as i32).clamp(LO, HI);
    let mut end = start + dy;
    if end > HI {
        start -= end - HI;
        end = HI;
    } else if end < LO {
        start += LO - end;
        end = LO;
    }
    let (y, end) = (start as u16, end as u16);
    let n = 6i32;
    let mut steps = vec![step(true, x, y, 10)];
    for i in 1..=n {
        let yi = (y as i32 + (end as i32 - y as i32) * i / n) as u16;
        steps.push(step(true, x, yi, 10));
    }
    steps.push(step(true, x, end, 60));
    steps.push(step(false, x, end, 0));
    steps
}

fn point(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

fn fit(area: &RECT, aspect: f32) -> RECT {
    let (aw, ah) = (width(area).max(1) as f32, height(area).max(1) as f32);
    let (w, h) = if aw / ah > aspect { (ah * aspect, ah) } else { (aw, aw / aspect) };
    let left = area.left + ((aw - w) / 2.0).round() as i32;
    let top = area.top + ((ah - h) / 2.0).round() as i32;
    RECT { left, top, right: left + w.round() as i32, bottom: top + h.round() as i32 }
}

fn fonts(scale: f32) -> (HFONT, HFONT) {
    let make = |size: f32, weight: i32| {
        let mut lf = LOGFONTW {
            lfHeight: -(size * scale).round() as i32,
            lfWeight: weight,
            lfQuality: CLEARTYPE_QUALITY,
            ..Default::default()
        };
        for (dst, src) in lf.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
            *dst = src;
        }
        unsafe { CreateFontIndirectW(&lf) }
    };
    (make(14.0, 400), make(20.0, 600))
}

fn fill_rect(dc: HDC, r: &RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(dc, r, brush);
        let _ = DeleteObject(brush);
    }
}

fn wide_nonull(s: &str) -> Vec<u16> {
    let mut v = wide(s);
    v.pop();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bu bilgisayarın klavye düzeninde metnin hangi tuşlara çevrildiğini yazdırır.
    #[test]
    #[ignore = "sonuc bilgisayarin klavye duzenine bagli"]
    fn strokes_for_turkish_text() {
        let layout = unsafe { GetKeyboardLayout(0) };
        println!("duzen: {:08X}", layout.0 as usize);
        let map = char_map(layout);
        for c in "aAiIçÇşŞğĞıİöÖüÜ@€\"!".chars() {
            println!("{c} -> {:?}", map.get(&c));
        }
        let (strokes, skipped) = text_to_strokes("ab\r\nc\t");
        assert_eq!(strokes.len(), 5);
        assert_eq!(skipped, 0);
        assert_eq!(strokes[2], (0, hid::USAGE_ENTER));
    }

    #[test]
    fn scroll_stays_away_from_edges() {
        let s = scroll(5000, 9990, -3);
        assert!(s.iter().all(|st| st.y >= 800 && st.y <= hid::TOUCH_MAX - 800));
        let first = s.first().unwrap().y as i32;
        let last = s.last().unwrap().y as i32;
        assert_eq!(first - last, 1800, "tam mesafe korunur");
    }
}
