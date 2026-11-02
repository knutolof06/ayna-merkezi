//! UxPlay AirPlay alıcısını arka planda çalıştırır ve video penceresini bulur.
//!
//! UxPlay, gizli bir sözde konsolda (ConPTY) çalıştırılır. uxplay-windows bir GUI uygulaması;
//! çıktısını üst sürecin konsoluna bağlanarak yazar. Çıktı bir dosyaya yönlendirildiğinde C
//! çalışma zamanı onu tam arabelleğe alıyor ve satırlar (bağlantı, kopma nedeni, kare
//! geometrisi) ancak arabellek dolunca ya da hiç diske ulaşmıyordu. Konsol bir karakter aygıtı
//! olduğu için her yazım anında gelir; bir iş parçacığı çıktıyı okuyup günlüğe yazar ve satırları
//! uygulamaya iletir.

use std::ffi::c_void;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE, HWND, LPARAM, RECT, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{GetShortPathNameW, ReadFile};
use windows::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, COORD, HPCON};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::JobObjects::*;
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, ResumeThread,
    UpdateProcThreadAttribute, WaitForSingleObject, CREATE_SUSPENDED, EXTENDED_STARTUPINFO_PRESENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTUPINFOEXW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::util;

/// iPhone'un Ekran Yansıtma listesinde görünen ad.
pub const RECEIVER_NAME: &str = "Ayna";

/// Gecikmeyi düşüren ayarlar: ses/görüntü eşitlemesi kapalı (-vsync no), 60 fps, dikey
/// iPhone ekranını tam çözünürlükte sığdıran alan, yeni bağlantı eskisini düşürür.
///
/// Çözücü/dönüştürücü UxPlay'in varsayılanında (decodebin + videoconvert) bırakılır. UxPlay
/// dönüştürücüden sonra her zaman `videoscale` koyar ve o yalnız sistem belleğiyle çalışır;
/// `-vc d3d11convert` görüntüyü GPU belleğinde bıraktığı için hat görüntü akmaya başlar
/// başlamaz uyuşmazlık hatasıyla kapanıyor ve iPhone bağlantıyı düşürüyordu.
///
/// `-d 1`: hata ayıklama günlüğü (kare geometrisi için), ama her video paketini yazmadan.
fn arguments() -> String {
    format!("-n {RECEIVER_NAME} -nh -s 1440x2560@60 -fps 60 -vsync no -vs d3d11videosink -nohold -reset 0 -d 1\n")
}

fn log_path() -> PathBuf {
    std::env::temp_dir().join("iPhoneAyna-uxplay.log")
}

/// iPhone'un gönderdiği video karesinin geometrisi. iOS, Apple TV'deki gibi telefon görüntüsünü
/// alıcının bildirdiği ekran oranındaki bir kareye siyah dolguyla yerleştirebilir: `source_*` tüm
/// kare, `width`/`height` içindeki telefon görüntüsü (ortalanmış).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Geometry {
    pub source_w: f32,
    pub source_h: f32,
    pub width: f32,
    pub height: f32,
}

impl Geometry {
    /// `raop_rtp_mirror width_source = 1440.0 height_source = 2560.0 width = 1178.0 height = 2560.0`
    fn parse(line: &str) -> Option<Geometry> {
        let rest = &line[line.find("width_source")?..];
        // "ad = değer" çiftleri anahtar adıyla eşlenir: biri ayrıştırılamazsa değerler kaymasın.
        let tokens: Vec<&str> = rest.split_whitespace().collect();
        let value = |key: &str| -> Option<f32> {
            let i = tokens.iter().position(|t| *t == key)?;
            (tokens.get(i + 1) == Some(&"=")).then_some(())?;
            tokens.get(i + 2)?.parse::<f32>().ok().filter(|v| v.is_finite() && *v > 0.0)
        };
        Some(Geometry {
            source_w: value("width_source")?,
            source_h: value("height_source")?,
            width: value("width")?,
            height: value("height")?,
        })
    }
}

