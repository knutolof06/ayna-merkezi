//! Mac penceresi: görev çubuğundaki Mac ikonunun tam üstünde, sağa doğru açılan çerçevesiz,
//! yuvarlak köşeli yatay bir pencere (yan yatmış telefon/tablet gibi).
//!
//! Bağlı değilken içinde kod yazma ekranı; ilk eşleşmede Mac'te onay beklenirken iki ekranda aynı
//! görünmesi gereken 6 haneli doğrulama kodu; bağlıyken Mac'in görüntüsü. Fare ve klavye Mac'e
//! gider. Pencere gizlenince bağlantı açık kalır (yalnız görüntü durur); koparsa kendiliğinden
//! yeniden bağlanır. Mac ikonuna sağ tık boyut penceresini açar.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, IsWindowEnabled, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::dragout;
use crate::ghost;
use crate::gst::{self, Player};
use crate::toast;
use crate::hub::{self, card_bg, card_bg_alpha, colorref, hit, palette, point, slider, text, to_rect, Fonts};
use crate::mac::{self, Mac, State};
use crate::mirror::{WM_APP_PLACE, WM_APP_SHOW, WM_APP_TOGGLE};
use crate::taskbar::{self, draw_mac, Canvas, Device, WM_APP_DISCONNECT};
use crate::util::{self, height, width};

pub const CLASS: PCWSTR = w!("iPhoneAyna.Mac");
const VIDEO_CLASS: PCWSTR = w!("iPhoneAyna.MacGoruntu");
const SIZER_CLASS: PCWSTR = w!("iPhoneAyna.MacBoyut");
/// Mac ikonuna sağ tık: boyut penceresi.
pub const WM_APP_SIZER: u32 = WM_APP + 25;
/// Kod alanında Enter.
const WM_APP_GO: u32 = WM_APP + 26;
const TIMER_TICK: usize = 1;
const EDIT_ID: usize = 100;

const ENABLED_SETTING: PCWSTR = w!("MacAcik");
const SIZE_SETTING: PCWSTR = w!("MacBoyutYuzde");
const SIZE_DEFAULT: u32 = 70;
const SIZE_MIN: u32 = 30;
const SIZE_MAX: u32 = 100;
/// Ekran boyutu gelene dek kullanılan oran (Mac ekranları çoğunlukla 16:10).
const DEFAULT_ASPECT: f32 = 16.0 / 10.0;
/// Çalışma alanı kenar boşluğu (96 DPI'da piksel).
const MARGIN: f32 = 10.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ctl {
    Button,
    /// İlk eşleşmede "Kodlar aynı".
    Yes,
}

struct View {
    hwnd: HWND,
    video: HWND,
    edit: HWND,
    mac: Mac,
    scale: f32,
    fonts: Fonts,
    hover: Option<Ctl>,
    pressed: Option<Ctl>,
    tracking: bool,
    painted: Option<State>,
    /// Kullanıcı bağlantıyı kendisi kesti: kendiliğinden yeniden bağlanılmaz.
    user_closed: bool,
    reconnect_at: Option<Instant>,
    backoff: Duration,
    /// Basılı fare düğmeleri (bit 0 sol, 1 sağ, 2 orta) ve tuşlar (Mac kodu): odak gidince bırakılır.
    buttons: u8,
    keys: Vec<u16>,
    /// AltGr'nin önündeki sahte sol Ctrl yutuldu; bırakılışı da yutulacak.
    fake_ctrl: bool,
    was_connected: bool,
    /// İlk eşleşmede kullanıcı doğrulama kodunu onayladı.
    decided: bool,
    /// Son fare konumu (0..65535): odak giderken düğme bu noktada bırakılır.
    last_pos: (u16, u16),
    /// Zamanlayıcının en son gördüğü durum (pencere gizliyken de değişim anlaşılsın).
    seen: Option<State>,
    /// Basılı fareyle pencere dışına çıkıldı, Mac'e "dosya sürükleniyor mu?" soruldu.
    drag_queried: bool,
    /// Mac'ten sürüklenen dosyalar alınıyor; fare bırakılana dek Mac'e fare iletilmez.
    dragging_out: bool,
    /// Mac imlecinin şekli, penceredeki görüntü ölçeğinde (görüntüde imleç çizilmez).
    mac_cursor: HCURSOR,
    /// Mac'e en son bildirilen sürükleme önizleme yeri (aynısı yinelenmez).
    last_hover: Option<(u16, u16)>,
    /// Mac'ten çıkarılan dosya yeniden Mac penceresine bırakıldı: inen dosyalar kaydedilmez.
    drag_cancelled: bool,
    /// Mac'e "sürükleniyor mu" soruldu ama yanıt gelmeden fare Mac penceresinde bırakıldı (dosya
    /// Mac'te bırakıldı): gelen liste yok sayılır.
    released_inside: bool,
}

thread_local! {
    static STATE: RefCell<Option<View>> = const { RefCell::new(None) };
    static HWND_CELL: Cell<isize> = const { Cell::new(0) };
    static EDIT_PREV: Cell<isize> = const { Cell::new(0) };
    static EDIT_BRUSH: Cell<isize> = const { Cell::new(0) };
    static SIZER: RefCell<Option<Sizer>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut View) -> R) -> Option<R> {
    STATE.with(|c| c.try_borrow_mut().ok().and_then(|mut o| o.as_mut().map(f)))
}

fn main_hwnd() -> HWND {
    HWND(HWND_CELL.with(|c| c.get()) as _)
}

pub fn enabled() -> bool {
    util::read_setting(ENABLED_SETTING) != Some(0)
}

/// Mac'i ana uygulamadan açar/kapatır: kapalıyken ikon kalkar, bağlantı kesilir.
pub fn set_enabled(on: bool) {
    util::write_setting(ENABLED_SETTING, u32::from(on));
    taskbar::set_enabled(Device::Mac, on);
    if !on {
        disconnect();
        unsafe {
            let _ = ShowWindow(main_hwnd(), SW_HIDE);
        }
        hide_sizer();
    }
}

pub fn connected() -> bool {
    with(|v| v.mac.connected()).unwrap_or(false)
}

/// Ana uygulamada gösterilen kısa durum.
pub fn status() -> String {
    if !enabled() {
        return "Kapalı".into();
    }
    with(|v| v.mac.state().text()).unwrap_or_default()
}

pub fn disconnect() {
    with(|v| {
        v.user_closed = true;
        v.reconnect_at = None;
        v.mac.clear_phone();
        v.mac.disconnect();
    });
    refresh(main_hwnd());
}

/// Mac penceresini açar (ana uygulamadaki düğme).
pub fn open() {
    if !enabled() {
        set_enabled(true);
    }
    unsafe {
        let _ = PostMessageW(main_hwnd(), WM_APP_SHOW, WPARAM(0), LPARAM(0));
    }
}

fn size_percent() -> u32 {
    util::read_setting(SIZE_SETTING).unwrap_or(SIZE_DEFAULT).clamp(SIZE_MIN, SIZE_MAX)
}

