//! Mac'in ek ekranları: Windows'taki her ek monitörde bir Mac penceresi.
//!
//! Kipler (Ayarlar › Mac › Ekranlar):
//! - **Tek pencere** (varsayılan): bugünkü gibi yalnız Mac penceresi.
//! - **Aynı ekran**: her monitördeki Mac ikonu aynı Mac ekranını gösteren bir pencere açar.
//! - **Ayrı ekranlar**: her ek monitör Mac'in ayrı bir (sanal) ekranıdır; Mac masaüstü Windows'taki
//!   dizilişle monitörlere yayılır.
//!
//! Birincil monitörde her zaman asıl Mac penceresi (`macview`) çalışır; bu modül diğer monitörlerin
//! pencerelerini yönetir. Pencereler kendiliğinden açılmaz: o monitördeki Mac ikonuyla açılır.
//! Bir monitör Mac'ten kaldırılabilir (o monitördeki Mac ikonunun sağ tık menüsü).

use std::cell::RefCell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::gst::{self, Player};
use crate::i18n::t;
use crate::taskbar::{self, Device};
use crate::{mac, macview, util};

const CLASS: PCWSTR = w!("iPhoneAyna.MacEkran");
const MODE_SETTING: PCWSTR = w!("MacEkranlar");
/// Mac'ten kaldırılan monitörler (aygıt adları, virgülle).
const OFF_SETTING: PCWSTR = w!("MacEkranKapali");
const DEFAULT_ASPECT: f32 = 16.0 / 10.0;
const MARGIN: f32 = 10.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Single,
    Mirror,
    Extended,
}

pub fn mode() -> Mode {
    match util::read_setting(MODE_SETTING) {
        Some(1) => Mode::Mirror,
        Some(2) => Mode::Extended,
        _ => Mode::Single,
    }
}

pub fn mode_label(m: Mode) -> &'static str {
    match m {
        Mode::Single => t("Tek pencere", "Single window"),
        Mode::Mirror => t("Aynı ekran", "Same screen"),
        Mode::Extended => t("Ayrı ekranlar", "Separate screens"),
    }
}

/// Sıradaki kip (Ayarlar'daki düğme).
pub fn cycle_mode() {
    let next = match mode() {
        Mode::Single => 1,
        Mode::Mirror => 2,
        Mode::Extended => 0,
    };
    util::write_setting(MODE_SETTING, next);
    apply();
}

#[derive(Clone, Debug)]
pub struct Monitor {
    pub name: String,
    pub rect: RECT,
    pub work: RECT,
    pub primary: bool,
}

/// Bağlı monitörler, soldan sağa (sonra yukarıdan aşağı).
pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn each(mon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = &mut *(data.0 as *mut Vec<Monitor>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(mon, &mut info.monitorInfo).as_bool() {
            let n = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            list.push(Monitor {
                name: String::from_utf16_lossy(&info.szDevice[..n]),
                rect: info.monitorInfo.rcMonitor,
                work: info.monitorInfo.rcWork,
                primary: info.monitorInfo.dwFlags & 1 != 0,
            });
        }
        TRUE
    }
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(HDC::default(), None, Some(each), LPARAM(&mut list as *mut _ as isize));
    }
    list.sort_by_key(|m| (m.rect.left, m.rect.top));
    list
}