/// Sözde konsolda çalışan `cmd /c start /wait uxplay-windows.exe`.
struct Running {
    /// cmd.exe; UxPlay kapanınca o da biter.
    process: HANDLE,
    pty: HPCON,
    /// Sözde konsolun girişi; kullanılmaz ama konsol yaşadıkça açık kalmalı.
    input: HANDLE,
    cmd_pid: u32,
}

pub struct Engine {
    dir: Option<PathBuf>,
    running: Option<Running>,
    lines: Option<Receiver<String>>,
    /// cmd'nin başlattığı asıl uxplay-windows.exe (video penceresinin sahibi).
    uxplay_pid: std::cell::Cell<Option<u32>>,
    /// Son süreç taraması; bulunana dek en fazla saniyede bir taranır.
    pid_scan: Option<Instant>,
    job: HANDLE,
    last_start: Option<Instant>,
    /// UxPlay'in paylaşılan ayar dosyasının bizden önceki içeriği (dosya yoksa None);
    /// çıkışta geri yazılır, başka bir uxplay-windows kurulumunun ayarları bozulmasın.
    original_args: Option<Option<Vec<u8>>>,
    geometry: Option<Geometry>,
}

impl Engine {
    pub fn new() -> Engine {
        // UxPlay, argv[0]'daki ASCII dışı karakterlerde ("anlık", "paylaşımı") hata verip çıkıyor;
        // 8.3 kısa yol bu klasörü ASCII adla açar.
        let dir = util::find_uxplay_dir().map(|d| short_path(&d));
        Engine {
            dir,
            running: None,
            lines: None,
            uxplay_pid: std::cell::Cell::new(None),
            pid_scan: None,
            job: HANDLE::default(),
            last_start: None,
            original_args: None,
            geometry: None,
        }
    }

    /// UxPlay'in yeni çıktı satırlarını tarar; yeni bir kare geometrisi geldiyse (yansıtma başladı,
    /// iPhone döndü) onu döner.
    pub fn poll_geometry(&mut self) -> Option<Geometry> {
        let rx = self.lines.as_ref()?;
        let latest = rx.try_iter().filter(|l| l.contains("width_source")).filter_map(|l| Geometry::parse(&l)).last()?;
        if self.geometry == Some(latest) {
            return None;
        }
        self.geometry = Some(latest);
        Some(latest)
    }

    /// Yansıtma başlatılamıyorsa kullanıcıya gösterilecek neden.
    pub fn problem(&self) -> Option<String> {
        match &self.dir {
            None => Some("bin\\uxplay\\uxplay-windows.exe bu uygulamanın yanında olmalı.".into()),
            Some(d) if !d.to_string_lossy().is_ascii() => Some(format!(
                "UxPlay Türkçe karakter içeren klasörlerde çalışmıyor ve bu diskte kısa (8.3) adlar kapalı. \
                 Uygulama klasörünü yalnız İngilizce harfli bir yola taşıyın (ör. C:\\iPhoneAyna).\n\n{}",
                d.display()
            )),
            Some(_) => None,
        }
    }

