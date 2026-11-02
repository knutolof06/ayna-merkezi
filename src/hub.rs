//! Ayna Merkezi: uygulamanın ana penceresi.
//!
//! iPhone ve Mac'i açıp kapatma (kapalıyken görev çubuğu ikonları da kalkar), iPhone ekran boyutu,
//! Mac bağlantı durumu ve genel ayarlar. Çerçevesiz, yuvarlak köşeli tek bir pencere; denetimler
//! (anahtar, kaydırıcı, düğme) kendi çizilir. Buradaki çizim yardımcılarını Mac penceresi de kullanır.

use std::cell::RefCell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::macview;
use crate::mirror;
use crate::taskbar::{self, draw_mac, draw_phone, fill, hex, round_rect_sdf, Canvas, Device, WM_APP_HUB};
use crate::update;
use crate::util::{self, height, width};

pub const CLASS: PCWSTR = w!("iPhoneAyna.Merkez");
const TIMER_STATUS: usize = 1;
pub(crate) const WM_MOUSELEAVE: u32 = 0x02A3;
const ZOOM_SETTING: PCWSTR = w!("BuyutmeYenile");

/// Pencere genişliği ve kart ölçüleri (96 DPI'da piksel).
const W: f32 = 400.0;
const PAD: f32 = 16.0;
const TITLE_H: f32 = 56.0;
const IPHONE_H: f32 = 142.0;
const MAC_H: f32 = 150.0;
const GENERAL_H: f32 = 108.0;
const GAP: f32 = 12.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ctl {
    Close,
    IPhoneSwitch,
    Slider,
    MacSwitch,
    MacButton,
    AutostartSwitch,
    ZoomSwitch,
}

struct Hub {
    hwnd: HWND,
    scale: f32,
    light: bool,
    hover: Option<Ctl>,
    pressed: Option<Ctl>,
    dragging: bool,
    tracking: bool,
    fonts: Fonts,
    /// Son çizimdeki durum metinleri; değişmedikçe zamanlayıcı yeniden çizmez.
    painted: (String, String, bool, bool),
}

pub(crate) struct Fonts {
    pub title: HFONT,
    pub name: HFONT,
    pub body: HFONT,
    pub small: HFONT,
    pub code: HFONT,
    pub big: HFONT,
}

impl Fonts {
    pub(crate) fn new(scale: f32) -> Fonts {
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
        Fonts {
            title: make(18.0, 600),
            name: make(15.0, 600),
            body: make(14.0, 400),
            small: make(12.0, 400),
            code: make(17.0, 500),
            big: make(30.0, 600),
        }
    }

    pub(crate) fn delete(&self) {
        unsafe {
            for f in [self.title, self.name, self.body, self.small, self.code, self.big] {
                let _ = DeleteObject(f);
            }
        }
    }
}

pub(crate) struct Palette {
    pub bg: [f32; 3],
    pub card: [f32; 3],
    pub field: [f32; 3],
    pub text: [f32; 3],
    pub secondary: [f32; 3],
    pub separator: [f32; 3],
    pub accent: [f32; 3],
    pub switch_off: [f32; 3],
    pub danger: [f32; 3],
}

pub(crate) fn palette(light: bool) -> Palette {
    if light {
        Palette {
            bg: hex(0xF2F2F7),
            card: hex(0xFFFFFF),
            field: hex(0xEFEFF4),
            text: hex(0x1C1C1E),
            secondary: hex(0x6E6E73),
            separator: hex(0xE5E5EA),
            accent: hex(0x007AFF),
            switch_off: hex(0xE3E3E8),
            danger: hex(0xFF3B30),
        }
    } else {
        Palette {
            bg: hex(0x1C1C1E),
            card: hex(0x2C2C2E),
            field: hex(0x3A3A3C),
            text: hex(0xF5F5F7),
            secondary: hex(0x98989D),
            separator: hex(0x3A3A3C),
            accent: hex(0x0A84FF),
            switch_off: hex(0x48484A),
            danger: hex(0xFF453A),
        }
    }
}

pub(crate) fn colorref(c: [f32; 3]) -> COLORREF {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    util::rgb(b(c[0]), b(c[1]), b(c[2]))
}