fn off_list() -> Vec<String> {
    util::read_text_setting(OFF_SETTING)
        .map(|s| s.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

pub fn monitor_enabled(name: &str) -> bool {
    !off_list().iter().any(|n| n == name)
}

pub fn set_monitor_enabled(name: &str, on: bool) {
    let mut list = off_list();
    list.retain(|n| n != name);
    if !on {
        list.push(name.to_string());
    }
    let _ = util::write_text_setting(OFF_SETTING, &list.join(","));
    apply();
}

/// Ek monitörler ve sıra numaraları (1..): birincil dışındakiler, soldan sağa. Kaldırılan monitörün
/// numarası da ayrılır (diğerlerinin numarası kaymasın).
fn numbered() -> Vec<(u8, Monitor)> {
    monitors().into_iter().filter(|m| !m.primary).enumerate().map(|(i, m)| (i as u8 + 1, m)).take(8).collect()
}

/// Mac'e açık ek monitörler.
fn extras() -> Vec<(u8, Monitor)> {
    numbered().into_iter().filter(|(_, m)| monitor_enabled(&m.name)).collect()
}

/// Ayrı ekranlar kipinde Mac'e bildirilecek düzen: [sıra, genişlik, yükseklik, x, y] (birincil
/// monitörün sol üst köşesine göre).
pub fn layout() -> Vec<(u8, u16, u16, i16, i16)> {
    if mode() != Mode::Extended {
        return Vec::new();
    }
    let Some(primary) = monitors().into_iter().find(|m| m.primary) else { return Vec::new() };
    extras()
        .into_iter()
        .map(|(i, m)| {
            (
                i,
                util::width(&m.rect) as u16,
                util::height(&m.rect) as u16,
                (m.rect.left - primary.rect.left) as i16,
                (m.rect.top - primary.rect.top) as i16,
            )
        })
        .collect()
}

/// Kip ya da monitörler değişti: Mac'e yeni düzen gider; artık geçersiz pencereler kapanır.
pub fn apply() {
    let mode = mode();
    let layout = layout();
    // Pencere ancak numarası hâlâ aynı monitörü gösteriyorsa kalır (monitör takılıp çıkarılınca numaralar kayar).
    let valid: Vec<(u8, String)> =
        if mode == Mode::Single { Vec::new() } else { extras().into_iter().map(|(i, m)| (i, m.name)).collect() };
    let primary = monitors().into_iter().find(|m| m.primary).map_or((1920, 1080), |m| (util::width(&m.rect) as u16, util::height(&m.rect) as u16));
    macview::with_mac(|m| {
        m.set_mirror(mode == Mode::Mirror);
        m.set_layout(primary, &layout);
    });
    let gone: Vec<(u8, HWND, HWND)> = with_all(|list| {
        list.iter().filter(|s| !valid.iter().any(|(i, n)| *i == s.index && *n == s.monitor)).map(|s| (s.index, s.hwnd, s.owner)).collect()
    });
    for (index, hwnd, owner) in gone {
        // Önce oynatıcı bırakılır (yok edilen pencereye çizmeye çalışmasın), sonra pencere.
        macview::with_mac(|m| m.set_screen_player(index, None));
        hide(index);
        SCREENS.with(|c| {
            if let Ok(mut v) = c.try_borrow_mut() {
                v.retain(|s| s.hwnd != hwnd);
            }
        });
        unsafe {
            let _ = DestroyWindow(hwnd);
            let _ = DestroyWindow(owner);
        }
    }
    // Kip değişince boyut ve oynatıcı yeniden kurulsun (aynı ekran ↔ ayrı ekranlar).
    let all: Vec<u8> = with_all(|list| list.iter().map(|s| s.index).collect());
    for index in all {
        with(index, |s| s.size = None);
        macview::with_mac(|m| m.set_screen_player(index, None));
    }
    sync();
    macview::update_pause();
}

/// Mac ikonunun en son tıklandığı monitör (ek monitörse ve Mac'e açıksa) sıra numarasıyla.
pub fn anchor_screen() -> Option<(u8, Monitor)> {
    if mode() == Mode::Single {
        return None;
    }
    let anchor = taskbar::icon_rect_for(Device::Mac)?;
    let mon = unsafe { MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if !unsafe { GetMonitorInfoW(mon, &mut info.monitorInfo) }.as_bool() {
        return None;
    }
    let n = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
    let name = String::from_utf16_lossy(&info.szDevice[..n]);
    numbered().into_iter().find(|(_, m)| m.name == name)
}

/// Mac ikonuna tıklandı: tıklanan ek monitör Mac'e açıksa onun penceresi açılır/kapanır (true).
pub fn handle_click(show_only: bool) -> bool {
    let connected = macview::with_mac(|m| m.connected()).unwrap_or(false);
    let Some((index, monitor)) = anchor_screen().filter(|(_, m)| monitor_enabled(&m.name)) else { return false };
    if !connected {
        return false;
    }
    let open = with(index, |s| unsafe { IsWindowVisible(s.hwnd).as_bool() && GetForegroundWindow() == s.hwnd }).unwrap_or(false);
    if open && !show_only {
        hide(index);
    } else {
        show(index, &monitor);
    }
    true
}

/// Aynı ekran kipinde ana görüntüyü gösteren bir ek pencere açık mı (ana akış durmasın).
pub fn mirror_visible() -> bool {
    mode() == Mode::Mirror && with_all(|list| list.iter().any(|s| unsafe { IsWindowVisible(s.hwnd).as_bool() }))
}

struct Screen {
    index: u8,
    monitor: String,
    hwnd: HWND,
    owner: HWND,
    video: HWND,
    /// Pencerenin ölçülendirildiği görüntü boyutu.
    size: Option<(u16, u16)>,
    buttons: u8,
    keys: Vec<u16>,
    fake_ctrl: bool,
    last_pos: (u16, u16),
}

thread_local! {
    static SCREENS: RefCell<Vec<Screen>> = const { RefCell::new(Vec::new()) };
}

fn with<R>(index: u8, f: impl FnOnce(&mut Screen) -> R) -> Option<R> {
    SCREENS.with(|c| c.try_borrow_mut().ok().and_then(|mut v| v.iter_mut().find(|s| s.index == index).map(f)))
}

fn with_hwnd<R>(hwnd: HWND, f: impl FnOnce(&mut Screen) -> R) -> Option<R> {
    SCREENS.with(|c| c.try_borrow_mut().ok().and_then(|mut v| v.iter_mut().find(|s| s.hwnd == hwnd).map(f)))
}

fn with_all<R: Default>(f: impl FnOnce(&Vec<Screen>) -> R) -> R {
    SCREENS.with(|c| c.try_borrow().ok().map(|v| f(&v)).unwrap_or_default())
}

fn create(index: u8, monitor: &str) -> Option<HWND> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None).ok()?.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hIcon: taskbar::app_icon(32),
            hIconSm: taskbar::app_icon(16),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // Sahipli pencerenin görev çubuğu düğmesi olmaz: giriş o monitördeki Mac ikonu.
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
        let video = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            macview::VIDEO_CLASS,
            w!(""),
            WS_CHILD | WS_DISABLED | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            0,
            0,
            0,
            0,
            hwnd,
            HMENU::default(),
            instance,
            None,
        );
        let Ok(video) = video else {
            let _ = DestroyWindow(hwnd);
            let _ = DestroyWindow(owner);
            return None;
        };
        SCREENS.with(|c| {
            c.borrow_mut().push(Screen {
                index,
                monitor: monitor.to_string(),
                hwnd,
                owner,
                video,
                size: None,
                buttons: 0,
                keys: Vec::new(),
                fake_ctrl: false,
                last_pos: (0, 0),
            })
        });
        Some(hwnd)
    }
}

