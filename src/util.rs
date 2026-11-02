use std::path::PathBuf;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, ERROR_SUCCESS, HWND, RECT};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

pub fn scale(hwnd: HWND) -> f32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }
}

pub fn width(r: &RECT) -> i32 {
    r.right - r.left
}

pub fn height(r: &RECT) -> i32 {
    r.bottom - r.top
}

pub fn contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// Sorun bildirmek için `%TEMP%\iPhoneAyna.log` dosyasına satır ekler.
pub fn log(msg: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("iPhoneAyna.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let _ = writeln!(f, "[{}] {msg}", t.as_secs());
    }
}

pub fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_default()
}

/// Exe'nin yanındaki (veya geliştirme sırasında üst klasörlerdeki) `bin\uxplay` klasörü.
pub fn find_uxplay_dir() -> Option<PathBuf> {
    let mut dir = exe_path().parent()?.to_path_buf();
    for _ in 0..5 {
        let candidate = dir.join("bin").join("uxplay");
        if candidate.join("uxplay-windows.exe").exists() {
            return Some(candidate);
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("iPhoneAyna");

pub fn read_dword(key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as _),
            Some(&mut size),
        )
    };
    (r == ERROR_SUCCESS).then_some(data)
}

/// Uygulamanın kendi ayarları (`HKCU\Software\iPhoneAyna`).
const APP_KEY: PCWSTR = w!("Software\\iPhoneAyna");

pub fn read_setting(name: PCWSTR) -> Option<u32> {
    read_dword(APP_KEY, name)
}

pub fn write_setting(name: PCWSTR, value: u32) {
    unsafe {
        let _ = RegSetKeyValueW(HKEY_CURRENT_USER, APP_KEY, name, REG_DWORD.0, Some(&value as *const u32 as _), 4);
    }
}

/// Uygulama ayarlarındaki metin değeri.
pub fn read_text_setting(name: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, APP_KEY, name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as _), Some(&mut size))
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let n = (size as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

pub fn write_text_setting(name: PCWSTR, value: &str) -> bool {
    let v = wide(value);
    unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, APP_KEY, name, REG_SZ.0, Some(v.as_ptr() as _), (v.len() * 2) as u32)
            == ERROR_SUCCESS
    }
}

pub fn delete_setting(name: PCWSTR) {
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, APP_KEY, name);
    }
}

/// Uygulamalar için açık tema seçili mi (Ayarlar > Kişiselleştirme > Renkler).
pub fn light_apps() -> bool {
    read_dword(
        w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
        w!("AppsUseLightTheme"),
    ) != Some(0)
}

pub fn light_taskbar() -> bool {
    read_dword(
        w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
        w!("SystemUsesLightTheme"),
    ) == Some(1)
}

pub fn autostart_enabled() -> bool {
    let mut size = 0u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, Some(&mut size)) };
    r == ERROR_SUCCESS
}

pub fn set_autostart(on: bool) {
    unsafe {
        if on {
            let cmd = wide(&format!("\"{}\" {}", exe_path().display(), crate::BACKGROUND_FLAG));
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(cmd.as_ptr() as _),
                (cmd.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        }
    }
}
