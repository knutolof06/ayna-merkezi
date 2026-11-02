//! Bırakılan dosyaları iCloud Drive üzerinden iPhone'a gönderir.
//!
//! iPhone, Apple dışı cihazlardan Bluetooth ya da AirDrop ile dosya kabul etmez. iCloud for
//! Windows'un eşitlediği `iCloudDrive` klasörüne kopyalanan dosya ise kendiliğinden iPhone'da
//! Dosyalar > iCloud Drive altında görünür; iPhone'a hiçbir şey kurmak gerekmez.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::CloudFilters::{
    CfGetPlaceholderInfo, CF_IN_SYNC_STATE_IN_SYNC, CF_PLACEHOLDER_BASIC_INFO, CF_PLACEHOLDER_INFO_BASIC,
};
use windows::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};

/// iCloud Drive içinde gönderilen dosyaların toplandığı klasör.
pub const FOLDER_NAME: &str = "iPhone Ayna";

/// iCloud for Windows'un Microsoft Store sayfası.
pub const STORE_URL: &str = "ms-windows-store://pdp/?productid=9PKTQ5699M62";

pub enum Target {
    /// Dosyaların kopyalanacağı klasör.
    Ready(PathBuf),
    /// iCloud kurulu ama iCloud Drive eşitlemesi açık değil.
    DriveOff,
    /// iCloud for Windows kurulu değil.
    NotInstalled,
}

pub fn target() -> Target {
    let home = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default());
    let drive = home.join("iCloudDrive");
    if drive.is_dir() {
        return Target::Ready(drive.join(FOLDER_NAME));
    }
    if icloud_installed() {
        Target::DriveOff
    } else {
        Target::NotInstalled
    }
}

/// Store sürümü `AppleInc.iCloud_*` paket klasörü, eski masaüstü sürümü kayıt anahtarı bırakır.
fn icloud_installed() -> bool {
    let packages = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("Packages");
    let store = std::fs::read_dir(packages)
        .map(|entries| {
            entries.flatten().any(|e| e.file_name().to_string_lossy().starts_with("AppleInc.iCloud"))
        })
        .unwrap_or(false);
    store || Path::new(r"C:\Program Files\Common Files\Apple\Internet Services\iCloud.exe").exists()
}

/// iCloud dosyayı buluta yükledi mi? Dosyanın bulut eşitleme durumu Windows'un bulut dosyaları
/// API'sinden okunur. iCloud Drive'da değilse ya da okunamıyorsa `None`.
pub fn uploaded(path: &Path) -> Option<bool> {
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    // Yalnız öznitelik okuma hakkıyla açılır: iCloud'un yükleme sırasındaki erişimini engellemez.
    let file = std::fs::OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES).share_mode(7).open(path).ok()?;
    // Temel bilgi değişken uzunlukta bir kimlik alanıyla biter; 8 bayt hizalı geniş tampon.
    let mut buf = [0u64; 128];
    unsafe {
        CfGetPlaceholderInfo(
            HANDLE(file.as_raw_handle() as _),
            CF_PLACEHOLDER_INFO_BASIC,
            buf.as_mut_ptr() as _,
            std::mem::size_of_val(&buf) as u32,
            None,
        )
        .ok()?;
        let info = &*(buf.as_ptr() as *const CF_PLACEHOLDER_BASIC_INFO);
        Some(info.InSyncState == CF_IN_SYNC_STATE_IN_SYNC)
    }
}

/// WM_DROPFILES ile gelen yolları okur ve bırakma tutamacını serbest bırakır.
pub fn dropped_paths(drop: HDROP) -> Vec<PathBuf> {
    unsafe {
        let count = DragQueryFileW(drop, u32::MAX, None);
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(drop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
            paths.push(PathBuf::from(OsString::from_wide(&buf[..got])));
        }
        DragFinish(drop);
        paths
    }
}

/// Bu sınırların üstündeki gönderimler için kullanıcıya onay sorulur (iCloud kotası, yanlışlıkla
/// bırakılan büyük klasörler).
pub const LARGE_FILES: usize = 200;
pub const LARGE_BYTES: u64 = 1 << 30;

/// Klasör derinliği sınırı; bağlantı ya da hatalı yapı sonsuz inişe yol açmasın.
const MAX_DEPTH: usize = 32;

#[derive(Default)]
pub struct Report {
    pub copied: usize,
    pub failed: Vec<(PathBuf, String)>,
    /// Kopyalanan dosyaların iCloud Drive'daki yolları (yükleme durumu izlemek için).
    pub files: Vec<PathBuf>,
}

/// Gönderim sınırları aşıyorsa (dosya sayısı, toplam bayt) döner; sınır aşılınca saymayı bırakır.
pub fn measure(paths: &[PathBuf]) -> (usize, u64) {
    fn walk(p: &Path, depth: usize, acc: &mut (usize, u64)) {
        if depth > MAX_DEPTH || acc.0 > LARGE_FILES || acc.1 > LARGE_BYTES {
            return;
        }
        let Ok(meta) = std::fs::symlink_metadata(p) else { return };
        if meta.file_type().is_symlink() {
            return;
        }
        if meta.is_dir() {
            for entry in std::fs::read_dir(p).into_iter().flatten().flatten() {
                walk(&entry.path(), depth + 1, acc);
            }
        } else {
            acc.0 += 1;
            acc.1 += meta.len();
        }
    }
    let mut acc = (0, 0);
    for p in paths {
        walk(p, 0, &mut acc);
    }
    acc
}

