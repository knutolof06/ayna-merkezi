//! Kurulum programıyla (AynaMerkezi-Setup.exe, `installer/AynaMerkezi.iss`) kurulan uygulamanın
//! Windows kayıtlarını güncel tutar. Uygulama kendini güncelledikçe exe değişir ama kurulum programı
//! yeniden çalışmaz; bu yüzden Programlar ve Özellikler'deki sürüm numarasını uygulama yazar.

use windows::core::{w, PCWSTR};

use crate::{update, util};

/// Inno Setup'ın kullanıcı başına kaldırma kaydı: `{AppId}_is1` (AppId kurulum betiğindekiyle aynı).
const UNINSTALL_KEY: PCWSTR =
    w!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{15E0CF97-DA98-430C-BCFB-369CC239E8A0}_is1");

pub fn on_startup() {
    let build = update::build();
    if build == 0 {
        // Yerel geliştirme derlemesi kurulu uygulamanın kayıtlarına dokunmaz.
        return;
    }
    let exe = util::exe_path();
    let Some(dir) = exe.parent() else { return };
    let installed = util::read_text(UNINSTALL_KEY, w!("InstallLocation"))
        .is_some_and(|location| same_dir(&location, &dir.to_string_lossy()));
    if installed {
        let version = build.to_string();
        if util::read_text(UNINSTALL_KEY, w!("DisplayVersion")).as_deref() != Some(version.as_str()) {
            util::write_text(UNINSTALL_KEY, w!("DisplayVersion"), &version);
        }
    }
    // "Windows ile başlat" açıksa: kurulu kopya kaydı kendine çevirir (ör. önce İndirilenler'den
    // çalıştırılıp sonra kurulduysa eski kopya açılıp tek kopya kilidini almasın). Taşınabilir bir
    // kopya kaydı yalnız gösterdiği exe silinmişse devralır; bir kez açılması kurulu kopyanınkini bozmaz.
    if let Some(cmd) = util::read_text(util::RUN_KEY, util::RUN_VALUE) {
        let target = run_target(&cmd);
        let stale = !std::path::Path::new(&target).exists();
        if (installed && !target.eq_ignore_ascii_case(&exe.to_string_lossy())) || stale {
            util::set_autostart(true);
        }
    }
}

/// Run kaydındaki komuttan exe yolu: `"C:\...\iPhoneAyna.exe" --arka-plan`.
fn run_target(cmd: &str) -> String {
    let cmd = cmd.trim();
    match cmd.strip_prefix('"').and_then(|rest| rest.split_once('"')) {
        Some((path, _)) => path.to_string(),
        None => cmd.split(' ').next().unwrap_or("").to_string(),
    }
}

fn same_dir(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::{run_target, same_dir};

    #[test]
    fn run_value_target() {
        assert_eq!(run_target("\"C:\\A B\\iPhoneAyna.exe\" --arka-plan"), "C:\\A B\\iPhoneAyna.exe");
        assert_eq!(run_target("C:\\x\\iPhoneAyna.exe --arka-plan"), "C:\\x\\iPhoneAyna.exe");
    }

    #[test]
    fn install_location_matches_exe_dir() {
        assert!(same_dir(
            "C:\\Users\\a\\AppData\\Local\\Programs\\Ayna Merkezi\\",
            "c:\\users\\a\\appdata\\local\\programs\\ayna merkezi"
        ));
        assert!(!same_dir("C:\\Programs\\Ayna Merkezi\\", "C:\\Users\\a\\Downloads"));
    }
}