    /// Video penceresinin sahibi olan uxplay-windows.exe'nin süreç kimliği.
    pub fn pid(&mut self) -> Option<u32> {
        if let Some(pid) = self.uxplay_pid.get() {
            return Some(pid);
        }
        if self.pid_scan.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return None;
        }
        self.pid_scan = Some(Instant::now());
        let pid = child_process(self.running.as_ref()?.cmd_pid, "uxplay-windows.exe")?;
        self.uxplay_pid.set(Some(pid));
        Some(pid)
    }

    /// Çalışmıyorsa başlatır, kapandıysa 3 sn aralıkla yeniden başlatır.
    pub fn ensure_running(&mut self) {
        if let Some(r) = &self.running {
            if unsafe { WaitForSingleObject(r.process, 0) } != WAIT_OBJECT_0 {
                return;
            }
            util::log("UxPlay kapandı; yeniden başlatılıyor (nedeni %TEMP%\\iPhoneAyna-uxplay.prev.log içinde).");
            self.stop_running();
        }
        if self.last_start.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
            return;
        }
        if self.problem().is_some() {
            return;
        }
        let Some(dir) = self.dir.clone() else { return };
        self.last_start = Some(Instant::now());
        self.geometry = None;
        if self.original_args.is_none() {
            self.original_args = Some(std::fs::read(arguments_path()).ok());
        }
        write_arguments();

        // Önceki çalıştırmanın günlüğü saklanır; başarısız bir denemenin boş günlüğü ise saklı
        // kaydın (asıl kapanma nedeninin) üzerine yazılmaz.
        if std::fs::metadata(log_path()).is_ok_and(|m| m.len() > 0) {
            let _ = std::fs::rename(log_path(), log_path().with_extension("prev.log"));
        }
        let log = std::fs::File::create(log_path()).ok();
        // Her çalıştırmaya yeni bir iş nesnesi: sonlandırılmakta olan eski iş nesnesine yeni süreç
        // eklenemiyor ("Erişim engellendi"), yansıtma ancak bir sonraki denemede geri geliyordu.
        self.job = unsafe { new_job() };
        match unsafe { spawn_in_pty(&dir, self.job, log) } {
            Ok((running, lines)) => {
                self.running = Some(running);
                self.lines = Some(lines);
            }
            Err(e) => {
                util::log(&format!("UxPlay başlatılamadı: {}", e.message()));
                self.stop_running();
            }
        }
    }

    fn stop_running(&mut self) {
        // Konsola bağlı kalmış süreçler (UxPlay'in yan süreçleri) önce kapanır; yoksa
        // ClosePseudoConsole onların bitmesini bekleyip arayüzü dondurabilir. İş nesnesinin son
        // tutamağını kapatmak (KILL_ON_JOB_CLOSE) içindeki her süreci sonlandırır.
        if !self.job.is_invalid() && !self.job.0.is_null() {
            unsafe {
                // Açıkça sonlandırılır (bayrak konamadıysa da); sonra tutamak kapanır.
                let _ = TerminateJobObject(self.job, 1);
                let _ = CloseHandle(self.job);
            }
        }
        self.job = HANDLE::default();
        if let Some(r) = self.running.take() {
            unsafe {
                // Konsol kapatma ayrı iş parçacığında: kapanınca okuma ucu kapanır, okuyucu biter.
                let _ = CloseHandle(r.process);
                let (pty, input) = (r.pty.0, r.input.0 as isize);
                std::thread::spawn(move || {
                    ClosePseudoConsole(HPCON(pty));
                    let _ = CloseHandle(HANDLE(input as _));
                });
            }
        }
        self.uxplay_pid.set(None);
        self.pid_scan = None;
    }

    /// UxPlay'i kapatır ve paylaşılan ayar dosyasını eski hâline getirir.
    pub fn shutdown(&mut self) {
        self.stop_running();
        match self.original_args.take() {
            Some(Some(bytes)) => {
                let _ = std::fs::write(arguments_path(), bytes);
            }
            Some(None) => {
                let _ = std::fs::remove_file(arguments_path());
            }
            None => {}
        }
    }

    /// UxPlay'in henüz gömülmemiş, görünür üst düzey video penceresi.
    pub fn find_video_window(&mut self) -> Option<HWND> {
        let pid = self.pid()?;
        let mut found = Search { pid, hwnd: None };
        unsafe {
            let _ = EnumWindows(Some(enum_proc), LPARAM(&mut found as *mut Search as isize));
        }
        found.hwnd
    }
}

