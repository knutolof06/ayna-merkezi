//! iPhone penceresinin kenarlarındaki fiziksel tuşlar: solda ses +/−, sağda güç (yan) tuşu.
//!
//! iPhone 16 Pro'daki yerlerinde, pencere kenarından taşan ince yuvarlatılmış çubuklar. Her biri
//! sahipli küçük bir katmanlı pencere; tıklanınca iPhone'a Bluetooth tüketici tuşu (ses, güç)
//! gönderilmesi için sahibine `WM_APP_BUTTON` iletir. Fareyle tutturmak kolay olsun diye tıklama
//! alanı görünen çubuktan geniştir.

use std::cell::{Cell, RefCell};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::hid;
use crate::taskbar::{fill, present, round_rect_sdf, Canvas};
use crate::util::{height, width};

const CLASS: PCWSTR = w!("iPhoneAyna.Tus");
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Tuşa tıklandı; WPARAM tüketici usage'ı (bkz. `hid::CONSUMER_*`).
pub const WM_APP_BUTTON: u32 = WM_APP + 7;

/// Görünen çubuğun kalınlığı ve tıklama alanının genişliği (96 DPI'da piksel).
const THICKNESS: f32 = 5.0;
/// Pencerenin çalışma alanı kenar boşluğundan (mirror::MARGIN) büyük olmamalı: taşan görünmez
/// tıklama alanı komşu monitörün ya da görev çubuğunun tıklamalarını çalardı.
const HIT: f32 = 10.0;

struct Key {
    usage: u16,
    /// Dikeyde sol kenar (ses) mı, sağ kenar (güç) mı.
    left: bool,
    /// Kenar boyunca başlangıç ve uzunluk, pencere yüksekliğinin oranı (iPhone 16 Pro).
    start: f32,
    length: f32,
}

const KEYS: [Key; 3] = [
    Key { usage: hid::CONSUMER_VOLUME_UP, left: true, start: 0.250, length: 0.075 },
    Key { usage: hid::CONSUMER_VOLUME_DOWN, left: true, start: 0.345, length: 0.075 },
    Key { usage: hid::CONSUMER_POWER, left: false, start: 0.285, length: 0.115 },
];

/// Çubuğun tıklama penceresinin hangi kenarında durduğu (telefona bakan kenar).
#[derive(Clone, Copy, PartialEq)]
enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy)]
struct Placed {
    rect: RECT,
    side: Side,
}

thread_local! {
    static HWNDS: Cell<[isize; 3]> = const { Cell::new([0; 3]) };
    static PLACED: RefCell<Vec<Placed>> = const { RefCell::new(Vec::new()) };
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
    /// 0 normal, 1 üzerinde, 2 basılı.
    static STATES: Cell<[u8; 3]> = const { Cell::new([0; 3]) };
}

/// Tuşları `rect` (iPhone penceresi) kenarlarına yerleştirip gösterir. Yatay görüntüde sol kenar
/// üst, sağ kenar alt kenar olur.
pub fn show(owner: HWND, rect: RECT, scale: f32, landscape: bool) {
    unsafe {
        let hwnds = ensure(owner);
        SCALE.with(|s| s.set(scale));
        let (t_hit, w, h) = ((HIT * scale) as i32, width(&rect), height(&rect));
        let placed: Vec<Placed> = KEYS
            .iter()
            .map(|k| {
                if !landscape {
                    let top = rect.top + (k.start * h as f32) as i32;
                    let bottom = top + (k.length * h as f32) as i32;
                    if k.left {
                        Placed { rect: RECT { left: rect.left - t_hit, top, right: rect.left, bottom }, side: Side::Right }
                    } else {
                        Placed { rect: RECT { left: rect.right, top, right: rect.right + t_hit, bottom }, side: Side::Left }
                    }
                } else {
                    let left = rect.left + (k.start * w as f32) as i32;
                    let right = left + (k.length * w as f32) as i32;
                    if k.left {
                        Placed { rect: RECT { left, top: rect.top - t_hit, right, bottom: rect.top }, side: Side::Bottom }
                    } else {
                        Placed { rect: RECT { left, top: rect.bottom, right, bottom: rect.bottom + t_hit }, side: Side::Top }
                    }
                }
            })
            .collect();
        PLACED.with(|p| *p.borrow_mut() = placed.clone());
        for (i, (&hwnd, p)) in hwnds.iter().zip(&placed).enumerate() {
            if hwnd.0.is_null() {
                continue;
            }
            draw(hwnd, i, p);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
}

pub fn hide() {
    STATES.with(|s| s.set([0; 3]));
    for h in HWNDS.with(|c| c.get()) {
        if h != 0 {
            unsafe {
                let _ = ShowWindow(HWND(h as _), SW_HIDE);
            }
        }
    }
}

unsafe fn ensure(owner: HWND) -> [HWND; 3] {
    let mut raw = HWNDS.with(|c| c.get());
    if raw.iter().all(|&h| h != 0 && IsWindow(HWND(h as _)).as_bool()) {
        return raw.map(|h| HWND(h as _));
    }
    let Ok(module) = GetModuleHandleW(None) else { return [HWND::default(); 3] };
    let instance: HINSTANCE = module.into();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        hCursor: LoadCursorW(HINSTANCE::default(), IDC_HAND).unwrap_or_default(),
        lpszClassName: CLASS,
        ..Default::default()
    };
    RegisterClassExW(&wc);
    for (i, slot) in raw.iter_mut().enumerate() {
        if *slot != 0 && IsWindow(HWND(*slot as _)).as_bool() {
            continue;
        }
        // Sahipli: iPhone penceresiyle birlikte üstte kalır; tıklanınca odak çalmaz.
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            CLASS,
            w!(""),
            WS_POPUP,
            0,
            0,
            1,
            1,
            owner,
            HMENU::default(),
            instance,
            None,
        )
        .unwrap_or_default();
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, i as isize);
        *slot = hwnd.0 as isize;
    }
    HWNDS.with(|c| c.set(raw));
    raw.map(|h| HWND(h as _))
}