/// Bu pencerenin göstereceği görüntünün boyutu.
fn screen_size(index: u8) -> Option<(u16, u16)> {
    macview::with_mac(|m| if mode() == Mode::Mirror { m.screen() } else { m.screen_size(index) }).flatten()
}

/// Mac penceresi gibi: o monitördeki Mac ikonunun üstünde, ayarlı boyda, görüntünün oranında.
unsafe fn place(hwnd: HWND, index: u8) {
    let Some(monitor) = with(index, |s| s.monitor.clone()) else { return };
    let Some(m) = monitors().into_iter().find(|x| x.name == monitor) else { return };
    let aspect = screen_size(index).map_or(DEFAULT_ASPECT, |(w, h)| w as f32 / h.max(1) as f32);
    let work = m.work;
    let scale = util::scale(hwnd);
    let margin = (MARGIN * scale) as i32;
    let pct = macview::size_percent() as i32;
    let max_w = (util::width(&work) - 2 * margin).max(200);
    let mut h = ((util::height(&work) - 2 * margin) * pct / 100).max(150);
    let mut w = (h as f32 * aspect) as i32;
    if w > max_w {
        w = max_w;
        h = (w as f32 / aspect) as i32;
    }
    let anchor = taskbar::icon_rect_on(Device::Mac, &m.rect);
    let lo = work.left + margin;
    let hi = (work.right - margin - w).max(lo);
    let x = anchor.map_or(work.left + (util::width(&work) - w) / 2, |a| a.left).clamp(lo, hi);
    let y = work.bottom - margin - h;
    let _ = SetWindowPos(hwnd, HWND_TOP, x, y, w, h, SWP_NOACTIVATE);
    layout_video(hwnd);
}

unsafe fn layout_video(hwnd: HWND) {
    let mut c = RECT::default();
    let _ = GetClientRect(hwnd, &mut c);
    if let Some(video) = with_hwnd(hwnd, |s| s.video) {
        let _ = MoveWindow(video, 0, 0, util::width(&c), util::height(&c), true);
    }
}

