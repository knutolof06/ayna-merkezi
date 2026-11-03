//! Mac'ten sürüklenip Mac penceresinin dışına çıkarılan dosyanın simgesi ve adı: dosyalar Windows'a
//! inene dek imleci izler (Mac'teki sürükleme iptal edildiği için görüntü kaybolmasın). Dosyalar
//! inince Windows'un kendi sürükleme görüntüsü devralır.

use std::cell::Cell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHGetImageList, SHFILEINFOW, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHIL_JUMBO};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::hub::card_bg_alpha;
use crate::i18n::tf;
use crate::taskbar::{present, Canvas};
use crate::util;

const CLASS: PCWSTR = w!("iPhoneAyna.Surukleme");
/// Ölçek 1'de boyutlar (piksel).
const WIDTH: f32 = 220.0;
const ICON: f32 = 72.0;
const LABEL: f32 = 24.0;
const GAP: f32 = 4.0;

thread_local! {
    static GHOST: Cell<HWND> = Cell::new(HWND::default());
    /// Pencerenin piksel boyutu ve simge boyutu (imleç simgenin ortasında durur).
    static SIZE: Cell<(i32, i32, i32)> = const { Cell::new((0, 0, 0)) };
}

/// `name`: ilk öğenin adı; `count` > 1 ise etikette öğe sayısı yazar.
pub fn show(name: &str, folder: bool, count: usize) {
    unsafe {
        let hwnd = ensure();
        if hwnd.is_invalid() {
            return;
        }
        let label = if count > 1 { tf("{} öğe", "{} items", &[&count]) } else { name.to_string() };
        // Önce imlecin olduğu monitöre taşınır: ölçek o monitörün DPI'ından alınsın.
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, pt.x, pt.y, 1, 1, SWP_NOACTIVATE);
        render(hwnd, name, folder, &label);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
}

/// İmlecin yeni yerine taşır (görünürse).
pub fn follow() {
    let hwnd = GHOST.with(|g| g.get());
    unsafe {
        if hwnd.is_invalid() || !IsWindowVisible(hwnd).as_bool() {
            return;
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let (w, _, icon) = SIZE.with(|s| s.get());
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, pt.x - w / 2, pt.y - icon / 2, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

pub fn hide() {
    let hwnd = GHOST.with(|g| g.get());
    if !hwnd.is_invalid() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

unsafe fn ensure() -> HWND {
    let existing = GHOST.with(|g| g.get());
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
    GHOST.with(|g| g.set(hwnd));
    hwnd
}

/// Dosya türünün büyük (256 px'e kadar) kabuk simgesi.
unsafe fn shell_icon(name: &str, folder: bool) -> Option<HICON> {
    let mut info = SHFILEINFOW::default();
    let wide = util::wide(name);
    let attrs = if folder { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL };
    let ok = SHGetFileInfoW(
        PCWSTR(wide.as_ptr()),
        attrs,
        Some(&mut info),
        std::mem::size_of::<SHFILEINFOW>() as u32,
        SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES,
    );
    if ok == 0 {
        return None;
    }
    let list: IImageList = SHGetImageList(SHIL_JUMBO as i32).ok()?;
    list.GetIcon(info.iIcon, ILD_TRANSPARENT.0).ok()
}

unsafe fn render(hwnd: HWND, name: &str, folder: bool, label: &str) {
    let s = util::scale(hwnd);
    let (w, icon, label_h, gap) = ((WIDTH * s) as i32, (ICON * s) as i32, (LABEL * s) as i32, (GAP * s) as i32);
    let h = icon + gap + label_h;
    SIZE.with(|c| c.set((w, h, icon)));

    // Etiket genişliği metne göre (sığmazsa kesilir).
    let font = CreateFontW(
        -(13.0 * s) as i32,
        0,
        0,
        0,
        FW_SEMIBOLD.0 as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET.0 as u32,
        OUT_DEFAULT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        ANTIALIASED_QUALITY.0 as u32,
        0,
        w!("Segoe UI"),
    );
    let screen = GetDC(HWND::default());
    let mem = CreateCompatibleDC(screen);
    let old_font = SelectObject(mem, font);
    let text: Vec<u16> = label.encode_utf16().collect();
    let mut extent = SIZE::default();
    let _ = GetTextExtentPoint32W(mem, &text, &mut extent);
    let pad = (8.0 * s) as i32;
    let label_w = (extent.cx + 2 * pad).min(w);

    // Etiket zemini (yuvarlak köşeli, Windows'un seçim mavisi).
    let mut canvas = Canvas::new(w as usize, h as usize);
    let lx = ((w - label_w) / 2) as f32;
    let ly = (icon + gap) as f32;
    card_bg_alpha(&mut canvas, [lx, ly, lx + label_w as f32, ly + label_h as f32], [0.0, 0.47, 0.84], 4.0 * s, 0.9);
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
    let n = (w * h) as usize;

    // Simge: kabuk simgesi önceden çarpılmış alfa ile şeffaf zemine çizilir.
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    if let Ok(bmp) = CreateDIBSection(mem, &info, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) {
        let old = SelectObject(mem, bmp);
        if let Some(hicon) = shell_icon(name, folder) {
            let _ = DrawIconEx(mem, (w - icon) / 2, 0, hicon, icon, icon, 0, HBRUSH::default(), DI_NORMAL);
            let _ = DestroyIcon(hicon);
        }
        let drawn = std::slice::from_raw_parts(bits as *const u32, n);
        for (i, px) in drawn.iter().enumerate().take(icon.max(0) as usize * w as usize) {
            if *px != 0 {
                pixels[i] = *px;
            }
        }
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
    }

    // Metin: siyah zemine beyaz yazılıp örtü olarak kullanılır (GDI alfayı korumaz).
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    if let Ok(bmp) = CreateDIBSection(mem, &info, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) {
        let old = SelectObject(mem, bmp);
        SetBkMode(mem, TRANSPARENT);
        SetTextColor(mem, COLORREF(0x00FF_FFFF));
        let mut r = RECT { left: (lx as i32) + pad / 2, top: ly as i32, right: (lx as i32) + label_w - pad / 2, bottom: ly as i32 + label_h };
        let mut t = text.clone();
        DrawTextW(mem, &mut t, &mut r, DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS);
        let mask = std::slice::from_raw_parts(bits as *const u32, n);
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
    SelectObject(mem, old_font);
    let _ = DeleteObject(font);
    let _ = DeleteDC(mem);
    ReleaseDC(HWND::default(), screen);

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    present(hwnd, pt.x - w / 2, pt.y - icon / 2, w, h, &pixels);
}
