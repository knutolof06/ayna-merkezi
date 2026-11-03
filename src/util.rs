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

pub const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
pub const RUN_VALUE: PCWSTR = w!("iPhoneAyna");

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

/// Kullanıcı kayıt defterindeki (HKCU) bir metin değeri.
pub fn read_text(key: PCWSTR, name: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key, name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as _), Some(&mut size))
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let n = (size as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

pub fn write_text(key: PCWSTR, name: PCWSTR, value: &str) -> bool {
    let v = wide(value);
    unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, key, name, REG_SZ.0, Some(v.as_ptr() as _), (v.len() * 2) as u32)
            == ERROR_SUCCESS
    }
}

/// Uygulama ayarlarındaki metin değeri.
pub fn read_text_setting(name: PCWSTR) -> Option<String> {
    read_text(APP_KEY, name)
}

pub fn write_text_setting(name: PCWSTR, value: &str) -> bool {
    write_text(APP_KEY, name, value)
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

/// Kullanıcının Masaüstü klasörü (OneDrive'a yönlendirilmiş olabilir).
pub fn desktop_dir() -> PathBuf {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Desktop, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_Desktop, KNOWN_FOLDER_FLAG(0), None) {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const _));
            if !s.is_empty() {
                return PathBuf::from(s);
            }
        }
    }
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join("Desktop")
}

/// Taşır; başka birime taşınamıyorsa (OneDrive'a yönlendirilmiş Masaüstü gibi) kopyalayıp siler.
pub fn move_path(from: &std::path::Path, to: &std::path::Path) -> bool {
    if std::fs::rename(from, to).is_ok() {
        return true;
    }
    fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
        if from.is_dir() {
            std::fs::create_dir_all(to)?;
            for e in std::fs::read_dir(from)? {
                let e = e?;
                copy_tree(&e.path(), &to.join(e.file_name()))?;
            }
            Ok(())
        } else {
            std::fs::copy(from, to).map(|_| ())
        }
    }
    if copy_tree(from, to).is_err() {
        return false;
    }
    let _ = if from.is_dir() { std::fs::remove_dir_all(from) } else { std::fs::remove_file(from) };
    true
}

/// `dir` içinde `name` adı boştaysa onu, doluysa "ad (2).uzantı" biçiminde ilk boş adı verir.
pub fn unique_in(dir: &std::path::Path, name: &std::ffi::OsStr) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = std::path::Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..10_000).map(|i| dir.join(format!("{stem} ({i}){ext}"))).find(|c| !c.exists()).unwrap_or(first)
}

/// Başka iş parçacığının (ya da sürecin) penceresi bizim pencerenin çocuğu olunca Windows iki iş
/// parçacığının girdi kuyruğunu birleştirir: görüntüyü çizen taraf her karede beklerken bizim fare ve
/// klavye işlememiz de bekler (kontrolde gecikme ve takılma). Kuyruklar yeniden ayrılır.
/// `HKCU\Software\iPhoneAyna\GirdiAyir` = 0 ile kapatılır (karşılaştırma için).
pub fn detach_input(window: HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    if read_setting(w!("GirdiAyir")) == Some(0) {
        return;
    }
    unsafe {
        let theirs = GetWindowThreadProcessId(window, None);
        let ours = GetCurrentThreadId();
        if theirs != 0 && theirs != ours {
            let _ = AttachThreadInput(ours, theirs, false);
        }
    }
}

thread_local! {
    /// Pencere başına: ölçüm başlangıcı, ileti sayısı, toplam ve en büyük gecikme (ms).
    static INPUT_LAG: std::cell::RefCell<Vec<(&'static str, std::time::Instant, u32, u64, u32)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Şu an işlenen fare iletisinin ne kadar geç işlendiğini ölçer (iletinin oluştuğu an ile şimdi);
/// 10 sn'de bir günlüğe yazar. Arayüz iş parçacığı takılırsa burada görünür.
pub fn note_input_lag(label: &'static str) {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::WindowsAndMessaging::GetMessageTime;
    let lag = unsafe { GetTickCount().wrapping_sub(GetMessageTime() as u32) };
    if lag > 60_000 {
        return; // ileti zamanı yok (gönderilen ileti)
    }
    INPUT_LAG.with(|c| {
        let mut v = c.borrow_mut();
        let i = match v.iter().position(|e| e.0 == label) {
            Some(i) => i,
            None => {
                v.push((label, std::time::Instant::now(), 0, 0, 0));
                v.len() - 1
            }
        };
        let e = &mut v[i];
        e.2 += 1;
        e.3 += lag as u64;
        e.4 = e.4.max(lag);
        if e.1.elapsed().as_secs() >= 10 {
            log(&format!("Girdi gecikmesi ({label}): ort {} ms, en çok {} ms, {} ileti", e.3 / e.2 as u64, e.4, e.2));
            *e = (label, std::time::Instant::now(), 0, 0, 0);
        }
    });
}