thread_local! {
    static STATE: RefCell<Option<Hub>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Hub) -> R) -> Option<R> {
    STATE.with(|c| c.try_borrow_mut().ok().and_then(|mut o| o.as_mut().map(f)))
}

pub fn create() -> Option<HWND> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None).ok()?.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hIcon: taskbar::app_icon(32),
            hIconSm: taskbar::app_icon(16),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // Sahipli pencerenin görev çubuğu düğmesi olmaz: girişler Başlat'ın solundaki ikonlar.
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
            w!("Ayna Merkezi"),
            WS_POPUP | WS_CLIPCHILDREN | WS_SYSMENU,
            0,
            0,
            400,
            500,
            owner,
            HMENU::default(),
            instance,
            None,
        )
        .ok()?;
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, 4);

        let scale = util::scale(hwnd);
        let hub = Hub {
            hwnd,
            scale,
            light: util::light_apps(),
            hover: None,
            pressed: None,
            dragging: false,
            tracking: false,
            fonts: Fonts::new(scale),
            painted: Default::default(),
        };
        STATE.with(|c| *c.borrow_mut() = Some(hub));
        with(|h| h.apply_theme());
        taskbar::set_hub(hwnd);
        SetTimer(hwnd, TIMER_STATUS, 500, None);
        Some(hwnd)
    }
}

