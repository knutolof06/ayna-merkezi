//! Otomatik güncelleme: GitHub'daki son sürümü (herkese açık depo) denetler, yeni Windows exe'sini
//! indirir ve uygulama kullanılmıyorken kendini değiştirip yeniden başlar.
//!
//! Güven: her sürümde CI bir bildirge (`manifest.txt`: "AYNA-SURUM <n>" ve dosya özetleri) üretir
//! ve onu projenin imza anahtarıyla (Mac uygulamasını imzalayan, yalnız GitHub gizli
//! değişkenlerinde duran RSA anahtarı) imzalar (`manifest.sig`). Exe yalnız imzası doğrulanan,
//! sürüm numarası etiketle aynı ve bu sürümden yeni olan bildirgedeki özetle eşleşirse kurulur:
//! GitHub hesabı ele geçirilse de anahtar olmadan güncelleme üretilemez; eski sürüme dönülemez.
//!
//! Sürüm numarası, GitHub Actions'ın çalıştırma numarasıdır (derlemede `AYNA_BUILD`). Yerel
//! geliştirme derlemelerinde numara yoktur ve güncelleme kapalıdır (geliştirilen exe ezilmesin).
//!
//! Değiştirme: çalışan exe `.eski` adına taşınır (Windows çalışan dosyanın adını değiştirmeye izin
//! verir), yenisi yerine konur, yeni süreç `--guncelleme-bekle <pid>` ile başlar ve eskisinin
//! kapanmasını bekleyip açılır; `.eski` sonraki açılışta silinir.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::crypto::{self, hex, unhex};
use crate::util;

/// Bir güncelleme indirildi ve uygulanmaya hazır (iş parçacığı → ana pencere).
pub const WM_APP_UPDATE: u32 = WM_APP + 27;
pub const WAIT_FLAG: &str = "--guncelleme-bekle";
pub const UPDATED_FLAG: &str = "--guncellendi";

const API_HOST: &str = "api.github.com";
const LATEST: &str = "/repos/knutolof06/ayna-merkezi/releases/latest";
/// İndirme bağlantısı yalnız bu depodan kabul edilir.
const DOWNLOAD_PREFIX: &str = "https://github.com/knutolof06/ayna-merkezi/releases/download/";
const ASSET: &str = "iPhoneAyna.exe";
/// Sürüm imza anahtarının açık kısmı (RSA-2048, üs 65537; sertifika SHA-1 c7251ef4…5fa0).
const RELEASE_KEY_MODULUS: &str = "b65d2f0e4e0132e42c1d8d53fd2c30b288dd69b45d1b3026421fe112f6ab598b586c76dca44fe705ba997cb6005b7adc846c2d80f7e8a96c2e91ac340993052f52e60104d04aed568c2062538b4e7ef811544cb4ab30dbbce105518bbd0f21caba6f867ad54078aeb5d5742e1833ce99aae71698aab0407a9274a86b4267cfe52c293cba4d56cd4a2cbb18b3ce50e833e1537e6b1e19dbf75e7efb8f1bb69774881c522a04f7cec7aeee8e85e0728db585a4738805670e82ca5f69abead33042fa7ca8942b168dc0bdc5ea6a697b1273a20aaf82a14ec55b02d432b40e5889c1b2cf5eaa892500cc3b08e1d9cc44ca792ddf2702991e6ab53b5c0ec84372c43b";
const FIRST_CHECK: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
const MAX_SIZE: usize = 64 << 20;

