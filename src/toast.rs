//! Kısa süre görünen bildirim balonu ("3 dosya gönderildi" gibi).
//!
//! Yansıtma penceresi açıksa onun üst kenarında, kapalıysa görev çubuğu ikonunun üstünde
//! belirir; odak çalmaz, birkaç saniye sonra ya da tıklanınca kaybolur.

use std::cell::RefCell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::taskbar;
use crate::util::{self, height, rgb, width, wide};

const CLASS: PCWSTR = w!("iPhoneAyna.Bildirim");
const TIMER_HIDE: usize = 1;
const VISIBLE_MS: u32 = 4000;

struct Toast {
    hwnd: HWND,
    text: String,
    font: HFONT,
}

thread_local! {
    static TOAST: RefCell<Option<Toast>> = const { RefCell::new(None) };
}

pub fn show(owner: HWND, text: &str) {
    unsafe {
        let hwnd = ensure_window(owner);
        if hwnd.0.is_null() {
            return;
        }
        let scale = util::scale(owner);
        let font = font(scale);
        let old = TOAST.with(|t| {
            let mut t = t.borrow_mut();
            let toast = t.as_mut()?;
            toast.text = text.to_string();
            Some(std::mem::replace(&mut toast.font, font))
        });
        if let Some(old) = old {
            let _ = DeleteObject(old);
        }

        // Metni en fazla 420 px genişlikte satırlara bölerek ölç.
        let max_w = (420.0 * scale) as i32;
        let pad_x = (18.0 * scale) as i32;
        let pad_y = (12.0 * scale) as i32;
        let dc = GetDC(hwnd);
        let prev = SelectObject(dc, font);
        let mut buf = text_buf(text);
        let mut r = RECT { left: 0, top: 0, right: max_w, bottom: 0 };
        DrawTextW(dc, &mut buf, &mut r, DT_CALCRECT | DT_WORDBREAK | DT_CENTER);
        SelectObject(dc, prev);
        ReleaseDC(hwnd, dc);
        let w = width(&r) + 2 * pad_x;
        let h = height(&r) + 2 * pad_y;

        let (x, y) = position(owner, w, h, scale);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        let radius = (16.0 * scale) as i32;
        SetWindowRgn(hwnd, CreateRoundRectRgn(0, 0, w + 1, h + 1, radius, radius), true);
        let _ = InvalidateRect(hwnd, None, false);
        SetTimer(hwnd, TIMER_HIDE, VISIBLE_MS, None);
    }
}

unsafe fn position(owner: HWND, w: i32, h: i32, scale: f32) -> (i32, i32) {
    let gap = (14.0 * scale) as i32;
    if IsWindowVisible(owner).as_bool() && !IsIconic(owner).as_bool() {
        let mut r = RECT::default();
        let _ = GetWindowRect(owner, &mut r);
        return (r.left + (width(&r) - w) / 2, r.top + gap * 3);
    }
    if let Some(icon) = taskbar::icon_rect() {
        return ((icon.left + icon.right) / 2 - w / 2, icon.top - h - gap);
    }
    let mut work = RECT::default();
    let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut RECT as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    (work.left + (width(&work) - w) / 2, work.bottom - h - gap)
}

unsafe fn ensure_window(owner: HWND) -> HWND {
    if let Some(h) = TOAST.with(|t| t.borrow().as_ref().map(|t| t.hwnd)) {
        if IsWindow(h).as_bool() {
            return h;
        }
    }
    let Ok(module) = GetModuleHandleW(None) else { return HWND::default() };
    let instance: HINSTANCE = module.into();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
        lpszClassName: CLASS,
        ..Default::default()
    };
    RegisterClassExW(&wc);
    let Ok(hwnd) = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
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
    ) else {
        return HWND::default();
    };
    TOAST.with(|t| *t.borrow_mut() = Some(Toast { hwnd, text: String::new(), font: HFONT::default() }));
    hwnd
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER | WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
            let _ = KillTimer(hwnd, TIMER_HIDE);
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut c = RECT::default();
            let _ = GetClientRect(hwnd, &mut c);
            let brush = CreateSolidBrush(rgb(0x2C, 0x2C, 0x2E));
            FillRect(dc, &c, brush);
            let _ = DeleteObject(brush);
            if let Some((text, font)) = TOAST.with(|t| t.borrow().as_ref().map(|t| (t.text.clone(), t.font))) {
                let scale = util::scale(hwnd);
                let mut r = RECT {
                    left: (18.0 * scale) as i32,
                    top: (12.0 * scale) as i32,
                    right: c.right - (18.0 * scale) as i32,
                    bottom: c.bottom - (12.0 * scale) as i32,
                };
                let prev = SelectObject(dc, font);
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, rgb(0xF2, 0xF2, 0xF7));
                let mut buf = text_buf(&text);
                DrawTextW(dc, &mut buf, &mut r, DT_WORDBREAK | DT_CENTER);
                SelectObject(dc, prev);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn text_buf(text: &str) -> Vec<u16> {
    let mut v = wide(text);
    v.pop();
    v
}

fn font(scale: f32) -> HFONT {
    let mut lf = LOGFONTW {
        lfHeight: -(14.0 * scale).round() as i32,
        lfWeight: 500,
        lfQuality: CLEARTYPE_QUALITY,
        ..Default::default()
    };
    for (dst, src) in lf.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
        *dst = src;
    }
    unsafe { CreateFontIndirectW(&lf) }
}