/// Pencereyi imlecin bulunduğu monitörün ortasında açar.
pub fn show(hwnd: HWND) {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetMonitorInfoW(mon, &mut info).as_bool() {
                let work = info.rcWork;
                // Önce o monitöre taşınır ki ölçek (DPI) doğru monitörden okunsun.
                let _ = SetWindowPos(hwnd, HWND::default(), work.left + 40, work.top + 40, 100, 100, SWP_NOZORDER | SWP_NOACTIVATE);
                let s = util::scale(hwnd);
                let (w, h) = ((W * s).round() as i32, (total_height() * s).round() as i32);
                let x = work.left + (width(&work) - w) / 2;
                let y = work.top + (height(&work) - h) / 2;
                let _ = SetWindowPos(hwnd, HWND::default(), x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        with(|h| h.apply_theme());
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn total_height() -> f32 {
    TITLE_H + IPHONE_H + GAP + MAC_H + GAP + GENERAL_H + PAD
}

/// Kartların ve denetimlerin konumları (fiziksel piksel).
struct Layout {
    close: [f32; 4],
    iphone: [f32; 4],
    iphone_switch: [f32; 4],
    slider: [f32; 4],
    mac: [f32; 4],
    mac_switch: [f32; 4],
    mac_button: [f32; 4],
    general: [f32; 4],
    autostart_switch: [f32; 4],
    zoom_switch: [f32; 4],
}

fn layout(s: f32) -> Layout {
    let x0 = PAD * s;
    let x1 = (W - PAD) * s;
    let iy = TITLE_H * s;
    let my = iy + (IPHONE_H + GAP) * s;
    let gy = my + (MAC_H + GAP) * s;
    let sw = |y: f32| [x1 - (16.0 + 46.0) * s, y, x1 - 16.0 * s, y + 28.0 * s];
    Layout {
        close: [(W - 44.0) * s, 14.0 * s, (W - 16.0) * s, 42.0 * s],
        iphone: [x0, iy, x1, iy + IPHONE_H * s],
        iphone_switch: sw(iy + 18.0 * s),
        slider: [x0 + 16.0 * s, iy + 106.0 * s, x1 - 16.0 * s, iy + 126.0 * s],
        mac: [x0, my, x1, my + MAC_H * s],
        mac_switch: sw(my + 18.0 * s),
        mac_button: [x0 + 16.0 * s, my + 78.0 * s, x1 - 16.0 * s, my + 112.0 * s],
        general: [x0, gy, x1, gy + GENERAL_H * s],
        autostart_switch: sw(gy + 13.0 * s),
        zoom_switch: sw(gy + 67.0 * s),
    }
}

pub(crate) fn hit(b: &[f32; 4], x: i32, y: i32) -> bool {
    let (x, y) = (x as f32, y as f32);
    x >= b[0] && x < b[2] && y >= b[1] && y < b[3]
}

pub(crate) fn to_rect(b: &[f32; 4]) -> RECT {
    RECT { left: b[0].round() as i32, top: b[1].round() as i32, right: b[2].round() as i32, bottom: b[3].round() as i32 }
}

enum Action {
    None,
    Hide,
    ToggleIPhone,
    Size(u32),
    ToggleMac,
    MacButton,
    Autostart,
    Zoom,
    Paint,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let action = match msg {
        WM_NCHITTEST => {
            let r = DefWindowProcW(hwnd, msg, wp, lp);
            if r.0 as u32 != HTCLIENT {
                return r;
            }
            let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut pt);
            let s = util::scale(hwnd);
            // Başlık şeridi pencereyi taşır.
            if (pt.y as f32) < TITLE_H * s && !hit(&layout(s).close, pt.x, pt.y) {
                return LRESULT(HTCAPTION as isize);
            }
            return r;
        }
        WM_PAINT => {
            with(|h| h.paint());
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_TIMER => {
            with(|h| h.refresh_if_changed());
            apply_update_when_idle();
            return LRESULT(0);
        }
        update::WM_APP_UPDATE => {
            UPDATE_PENDING.with(|p| p.set(true));
            return LRESULT(0);
        }
        WM_MOUSEMOVE => with(|h| h.mouse_move(point(lp))).unwrap_or(Action::None),
        WM_MOUSELEAVE => with(|h| {
            h.tracking = false;
            h.hover = None;
            Action::Paint
        })
        .unwrap_or(Action::None),
        WM_LBUTTONDOWN => with(|h| h.mouse_down(point(lp))).unwrap_or(Action::None),
        WM_LBUTTONUP => with(|h| h.mouse_up(point(lp))).unwrap_or(Action::None),
        WM_CAPTURECHANGED => {
            with(|h| {
                h.dragging = false;
                h.pressed = None;
            });
            Action::Paint
        }
        WM_KEYDOWN if wp.0 == 0x1B => Action::Hide,
        WM_DPICHANGED => {
            let suggested = &*(lp.0 as *const RECT);
            with(|h| {
                h.scale = (wp.0 & 0xFFFF) as f32 / 96.0;
                h.fonts.delete();
                h.fonts = Fonts::new(h.scale);
                h.apply_theme();
            });
            let s = (wp.0 & 0xFFFF) as f32 / 96.0;
            let _ = SetWindowPos(
                hwnd,
                HWND::default(),
                suggested.left,
                suggested.top,
                (W * s).round() as i32,
                (total_height() * s).round() as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            Action::Paint
        }
        WM_SETTINGCHANGE => {
            with(|h| h.apply_theme());
            Action::Paint
        }
        WM_CLOSE => Action::Hide,
        WM_APP_HUB => {
            show(hwnd);
            Action::None
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    };
    // Durum ödünç alınmadan: ayar değiştirmek başka pencerelere mesaj gönderebilir.
    match action {
        Action::None => {}
        Action::Hide => {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        Action::ToggleIPhone => {
            mirror::set_enabled(!mirror::enabled());
            invalidate(hwnd);
        }
        Action::Size(pct) => {
            mirror::set_size_percent(pct);
            invalidate(hwnd);
        }
        Action::ToggleMac => {
            macview::set_enabled(!macview::enabled());
            invalidate(hwnd);
        }
        Action::MacButton => {
            if macview::connected() {
                macview::disconnect();
            } else {
                macview::open();
            }
            invalidate(hwnd);
        }
        Action::Autostart => {
            util::set_autostart(!util::autostart_enabled());
            invalidate(hwnd);
        }
        Action::Zoom => {
            let on = util::read_setting(ZOOM_SETTING) != Some(0);
            util::write_setting(ZOOM_SETTING, u32::from(!on));
            invalidate(hwnd);
        }
        Action::Paint => invalidate(hwnd),
    }
    LRESULT(0)
}

thread_local! {
    /// Bir güncelleme indirildi; uygulama kullanılmıyorken uygulanacak.
    static UPDATE_PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// İndirilmiş güncelleme, hiçbir pencere açık değilken (iPhone, Mac, Ayna Merkezi) uygulanır;
/// uygulama yeni sürümle yeniden başlar.
fn apply_update_when_idle() {
    if !UPDATE_PENDING.with(|p| p.get()) {
        return;
    }
    unsafe {
        let visible = |class: PCWSTR| {
            FindWindowW(class, PCWSTR::null()).is_ok_and(|h| IsWindowVisible(h).as_bool())
        };
        if visible(mirror::CLASS) || visible(crate::macview::CLASS) || visible(CLASS) {
            return;
        }
        UPDATE_PENDING.with(|p| p.set(false));
        if update::apply() {
            if let Ok(m) = FindWindowW(mirror::CLASS, PCWSTR::null()) {
                let _ = DestroyWindow(m);
            }
        }
    }
}

pub(crate) fn point(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

pub(crate) fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

impl Hub {
    fn apply_theme(&mut self) {
        self.light = util::light_apps();
        let dark = BOOL::from(!self.light);
        unsafe {
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        }
    }

    fn control_at(&self, x: i32, y: i32) -> Option<Ctl> {
        let l = layout(self.scale);
        [
            (Ctl::Close, l.close),
            (Ctl::IPhoneSwitch, l.iphone_switch),
            (Ctl::Slider, [l.slider[0] - 8.0 * self.scale, l.slider[1] - 6.0 * self.scale, l.slider[2] + 8.0 * self.scale, l.slider[3] + 6.0 * self.scale]),
            (Ctl::MacSwitch, l.mac_switch),
            (Ctl::MacButton, l.mac_button),
            (Ctl::AutostartSwitch, l.autostart_switch),
            (Ctl::ZoomSwitch, l.zoom_switch),
        ]
        .into_iter()
        .find(|(_, b)| hit(b, x, y))
        .map(|(c, _)| c)
    }

    fn slider_value(&self, x: i32) -> u32 {
        let l = layout(self.scale);
        let t = ((x as f32 - l.slider[0]) / (l.slider[2] - l.slider[0])).clamp(0.0, 1.0);
        let (lo, hi) = (mirror::SIZE_MIN as f32, mirror::SIZE_MAX as f32);
        ((lo + t * (hi - lo)) / 5.0).round() as u32 * 5
    }

    fn mouse_move(&mut self, (x, y): (i32, i32)) -> Action {
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
        if self.dragging {
            let v = self.slider_value(x);
            if v != mirror::size_percent() {
                return Action::Size(v);
            }
            return Action::None;
        }
        let hover = self.control_at(x, y);
        if hover != self.hover {
            self.hover = hover;
            return Action::Paint;
        }
        Action::None
    }

    fn mouse_down(&mut self, (x, y): (i32, i32)) -> Action {
        let c = self.control_at(x, y);
        self.pressed = c;
        unsafe { SetCapture(self.hwnd) };
        if c == Some(Ctl::Slider) {
            self.dragging = true;
            return Action::Size(self.slider_value(x));
        }
        Action::Paint
    }

    fn mouse_up(&mut self, (x, y): (i32, i32)) -> Action {
        let pressed = self.pressed.take();
        let was_dragging = std::mem::take(&mut self.dragging);
        unsafe {
            let _ = ReleaseCapture();
        }
        if was_dragging {
            return Action::Paint;
        }
        if pressed.is_none() || pressed != self.control_at(x, y) {
            return Action::Paint;
        }
        match pressed {
            Some(Ctl::Close) => Action::Hide,
            Some(Ctl::IPhoneSwitch) => Action::ToggleIPhone,
            Some(Ctl::MacSwitch) => Action::ToggleMac,
            Some(Ctl::MacButton) => Action::MacButton,
            Some(Ctl::AutostartSwitch) => Action::Autostart,
            Some(Ctl::ZoomSwitch) => Action::Zoom,
            _ => Action::Paint,
        }
    }

    fn refresh_if_changed(&mut self) {
        let now = (mirror::status(), macview::status(), mirror::enabled(), macview::enabled());
        if now != self.painted {
            invalidate(self.hwnd);
        }
    }

    fn paint(&mut self) {
        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut rc);
            let (w, h) = (width(&rc), height(&rc));
            if w <= 0 || h <= 0 {
                return;
            }
            let s = self.scale;
            let p = palette(self.light);
            let l = layout(s);
            let iphone_status = mirror::status();
            let mac_status = macview::status();
            let iphone_on = mirror::enabled();
            let mac_on = macview::enabled();
            let mac_connected = macview::connected();
            self.painted = (iphone_status.clone(), mac_status.clone(), iphone_on, mac_on);

            // Şekiller: yazılım rasterleyicisi (kenar yumuşatmalı).
            let mut c = Canvas::new(w as usize, h as usize);
            c.fill_background(p.bg, 1.0);
            for card in [l.iphone, l.mac, l.general] {
                card_bg(&mut c, card, p.card, 14.0 * s);
            }
            if self.hover == Some(Ctl::Close) {
                let b = l.close;
                let (cx, cy) = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
                c.draw_in(b, p.field, 1.0, |x, y| fill(((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - 14.0 * s));
            }
            close_glyph(&mut c, l.close, p.secondary, s);

            // iPhone kartı.
            let ib = l.iphone;
            badge(&mut c, ib[0] + 16.0 * s, ib[1] + 16.0 * s, s, p.accent, Device::IPhone);
            separator(&mut c, ib[0] + 16.0 * s, ib[2], ib[1] + 64.0 * s, p.separator, s);
            switch(&mut c, l.iphone_switch, iphone_on, p.accent, p.switch_off, s);
            let pct = mirror::size_percent();
            let t = (pct - mirror::SIZE_MIN) as f32 / (mirror::SIZE_MAX - mirror::SIZE_MIN) as f32;
            slider(&mut c, l.slider, t, iphone_on, &p, s, self.dragging || self.hover == Some(Ctl::Slider));

            // Mac kartı.
            let mb = l.mac;
            badge(&mut c, mb[0] + 16.0 * s, mb[1] + 16.0 * s, s, hex(0x8E8E93), Device::Mac);
            separator(&mut c, mb[0] + 16.0 * s, mb[2], mb[1] + 64.0 * s, p.separator, s);
            switch(&mut c, l.mac_switch, mac_on, p.accent, p.switch_off, s);
            let (label, color) = if mac_connected { ("Bağlantıyı kes", p.danger) } else { ("Mac ekranını aç", p.accent) };
            let pressed = self.pressed == Some(Ctl::MacButton);
            let hover = self.hover == Some(Ctl::MacButton);
            let alpha = if pressed { 0.75 } else if hover { 0.9 } else { 1.0 };
            card_bg_alpha(&mut c, l.mac_button, if mac_on { color } else { p.switch_off }, 10.0 * s, alpha);

            // Genel kart.
            let gb = l.general;
            separator(&mut c, gb[0] + 16.0 * s, gb[2], gb[1] + 54.0 * s, p.separator, s);
            switch(&mut c, l.autostart_switch, util::autostart_enabled(), p.accent, p.switch_off, s);
            switch(&mut c, l.zoom_switch, util::read_setting(ZOOM_SETTING) != Some(0), p.accent, p.switch_off, s);

            blit(self.hwnd, w, h, &c, |mem| {
                // Metinler.
                let f = &self.fonts;
                text(mem, f.title, p.text, "Ayna Merkezi", [20.0 * s, 16.0 * s, 300.0 * s, 42.0 * s], DT_LEFT);
                let version = match update::build() {
                    0 => "geliştirme".to_string(),
                    n => format!("sürüm {n}"),
                };
                text(mem, f.small, p.secondary, &version, [140.0 * s, 16.0 * s, (W - 52.0) * s, 42.0 * s], DT_RIGHT);

                let tx = ib[0] + 62.0 * s;
                text(mem, f.name, p.text, "iPhone", [tx, ib[1] + 13.0 * s, ib[2] - 80.0 * s, ib[1] + 34.0 * s], DT_LEFT);
                text(mem, f.small, p.secondary, &iphone_status, [tx, ib[1] + 34.0 * s, ib[2] - 80.0 * s, ib[1] + 54.0 * s], DT_LEFT | DT_END_ELLIPSIS);
                let size_color = if iphone_on { p.text } else { p.secondary };
                text(mem, f.body, size_color, "Ekran boyutu", [ib[0] + 16.0 * s, ib[1] + 76.0 * s, ib[2] - 16.0 * s, ib[1] + 98.0 * s], DT_LEFT);
                text(mem, f.body, p.secondary, &format!("%{pct}"), [ib[0] + 16.0 * s, ib[1] + 76.0 * s, ib[2] - 16.0 * s, ib[1] + 98.0 * s], DT_RIGHT);

                text(mem, f.name, p.text, "Mac", [tx, mb[1] + 13.0 * s, mb[2] - 80.0 * s, mb[1] + 34.0 * s], DT_LEFT);
                text(mem, f.small, p.secondary, &mac_status, [tx, mb[1] + 34.0 * s, mb[2] - 80.0 * s, mb[1] + 54.0 * s], DT_LEFT | DT_END_ELLIPSIS);
                text(mem, f.body, [1.0, 1.0, 1.0], label, l.mac_button, DT_CENTER);
                text(
                    mem,
                    f.small,
                    p.secondary,
                    "Kod, Mac'teki Ayna Ajan menüsünde; Mac ekranında yazılır.",
                    [mb[0] + 16.0 * s, mb[1] + 118.0 * s, mb[2] - 16.0 * s, mb[1] + 140.0 * s],
                    DT_LEFT,
                );

                text(mem, f.body, p.text, "Windows ile başlat", [gb[0] + 16.0 * s, gb[1] + 14.0 * s, gb[2] - 80.0 * s, gb[1] + 40.0 * s], DT_LEFT);
                text(mem, f.body, p.text, "Bağlanınca Büyütme'yi yenile", [gb[0] + 16.0 * s, gb[1] + 68.0 * s, gb[2] - 80.0 * s, gb[1] + 94.0 * s], DT_LEFT);
            });
        }
    }
}

/// Tuvali pencereye basar; `texts` GDI ile üstüne metin yazar (ClearType).
pub(crate) unsafe fn blit(hwnd: HWND, w: i32, h: i32, c: &Canvas, texts: impl FnOnce(HDC)) {
    let pixels = c.to_premultiplied_bgra();
    let screen = GetDC(hwnd);
    let mem = CreateCompatibleDC(screen);
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    if let Ok(bmp) = CreateDIBSection(mem, &info, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) {
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, pixels.len());
        let old = SelectObject(mem, bmp);
        SetBkMode(mem, TRANSPARENT);
        texts(mem);
        let _ = BitBlt(screen, 0, 0, w, h, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
    }
    let _ = DeleteDC(mem);
    ReleaseDC(hwnd, screen);
    let _ = ValidateRect(hwnd, None);
}

pub(crate) unsafe fn text(dc: HDC, font: HFONT, color: [f32; 3], s: &str, b: [f32; 4], align: DRAW_TEXT_FORMAT) {
    let old = SelectObject(dc, font);
    SetTextColor(dc, colorref(color));
    let mut r = to_rect(&b);
    let mut t: Vec<u16> = s.encode_utf16().collect();
    DrawTextW(dc, &mut t, &mut r, align | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
    SelectObject(dc, old);
}

pub(crate) fn card_bg(c: &mut Canvas, b: [f32; 4], rgb: [f32; 3], r: f32) {
    card_bg_alpha(c, b, rgb, r, 1.0);
}

pub(crate) fn card_bg_alpha(c: &mut Canvas, b: [f32; 4], rgb: [f32; 3], r: f32, alpha: f32) {
    let (cx, cy, hw, hh) = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0, (b[2] - b[0]) / 2.0, (b[3] - b[1]) / 2.0);
    c.draw_in(b, rgb, alpha, |x, y| fill(round_rect_sdf(x, y, cx, cy, hw, hh, r)));
}

pub(crate) fn separator(c: &mut Canvas, x0: f32, x1: f32, y: f32, rgb: [f32; 3], s: f32) {
    let t = s.max(1.0);
    c.draw_in([x0, y - t, x1, y + t], rgb, 1.0, |_, py| if (py - y).abs() <= t / 2.0 { 1.0 } else { 0.0 });
}

/// Renkli yuvarlak kare içinde beyaz cihaz simgesi (Ayarlar uygulamasındaki gibi).
pub(crate) fn badge(c: &mut Canvas, x: f32, y: f32, s: f32, rgb: [f32; 3], device: Device) {
    let size = 34.0 * s;
    let (cx, cy) = (x + size / 2.0, y + size / 2.0);
    c.draw_in([x, y, x + size, y + size], rgb, 1.0, |px, py| {
        fill(round_rect_sdf(px, py, cx, cy, size / 2.0, size / 2.0, 8.0 * s))
    });
    match device {
        Device::IPhone => draw_phone(c, cx, cy, s * 1.05, [1.0, 1.0, 1.0]),
        Device::Mac => draw_mac(c, cx, cy + 1.0 * s, s * 1.05, [1.0, 1.0, 1.0]),
    }
}

/// iOS tarzı anahtar.
pub(crate) fn switch(c: &mut Canvas, b: [f32; 4], on: bool, accent: [f32; 3], off: [f32; 3], s: f32) {
    let (cx, cy, hw, hh) = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0, (b[2] - b[0]) / 2.0, (b[3] - b[1]) / 2.0);
    let track = if on { accent } else { off };
    c.draw_in(b, track, 1.0, |x, y| fill(round_rect_sdf(x, y, cx, cy, hw, hh, hh)));
    let r = hh - 2.0 * s;
    let kx = if on { b[2] - hh } else { b[0] + hh };
    c.draw_in(b, [1.0, 1.0, 1.0], 1.0, |x, y| fill(((x - kx).powi(2) + (y - cy).powi(2)).sqrt() - r));
}

/// Kaydırıcı; `t` 0..1 konum.
pub(crate) fn slider(c: &mut Canvas, b: [f32; 4], t: f32, enabled: bool, p: &Palette, s: f32, active: bool) {
    let cy = (b[1] + b[3]) / 2.0;
    let kx = b[0] + t.clamp(0.0, 1.0) * (b[2] - b[0]);
    let th = 2.0 * s;
    let tb = [b[0] - th, cy - th, b[2] + th, cy + th];
    let (mx, hw) = ((b[0] + b[2]) / 2.0, (b[2] - b[0]) / 2.0);
    c.draw_in(tb, p.switch_off, 1.0, |x, y| fill(round_rect_sdf(x, y, mx, cy, hw, th, th)));
    let fill_rgb = if enabled { p.accent } else { p.secondary };
    let (fx, fw) = ((b[0] + kx) / 2.0, (kx - b[0]) / 2.0);
    c.draw_in([b[0] - th, cy - th, kx + th, cy + th], fill_rgb, 1.0, |x, y| fill(round_rect_sdf(x, y, fx, cy, fw.max(th), th, th)));
    let r = if active { 11.0 * s } else { 10.0 * s };
    let kb = [kx - r - 2.0, cy - r - 2.0, kx + r + 2.0, cy + r + 2.0];
    c.draw_in(kb, [0.0, 0.0, 0.0], 0.12, |x, y| fill(((x - kx).powi(2) + (y - cy - 0.8 * s).powi(2)).sqrt() - r));
    c.draw_in(kb, [1.0, 1.0, 1.0], 1.0, |x, y| fill(((x - kx).powi(2) + (y - cy).powi(2)).sqrt() - r + 0.5));
}

pub(crate) fn close_glyph(c: &mut Canvas, b: [f32; 4], rgb: [f32; 3], s: f32) {
    let (cx, cy) = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
    let (len, t) = (5.0 * s, 0.8 * s);
    c.draw_in(b, rgb, 1.0, |x, y| {
        let (dx, dy) = (x - cx, y - cy);
        let d1 = ((dx - dy).abs() / std::f32::consts::SQRT_2 - t).max((dx + dy).abs() / std::f32::consts::SQRT_2 - len);
        let d2 = ((dx + dy).abs() / std::f32::consts::SQRT_2 - t).max((dx - dy).abs() / std::f32::consts::SQRT_2 - len);
        fill(d1.min(d2))
    });
}
