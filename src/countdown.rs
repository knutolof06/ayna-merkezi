//! Fare kilidinden çıkış sayacı: Ctrl basılı tutulurken Mac penceresinin ortasında büyük bir sayı ve
//! kısa açıklama. Yalnız Windows'ta görünür (Mac'e hiçbir şey gitmez), tıklanamaz, odak almaz.

use std::cell::Cell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::hub::card_bg_alpha;
use crate::taskbar::{present, Canvas};
use crate::util;

const CLASS: PCWSTR = w!("iPhoneAyna.Sayac");
const WIDTH: f32 = 300.0;
const HEIGHT: f32 = 150.0;

thread_local! {
    static WINDOW: Cell<HWND> = Cell::new(HWND::default());
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

unsafe fn ensure() -> HWND {
    let existing = WINDOW.with(|w| w.get());
    if !existing.is_invalid() {
        return existing;
    }
    let Ok(module) = GetModuleHandleW(None) else { return HWND::default() };
    let instance: HINSTANCE = module.into();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(proc),
        hInstance: instance,
        lpszClassName: CLASS,
        ..Default::default()
    };
    RegisterClassExW(&wc);
    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
        CLASS,
        w!(""),
        WS_POPUP,
        0,
        0,
        1,
        1,
        HWND::default(),
        HMENU::default(),
        instance,
        None,
    )
    .unwrap_or_default();
    WINDOW.with(|w| w.set(hwnd));
    hwnd
}

/// `over` penceresinin ortasında `big` (sayı) ve `small` (açıklama) gösterir.
pub fn show(over: HWND, big: &str, small: &str) {
    unsafe {
        let hwnd = ensure();
        if hwnd.is_invalid() {
            return;
        }
        let mut r = RECT::default();
        let _ = GetWindowRect(over, &mut r);
        let s = util::scale(over);
        let (w, h) = ((WIDTH * s) as i32, (HEIGHT * s) as i32);
        let x = (r.left + r.right - w) / 2;
        let y = (r.top + r.bottom - h) / 2;
        let pixels = render(w, h, s, big, small);
        present(hwnd, x, y, w, h, &pixels);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
}

pub fn hide() {
    let hwnd = WINDOW.with(|w| w.get());
    if !hwnd.is_invalid() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// Koyu yarı saydam kart üstüne beyaz metin (önceden çarpılmış BGRA).
unsafe fn render(w: i32, h: i32, s: f32, big: &str, small: &str) -> Vec<u32> {
    let mut canvas = Canvas::new(w as usize, h as usize);
    card_bg_alpha(&mut canvas, [0.0, 0.0, w as f32, h as f32], [0.08, 0.08, 0.1], 18.0 * s, 0.82);
    let mut pixels = canvas.to_premultiplied_bgra();

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
    let screen = GetDC(HWND::default());
    let mem = CreateCompatibleDC(screen);
    let font = |px: f32, weight: i32| {
        CreateFontW(
            -(px * s) as i32,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            ANTIALIASED_QUALITY.0 as u32,
            0,
            w!("Segoe UI"),
        )
    };
    let (big_font, small_font) = (font(56.0, FW_BOLD.0 as i32), font(14.0, FW_SEMIBOLD.0 as i32));
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    if let Ok(bmp) = CreateDIBSection(mem, &info, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) {
        let old = SelectObject(mem, bmp);
        SetBkMode(mem, TRANSPARENT);
        SetTextColor(mem, COLORREF(0x00FF_FFFF));
        let old_font = SelectObject(mem, big_font);
        let mut t: Vec<u16> = big.encode_utf16().collect();
        let mut r = RECT { left: 0, top: (12.0 * s) as i32, right: w, bottom: (96.0 * s) as i32 };
        DrawTextW(mem, &mut t, &mut r, DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
        SelectObject(mem, small_font);
        let mut t: Vec<u16> = small.encode_utf16().collect();
        let mut r = RECT { left: (12.0 * s) as i32, top: (96.0 * s) as i32, right: w - (12.0 * s) as i32, bottom: h - (10.0 * s) as i32 };
        DrawTextW(mem, &mut t, &mut r, DT_CENTER | DT_WORDBREAK | DT_NOPREFIX);
        SelectObject(mem, old_font);
        // GDI alfayı korumaz: beyaz metin örtü olarak karta karıştırılır.
        let mask = std::slice::from_raw_parts(bits as *const u32, (w * h) as usize);
        for (dst, m) in pixels.iter_mut().zip(mask) {
            let cov = (m & 0xFF) as f32 / 255.0;
            if cov <= 0.0 {
                continue;
            }
            let blend = |shift: u32| {
                let d = ((*dst >> shift) & 0xFF) as f32;
                ((255.0 * cov + d * (1.0 - cov)).round() as u32).min(255) << shift
            };
            *dst = blend(0) | blend(8) | blend(16) | blend(24);
        }
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
    }
    let _ = DeleteObject(big_font);
    let _ = DeleteObject(small_font);
    let _ = DeleteDC(mem);
    ReleaseDC(HWND::default(), screen);
    pixels
}