/// Bu derlemenin sürüm numarası (0: yerel geliştirme derlemesi).
pub fn build() -> u32 {
    option_env!("AYNA_BUILD").and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// İndirilmiş, uygulanmayı bekleyen sürüm: numara, dosya, beklenen özet.
static READY: Mutex<Option<(u32, PathBuf, String)>> = Mutex::new(None);

/// Arka planda denetimi başlatır; yeni sürüm indirilince `notify` penceresine `WM_APP_UPDATE`.
pub fn start(notify: HWND) {
    if build() == 0 {
        return;
    }
    let notify = notify.0 as isize;
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK);
        loop {
            match check() {
                Ok(Some(v)) => {
                    util::log(&format!("Güncelleme indirildi: sürüm {v}."));
                    unsafe {
                        let _ = PostMessageW(HWND(notify as _), WM_APP_UPDATE, WPARAM(v as usize), LPARAM(0));
                    }
                    return;
                }
                Ok(None) => {}
                Err(e) => util::log(&format!("Güncelleme denetlenemedi: {e}")),
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

/// Yeni sürüm varsa indirir ve doğrular; `Some(sürüm)`.
fn check() -> Result<Option<u32>, String> {
    let json = String::from_utf8_lossy(&https_get(API_HOST, LATEST)?).to_string();
    let Some(version) = json_str(&json, "tag_name").and_then(|t| t.strip_prefix('v').and_then(|n| n.parse::<u32>().ok())) else {
        return Err("sürüm bilgisi okunamadı".into());
    };
    if version <= build() {
        return Ok(None);
    }
    // İmzalı bildirge: sürüm numarası ve özetler yalnız buradan alınır.
    let manifest = download(&json, "manifest.txt")?;
    let sig = unhex(String::from_utf8_lossy(&download(&json, "manifest.sig")?).trim()).ok_or("imza okunamadı")?;
    let modulus = unhex(RELEASE_KEY_MODULUS).ok_or("anahtar okunamadı")?;
    if !crypto::rsa_sha256_verify(&modulus, &[1, 0, 1], &manifest, &sig) {
        return Err("sürüm imzası doğrulanamadı".into());
    }
    let manifest = String::from_utf8_lossy(&manifest).to_string();
    if manifest.lines().next() != Some(format!("AYNA-SURUM {version}").as_str()) {
        return Err("bildirgedeki sürüm etiketle aynı değil".into());
    }
    let expected = expected_sha256(&manifest, ASSET).ok_or("bildirgede Windows dosyası yok")?;
    let bytes = download(&json, ASSET)?;
    let digest = crypto::sha256(&bytes).ok_or("özet hesaplanamadı")?;
    if hex(&digest) != expected {
        return Err("indirilen dosyanın özeti tutmuyor".into());
    }
    if bytes.len() < 100_000 || !bytes.starts_with(b"MZ") {
        return Err("indirilen dosya geçerli bir program değil".into());
    }
    // Uzantısız: çift tıklanıp ya da görev çubuğuna sabitlenip uygulama bu dosyadan açılmasın.
    let exe = util::exe_path();
    let new = exe.with_extension("guncelleme");
    std::fs::write(&new, &bytes).map_err(|e| format!("yazılamadı: {e}"))?;
    *READY.lock().unwrap() = Some((version, new, expected));
    Ok(Some(version))
}

/// Sürümdeki bir dosyayı (yalnız bu deponun indirme adresinden) indirir.
fn download(json: &str, name: &str) -> Result<Vec<u8>, String> {
    let url = asset_url(json, name).ok_or_else(|| format!("sürümde {name} yok"))?;
    let rest = url.strip_prefix("https://github.com").ok_or("beklenmeyen indirme adresi")?;
    https_get("github.com", rest)
}

/// İndirilmiş güncellemeyi uygular: dosyaları değiştirir ve yeni sürümü başlatır. `true` dönerse
/// çağıran uygulamayı kapatmalıdır.
pub fn apply() -> bool {
    let Some((version, new, expected)) = READY.lock().unwrap().take() else { return false };
    // İndirmeyle kurulum arasında dosya değiştirilmiş olmasın: hemen önce yeniden doğrulanır.
    let intact = std::fs::read(&new).ok().and_then(|b| crypto::sha256(&b)).is_some_and(|d| hex(&d) == expected);
    if !intact {
        util::log("Güncelleme dosyası değişmiş; kurulmadı.");
        let _ = std::fs::remove_file(&new);
        return false;
    }
    let exe = util::exe_path();
    let old = exe.with_extension("eski.exe");
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(&exe, &old) {
        util::log(&format!("Güncelleme uygulanamadı (taşıma): {e}"));
        return false;
    }
    if let Err(e) = std::fs::rename(&new, &exe) {
        util::log(&format!("Güncelleme uygulanamadı (yerleştirme): {e}"));
        let _ = std::fs::rename(&old, &exe);
        return false;
    }
    let started = std::process::Command::new(&exe)
        .args([crate::BACKGROUND_FLAG, UPDATED_FLAG, WAIT_FLAG, &std::process::id().to_string()])
        .spawn();
    match started {
        Ok(_) => {
            util::log(&format!("Sürüm {version} başlatıldı; eski sürüm kapanıyor."));
            true
        }
        Err(e) => {
            util::log(&format!("Yeni sürüm başlatılamadı: {e}; eski sürüme dönülüyor."));
            let _ = std::fs::rename(&exe, &new);
            let _ = std::fs::rename(&old, &exe);
            false
        }
    }
}

/// Açılışta: güncelleme yaptıran eski süreç kapanana dek bekler, eski exe'yi siler.
pub fn on_startup() {
    // Eski sürümler güncellemeyi ".yeni.exe" adıyla indiriyordu; çift tıklanır ya da görev çubuğuna
    // sabitlenirse uygulama ondan açılıyordu. Öyle açıldıysa asıl program başlatılır; asıl program da
    // açılışta artık gereksiz bu dosyayı siler.
    let exe = util::exe_path();
    if let Some(stem) = exe.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".yeni.exe")) {
        let main = exe.with_file_name(format!("{stem}.exe"));
        if main.exists() && std::process::Command::new(&main).args(std::env::args().skip(1)).spawn().is_ok() {
            util::log("Uygulama güncelleme dosyasından açılmıştı; asıl program başlatıldı.");
            std::process::exit(0);
        }
    } else {
        let _ = std::fs::remove_file(exe.with_extension("yeni.exe"));
    }
    let args: Vec<String> = std::env::args().collect();
    if let Some(pid) = args.iter().position(|a| a == WAIT_FLAG).and_then(|i| args.get(i + 1)).and_then(|p| p.parse::<u32>().ok()) {
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                WaitForSingleObject(h, 20_000);
                let _ = CloseHandle(h);
            }
        }
    }
    // Eski sürüm, yeni sürüm bir dakika sorunsuz çalıştıktan sonra silinir (o zamana dek elle
    // geri dönülebilir).
    let old = util::exe_path().with_extension("eski.exe");
    if old.exists() {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(60));
            let _ = std::fs::remove_file(&old);
        });
    }
}