fn saved_code() -> Option<String> {
    util::read_text_setting(mac::CODE_SETTING).filter(|c| c.len() >= 6 && c.chars().all(|ch| ch.is_ascii_digit()))
}

pub fn create() -> Option<HWND> {
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
        let vc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(def_proc),
            hInstance: instance,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: VIDEO_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&vc);

        // Sahipli pencerenin görev çubuğu düğmesi olmaz: giriş Başlat'ın yanındaki Mac ikonu.
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
            w!("Mac"),
            WS_POPUP | WS_CLIPCHILDREN,
            0,
            0,
            800,
            500,
            owner,
            HMENU::default(),
            instance,
            None,
        )
        .ok()?;
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, 4);
        let dark = BOOL::from(true);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);

        // Görüntü bu çocuk pencerenin içine çizilir. Devre dışı: fare iletileri ana pencereye gelir.
        let video = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            VIDEO_CLASS,
            w!(""),
            WS_CHILD | WS_DISABLED | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            0,
            0,
            10,
            10,
            hwnd,
            HMENU::default(),
            instance,
            None,
        )
        .unwrap_or_default();

        let edit = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("EDIT"),
            w!(""),
            WS_CHILD | WS_TABSTOP | WINDOW_STYLE((ES_CENTER | ES_NUMBER | ES_AUTOHSCROLL) as u32),
            0,
            0,
            10,
            10,
            hwnd,
            HMENU(EDIT_ID as _),
            instance,
            None,
        )
        .unwrap_or_default();
        SendMessageW(edit, 0x00C5 /* EM_LIMITTEXT */, WPARAM(12), LPARAM(0));
        let prev = SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as isize);
        EDIT_PREV.with(|p| p.set(prev));
        if let Some(code) = saved_code() {
            let t = util::wide(&code);
            let _ = SetWindowTextW(edit, PCWSTR(t.as_ptr()));
        }

        let scale = util::scale(hwnd);
        let brush = CreateSolidBrush(colorref(palette(false).field));
        EDIT_BRUSH.with(|b| b.set(brush.0 as isize));
        let view = View {
            hwnd,
            video,
            edit,
            mac: Mac::new(hwnd),
            scale,
            fonts: Fonts::new(scale),
            hover: None,
            pressed: None,
            tracking: false,
            painted: None,
            user_closed: false,
            reconnect_at: None,
            backoff: Duration::from_secs(3),
            buttons: 0,
            keys: Vec::new(),
            fake_ctrl: false,
            was_connected: false,
            decided: false,
            last_pos: (u16::MAX / 2, u16::MAX / 2),
            seen: None,
            drag_queried: false,
            dragging_out: false,
            mac_cursor: HCURSOR::default(),
            last_hover: None,
            drag_cancelled: false,
            released_inside: false,
        };
        STATE.with(|c| *c.borrow_mut() = Some(view));
        HWND_CELL.with(|c| c.set(hwnd.0 as isize));
        SendMessageW(edit, WM_SETFONT, WPARAM(with(|v| v.fonts.code.0 as usize).unwrap_or(0)), LPARAM(1));

        // Windows'tan sürüklenen dosyalar bu pencereye bırakılınca Mac'e gider (OLE hedefi: sürüklerken
        // Mac'te dosyanın simgesi görünür).
        dragout::register_target(hwnd);
        taskbar::set_target(Device::Mac, hwnd);
        taskbar::set_view(Device::Mac, hwnd);
        taskbar::set_enabled(Device::Mac, enabled());
        SetTimer(hwnd, TIMER_TICK, 200, None);

        // Eşleşilmiş Mac'e açılışta arka planda bağlanılır: ikona tıklayınca ekran hemen gelsin.
        if enabled() {
            if let Some(code) = saved_code().filter(|c| Mac::paired(c)) {
                with(|v| v.mac.connect(&code));
            }
        }
        Some(hwnd)
    }
}

unsafe extern "system" fn def_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// Kod alanı: Enter bağlanır, Esc pencereyi gizler.
unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_CHAR && (wp.0 == 0x0D || wp.0 == 0x1B) {
        return LRESULT(0);
    }
    if msg == WM_KEYDOWN && wp.0 == 0x0D {
        let _ = PostMessageW(GetParent(hwnd).unwrap_or_default(), WM_APP_GO, WPARAM(0), LPARAM(0));
        return LRESULT(0);
    }
    if msg == WM_KEYDOWN && wp.0 == 0x1B {
        hide(GetParent(hwnd).unwrap_or_default());
        return LRESULT(0);
    }
    let prev: WNDPROC = std::mem::transmute(EDIT_PREV.with(|p| p.get()));
    CallWindowProcW(prev, hwnd, msg, wp, lp)
}

fn refresh(hwnd: HWND) {
    unsafe {
        let _ = PostMessageW(hwnd, mac::WM_APP_MAC, WPARAM(0), LPARAM(0));
    }
}

unsafe fn show(hwnd: HWND) {
    hide_sizer();
    with(|v| {
        // Kod girilmiş ve eşleşilmiş bir Mac varsa, bağlı değilken açılış bağlanmayı da başlatır.
        if matches!(v.mac.state(), State::Idle | State::Failed(_)) && !v.mac.dropped() {
            if let Some(code) = saved_code().filter(|c| Mac::paired(c)) {
                v.user_closed = false;
                v.mac.connect(&code);
            }
        }
        v.mac.pause(false);
    });
    place(hwnd);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    update_mode(hwnd);
    taskbar::refresh();
}

unsafe fn hide(hwnd: HWND) {
    with(|v| {
        v.release_input();
        v.mac.pause(true);
    });
    let _ = ShowWindow(hwnd, SW_HIDE);
    taskbar::refresh();
}

/// Pencereyi Mac ikonunun üstüne koyar: sol kenarı ikonda, sağa doğru açılır; boyu çalışma alanı
/// yüksekliğinin ayarlanan yüzdesi, eni Mac ekranının oranında.
unsafe fn place(hwnd: HWND) {
    let aspect = with(|v| v.mac.screen()).flatten().map_or(DEFAULT_ASPECT, |(w, h)| w as f32 / h as f32);
    let anchor = taskbar::icon_rect_for(Device::Mac);
    let mon = match anchor {
        Some(a) => MonitorFromRect(&a, MONITOR_DEFAULTTOPRIMARY),
        None => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY)
        }
    };
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !GetMonitorInfoW(mon, &mut info).as_bool() {
        return;
    }
    let work = info.rcWork;
    let scale = util::scale(hwnd);
    let m = (MARGIN * scale) as i32;
    let pct = size_percent() as i32;
    let max_w = (width(&work) - 2 * m).max(200);
    let mut h = ((height(&work) - 2 * m) * pct / 100).max(150);
    let mut w = (h as f32 * aspect) as i32;
    if w > max_w {
        w = max_w;
        h = (w as f32 / aspect) as i32;
    }
    let lo = work.left + m;
    let hi = (work.right - m - w).max(lo);
    let x = anchor.map_or(work.left + (width(&work) - w) / 2, |a| a.left).clamp(lo, hi);
    let y = work.bottom - m - h;
    let _ = SetWindowPos(hwnd, HWND_TOP, x, y, w, h, SWP_NOACTIVATE);
    layout(hwnd);
}