fn show(index: u8, monitor: &Monitor) {
    let hwnd = with(index, |s| s.hwnd).or_else(|| create(index, &monitor.name));
    let Some(hwnd) = hwnd else { return };
    unsafe {
        place(hwnd, index);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    if mode() == Mode::Extended {
        macview::with_mac(|m| m.set_screen_paused(index, false));
    }
    macview::update_pause();
    sync();
}

fn hide(index: u8) {
    let Some(hwnd) = with(index, |s| s.hwnd) else { return };
    release_input(hwnd);
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    macview::with_mac(|m| {
        if mode() == Mode::Extended {
            m.set_screen_paused(index, true);
        }
        // Fare ve klavye yeniden ana ekrana.
        m.set_screen_target(0);
    });
    macview::update_pause();
}

/// Boyut ayarı değişti.
pub fn replace() {
    let open: Vec<(u8, HWND)> =
        with_all(|list| list.iter().filter(|s| unsafe { IsWindowVisible(s.hwnd).as_bool() }).map(|s| (s.index, s.hwnd)).collect());
    for (index, hwnd) in open {
        unsafe { place(hwnd, index) };
    }
}

/// Pencereleri bağlantının durumuna getirir: oynatıcılar, boyutlar. Mac penceresinin zamanlayıcısından
/// ve Mac'ten ekran boyutu gelince çağrılır.
pub fn sync() {
    let list: Vec<(u8, HWND, HWND, Option<(u16, u16)>)> = with_all(|v| v.iter().map(|s| (s.index, s.hwnd, s.video, s.size)).collect());
    let connected = macview::with_mac(|m| m.connected()).unwrap_or(false);
    for (index, hwnd, video, sized) in list {
        unsafe {
            let visible = IsWindowVisible(hwnd).as_bool();
            if !connected {
                if visible {
                    hide(index);
                }
                macview::with_mac(|m| m.set_screen_player(index, None));
                let _ = ShowWindow(video, SW_HIDE);
                continue;
            }
            let size = screen_size(index);
            if size.is_some() && size != sized {
                with(index, |s| s.size = size);
                if visible {
                    place(hwnd, index);
                }
            }
            if visible && size.is_some() {
                let _ = ShowWindow(video, SW_SHOWNA);
                if !macview::with_mac(|m| m.has_screen_player(index)).unwrap_or(true) && gst::available() {
                    let player = Player::new(video);
                    macview::with_mac(|m| m.set_screen_player(index, player));
                }
                macview::disable_children(video);
            }
        }
    }
}

fn release_input(hwnd: HWND) {
    let Some((keys, buttons, pos)) = with_hwnd(hwnd, |s| (std::mem::take(&mut s.keys), std::mem::take(&mut s.buttons), s.last_pos)) else {
        return;
    };
    macview::with_mac(|m| {
        if !m.connected() {
            return;
        }
        for k in keys {
            m.send(mac::key(k, false));
        }
        for b in 0..3u8 {
            if buttons & (1 << b) != 0 {
                m.send(mac::mouse_button(b, false, pos.0, pos.1));
            }
        }
    });
}

fn normalized(hwnd: HWND, x: i32, y: i32) -> (u16, u16) {
    let mut c = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut c);
    }
    let axis = |v: i32, span: i32| ((v as f32 / (span - 1).max(1) as f32).clamp(0.0, 1.0) * 65535.0).round() as u16;
    (axis(x, util::width(&c)), axis(y, util::height(&c)))
}

/// Fare bu penceredeyken Mac'te hangi ekrana eşlensin (aynı ekran kipinde ana ekran).
fn target(m: &mac::Mac, index: u8) {
    m.set_target(Some(false), None);
    m.set_screen_target(if mode() == Mode::Extended { index } else { 0 });
}

fn point(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as u16 as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32)
}

unsafe fn mouse(hwnd: HWND, msg: u32, lp: LPARAM) {
    let Some(index) = with_hwnd(hwnd, |s| s.index) else { return };
    let (x, y) = point(lp);
    let pos = normalized(hwnd, x, y);
    let button = match msg {
        WM_LBUTTONDOWN | WM_LBUTTONUP => Some(0u8),
        WM_RBUTTONDOWN | WM_RBUTTONUP => Some(1),
        WM_MBUTTONDOWN | WM_MBUTTONUP => Some(2),
        _ => None,
    };
    let down = matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN);
    let held = with_hwnd(hwnd, |s| {
        s.last_pos = pos;
        if let Some(b) = button {
            if down {
                s.buttons |= 1 << b;
            } else {
                s.buttons &= !(1 << b);
            }
        }
        s.buttons
    })
    .unwrap_or(0);
    if let Some(b) = button {
        if down {
            SetCapture(hwnd);
        } else if held == 0 {
            let _ = ReleaseCapture();
        }
        macview::with_mac(|m| {
            if m.connected() {
                target(m, index);
                m.send(mac::mouse_button(b, down, pos.0, pos.1));
            }
        });
    } else {
        macview::with_mac(|m| {
            if m.connected() {
                target(m, index);
                m.send(mac::mouse_move(pos.0, pos.1));
            }
        });
    }
}