/// Bu açılış bir güncellemeden mi geliyor (bildirim için)?
pub fn just_updated() -> bool {
    std::env::args().any(|a| a == UPDATED_FLAG)
}

// ---------------------------------------------------------------------------------------------
// JSON'dan gereken alanlar (tam ayrıştırıcı gerekmez: GitHub'ın biçimi sabit).

fn json_str(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":");
    let i = json.find(&pat)? + pat.len();
    let rest = json[i..].trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// `browser_download_url` alanlarından, bu depoya ait ve `name` ile biteni.
fn asset_url(json: &str, name: &str) -> Option<String> {
    let pat = "\"browser_download_url\":";
    let mut from = 0;
    while let Some(i) = json[from..].find(pat) {
        let start = from + i + pat.len();
        let rest = json[start..].trim_start().strip_prefix('"')?;
        let end = rest.find('"')?;
        let url = &rest[..end];
        if url.starts_with(DOWNLOAD_PREFIX) && url.ends_with(&format!("/{name}")) {
            return Some(url.to_string());
        }
        from = start;
    }
    None
}

/// Bildirgedeki `sha256sum` satırı: `<64 onaltılık>  <dosya>`.
fn expected_sha256(manifest: &str, name: &str) -> Option<String> {
    manifest.lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        let h = parts.next()?;
        let f = parts.next()?.trim_start_matches('*');
        (f == name && h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).then(|| h.to_ascii_lowercase())
    })
}