/// Kapandığında (ya da uygulama çöktüğünde) içindeki tüm süreçleri sonlandıran iş nesnesi.
unsafe fn new_job() -> HANDLE {
    let job = match CreateJobObjectW(None, None) {
        Ok(job) => job,
        Err(e) => {
            util::log(&format!("İş nesnesi oluşturulamadı; UxPlay uygulamayla birlikte kapanmayabilir: {}", e.message()));
            return HANDLE::default();
        }
    };
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let set = SetInformationJobObject(
        job,
        JobObjectExtendedLimitInformation,
        &info as *const _ as _,
        std::mem::size_of_val(&info) as u32,
    );
    if let Err(e) = set {
        // Bayrak yoksa tutamağı kapatmak süreçleri öldürmez; stop_running yine de
        // TerminateJobObject ile sonlandırır.
        util::log(&format!("İş nesnesine kapanınca-öldür bayrağı konamadı: {}", e.message()));
    }
    job
}

/// Hata yollarında açılmış tutamakları kapatan bekçi; başarıda alanlar devralınır.
#[derive(Default)]
struct Handles {
    handles: Vec<HANDLE>,
    pty: Option<HPCON>,
}

impl Drop for Handles {
    fn drop(&mut self) {
        unsafe {
            if let Some(pty) = self.pty.take() {
                ClosePseudoConsole(pty);
            }
            for h in self.handles.drain(..) {
                if !h.is_invalid() && !h.0.is_null() {
                    let _ = CloseHandle(h);
                }
            }
        }
    }
}

/// `cmd /c start "" /wait uxplay-windows.exe` gizli bir sözde konsolda, askıda başlatılır, iş
/// nesnesine eklenir (UxPlay ve yan süreçleri de ona girer) ve sonra devam ettirilir.
unsafe fn spawn_in_pty(
    dir: &Path,
    job: HANDLE,
    log: Option<std::fs::File>,
) -> windows::core::Result<(Running, Receiver<String>)> {
    let mut guard = Handles::default();
    let (mut in_read, mut in_write, mut out_read, mut out_write) =
        (HANDLE::default(), HANDLE::default(), HANDLE::default(), HANDLE::default());
    CreatePipe(&mut in_read, &mut in_write, None, 0)?;
    guard.handles.extend([in_read, in_write]);
    CreatePipe(&mut out_read, &mut out_write, None, 0)?;
    guard.handles.extend([out_read, out_write]);
    // Geniş konsol: uzun satırlar (geometri ~120 karakter) bölünmesin.
    let pty = CreatePseudoConsole(COORD { X: 400, Y: 50 }, in_read, out_write, 0)?;
    guard.pty = Some(pty);
    // Sözde konsol kendi kopyalarını tutar.
    let _ = CloseHandle(in_read);
    let _ = CloseHandle(out_write);
    guard.handles.retain(|h| *h != in_read && *h != out_write);

    let mut size = 0usize;
    let _ = InitializeProcThreadAttributeList(LPPROC_THREAD_ATTRIBUTE_LIST::default(), 1, 0, &mut size);
    let mut attrs = vec![0u8; size];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(attrs.as_mut_ptr() as *mut c_void);
    InitializeProcThreadAttributeList(list, 1, 0, &mut size)?;
    let attr = UpdateProcThreadAttribute(
        list,
        0,
        PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
        Some(pty.0 as *const c_void),
        std::mem::size_of::<HPCON>(),
        None,
        None,
    );
    if let Err(e) = attr {
        DeleteProcThreadAttributeList(list);
        return Err(e);
    }
    let mut si = STARTUPINFOEXW::default();
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.lpAttributeList = list;

    // Tam yol: arama sırasında uygulama klasöründeki bir "cmd.exe" çalıştırılmasın.
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    let cmd = PathBuf::from(system).join("System32").join("cmd.exe");
    let exe = dir.join("uxplay-windows.exe");
    let mut command = util::wide(&format!("\"{}\" /d /c start \"\" /wait \"{}\"", cmd.display(), exe.display()));
    let cwd = util::wide(&dir.to_string_lossy());
    let mut pi = PROCESS_INFORMATION::default();
    let created = CreateProcessW(
        None,
        PWSTR(command.as_mut_ptr()),
        None,
        None,
        false,
        EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED,
        None,
        PCWSTR(cwd.as_ptr()),
        &si.StartupInfo,
        &mut pi,
    );
    DeleteProcThreadAttributeList(list);
    created?;
    guard.handles.extend([pi.hProcess, pi.hThread]);
    // İş nesnesi dışında kalan UxPlay, uygulama kapanınca yetim kalır ve 7000 portunu tutar.
    if !job.is_invalid() {
        if let Err(e) = AssignProcessToJobObject(job, pi.hProcess) {
            let _ = windows::Win32::System::Threading::TerminateProcess(pi.hProcess, 1);
            return Err(e);
        }
    }
    if ResumeThread(pi.hThread) == u32::MAX {
        let e = windows::core::Error::from_win32();
        let _ = windows::Win32::System::Threading::TerminateProcess(pi.hProcess, 1);
        return Err(e);
    }

    // Başarı: tutamaklar artık Running'in; bekçi yalnız iş parçacığı tutamağını kapatır.
    guard.pty = None;
    guard.handles.retain(|h| *h == pi.hThread);
    drop(guard);

    let (tx, rx) = mpsc::channel();
    let out = out_read.0 as isize;
    std::thread::spawn(move || read_output(HANDLE(out as _), log, tx));
    Ok((Running { process: pi.hProcess, pty, input: in_write, cmd_pid: pi.dwProcessId }, rx))
}