unsafe fn draw(hwnd: HWND, index: usize, p: &Placed) {
    let (w, h) = (width(&p.rect), height(&p.rect));
    if w <= 0 || h <= 0 {
        return;
    }
    let state = STATES.with(|s| s.get()[index]);
    let pixels = render(w, h, p.side, state, SCALE.with(|s| s.get()));
    present(hwnd, p.rect.left, p.rect.top, w, h, &pixels);
}

fn set_state(index: usize, state: u8) -> bool {
    STATES.with(|s| {
        let mut all = s.get();
        let changed = all[index] != state;
        all[index] = state;
        s.set(all);
        changed
    })
}

unsafe fn redraw(hwnd: HWND, index: usize) {
    if let Some(p) = PLACED.with(|p| p.borrow().get(index).copied()) {
        draw(hwnd, index, &p);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let index = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as usize;
    if index >= KEYS.len() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let current = STATES.with(|s| s.get()[index]);
            if current == 0 {
                let mut t = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut t);
                set_state(index, 1);
                redraw(hwnd, index);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if set_state(index, 0) {
                redraw(hwnd, index);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            set_state(index, 2);
            redraw(hwnd, index);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let was_pressed = STATES.with(|s| s.get()[index]) == 2;
            set_state(index, 1);
            redraw(hwnd, index);
            if was_pressed {
                if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
                    let _ = PostMessageW(owner, WM_APP_BUTTON, WPARAM(KEYS[index].usage as usize), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Telefona bakan kenarda yuvarlatılmış çubuk; geri kalan alan neredeyse saydam ama tıklanabilir
/// (alfa 0 olan pikseller tıklamayı alttaki pencereye geçirirdi).
fn render(w: i32, h: i32, side: Side, state: u8, scale: f32) -> Vec<u32> {
    let mut c = Canvas::new(w as usize, h as usize);
    c.fill_background([1.0, 1.0, 1.0], 1.0 / 255.0);
    let t = (THICKNESS * scale).max(3.0);
    let (wf, hf) = (w as f32, h as f32);
    let (cx, cy, hw, hh) = match side {
        Side::Right => (wf - t / 2.0, hf / 2.0, t / 2.0, hf / 2.0),
        Side::Left => (t / 2.0, hf / 2.0, t / 2.0, hf / 2.0),
        Side::Bottom => (wf / 2.0, hf - t / 2.0, wf / 2.0, t / 2.0),
        Side::Top => (wf / 2.0, t / 2.0, wf / 2.0, t / 2.0),
    };
    let rgb = match state {
        1 => [0.47, 0.47, 0.50],
        2 => [0.17, 0.17, 0.18],
        _ => [0.30, 0.30, 0.32],
    };
    c.draw(rgb, 1.0, |x, y| fill(round_rect_sdf(x, y, cx, cy, hw, hh, t / 2.0)));
    c.to_premultiplied_bgra()
}

#[cfg(test)]
mod tests {
    use super::{render, Side};

    #[test]
    fn bar_sits_on_phone_side_and_rest_is_clickable() {
        let (w, h) = (14, 60);
        let px = render(w, h, Side::Right, 0, 1.0);
        let alpha = |x: i32, y: i32| px[(y * w + x) as usize] >> 24;
        assert_eq!(alpha(12, 30), 255, "telefona bakan kenarda opak çubuk");
        assert_eq!(alpha(1, 30), 1, "geri kalan alan neredeyse saydam ama tıklanabilir");
    }
}