/// Dosyaları (klasörleri içerikleriyle) `dest` altına kopyalar. Aynı adlı dosyanın üzerine
/// yazılmaz, "ad (2).uzantı" verilir. Hatalar öğe başına toplanır; yarım kalan dosya silinir
/// (yoksa iCloud kesik dosyayı iPhone'a yüklerdi).
pub fn copy_all(paths: &[PathBuf], dest: &Path) -> Report {
    let mut report = Report::default();
    if let Err(e) = std::fs::create_dir_all(dest) {
        report.failed.extend(paths.iter().map(|p| (p.clone(), e.to_string())));
        return report;
    }
    let dest_real = std::fs::canonicalize(dest).unwrap_or_else(|_| dest.to_path_buf());
    for src in paths {
        let Some(name) = src.file_name() else {
            report.failed.push((src.clone(), "sürücünün tamamı gönderilemez".into()));
            continue;
        };
        // iCloud Drive'ın kendisini ya da onu içeren bir klasörü göndermek, hedefi kaynağın
        // içine kopyalar ve sonsuza kadar iç içe klasör üretirdi.
        if let Ok(src_real) = std::fs::canonicalize(src) {
            if dest_real.starts_with(&src_real) {
                report.failed.push((src.clone(), "iCloud Drive klasörünü içerdiği için gönderilemez".into()));
                continue;
            }
        }
        copy_entry(src, &unique_path(dest, Path::new(name)), 0, &mut report);
    }
    report
}

fn copy_entry(src: &Path, dest: &Path, depth: usize, report: &mut Report) {
    let meta = match std::fs::symlink_metadata(src) {
        Ok(m) => m,
        Err(e) => return report.failed.push((src.to_path_buf(), e.to_string())),
    };
    // Kısayol bağlantıları ve junction'lar (ör. profildeki "Application Data") izlenmez.
    if meta.file_type().is_symlink() {
        return;
    }
    if !meta.is_dir() {
        return copy_file(src, dest, report);
    }
    if depth >= MAX_DEPTH {
        return report.failed.push((src.to_path_buf(), "klasör çok derin".into()));
    }
    if let Err(e) = std::fs::create_dir_all(dest) {
        return report.failed.push((src.to_path_buf(), e.to_string()));
    }
    match std::fs::read_dir(src) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry) => copy_entry(&entry.path(), &dest.join(entry.file_name()), depth + 1, report),
                    Err(e) => report.failed.push((src.to_path_buf(), e.to_string())),
                }
            }
        }
        Err(e) => report.failed.push((src.to_path_buf(), e.to_string())),
    }
}

/// Hedef dosya `create_new` ile açılır: aynı anda yapılan iki bırakma aynı adı seçerse biri
/// diğerinin üzerine yazmaz, bir sonraki boş adı alır.
fn copy_file(src: &Path, dest: &Path, report: &mut Report) {
    let mut input = match std::fs::File::open(src) {
        Ok(f) => f,
        Err(e) => return report.failed.push((src.to_path_buf(), e.to_string())),
    };
    let mut target = dest.to_path_buf();
    for _ in 0..8 {
        let out = std::fs::OpenOptions::new().write(true).create_new(true).open(&target);
        match out {
            Ok(mut out) => {
                return match io::copy(&mut input, &mut out) {
                    Ok(_) => {
                        report.copied += 1;
                        report.files.push(target.clone());
                    }
                    Err(e) => {
                        drop(out);
                        let _ = std::fs::remove_file(&target);
                        report.failed.push((src.to_path_buf(), e.to_string()));
                    }
                };
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
                let name = target.file_name().map(PathBuf::from).unwrap_or_default();
                target = unique_path(&dir, &name);
            }
            Err(e) => return report.failed.push((src.to_path_buf(), e.to_string())),
        }
    }
    report.failed.push((src.to_path_buf(), "boş bir dosya adı bulunamadı".into()));
}

fn unique_path(dir: &Path, name: &Path) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let stem = name.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = name.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_files_and_folders_without_overwriting() {
        let root = std::env::temp_dir().join(format!("ayna-test-{}", std::process::id()));
        let src = root.join("src");
        let dest = root.join("dest");
        std::fs::create_dir_all(src.join("klasör")).unwrap();
        std::fs::write(src.join("not.txt"), "1").unwrap();
        std::fs::write(src.join("klasör").join("iç.txt"), "2").unwrap();

        let paths = vec![src.join("not.txt"), src.join("klasör")];
        let r = copy_all(&paths, &dest);
        assert_eq!((r.copied, r.failed.len()), (2, 0));
        assert_eq!(copy_all(&paths[..1], &dest).copied, 1);
        assert_eq!(measure(&paths), (2, 2));

        assert!(dest.join("not.txt").exists());
        assert!(dest.join("not (2).txt").exists());
        assert!(dest.join("klasör").join("iç.txt").exists());

        // iCloud dışındaki sıradan dosya bulut yer tutucusu değildir.
        assert_eq!(uploaded(&dest.join("not.txt")), None);

        // Hedefi içeren klasör kendi içine kopyalanmaz.
        let r = copy_all(&[root.clone()], &dest);
        assert_eq!(r.copied, 0);
        assert_eq!(r.failed.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod icloud_tests {
    use super::*;

    /// Gerçek iCloud Drive klasöründeki dosyaların yükleme durumunu yazdırır.
    #[test]
    #[ignore = "iCloud for Windows ve iCloud Drive klasoru gerekir"]
    fn icloud_upload_state() {
        let Target::Ready(dir) = target() else { panic!("iCloud Drive yok") };
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            println!("{:?} -> {:?}", e.file_name(), uploaded(&e.path()));
        }
    }
}
