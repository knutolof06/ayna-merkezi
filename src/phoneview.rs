//! Mac üzerinden gösterilen iPhone'un penceresi. Görüntü Mac'in görünmez ekranındaki iPhone
//! Mirroring'den gelir; Mac penceresi bu sırada Mac'i göstermeyi sürdürür. Fare hangi pencerenin
//! üstündeyse, klavye hangi pencere etkinse Mac'e o bildirilir (`Mac::set_target`).
//! Pencereyi kapatmak kipi de kapatır.

use std::cell::RefCell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::gst::{self, Player};
use crate::i18n::t;
use crate::{mac, macview, taskbar, util};

const CLASS: PCWSTR = w!("iPhoneAyna.MacIphone");
/// Görüntü boyutu gelene dek kullanılan oran (iPhone dikey).
const DEFAULT_ASPECT: f32 = 9.0 / 19.5;
/// Açılış yüksekliği: çalışma alanının bu kadarı.
const START_HEIGHT: f32 = 0.75;
const STYLE: WINDOW_STYLE = WINDOW_STYLE(
    WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_THICKFRAME.0 | WS_CLIPCHILDREN.0,
);

struct Phone {
    hwnd: HWND,
    video: HWND,
    /// Mac'in bildirdiği görüntü boyutu; pencere buna göre ölçülendi.
    screen: Option<(u16, u16)>,
    /// Görüntünün penceredeki yeri.
    rect: RECT,
    /// Basılı fare düğmeleri (bit 0 sol, 1 sağ, 2 orta) ve tuşlar (Mac kodu): odak gidince bırakılır.
    buttons: u8,
    keys: Vec<u16>,
    /// AltGr'nin önündeki sahte sol Ctrl yutuldu; bırakılışı da yutulacak.
    fake_ctrl: bool,
    last_pos: (u16, u16),
}

thread_local! {
    static STATE: RefCell<Option<Phone>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Phone) -> R) -> Option<R> {
    STATE.with(|c| c.try_borrow_mut().ok().and_then(|mut o| o.as_mut().map(f)))
}

/// Görüntü oranını koruyarak pencereye ortalar.
fn video_rect(w: i32, h: i32, screen: Option<(u16, u16)>) -> RECT {
    let Some((sw, sh)) = screen.filter(|s| s.0 > 0 && s.1 > 0) else {
        return RECT { left: 0, top: 0, right: w, bottom: h };
    };
    let aspect = sw as f32 / sh as f32;
    let (vw, vh) = if w as f32 / (h.max(1) as f32) > aspect {
        ((h as f32 * aspect).round() as i32, h)
    } else {
        (w, (w as f32 / aspect).round() as i32)
    };
    let (x, y) = ((w - vw) / 2, (h - vh) / 2);
    RECT { left: x, top: y, right: x + vw, bottom: y + vh }
}

/// Pencere noktası → görüntüde 0..65535 (görüntünün dışı kenara çekilir).
fn normalized(r: &RECT, x: i32, y: i32) -> (u16, u16) {
    let axis = |v: i32, a: i32, b: i32| {
        let span = (b - a - 1).max(1) as f32;
        (((v - a) as f32 / span).clamp(0.0, 1.0) * 65535.0).round() as u16
    };
    (axis(x, r.left, r.right), axis(y, r.top, r.bottom))
}

fn create() -> Option<HWND> {
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
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            CLASS,
            w!("iPhone"),
            STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            360,
            760,
            HWND::default(),
            HMENU::default(),
            instance,
            None,
        )
        .ok()?;
        let dark = BOOL::from(true);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        // Görüntü bu çocuk pencereye çizilir. Devre dışı: fare iletileri ana pencereye gelir.
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
            return None;
        };
        STATE.with(|c| {
            *c.borrow_mut() = Some(Phone {
                hwnd,
                video,
                screen: None,
                rect: RECT::default(),
                buttons: 0,
                keys: Vec::new(),
                fake_ctrl: false,
                last_pos: (0, 0),
            });
        });
        Some(hwnd)
    }
}