// ---------------------------------------------------------------------------------------------
// HTTPS (WinHTTP; yönlendirmeler — ör. indirme sunucusu — kendiliğinden izlenir)

struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn https_get(host: &str, path: &str) -> Result<Vec<u8>, String> {
    unsafe {
        let session = Handle(WinHttpOpen(w!("AynaMerkezi"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return Err("WinHTTP açılamadı".into());
        }
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 30_000, 60_000);
        let h = util::wide(host);
        let conn = Handle(WinHttpConnect(session.0, PCWSTR(h.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0));
        if conn.0.is_null() {
            return Err(format!("{host} bağlantısı kurulamadı"));
        }
        let p = util::wide(path);
        let req = Handle(WinHttpOpenRequest(
            conn.0,
            w!("GET"),
            PCWSTR(p.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if req.0.is_null() {
            return Err("istek oluşturulamadı".into());
        }
        let headers: Vec<u16> = "Accept: application/vnd.github+json\r\n".encode_utf16().collect();
        WinHttpSendRequest(req.0, Some(&headers), None, 0, 0, 0).map_err(|e| format!("istek gönderilemedi: {}", e.message()))?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(|e| format!("yanıt alınamadı: {}", e.message()))?;
        let mut status = 0u32;
        let mut len = 4u32;
        let mut index = 0u32;
        let _ = WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            &mut index,
        );
        if status != 200 {
            return Err(format!("sunucu yanıtı {status}"));
        }
        let mut out = Vec::new();
        loop {
            let mut avail = 0u32;
            WinHttpQueryDataAvailable(req.0, &mut avail).map_err(|e| e.message().to_string())?;
            if avail == 0 {
                break;
            }
            let mut buf = vec![0u8; avail as usize];
            let mut read = 0u32;
            WinHttpReadData(req.0, buf.as_mut_ptr() as *mut _, avail, &mut read).map_err(|e| e.message().to_string())?;
            out.extend_from_slice(&buf[..read as usize]);
            if out.len() > MAX_SIZE {
                return Err("yanıt çok büyük".into());
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_release_json() {
        let json = r#"{"tag_name":"v12","assets":[{"browser_download_url":"https://evil.example/iPhoneAyna.exe"},{"browser_download_url":"https://github.com/knutolof06/ayna-merkezi/releases/download/v12/iPhoneAyna.exe"}]}"#;
        let manifest = "AYNA-SURUM 12\n0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  iPhoneAyna.exe\nffff  x\n";
        assert_eq!(json_str(json, "tag_name").as_deref(), Some("v12"));
        assert_eq!(
            asset_url(json, "iPhoneAyna.exe").as_deref(),
            Some("https://github.com/knutolof06/ayna-merkezi/releases/download/v12/iPhoneAyna.exe")
        );
        assert_eq!(
            expected_sha256(manifest, "iPhoneAyna.exe").as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
    }
}

#[cfg(test)]
mod live {
    /// Ağ gerektirir: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn github_api_over_winhttp() {
        let body = super::https_get(super::API_HOST, "/repos/knutolof06/ayna-merkezi").unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("\"full_name\":\"knutolof06/ayna-merkezi\""), "{}", &text[..200.min(text.len())]);
    }

    /// Ağ gerektirir: gerçek son sürümü imza ve özetiyle baştan sona doğrular.
    #[test]
    #[ignore]
    fn latest_release_verifies() {
        let got = super::check().unwrap();
        assert!(got.is_some_and(|v| v >= 6), "{got:?}");
        let new = crate::util::exe_path().with_extension("guncelleme");
        assert!(new.exists());
        let _ = std::fs::remove_file(new);
    }
}
