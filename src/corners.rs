//! iPhone penceresinin yuvarlak köşelerini kenar yumuşatmalı gösteren katman.
//!
//! Pencere bölgesi (SetWindowRgn) piksel piksel keser, köşeler tırtıklı görünür. Her köşeye
//! tıklamaları geçiren, sahipli küçük bir katmanlı pencere konur: köşe yayı boyunca dar bir siyah
//! şerit çizer (bölgenin tırtıklı kenarını örter) ve yayın dışına doğru alfa ile yumuşayarak
//! masaüstüne karışır. Pencereler yalnız köşe karelerini kaplar: videonun tamamını örten saydam
//! bir katman, DWM'in videoyu doğrudan ekrana basmasını (DirectFlip) engelleyip gecikme eklerdi.

use std::cell::Cell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::taskbar::{fill, present};
use crate::util::{height, width};

const CLASS: PCWSTR = w!("iPhoneAyna.Kose");

thread_local! {
    /// Sol üst, sağ üst, sol alt, sağ alt.
    static HWNDS: Cell<[isize; 4]> = const { Cell::new([0; 4]) };
    /// Son çizilen (yarıçap, ölçek×100); aynıysa köşeler yeniden çizilmez, yalnız taşınır.
    static DRAWN: Cell<(i32, i32)> = const { Cell::new((0, 0)) };
}

/// Köşeleri `owner` penceresinin `rect` alanına, `radius` köşe yarıçapıyla yerleştirip gösterir.
pub fn show(owner: HWND, rect: RECT, radius: i32, scale: f32) {
    let (w, h) = (width(&rect), height(&rect));
    if w <= 0 || h <= 0 || radius <= 0 {
        return;
    }
    unsafe {
        let hwnds = ensure(owner);
        let size = radius + 1;
        let origins = [
            (rect.left, rect.top),
            (rect.right - size, rect.top),
            (rect.left, rect.bottom - size),
            (rect.right - size, rect.bottom - size),
        ];
        let key = (radius, (scale * 100.0) as i32);
        let redraw = DRAWN.with(|d| d.get()) != key;
        for (i, (&hwnd, &(x, y))) in hwnds.iter().zip(&origins).enumerate() {
            if hwnd.0.is_null() {
                continue;
            }
            if redraw {
                present(hwnd, x, y, size, size, &render_corner(i, size, radius as f32, scale));
            } else {
                let _ = SetWindowPos(hwnd, HWND::default(), x, y, size, size, SWP_NOACTIVATE | SWP_NOZORDER);
            }
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        if redraw {
            DRAWN.with(|d| d.set(key));
        }
    }
}

pub fn hide() {
    for h in HWNDS.with(|c| c.get()) {
        if h != 0 {
            unsafe {
                let _ = ShowWindow(HWND(h as _), SW_HIDE);
            }
        }
    }
}

unsafe fn ensure(owner: HWND) -> [HWND; 4] {
    let mut raw = HWNDS.with(|c| c.get());
    if raw.iter().all(|&h| h != 0 && IsWindow(HWND(h as _)).as_bool()) {
        return raw.map(|h| HWND(h as _));
    }
    let Ok(module) = GetModuleHandleW(None) else { return [HWND::default(); 4] };
    let instance: HINSTANCE = module.into();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        lpszClassName: CLASS,
        ..Default::default()
    };
    RegisterClassExW(&wc);
    for slot in raw.iter_mut() {
        if *slot != 0 && IsWindow(HWND(*slot as _)).as_bool() {
            continue;
        }
        // Sahipli: her zaman iPhone penceresinin hemen üstünde kalır. WS_EX_TRANSPARENT: tıklama,
        // sürükle-bırak ve dokunma alttaki pencereye geçer.
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
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
        *slot = hwnd.0 as isize;
    }
    HWNDS.with(|c| c.set(raw));
    DRAWN.with(|d| d.set((0, 0)));
    raw.map(|h| HWND(h as _))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// `corner` köşesinin `size`×`size` karesi (0 sol üst, 1 sağ üst, 2 sol alt, 3 sağ alt), önceden
/// çarpılmış BGRA. Yay içinde `band` genişliğinde opak siyah, dışında kapsama oranında yarı saydam
/// siyah; geri kalan her yer tamamen saydam.
fn render_corner(corner: usize, size: i32, radius: f32, scale: f32) -> Vec<u32> {
    let n = size as usize;
    let band_max = (2.0 * scale).max(1.5);
    // Yay merkezi, kare içindeki koordinatlarda.
    let (cx, cy) = (
        if corner % 2 == 0 { radius } else { size as f32 - radius },
        if corner < 2 { radius } else { size as f32 - radius },
    );
    let mut px = vec![0u32; n * n];
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = ((x as f32 + 0.5 - cx).abs(), (y as f32 + 0.5 - cy).abs());
            // Tırtık yayın ortasında (45°) en büyük, yayın düz kenarla birleştiği yerde (dx ya da
            // dy ≈ 0) sıfırdır; opak şerit de oraya doğru incelir, düz kenarda siyah çizgi kalmaz.
            let band = band_max * (dx.min(dy) / (radius * 0.3)).clamp(0.0, 1.0);
            // Yay merkezine uzaklık − yarıçap: içeride negatif, dışarıda pozitif.
            let d = (dx * dx + dy * dy).sqrt() - radius;
            let a = if d < -band { 0.0 } else { fill(d) };
            if a > 0.0 {
                px[y * n + x] = ((a * 255.0 + 0.5) as u32) << 24; // siyah: renk kanalları 0
            }
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::render_corner;

    #[test]
    fn corner_is_smooth_and_clear_inside() {
        let size = 31;
        let px = render_corner(0, size, 30.0, 1.0);
        let alpha = |x: usize, y: usize| px[y * size as usize + x] >> 24;
        assert_eq!(alpha(0, 0), 0, "yayın çok dışı (pencere bölgesi dışı) saydam: masaüstü görünür");
        assert_eq!(alpha(30, 30), 0, "yayın içi (ekran) saydam");
        // Yay üzerinde (45°) kısmi ya da tam örtü; yayın içine doğru saydamlaşır.
        let on_arc = (0..31).map(|i| alpha(i, i)).max().unwrap();
        assert!(on_arc > 0);
        // Düz kenarla birleşme noktasında (yay merkezinin hizası) opak şerit yok.
        assert_eq!(alpha(30, 1), 0);
    }
}