/// Pencereyi fare hangi monitördeyse ona, görüntü oranında ortalar.
unsafe fn place(hwnd: HWND, aspect: f32) {
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !GetMonitorInfoW(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() {
        return;
    }
    let work = info.rcWork;
    let h = (util::height(&work) as f32 * START_HEIGHT) as i32;
    let outer = outer_size((h as f32 * aspect).round() as i32, h);
    let x = work.left + (util::width(&work) - outer.0) / 2;
    let y = work.top + (util::height(&work) - outer.1) / 2;
    let _ = SetWindowPos(hwnd, HWND::default(), x, y, outer.0, outer.1, SWP_NOZORDER | SWP_NOACTIVATE);
}

/// İstemci alanı `w`×`h` olan pencerenin dış boyutu.
unsafe fn outer_size(w: i32, h: i32) -> (i32, i32) {
    let mut r = RECT { left: 0, top: 0, right: w, bottom: h };
    let _ = AdjustWindowRectEx(&mut r, STYLE, false, WINDOW_EX_STYLE::default());
    (util::width(&r), util::height(&r))
}

/// Kip açıldı: pencere gösterilir.
pub fn show() {
    let hwnd = with(|p| p.hwnd).or_else(create);
    let Some(hwnd) = hwnd else { return };
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            let screen = with(|p| p.screen).flatten();
            place(hwnd, screen.map_or(DEFAULT_ASPECT, |s| s.0 as f32 / s.1.max(1) as f32));
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    sync();
}

/// Pencereyi kipin durumuna getirir: kip kapandıysa kapatır; görüntü boyutu geldiyse ölçülendirir
/// ve oynatıcıyı kurar. Mac penceresinin zamanlayıcısından ve Mac'ten gelen iletilerde çağrılır.
pub fn sync() {
    let Some((hwnd, video, sized)) = with(|p| (p.hwnd, p.video, p.screen)) else { return };
    // Mac penceresi o an meşgulse bu tur atlanır (kip kapalı sayılmaz; zamanlayıcı yeniden çağırır).
    let Some((on, connected, screen, has_player)) =
        macview::with_mac(|m| (m.phone(), m.connected(), m.phone_screen(), m.has_phone_player()))
    else {
        return;
    };
    unsafe {
        if !on {
            if IsWindowVisible(hwnd).as_bool() {
                release_input();
                let _ = ShowWindow(hwnd, SW_HIDE);
                let _ = ShowWindow(video, SW_HIDE);
            }
            if has_player {
                macview::with_mac(|m| m.set_phone_player(None));
            }
            with(|p| p.screen = None);
            return;
        }
        // Simge durumundayken istemci alanı 0×0'dır: boyut pencere geri gelince uygulanır.
        if screen.is_some() && screen != sized && !IsIconic(hwnd).as_bool() {
            with(|p| p.screen = screen);
            if let (Some((sw, sh)), None) = (screen, sized) {
                // İlk boyut: pencere gerçek oranıyla yeniden ortalanır.
                place(hwnd, sw as f32 / sh.max(1) as f32);
            } else if let Some((sw, sh)) = screen {
                // Yükseklik korunur, genişlik görüntünün oranına gelir.
                let mut c = RECT::default();
                let _ = GetClientRect(hwnd, &mut c);
                let h = util::height(&c).max(1);
                let w = (h as f32 * sw as f32 / sh.max(1) as f32).round() as i32;
                let outer = outer_size(w, h);
                let _ = SetWindowPos(hwnd, HWND::default(), 0, 0, outer.0, outer.1, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
            layout(hwnd);
        }
        if !connected {
            // Koptu: donmuş son kare yerine bekleme yazısı; yeniden bağlanınca oynatıcı yeniden kurulur.
            if has_player {
                macview::with_mac(|m| m.set_phone_player(None));
            }
            if IsWindowVisible(video).as_bool() {
                let _ = ShowWindow(video, SW_HIDE);
                let _ = InvalidateRect(hwnd, None, true);
            }
        } else if screen.is_some() {
            let _ = ShowWindow(video, SW_SHOWNA);
            if !has_player && gst::available() {
                let player = Player::new(video);
                macview::with_mac(|m| m.set_phone_player(player));
            }
            // GStreamer'ın çizim penceresi de devre dışı kalır: fare ve klavye bu pencereye gelsin.
            macview::disable_children(video);
        }
    }
}

unsafe fn layout(hwnd: HWND) {
    let mut c = RECT::default();
    let _ = GetClientRect(hwnd, &mut c);
    let Some((video, rect)) = with(|p| {
        p.rect = video_rect(util::width(&c), util::height(&c), p.screen);
        (p.video, p.rect)
    }) else {
        return;
    };
    let _ = MoveWindow(video, rect.left, rect.top, util::width(&rect), util::height(&rect), true);
}

/// Odak giderken ya da kip kapanırken basılı tuş ve düğmeler Mac'te basılı kalmasın.
pub fn release_input() {
    let Some((keys, buttons, pos)) = with(|p| (std::mem::take(&mut p.keys), std::mem::take(&mut p.buttons), p.last_pos)) else { return };
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

fn point(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as u16 as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32)
}

fn mouse_move(pt: (i32, i32)) {
    let Some((x, y)) = with(|p| {
        p.last_pos = normalized(&p.rect, pt.0, pt.1);
        p.last_pos
    }) else {
        return;
    };
    macview::with_mac(|m| {
        if m.connected() {
            m.set_target(Some(true), None);
            m.send(mac::mouse_move(x, y));
        }
    });
}

unsafe fn button(hwnd: HWND, button: u8, down: bool, pt: (i32, i32)) {
    let Some((x, y, held)) = with(|p| {
        p.last_pos = normalized(&p.rect, pt.0, pt.1);
        if down {
            p.buttons |= 1 << button;
        } else {
            p.buttons &= !(1 << button);
        }
        (p.last_pos.0, p.last_pos.1, p.buttons)
    }) else {
        return;
    };
    if down {
        SetCapture(hwnd);
    } else if held == 0 {
        let _ = ReleaseCapture();
    }
    macview::with_mac(|m| {
        if m.connected() {
            m.set_target(Some(true), None);
            m.send(mac::mouse_button(button, down, x, y));
        }
    });
}

fn key(scan: u16, down: bool) {
    let code = with(|p| {
        if scan == 0x1D && p.fake_ctrl {
            if !down {
                p.fake_ctrl = false;
            }
            return None;
        }
        let code = mac::mac_keycode(scan)?;
        if down {
            if !p.keys.contains(&code) {
                p.keys.push(code);
            }
        } else {
            p.keys.retain(|&k| k != code);
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
    let waiting = with(|p| !IsWindowVisible(p.video).as_bool()).unwrap_or(false);
    if waiting {
        let mut c = RECT::default();
        let _ = GetClientRect(hwnd, &mut c);
        let mut label: Vec<u16> = t("iPhone açılıyor…", "Opening iPhone…").encode_utf16().collect();
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
            with(|p| p.fake_ctrl = true);
            return LRESULT(0);
        }
    }
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_SIZE => {
            layout(hwnd);
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let info = &mut *(lp.0 as *mut MINMAXINFO);
            info.ptMinTrackSize = POINT { x: 200, y: 300 };
            LRESULT(0)
        }
        WM_CLOSE => {
            // Pencereyi kapatmak iPhone kipini kapatır.
            macview::stop_phone();
            LRESULT(0)
        }
        WM_SETFOCUS => {
            macview::with_mac(|m| m.set_target(None, Some(true)));
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            release_input();
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            if with(|p| p.buttons != 0).unwrap_or(false) {
                release_input();
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            mouse_move(point(lp));
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            let which = match msg {
                WM_LBUTTONDOWN | WM_LBUTTONUP => 0,
                WM_RBUTTONDOWN | WM_RBUTTONUP => 1,
                _ => 2,
            };
            button(hwnd, which, matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN), point(lp));
            LRESULT(0)
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            // Bir tekerlek çentiği (120) ≈ 3 satır.
            let px = (delta * 36 / 120).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            macview::with_mac(|m| {
                if m.connected() {
                    m.set_target(Some(true), None);
                    m.send(if msg == WM_MOUSEWHEEL { mac::scroll(0, px) } else { mac::scroll(px, 0) });
                }
            });
            LRESULT(0)
        }
        // Alt+F4 bu pencereyi kapatır (iPhone Mirroring'e gitmez).
        WM_SYSKEYDOWN if wp.0 == 0x73 => DefWindowProcW(hwnd, msg, wp, lp),
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            key(((lp.0 >> 16) & 0xFF) as u16 | if (lp.0 >> 24) & 1 == 1 { 0xE000 } else { 0 }, down);
            LRESULT(0)
        }
        // Tuşlar Mac'e gidince bip sesi ve Alt menüsü olmasın.
        WM_CHAR | WM_SYSCHAR | WM_DEADCHAR | WM_SYSDEADCHAR => LRESULT(0),
        WM_SYSCOMMAND if (wp.0 & 0xFFF0) == SC_KEYMENU as usize => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_keeps_aspect_and_centres() {
        // Geniş pencerede yanlardan boşluk kalır.
        let r = video_rect(1000, 500, Some((500, 1000)));
        assert_eq!((r.left, r.top, r.right, r.bottom), (375, 0, 625, 500));
        // Dar pencerede üstten ve alttan.
        let r = video_rect(250, 1000, Some((500, 500)));
        assert_eq!((r.left, r.top, r.right, r.bottom), (0, 375, 250, 625));
        // Boyut bilinmiyorsa tüm pencere.
        let r = video_rect(300, 400, None);
        assert_eq!((r.right, r.bottom), (300, 400));
    }

    #[test]
    fn points_map_to_the_video_and_clamp() {
        let r = RECT { left: 100, top: 0, right: 301, bottom: 201 };
        assert_eq!(normalized(&r, 100, 0), (0, 0));
        assert_eq!(normalized(&r, 300, 200), (65535, 65535));
        assert_eq!(normalized(&r, 200, 100), (32768, 32768));
        // Görüntünün dışı kenara çekilir.
        assert_eq!(normalized(&r, 0, 500), (0, 65535));
    }
}