/// Çocuk pencerelerin yerleşimi: görüntü tüm iç alanı kaplar; kod alanı formun ortasında.
unsafe fn layout(hwnd: HWND) {
    let mut c = RECT::default();
    let _ = GetClientRect(hwnd, &mut c);
    with(|v| {
        let _ = SetWindowPos(v.video, HWND::default(), 0, 0, width(&c), height(&c), SWP_NOZORDER | SWP_NOACTIVATE);
        let f = to_rect(&form_layout(&c, v.scale).field);
        let inset = (12.0 * v.scale) as i32;
        let text_h = (26.0 * v.scale) as i32;
        let _ = SetWindowPos(
            v.edit,
            HWND::default(),
            f.left + inset,
            f.top + (height(&f) - text_h) / 2,
            width(&f) - 2 * inset,
            text_h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    });
}

/// Bağlıyken görüntü, değilken form. Oynatıcı ilk bağlantıda kurulur ve bağlantı sürdükçe kalır.
unsafe fn update_mode(hwnd: HWND) {
    let Some((connected, video, edit, form_edit)) = with(|v| {
        let state = v.mac.state();
        let connected = matches!(state, State::Connected(_));
        (connected, v.video, v.edit, matches!(state, State::Idle | State::Failed(_)))
    }) else {
        return;
    };
    // Kod alanı gizlenmeden önce odak pencereye alınır (gizlenen denetimde kalmasın).
    if connected && GetFocus() == edit {
        let _ = SetFocus(hwnd);
    }
    let _ = ShowWindow(video, if connected { SW_SHOWNA } else { SW_HIDE });
    let _ = ShowWindow(edit, if form_edit && !connected { SW_SHOWNA } else { SW_HIDE });
    if connected {
        let needs = with(|v| !v.mac.has_player()).unwrap_or(false);
        if needs && gst::available() {
            let player = Player::new(video);
            with(|v| v.mac.set_player(player));
        }
        // GStreamer'ın çizim penceresi de devre dışı kalır: fare ve klavye bu pencereye gelsin.
        disable_children(video);
    } else {
        with(|v| {
            if v.mac.has_player() {
                v.mac.set_player(None);
            }
        });
        if form_edit && IsWindowVisible(hwnd).as_bool() && GetForegroundWindow() == hwnd {
            let _ = SetFocus(edit);
        }
    }
    hub::invalidate(hwnd);
}

unsafe fn disable_children(parent: HWND) {
    unsafe extern "system" fn each(child: HWND, _: LPARAM) -> BOOL {
        if IsWindowEnabled(child).as_bool() {
            let _ = EnableWindow(child, false);
        }
        // Çizim penceresi GStreamer'ın iş parçacığında: girdi kuyruğu bizimkinden ayrılır.
        util::detach_input(child);
        TRUE
    }
    let _ = EnumChildWindows(parent, Some(each), LPARAM(0));
}

enum After {
    None,
    Show,
    Hide,
    Toggle,
    Place,
    Mode,
    Sizer,
    Go,
    Decide,
    Paint,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // AltGr: Windows sağ Alt'tan önce sahte bir sol Ctrl gönderir; Mac'e Command gitmesin.
    if (msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN) && ((lp.0 >> 16) & 0x1FF) == 0x1D {
        let mut next = MSG::default();
        let altgr = PeekMessageW(&mut next, hwnd, WM_KEYDOWN, WM_SYSKEYDOWN, PM_NOREMOVE).as_bool()
            && ((next.lParam.0 >> 16) & 0x1FF) == 0x138
            && next.time == GetMessageTime() as u32;
        if altgr {
            with(|v| v.fake_ctrl = true);
            return LRESULT(0);
        }
    }
    let after = match msg {
        WM_PAINT => {
            with(|v| v.paint());
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_CTLCOLOREDIT => {
            let dc = HDC(wp.0 as _);
            let p = palette(false);
            SetTextColor(dc, colorref(p.text));
            SetBkColor(dc, colorref(p.field));
            return LRESULT(EDIT_BRUSH.with(|b| b.get()));
        }
        WM_TIMER => with(|v| v.tick()).unwrap_or(After::None),
        mac::WM_APP_MAC => After::Mode,
        WM_APP_TOGGLE => After::Toggle,
        WM_APP_SHOW => After::Show,
        WM_APP_PLACE => After::Place,
        WM_APP_SIZER => After::Sizer,
        WM_APP_DISCONNECT => {
            disconnect();
            After::Mode
        }
        WM_APP_GO => After::Go,
        mac::WM_APP_FILES => {
            files_event(hwnd, wp.0);
            return LRESULT(0);
        }
        mac::WM_APP_CURSOR => {
            with(|v| v.rebuild_cursor());
            return LRESULT(0);
        }
        WM_SETCURSOR => {
            // Görüntünün üstünde Windows imleci Mac imlecinin şeklini alır (çift imleç olmasın).
            let hit = (lp.0 & 0xFFFF) as u16;
            if hit == HTCLIENT as u16 || hit == HTERROR as u16 {
                if let Some(c) = with(|v| v.mac.connected().then_some(v.mac_cursor)).flatten().filter(|c| !c.is_invalid()) {
                    SetCursor(c);
                    return LRESULT(1);
                }
            }
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        WM_SIZE => {
            layout(hwnd);
            After::Paint
        }
        WM_DPICHANGED => {
            with(|v| {
                v.scale = (wp.0 & 0xFFFF) as f32 / 96.0;
                v.fonts.delete();
                v.fonts = Fonts::new(v.scale);
                SendMessageW(v.edit, WM_SETFONT, WPARAM(v.fonts.code.0 as usize), LPARAM(1));
                // İmleç DPI'a göre boyutlanır.
                v.rebuild_cursor();
            });
            After::Place
        }
        WM_DISPLAYCHANGE => After::Place,
        WM_CLOSE => After::Hide,
        WM_ACTIVATE => {
            if wp.0 & 0xFFFF == 0 {
                with(|v| v.release_input());
            } else if with(|v| matches!(v.mac.state(), State::Idle | State::Failed(_))).unwrap_or(false) {
                if let Some(edit) = with(|v| v.edit) {
                    let _ = SetFocus(edit);
                }
            } else {
                // Bağlıyken klavye Mac'e gitsin diye odak pencerenin kendisinde.
                let _ = SetFocus(hwnd);
            }
            taskbar::refresh();
            After::None
        }
        WM_KILLFOCUS => {
            with(|v| v.release_input());
            After::None
        }
        WM_MOUSEMOVE => {
            util::note_input_lag("Mac");
            with(|v| v.mouse_move(point(lp))).unwrap_or(After::None)
        }
        hub::WM_MOUSELEAVE => with(|v| {
            v.tracking = false;
            v.hover = None;
            After::Paint
        })
        .unwrap_or(After::None),
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let button = match msg {
                WM_LBUTTONDOWN => 0,
                WM_RBUTTONDOWN => 1,
                _ => 2,
            };
            with(|v| v.button(button, true, point(lp))).unwrap_or(After::None)
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            let button = match msg {
                WM_LBUTTONUP => 0,
                WM_RBUTTONUP => 1,
                _ => 2,
            };
            with(|v| v.button(button, false, point(lp))).unwrap_or(After::None)
        }
        WM_CAPTURECHANGED => {
            with(|v| {
                if v.buttons != 0 {
                    v.release_input();
                }
                v.pressed = None;
            });
            After::Paint
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            // Bir tekerlek çentiği (120) ≈ 3 satır.
            let px = (delta * 36 / 120).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            with(|v| {
                if v.mac.connected() {
                    let m = if msg == WM_MOUSEWHEEL { mac::scroll(0, px) } else { mac::scroll(px, 0) };
                    v.mac.send(m);
                }
            });
            return LRESULT(0);
        }
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let scan = ((lp.0 >> 16) & 0xFF) as u16 | if (lp.0 >> 24) & 1 == 1 { 0xE000 } else { 0 };
            let sent = with(|v| v.key(scan, down)).unwrap_or(false);
            if sent {
                return LRESULT(0);
            }
            if msg == WM_KEYDOWN && wp.0 == 0x1B {
                After::Hide
            } else {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
        }
        // Tuşlar Mac'e gidince bip sesi ve Alt menüsü olmasın.
        WM_CHAR | WM_SYSCHAR | WM_DEADCHAR | WM_SYSDEADCHAR if connected() => return LRESULT(0),
        WM_SYSCOMMAND if (wp.0 & 0xFFF0) == SC_KEYMENU as usize && connected() => return LRESULT(0),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    };
    match after {
        After::None => {}
        After::Show => show(hwnd),
        After::Hide => hide(hwnd),
        After::Toggle => {
            if IsWindowVisible(hwnd).as_bool() && GetForegroundWindow() == hwnd {
                hide(hwnd);
            } else {
                show(hwnd);
            }
        }
        After::Place => {
            if IsWindowVisible(hwnd).as_bool() {
                place(hwnd);
            }
        }
        After::Mode => {
            // Ekran boyutu gelmiş olabilir (oran).
            if IsWindowVisible(hwnd).as_bool() {
                place(hwnd);
            }
            update_mode(hwnd);
            taskbar::refresh();
        }
        After::Sizer => show_sizer(),
        After::Go => {
            with(|v| v.go());
            update_mode(hwnd);
        }
        After::Decide => {
            with(|v| {
                v.decided = true;
                v.mac.confirm(true);
            });
            hub::invalidate(hwnd);
        }
        After::Paint => hub::invalidate(hwnd),
    }
    LRESULT(0)
}

/// Form düzeni (fiziksel piksel): ortada sütun.
struct Form {
    glyph: (f32, f32),
    title: [f32; 4],
    subtitle: [f32; 4],
    field: [f32; 4],
    button: [f32; 4],
    /// Doğrulama kodu adımında iki düğme: "Kodlar aynı" ve "Vazgeç".
    yes: [f32; 4],
    no: [f32; 4],
    note: [f32; 4],
    sas: [f32; 4],
}

fn form_layout(c: &RECT, s: f32) -> Form {
    let (w, h) = (width(c) as f32, height(c) as f32);
    let col = (360.0 * s).min(w - 40.0 * s);
    let x0 = (w - col) / 2.0;
    let x1 = x0 + col;
    let top = (h / 2.0 - 140.0 * s).max(12.0 * s);
    Form {
        glyph: (w / 2.0, top + 30.0 * s),
        title: [x0, top + 70.0 * s, x1, top + 100.0 * s],
        subtitle: [x0 - 40.0 * s, top + 102.0 * s, x1 + 40.0 * s, top + 124.0 * s],
        field: [x0 + 30.0 * s, top + 140.0 * s, x1 - 30.0 * s, top + 182.0 * s],
        button: [x0 + 30.0 * s, top + 194.0 * s, x1 - 30.0 * s, top + 232.0 * s],
        yes: [x0 + 30.0 * s, top + 194.0 * s, (x0 + x1) / 2.0 - 6.0 * s, top + 232.0 * s],
        no: [(x0 + x1) / 2.0 + 6.0 * s, top + 194.0 * s, x1 - 30.0 * s, top + 232.0 * s],
        note: [x0 - 60.0 * s, top + 240.0 * s, x1 + 60.0 * s, top + 280.0 * s],
        sas: [x0, top + 132.0 * s, x1, top + 186.0 * s],
    }
}

impl View {
    fn tick(&mut self) -> After {
        self.mac.check_phone_answer();
        // Kendiliğinden yeniden bağlanma: koptuysa (kullanıcı kesmediyse) artan aralıkla.
        let state = self.mac.state();
        let connected = matches!(state, State::Connected(_));
        if connected {
            if !self.was_connected {
                self.backoff = Duration::from_secs(3);
                // Pencere kapalıyken bağlandı (açılışta ya da yeniden bağlanınca): görüntü bekler.
                if unsafe { !IsWindowVisible(self.hwnd).as_bool() } {
                    self.mac.pause(true);
                }
            }
            self.reconnect_at = None;
            // GStreamer'ın çizim penceresi ilk kareyle oluşur: devre dışı ve girdi kuyruğu ayrı kalsın.
            unsafe { disable_children(self.video) };
        } else if self.mac.dropped() && !self.user_closed && enabled() {
            match self.reconnect_at {
                None => self.reconnect_at = Some(Instant::now() + self.backoff),
                Some(t) if Instant::now() >= t => {
                    self.reconnect_at = None;
                    self.backoff = (self.backoff * 2).min(Duration::from_secs(30));
                    if let Some(code) = saved_code() {
                        self.mac.connect(&code);
                    }
                }
                Some(_) => {}
            }
        }
        self.was_connected = connected;
        if matches!(state, State::Idle | State::Searching | State::Failed(_)) {
            self.decided = false;
        }
        if self.seen.as_ref() != Some(&state) {
            self.seen = Some(state);
            return After::Mode;
        }
        After::None
    }

    fn code(&self) -> String {
        let mut buf = [0u16; 32];
        let n = unsafe { GetWindowTextW(self.edit, &mut buf) } as usize;
        String::from_utf16_lossy(&buf[..n]).chars().filter(|c| c.is_ascii_digit()).collect()
    }

    /// Form düğmesi: bağlı değilse bağlan, bağlanırken vazgeç.
    fn go(&mut self) {
        match self.mac.state() {
            State::Idle | State::Failed(_) => {
                self.decided = false;
                let code = self.code();
                if code.len() < 6 {
                    self.mac.fail("Mac'teki Ayna Ajan menüsündeki kodu yazın.");
                    return;
                }
                self.user_closed = false;
                self.backoff = Duration::from_secs(3);
                self.mac.connect(&code);
            }
            State::Connected(_) => {}
            _ => {
                self.user_closed = true;
                self.mac.disconnect();
            }
        }
    }

    fn release_input(&mut self) {
        self.fake_ctrl = false;
        self.drag_queried = false;
        self.drag_cancelled = false;
        self.released_inside = false;
        if std::mem::take(&mut self.dragging_out) {
            ghost::hide();
        }
        if !self.mac.connected() {
            self.keys.clear();
            self.buttons = 0;
            return;
        }
        for k in std::mem::take(&mut self.keys) {
            self.mac.send(mac::key(k, false));
        }
        let (x, y) = self.last_pos;
        for b in 0..3u8 {
            if self.buttons & (1 << b) != 0 {
                self.mac.send(mac::mouse_button(b, false, x, y));
            }
        }
        self.buttons = 0;
    }

    /// Mac imlecinin resmini penceredeki görüntü ölçeğine getirip Windows imleci yapar.
    fn rebuild_cursor(&mut self) {
        let image = self.mac.cursor();
        // Mac imleci nokta başına 2 piksel gelir; Windows'ta olağan imleç boyutunda (DPI'a göre).
        let new = match image {
            Some(img) => make_cursor(&img, util::scale(self.hwnd) / 2.0),
            None => HCURSOR::default(),
        };
        let old = std::mem::replace(&mut self.mac_cursor, new);
        unsafe {
            // İmleç penceredeyse hemen değişir.
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let over = WindowFromPoint(pt) == self.hwnd || IsChild(self.hwnd, WindowFromPoint(pt)).as_bool();
            let lp = LPARAM(((pt.y as u16 as isize) << 16) | (pt.x as u16 as isize));
            if over && SendMessageW(self.hwnd, WM_NCHITTEST, WPARAM(0), lp).0 == HTCLIENT as isize {
                SetCursor(if new.is_invalid() { LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default() } else { new });
            }
            if !old.is_invalid() {
                let _ = DestroyCursor(old);
            }
        }
    }

    fn normalized(&self, (x, y): (i32, i32)) -> (u16, u16) {
        let mut c = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut c);
        }
        let nx = (x as f32 / width(&c).max(1) as f32).clamp(0.0, 1.0);
        let ny = (y as f32 / height(&c).max(1) as f32).clamp(0.0, 1.0);
        ((nx * 65535.0) as u16, (ny * 65535.0) as u16)
    }

    fn mouse_move(&mut self, pt: (i32, i32)) -> After {
        if !self.tracking {
            self.tracking = true;
            let mut t = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut t);
            }
        }
        if self.mac.connected() {
            if self.dragging_out {
                ghost::follow();
                return After::None;
            }
            // Basılı fareyle pencerenin dışına çıkıldı: Mac'te dosya sürükleniyorsa Windows'a alınır.
            let mut c = RECT::default();
            unsafe {
                let _ = GetClientRect(self.hwnd, &mut c);
            }
            if self.buttons & 1 != 0 && !self.drag_queried && !util::contains(&c, pt.0, pt.1) {
                self.drag_queried = true;
                self.mac.query_drag();
            }
            let (x, y) = self.normalized(pt);
            self.last_pos = (x, y);
            self.mac.send(mac::mouse_move(x, y));
            return After::None;
        }
        let hover = self.control_at(pt);
        if hover != self.hover {
            self.hover = hover;
            return After::Paint;
        }
        After::None
    }

    fn button(&mut self, button: u8, down: bool, pt: (i32, i32)) -> After {
        if self.mac.connected() {
            if button == 0 && down {
                self.drag_queried = false;
                self.released_inside = false;
            }
            if button == 0 && !down {
                let mut c = RECT::default();
                unsafe {
                    let _ = GetClientRect(self.hwnd, &mut c);
                }
                self.released_inside = self.drag_queried && !self.dragging_out && util::contains(&c, pt.0, pt.1);
                self.drag_queried = false;
                if std::mem::take(&mut self.dragging_out) {
                    ghost::hide();
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    // Mac penceresine geri getirilip bırakıldı: vazgeçildi (Windows'a kaydedilmez).
                    let mut c = RECT::default();
                    unsafe {
                        let _ = GetClientRect(self.hwnd, &mut c);
                    }
                    if util::contains(&c, pt.0, pt.1) {
                        self.drag_cancelled = true;
                        toast::show(self.hwnd, "Sürükleme iptal edildi.");
                    }
                    return After::None;
                }
            }
            let (x, y) = self.normalized(pt);
            self.last_pos = (x, y);
            unsafe {
                if down {
                    SetCapture(self.hwnd);
                    self.buttons |= 1 << button;
                } else {
                    self.buttons &= !(1 << button);
                    if self.buttons == 0 {
                        let _ = ReleaseCapture();
                    }
                }
            }
            self.mac.send(mac::mouse_button(button, down, x, y));
            return After::None;
        }
        if button != 0 {
            return After::None;
        }
        if down {
            self.pressed = self.control_at(pt);
            return After::Paint;
        }
        let pressed = self.pressed.take();
        match (pressed, self.control_at(pt)) {
            (Some(Ctl::Button), Some(Ctl::Button)) => After::Go,
            (Some(Ctl::Yes), Some(Ctl::Yes)) => After::Decide,
            _ => After::Paint,
        }
    }

    fn control_at(&self, pt: (i32, i32)) -> Option<Ctl> {
        let mut c = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut c);
        }
        let f = form_layout(&c, self.scale);
        if self.asks_code() {
            if hit(&f.yes, pt.0, pt.1) {
                return Some(Ctl::Yes);
            }
            return hit(&f.no, pt.0, pt.1).then_some(Ctl::Button);
        }
        (!self.mac.connected() && hit(&f.button, pt.0, pt.1)).then_some(Ctl::Button)
    }

    /// İlk eşleşmenin doğrulama kodu adımı (henüz onaylanmadı): iki düğme gösterilir.
    fn asks_code(&self) -> bool {
        !self.decided && matches!(self.mac.state(), State::Waiting { .. } | State::Confirm { .. })
    }

    /// Bağlıyken tuşları Mac'e iletir; iletildiyse true.
    fn key(&mut self, scan: u16, down: bool) -> bool {
        if !self.mac.connected() {
            return false;
        }
        if scan == 0x1D && self.fake_ctrl {
            if !down {
                self.fake_ctrl = false;
            }
            return true;
        }
        let Some(code) = mac::mac_keycode(scan) else { return true };
        if down {
            if !self.keys.contains(&code) {
                self.keys.push(code);
            }
        } else {
            self.keys.retain(|&k| k != code);
        }
        self.mac.send(mac::key(code, down));
        true
    }

    fn paint(&mut self) {
        let state = self.mac.state();
        self.painted = Some(state.clone());
        if matches!(state, State::Connected(_)) {
            // Görüntü çocuk penceresi tüm alanı kaplar; altta yalnız siyah zemin.
            unsafe {
                let mut c = RECT::default();
                let _ = GetClientRect(self.hwnd, &mut c);
                let dc = GetDC(self.hwnd);
                FillRect(dc, &c, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                ReleaseDC(self.hwnd, dc);
                let _ = ValidateRect(self.hwnd, None);
            }
            return;
        }
        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut rc);
            let (w, h) = (width(&rc), height(&rc));
            if w <= 0 || h <= 0 {
                return;
            }
            let s = self.scale;
            let p = palette(false);
            let f = form_layout(&rc, s);
            let mut c = Canvas::new(w as usize, h as usize);
            c.fill_background(p.bg, 1.0);
            draw_mac(&mut c, f.glyph.0, f.glyph.1, s * 2.6, p.secondary);

            let (title, subtitle, button, note, note_color) = match &state {
                State::Waiting { name, .. } => (
                    "Doğrulama kodu".to_string(),
                    format!("{name} ekranındaki kod bu mu?"),
                    "Vazgeç",
                    if self.decided {
                        "Şimdi Mac'te \"İzin ver\"e basın.".to_string()
                    } else {
                        "Aynıysa burada \"Kodlar aynı\"ya, Mac'te \"İzin ver\"e basın.".to_string()
                    },
                    p.secondary,
                ),
                State::Confirm { name, .. } => (
                    "Doğrulama kodu".to_string(),
                    format!("{name} onayladı. Mac'in ekranındaki kod bu mu?"),
                    "Vazgeç",
                    "Kod farklıysa Vazgeç'e basın: ağdaki başka bir cihaz kendini Mac gibi gösteriyor olabilir.".to_string(),
                    p.secondary,
                ),
                State::Searching | State::Connecting(_) => (
                    "Bağlanılıyor…".to_string(),
                    state.text(),
                    "Vazgeç",
                    String::new(),
                    p.secondary,
                ),
                State::Failed(why) => (
                    "Mac'e bağlan".to_string(),
                    "Mac'teki Ayna Ajan menüsündeki kodu yazın.".to_string(),
                    "Bağlan",
                    why.clone(),
                    p.danger,
                ),
                _ => (
                    "Mac'e bağlan".to_string(),
                    "Mac'teki Ayna Ajan menüsündeki kodu yazın.".to_string(),
                    "Bağlan",
                    if gst::available() { String::new() } else { "Görüntü motoru bulunamadı (bin\\uxplay).".into() },
                    p.danger,
                ),
            };
            let show_field = matches!(state, State::Idle | State::Failed(_));
            if show_field {
                card_bg(&mut c, f.field, p.field, 12.0 * s);
            }
            let two = self.asks_code();
            let shade = |ctl: Ctl| {
                if self.pressed == Some(ctl) {
                    0.75
                } else if self.hover == Some(ctl) {
                    0.9
                } else {
                    1.0
                }
            };
            if two {
                card_bg_alpha(&mut c, f.yes, p.accent, 12.0 * s, shade(Ctl::Yes));
                card_bg_alpha(&mut c, f.no, p.switch_off, 12.0 * s, shade(Ctl::Button));
            } else {
                let color = if button == "Bağlan" { p.accent } else { p.switch_off };
                card_bg_alpha(&mut c, f.button, color, 12.0 * s, shade(Ctl::Button));
            }
            let fonts = &self.fonts;
            hub::blit(self.hwnd, w, h, &c, |mem| {
                text(mem, fonts.title, p.text, &title, f.title, DT_CENTER);
                text(mem, fonts.body, p.secondary, &subtitle, f.subtitle, DT_CENTER | DT_END_ELLIPSIS);
                if let State::Waiting { sas, .. } | State::Confirm { sas, .. } = &state {
                    text(mem, fonts.big, p.text, sas, f.sas, DT_CENTER);
                }
                if two {
                    text(mem, fonts.body, [1.0, 1.0, 1.0], "Kodlar aynı", f.yes, DT_CENTER);
                    text(mem, fonts.body, [1.0, 1.0, 1.0], "Vazgeç", f.no, DT_CENTER);
                } else {
                    text(mem, fonts.body, [1.0, 1.0, 1.0], button, f.button, DT_CENTER);
                }
                if !note.is_empty() {
                    let mut r = to_rect(&f.note);
                    let old = SelectObject(mem, fonts.small);
                    SetTextColor(mem, colorref(note_color));
                    let mut t: Vec<u16> = note.encode_utf16().collect();
                    DrawTextW(mem, &mut t, &mut r, DT_CENTER | DT_WORDBREAK | DT_NOPREFIX);
                    SelectObject(mem, old);
                }
            });
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Windows'tan Mac penceresine sürükleme (OLE hedefinden çağrılır; ekran koordinatı)

/// Sürüklenen dosya pencerenin üstünde: Mac'te simgesi ve adı gösterilir. Kabul edilirse true.
/// `enter`: yeni sürükleme (önceki konum unutulur).
pub fn drop_hover(x: i32, y: i32, count: usize, folder: bool, name: &str, enter: bool) -> bool {
    let accepted = with(|v| {
        if !v.mac.connected() || v.mac.phone() {
            return false;
        }
        if enter {
            v.last_hover = None;
        }
        let mut pt = POINT { x, y };
        unsafe {
            let _ = ScreenToClient(v.hwnd, &mut pt);
        }
        let pos = v.normalized((pt.x, pt.y));
        if v.last_hover != Some(pos) {
            v.last_hover = Some(pos);
            v.mac.drag_hover(pos.0, pos.1, count, folder, name);
        }
        true
    })
    .unwrap_or(false);
    if enter && !accepted && !connected() {
        toast::show(main_hwnd(), "Dosya göndermek için önce Mac'e bağlanın.");
    }
    accepted
}

pub fn drop_leave() {
    with(|v| {
        v.last_hover = None;
        v.mac.drag_hover_end();
    });
}

/// Bırakıldı: dosyalar Mac'te bırakılan noktadaki klasöre gider.
pub fn drop_files(x: i32, y: i32, paths: Vec<std::path::PathBuf>) -> bool {
    let sent = with(|v| {
        let mut pt = POINT { x, y };
        unsafe {
            let _ = ScreenToClient(v.hwnd, &mut pt);
        }
        let (nx, ny) = v.normalized((pt.x, pt.y));
        v.mac.send_files(paths, nx, ny)
    })
    .unwrap_or(false);
    if sent {
        toast::show(main_hwnd(), "Mac'e gönderiliyor…");
    }
    sent
}

/// Mac imleci resmini (önceden çarpılmış BGRA) `k` oranında ölçekleyip Windows imleci yapar.
fn make_cursor(img: &mac::CursorImage, k: f32) -> HCURSOR {
    let (sw, sh) = (img.width, img.height);
    let w = ((sw as f32 * k).round() as usize).clamp(1, 256);
    let h = ((sh as f32 * k).round() as usize).clamp(1, 256);
    // Çift doğrusal örnekleme (önceden çarpılmış değerlerde doğru), sonra düz alfaya çevrilir.
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        let fy = ((y as f32 + 0.5) * sh as f32 / h as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..w {
            let fx = ((x as f32 + 0.5) * sw as f32 / w as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let x1 = (x0 + 1).min(sw - 1);
            let px = |xx: usize, yy: usize, c: usize| img.bgra[(yy * sw + xx) * 4 + c] as f32;
            let mut v = [0f32; 4];
            for (c, slot) in v.iter_mut().enumerate() {
                let top = px(x0, y0, c) * (1.0 - tx) + px(x1, y0, c) * tx;
                let bottom = px(x0, y1, c) * (1.0 - tx) + px(x1, y1, c) * tx;
                *slot = top * (1.0 - ty) + bottom * ty;
            }
            let a = v[3];
            let o = (y * w + x) * 4;
            for c in 0..3 {
                out[o + c] = if a > 0.0 { (v[c] * 255.0 / a).round().clamp(0.0, 255.0) as u8 } else { 0 };
            }
            out[o + 3] = a.round().clamp(0.0, 255.0) as u8;
        }
    }
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let screen = GetDC(HWND::default());
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0);
        ReleaseDC(HWND::default(), screen);
        let Ok(color) = color else { return HCURSOR::default() };
        std::ptr::copy_nonoverlapping(out.as_ptr(), bits as *mut u8, out.len());
        // Tek renkli maske boş: şeffaflığı alfa belirler.
        let mask_bytes = vec![0u8; w.div_ceil(16) * 2 * h];
        let mask = CreateBitmap(w as i32, h as i32, 1, 1, Some(mask_bytes.as_ptr() as _));
        let icon = ICONINFO {
            fIcon: FALSE,
            xHotspot: ((img.hot.0 as usize * w / sw) as u32).min(w as u32 - 1),
            yHotspot: ((img.hot.1 as usize * h / sh) as u32).min(h as u32 - 1),
            hbmMask: mask,
            hbmColor: color,
        };
        let cursor = CreateIconIndirect(&icon).map(|h| HCURSOR(h.0)).unwrap_or_default();
        let _ = DeleteObject(color);
        let _ = DeleteObject(mask);
        cursor
    }
}