/// Sözde konsol çıktısını okur: terminal kaçış dizilerini ayıklar, satırları günlüğe yazar ve
/// uygulamaya iletir. Boru sürekli boşaltılmalı: dolarsa UxPlay konsola yazarken bloklanır.
fn read_output(pipe: HANDLE, mut log: Option<std::fs::File>, tx: Sender<String>) {
    let mut filter = VtFilter::default();
    let mut line = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let mut n = 0u32;
        let ok = unsafe { ReadFile(pipe, Some(&mut buf), Some(&mut n), None) };
        if ok.is_err() {
            break; // ERROR_BROKEN_PIPE: sözde konsol kapandı
        }
        for &b in &buf[..n as usize] {
            let Some(b) = filter.feed(b) else { continue };
            match b {
                b'\n' => {
                    let text = String::from_utf8_lossy(&line).trim_end_matches('\r').trim_end().to_string();
                    line.clear();
                    if text.is_empty() {
                        continue;
                    }
                    if let Some(f) = log.as_mut() {
                        let _ = writeln!(f, "{text}");
                        let _ = f.flush();
                    }
                    let _ = tx.send(text);
                }
                b'\r' => {}
                _ => line.push(b),
            }
        }
    }
    unsafe {
        let _ = CloseHandle(pipe);
    }
}

/// Terminal (VT) kaçış dizilerini atan küçük durum makinesi: CSI (`ESC [ … son`), OSC
/// (`ESC ] … BEL` ya da `ESC \`) ve tek karakterlik `ESC x`. ConPTY bazen satır sonu yerine imleci
/// konumlandırır; satır değiştiren CSI'ler (H f A B E F d J) `\n` sayılır ki iki satır birleşmesin
/// (fazladan boş satırlar atılır).
#[derive(Default)]
struct VtFilter {
    state: u8, // 0 düz metin, 1 ESC görüldü, 2 CSI içinde, 3 OSC içinde, 4 OSC içinde ESC
}

impl VtFilter {
    fn feed(&mut self, b: u8) -> Option<u8> {
        match (self.state, b) {
            (0, 0x1B) => self.state = 1,
            (0, _) => return Some(b),
            (1, b'[') => self.state = 2,
            (1, b']') => self.state = 3,
            (1, _) => self.state = 0,
            (2, b'H' | b'f' | b'A' | b'B' | b'E' | b'F' | b'd' | b'J') => {
                self.state = 0;
                return Some(b'\n');
            }
            (2, 0x40..=0x7E) => self.state = 0,
            (2, _) => {}
            (3, 0x07) => self.state = 0,
            (3, 0x1B) => self.state = 4,
            (3, _) => {}
            (4, _) => self.state = 0,
            _ => self.state = 0,
        }
        None
    }
}