fn key(hwnd: HWND, scan: u16, down: bool) {
    let code = with_hwnd(hwnd, |s| {
        if scan == 0x1D && s.fake_ctrl {
            if !down {
                s.fake_ctrl = false;
            }
            return None;
        }
        let code = mac::mac_keycode(scan)?;
        if down {
            if !s.keys.contains(&code) {
                s.keys.push(code);
            }
        } else {
            s.keys.retain(|&k| k != code);
        }
        Some(code)
    })
    .flatten();
    if let Some(code) = code {
        macview::with_mac(|m| {
            if m.connected() {
                m.send(mac::key(code, down));
            }
        });
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut ps);
    let waiting = with_hwnd(hwnd, |s| !IsWindowVisible(s.video).as_bool()).unwrap_or(false);
    if waiting {
        let mut c = RECT::default();
        let _ = GetClientRect(hwnd, &mut c);
        let text = if mode() == Mode::Extended {
            t("Mac ekranı açılıyor… (Mac'teki Ayna Ajan güncel olmalı)", "Opening the Mac screen… (Ayna Ajan on the Mac must be up to date)")
        } else {
            t("Mac ekranı açılıyor…", "Opening the Mac screen…")
        };
        let mut label: Vec<u16> = text.encode_utf16().collect();
        let old = SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, util::rgb(220, 220, 220));
        DrawTextW(dc, &mut label, &mut c, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        SelectObject(dc, old);
    }
    let _ = EndPaint(hwnd, &ps);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // AltGr: Windows sağ Alt'tan önce sahte bir sol Ctrl gönderir; Mac'e Command gitmesin.
    if (msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN) && ((lp.0 >> 16) & 0x1FF) == 0x1D {
        let mut next = MSG::default();
        let altgr = PeekMessageW(&mut next, hwnd, WM_KEYDOWN, WM_SYSKEYDOWN, PM_NOREMOVE).as_bool()
            && ((next.lParam.0 >> 16) & 0x1FF) == 0x138
            && next.time == GetMessageTime() as u32;
        if altgr {
            with_hwnd(hwnd, |s| s.fake_ctrl = true);
            return LRESULT(0);
        }
    }
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_SIZE => {
            layout_video(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            if let Some(index) = with_hwnd(hwnd, |s| s.index) {
                hide(index);
            }
            LRESULT(0)
        }
        WM_SETFOCUS => {
            macview::with_mac(|m| m.set_target(None, Some(false)));
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            release_input(hwnd);
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            if with_hwnd(hwnd, |s| s.buttons != 0).unwrap_or(false) {
                release_input(hwnd);
            }
            LRESULT(0)
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u16 == HTCLIENT as u16 => {
            // Windows imleci Mac imlecinin şeklini alır (görüntüde Mac imleci çizilmez).
            let c = macview::mac_cursor();
            if c.is_invalid() {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
            SetCursor(c);
            LRESULT(1)
        }
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN | WM_MBUTTONUP => {
            mouse(hwnd, msg, lp);
            LRESULT(0)
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            // Bir tekerlek çentiği (120) ≈ 3 satır.
            let px = (delta * 36 / 120).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            if let Some(index) = with_hwnd(hwnd, |s| s.index) {
                macview::with_mac(|m| {
                    if m.connected() {
                        target(m, index);
                        m.send(if msg == WM_MOUSEWHEEL { mac::scroll(0, px) } else { mac::scroll(px, 0) });
                    }
                });
            }
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            key(hwnd, ((lp.0 >> 16) & 0xFF) as u16 | if (lp.0 >> 24) & 1 == 1 { 0xE000 } else { 0 }, down);
            LRESULT(0)
        }
        // Tuşlar Mac'e gidince bip sesi ve Alt menüsü olmasın.
        WM_CHAR | WM_SYSCHAR | WM_DEADCHAR | WM_SYSDEADCHAR => LRESULT(0),
        WM_SYSCOMMAND if (wp.0 & 0xFFF0) == SC_KEYMENU as usize => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