// ---------------------------------------------------------------------------------------------
// Mac'ten Windows'a sürüklenen dosyalar

unsafe fn files_event(hwnd: HWND, what: usize) {
    match what {
        mac::FILES_LISTED => {
            // Mac'teki sürükleme iptal edilir (Esc, sonra fare bırakılır): dosya orada bir yere
            // düşmesin. Windows'ta fare hâlâ basılı; dosyalar gelince sürükleme burada sürer.
            // Fare Windows'ta zaten bırakıldıysa (dosyalar Masaüstü'ne kaydedilir) iptal gerekmez.
            // İmleç bu arada Mac penceresine geri döndüyse Mac'teki sürükleme sürer: liste unutulur.
            let back_inside = with(|v| {
                let mut pt = POINT::default();
                let mut c = RECT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = ScreenToClient(v.hwnd, &mut pt);
                let _ = GetClientRect(v.hwnd, &mut c);
                let inside = (v.buttons & 1 != 0 && util::contains(&c, pt.x, pt.y)) || std::mem::take(&mut v.released_inside);
                if inside {
                    v.drag_queried = false;
                    let _ = v.mac.take_dragged();
                }
                inside
            })
            .unwrap_or(false);
            if back_inside {
                return;
            }
            with(|v| v.drag_cancelled = false);
            let count = with(|v| {
                if v.buttons & 1 != 0 {
                    v.dragging_out = true;
                    v.mac.send(mac::key(0x35, true));
                    v.mac.send(mac::key(0x35, false));
                    let (x, y) = v.last_pos;
                    v.mac.send(mac::mouse_button(0, false, x, y));
                    v.buttons &= !1;
                }
                v.mac.request_dragged();
                v.mac.dragged_count()
            })
            .unwrap_or(0);
            // Mac'teki sürükleme iptal edildi: dosyalar inene dek simgesi ve adı imleci izler.
            if let Some((name, folder, items)) = with(|v| if v.dragging_out { v.mac.dragged_preview() } else { None }).flatten() {
                ghost::show(&name, folder, items);
            } else if count > 0 {
                toast::show(hwnd, &format!("Mac'ten alınıyor: {count} dosya…"));
            }
        }
        mac::FILES_READY => {
            let Some((items, cancelled)) = with(|v| {
                // OLE sürüklemesi fare bırakışını pencereye ulaştırmaz: bayraklar burada sıfırlanır.
                v.dragging_out = false;
                v.drag_queried = false;
                (v.mac.take_dragged(), std::mem::take(&mut v.drag_cancelled))
            }) else {
                return;
            };
            let _ = ReleaseCapture();
            ghost::hide();
            if cancelled {
                // Kabuk bunları kopyalamadı: geçici klasör hemen silinir.
                if let Some(dir) = items.first().and_then(|p| p.parent()) {
                    let _ = std::fs::remove_dir_all(dir);
                }
                return;
            }
            if items.is_empty() {
                toast::show(hwnd, "Mac'ten dosyalar alınamadı.");
                return;
            }
            let held = windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(0x01 /* VK_LBUTTON */) < 0;
            if held {
                dragout::drag(hwnd, &items);
            } else {
                // Fare dosyalar gelmeden bırakıldı: Masaüstü'ne kaydedilir.
                let desktop = util::desktop_dir();
                let mut saved = 0;
                for item in &items {
                    if let Some(name) = item.file_name() {
                        let target = util::unique_in(&desktop, name);
                        if util::move_path(item, &target) {
                            saved += 1;
                        }
                    }
                }
                let note = if saved == items.len() {
                    format!("Masaüstüne kaydedildi: {saved} öğe")
                } else {
                    format!("Masaüstüne kaydedildi: {saved}/{} öğe", items.len())
                };
                toast::show(hwnd, &note);
            }
        }
        mac::FILES_NOTE => {
            if let Some(note) = with(|v| v.mac.take_note()).flatten() {
                toast::show(hwnd, &note);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Boyut penceresi (Mac ikonuna sağ tık)

struct Sizer {
    hwnd: HWND,
    scale: f32,
    fonts: Fonts,
    dragging: bool,
    hover_button: bool,
    hover_phone: bool,
}

const SIZER_W: f32 = 300.0;
const SIZER_H: f32 = 194.0;

/// Kaydırıcı, iPhone düğmesi, bağlantı düğmesi.
fn sizer_layout(s: f32) -> ([f32; 4], [f32; 4], [f32; 4]) {
    let slider = [20.0 * s, 64.0 * s, (SIZER_W - 20.0) * s, 84.0 * s];
    let phone = [16.0 * s, 102.0 * s, (SIZER_W - 16.0) * s, 136.0 * s];
    let button = [16.0 * s, 146.0 * s, (SIZER_W - 16.0) * s, 180.0 * s];
    (slider, phone, button)
}

/// iPhone kipini açar/kapatır; bağlı değilse önce bağlanılır (kip bağlanınca uygulanır).
fn toggle_phone() {
    // Eşleşilmiş bir Mac yoksa önce kod girilmeli: kip istenmez, yalnız pencere açılır.
    if !connected() && saved_code().filter(|c| Mac::paired(c)).is_none() {
        open();
        return;
    }
    let on = with(|v| {
        let on = !v.mac.phone();
        v.mac.set_phone(on);
        on
    })
    .unwrap_or(false);
    if on {
        open();
    }
}

fn hide_sizer() {
    if let Some(h) = SIZER.with(|c| c.try_borrow().ok().and_then(|o| o.as_ref().map(|z| z.hwnd))) {
        unsafe {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
}

unsafe fn show_sizer() {
    let existing = SIZER.with(|c| c.borrow().as_ref().map(|z| z.hwnd));
    let hwnd = match existing {
        Some(h) => h,
        None => {
            let Ok(module) = GetModuleHandleW(None) else { return };
            let instance: HINSTANCE = module.into();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_DROPSHADOW,
                lpfnWndProc: Some(sizer_proc),
                hInstance: instance,
                hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
                lpszClassName: SIZER_CLASS,
                ..Default::default()
            };
            RegisterClassExW(&wc);
            let Ok(h) = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                SIZER_CLASS,
                w!("Mac ekran boyutu"),
                WS_POPUP,
                0,
                0,
                300,
                150,
                main_hwnd(),
                HMENU::default(),
                instance,
                None,
            ) else {
                return;
            };
            let corner = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, 4);
            let dark = BOOL::from(true);
            let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
            let scale = util::scale(h);
            SIZER.with(|c| {
                *c.borrow_mut() =
                    Some(Sizer { hwnd: h, scale, fonts: Fonts::new(scale), dragging: false, hover_button: false, hover_phone: false })
            });
            h
        }
    };
    // İkonun üstünde, sol kenarı ikonla hizalı.
    let anchor = taskbar::icon_rect_for(Device::Mac).unwrap_or_default();
    let mon = MonitorFromRect(&anchor, MONITOR_DEFAULTTOPRIMARY);
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = GetMonitorInfoW(mon, &mut info);
    let work = info.rcWork;
    let _ = SetWindowPos(hwnd, HWND_TOPMOST, anchor.left, work.bottom - 200, 100, 100, SWP_NOACTIVATE);
    let s = util::scale(hwnd);
    SIZER.with(|c| {
        if let Some(z) = c.borrow_mut().as_mut() {
            z.dragging = false;
            z.hover_button = false;
            z.hover_phone = false;
            if (z.scale - s).abs() > 0.01 {
                z.fonts.delete();
                z.fonts = Fonts::new(s);
                z.scale = s;
            }
        }
    });
    let (w, h) = ((SIZER_W * s) as i32, (SIZER_H * s) as i32);
    let m = (MARGIN * s) as i32;
    let x = anchor.left.clamp(work.left + m, (work.right - m - w).max(work.left + m));
    let y = work.bottom - m - h;
    let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_SHOWWINDOW);
    let _ = SetForegroundWindow(hwnd);
    hub::invalidate(hwnd);
}

fn sizer_value(s: f32, x: i32) -> u32 {
    let (sl, _, _) = sizer_layout(s);
    let t = ((x as f32 - sl[0]) / (sl[2] - sl[0])).clamp(0.0, 1.0);
    let v = SIZE_MIN as f32 + t * (SIZE_MAX - SIZE_MIN) as f32;
    ((v / 5.0).round() as u32 * 5).clamp(SIZE_MIN, SIZE_MAX)
}

fn set_size(pct: u32) {
    if pct == size_percent() {
        return;
    }
    util::write_setting(SIZE_SETTING, pct);
    unsafe {
        let _ = PostMessageW(main_hwnd(), WM_APP_PLACE, WPARAM(0), LPARAM(0));
    }
}

unsafe extern "system" fn sizer_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let s = util::scale(hwnd);
    match msg {
        WM_PAINT => {
            paint_sizer(hwnd, s);
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_ACTIVATE => {
            if wp.0 & 0xFFFF == 0 {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 == 0x1B => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = point(lp);
            let (sl, ph, bt) = sizer_layout(s);
            let grab = [sl[0] - 12.0 * s, sl[1] - 10.0 * s, sl[2] + 12.0 * s, sl[3] + 10.0 * s];
            if hit(&grab, x, y) {
                SIZER.with(|c| c.borrow_mut().as_mut().map(|z| z.dragging = true));
                SetCapture(hwnd);
                set_size(sizer_value(s, x));
                hub::invalidate(hwnd);
            } else if hit(&ph, x, y) {
                let _ = ShowWindow(hwnd, SW_HIDE);
                toggle_phone();
            } else if hit(&bt, x, y) {
                let _ = ShowWindow(hwnd, SW_HIDE);
                if connected() {
                    disconnect();
                } else {
                    open();
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = point(lp);
            let dragging = SIZER.with(|c| c.borrow().as_ref().is_some_and(|z| z.dragging));
            if dragging {
                set_size(sizer_value(s, x));
                hub::invalidate(hwnd);
            } else {
                let (_, ph, bt) = sizer_layout(s);
                let (over, over_phone) = (hit(&bt, x, y), hit(&ph, x, y));
                let changed = SIZER.with(|c| {
                    c.borrow_mut().as_mut().is_some_and(|z| {
                        let ch = z.hover_button != over || z.hover_phone != over_phone;
                        z.hover_button = over;
                        z.hover_phone = over_phone;
                        ch
                    })
                });
                if changed {
                    hub::invalidate(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_CAPTURECHANGED => {
            SIZER.with(|c| c.borrow_mut().as_mut().map(|z| z.dragging = false));
            if msg == WM_LBUTTONUP {
                let _ = ReleaseCapture();
            }
            hub::invalidate(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn paint_sizer(hwnd: HWND, s: f32) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (width(&rc), height(&rc));
    if w <= 0 || h <= 0 {
        return;
    }
    let p = palette(false);
    let pct = size_percent();
    let (sl, ph, bt) = sizer_layout(s);
    let (dragging, hover, hover_phone) =
        SIZER.with(|c| c.borrow().as_ref().map_or((false, false, false), |z| (z.dragging, z.hover_button, z.hover_phone)));
    let phone_on = with(|v| v.mac.phone()).unwrap_or(false);
    let mut c = Canvas::new(w as usize, h as usize);
    c.fill_background(p.card, 1.0);
    let t = (pct - SIZE_MIN) as f32 / (SIZE_MAX - SIZE_MIN) as f32;
    slider(&mut c, sl, t, true, &p, s, dragging);
    let is_connected = connected();
    let color = if is_connected { p.danger } else { p.accent };
    card_bg_alpha(&mut c, bt, color, 10.0 * s, if hover { 0.9 } else { 1.0 });
    card_bg_alpha(&mut c, ph, p.field, 10.0 * s, if hover_phone { 0.8 } else { 1.0 });
    let label = if is_connected { "Bağlantıyı kes" } else { "Mac ekranını aç" };
    let phone_label = if phone_on { "Mac ekranına dön" } else { "iPhone'u Mac üzerinden göster" };
    SIZER.with(|cell| {
        if let Some(z) = cell.borrow().as_ref() {
            hub::blit(hwnd, w, h, &c, |mem| {
                text(mem, z.fonts.name, p.text, "Mac ekran boyutu", [20.0 * s, 14.0 * s, w as f32 - 20.0 * s, 38.0 * s], DT_LEFT);
                text(mem, z.fonts.body, p.secondary, &format!("%{pct}"), [20.0 * s, 14.0 * s, w as f32 - 20.0 * s, 38.0 * s], DT_RIGHT);
                text(mem, z.fonts.body, [1.0, 1.0, 1.0], label, bt, DT_CENTER);
                text(mem, z.fonts.body, p.text, phone_label, ph, DT_CENTER);
            });
        }
    });
}