/// `parent` sürecinin `name` adlı alt sürecini bulur.
fn child_process(parent: u32, name: &str) -> Option<u32> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = None;
        let mut ok = Process32FirstW(snap, &mut entry).is_ok();
        while ok {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let exe = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if entry.th32ParentProcessID == parent && exe.eq_ignore_ascii_case(name) {
                found = Some(entry.th32ProcessID);
                break;
            }
            ok = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

/// Adı verilen tüm süreçlerin kimlikleri.
pub fn processes_named(name: &str) -> Vec<u32> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut entry).is_ok();
        while ok {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case(name) {
                out.push(entry.th32ProcessID);
            }
            ok = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

struct Search {
    pid: u32,
    hwnd: Option<HWND>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let search = &mut *(lparam.0 as *mut Search);
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid != search.pid || !IsWindowVisible(hwnd).as_bool() {
        return true.into();
    }
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    if ex & WS_EX_TOOLWINDOW.0 != 0 {
        return true.into();
    }
    // uxplay-windows bir Qt tepsi uygulaması: kendi ayar penceresi ve iletişim kutuları
    // video penceresiyle karıştırılıp gömülmemeli.
    let mut class = [0u16; 128];
    let n = GetClassNameW(hwnd, &mut class) as usize;
    let class = String::from_utf16_lossy(&class[..n]);
    if class.starts_with("Qt") || class == "#32770" {
        return true.into();
    }
    let mut r = RECT::default();
    if GetClientRect(hwnd, &mut r).is_ok() && util::width(&r) >= 64 && util::height(&r) >= 64 {
        search.hwnd = Some(hwnd);
        return false.into();
    }
    true.into()
}

fn short_path(path: &Path) -> PathBuf {
    let long = util::wide(&path.to_string_lossy());
    let mut buf = vec![0u16; 1024];
    let n = unsafe { GetShortPathNameW(PCWSTR(long.as_ptr()), Some(&mut buf)) } as usize;
    if n == 0 || n >= buf.len() {
        return path.to_path_buf();
    }
    PathBuf::from(String::from_utf16_lossy(&buf[..n]))
}

/// uxplay-windows ayarları komut satırından değil bu dosyadan okur.
fn arguments_path() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_default())
        .join("leapbtw")
        .join("uxplay-windows")
        .join("arguments.txt")
}

fn write_arguments() {
    let path = arguments_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, arguments());
}

#[cfg(test)]
mod tests {
    use super::{Geometry, VtFilter};

    #[test]
    fn parses_geometry_line() {
        let line = "raop_rtp_mirror width_source = 1440.000000 height_source = 2560.000000 width = 1178.000000 height = 2560.000000";
        let g = Geometry::parse(line).unwrap();
        assert_eq!((g.source_w, g.source_h, g.width, g.height), (1440.0, 2560.0, 1178.0, 2560.0));
        assert!(Geometry::parse("Begin streaming to GStreamer video pipeline").is_none());
        // Bir değer bozuksa diğerleri kaymaz, geometri reddedilir.
        assert!(Geometry::parse("width_source = x height_source = 2560.0 width = 1178.0 height = 2560.0").is_none());
    }

    #[test]
    fn strips_terminal_sequences() {
        let mut f = VtFilter::default();
        let input = b"\x1b[?25l\x1b]0;baslik\x07UxPlay \x1b[32m1.73\x1b[0m\r\n";
        let out: Vec<u8> = input.iter().filter_map(|&b| f.feed(b)).collect();
        assert_eq!(out, b"UxPlay 1.73\r\n");
        // İmleç konumlandırmayla ayrılan satırlar birleşmez.
        let out: Vec<u8> = b"bir\x1b[5;1Hiki".iter().filter_map(|&b| f.feed(b)).collect();
        assert_eq!(out, b"bir\niki");
    }
}
