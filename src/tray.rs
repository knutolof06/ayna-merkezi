//! Bildirim alanı ("gizli simgeleri göster") simgesi: uygulamanın logosu. Sol tık ayarları açar,
//! sağ tık menü (Ayarlar, güncelleme, çık). UxPlay'in arayüzü (uxplay-windows) kendi simgesini oraya
//! koyuyor; o simge her açılışta Windows'tan silinir, yerinde yalnız bu uygulama görünür.

use std::cell::Cell;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION, NOTIFYICONDATAW,
    NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::i18n::t;
use crate::taskbar;
use crate::util;

const CLASS: PCWSTR = w!("iPhoneAyna.Bildirim");
const WM_APP_TRAY: u32 = WM_APP + 40;
const ICON_ID: u32 = 1;
const MENU_SETTINGS: usize = 1;
const MENU_UPDATE: usize = 2;
const MENU_EXIT: usize = 3;

thread_local! {
    static WINDOW: Cell<HWND> = Cell::new(HWND::default());
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    /// UxPlay simgesi: bulunduğu pencere ve kimlik (bulununca yalnız o yeniden silinir).
    static FOREIGN: Cell<Option<(isize, u32)>> = const { Cell::new(None) };
    static LAST_SWEEP: Cell<Option<Instant>> = const { Cell::new(None) };
    /// İzlenen UxPlay süreci ve ilk görüldüğü an (ilk dakika sık, sonra seyrek bakılır).
    static WATCHED: Cell<Option<(u32, Instant)>> = const { Cell::new(None) };
    /// Bildirim simgesi (bir kez oluşturulur).
    static ICON: Cell<isize> = const { Cell::new(0) };
}

pub fn create() {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let instance: HINSTANCE = module.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE::default(), CLASS, w!(""), WS_POPUP, 0, 0, 0, 0, HWND::default(), HMENU::default(), instance, None)
        else {
            return;
        };
        WINDOW.with(|w| w.set(hwnd));
        TASKBAR_CREATED.with(|c| c.set(RegisterWindowMessageW(w!("TaskbarCreated"))));
        add(hwnd);
    }
}

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: ICON_ID, ..Default::default() }
}

unsafe fn add(hwnd: HWND) {
    let mut d = data(hwnd);
    d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    d.uCallbackMessage = WM_APP_TRAY;
    let icon = ICON.with(|i| {
        if i.get() == 0 {
            i.set(taskbar::app_icon(GetSystemMetrics(SM_CXSMICON).max(16)).0 as isize);
        }
        i.get()
    });
    d.hIcon = HICON(icon as _);
    let tip: Vec<u16> = "Ayna Merkezi".encode_utf16().collect();
    d.szTip[..tip.len()].copy_from_slice(&tip);
    let _ = Shell_NotifyIconW(NIM_ADD, &d);
    d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
}

/// Çıkışta simge kaldırılır (Windows'ta artık simge kalmasın).
pub fn remove() {
    let hwnd = WINDOW.with(|w| w.get());
    if !hwnd.is_invalid() {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data(hwnd));
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_APP_TRAY {
        // Sürüm 4: olay alt 16 bitte.
        // Sürüm 4'te tıklama NIN_SELECT/NIN_KEYSELECT, sağ tık WM_CONTEXTMENU olarak bir kez gelir.
        match (lp.0 & 0xFFFF) as u32 {
            0x0400 | 0x0401 => taskbar::open_hub(),
            WM_CONTEXTMENU => menu(hwnd),
            _ => {}
        }
        return LRESULT(0);
    }
    if msg == TASKBAR_CREATED.with(|c| c.get()) && msg != 0 {
        // Gezgin yeniden başladı: simge yeniden eklenir.
        add(hwnd);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

unsafe fn menu(hwnd: HWND) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let items = [util::wide(t("Ayarları aç", "Settings")), util::wide(t("Güncellemeleri denetle", "Check for updates")), util::wide(t("Çık", "Quit"))];
    let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, PCWSTR(items[0].as_ptr()));
    let _ = AppendMenuW(menu, MF_STRING, MENU_UPDATE, PCWSTR(items[1].as_ptr()));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, PCWSTR(items[2].as_ptr()));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, None);
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
    match cmd.0 as usize {
        MENU_SETTINGS => taskbar::open_hub(),
        MENU_UPDATE => {
            taskbar::open_hub();
            crate::hub::check_updates();
        }
        MENU_EXIT => taskbar::quit(),
        _ => {}
    }
}

/// Süreç gerçekten uxplay-windows mı (süreç numarası kapanan bir sürecin ardından başkasına geçmiş
/// olabilir; başka bir uygulamanın simgesi asla silinmez).
unsafe fn is_uxplay(pid: u32) -> bool {
    let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
    let _ = CloseHandle(process);
    ok && String::from_utf16_lossy(&buf[..len as usize]).to_lowercase().ends_with("\\uxplay-windows.exe")
}

/// UxPlay sürecinin (uxplay-windows) bildirim simgesini siler. Sık çağrılabilir: ilk dakika 2 sn'de,
/// sonra 15 sn'de bir iş yapar. Simge bulununca yalnız o yeniden silinir (Gezgin yeniden başlayınca
/// UxPlay simgeyi yeniden ekler); bulunamadıysa sürecin Qt pencereleri taranır.
pub fn hide_foreign(pid: u32) {
    unsafe {
        let now = Instant::now();
        let first = match WATCHED.with(|w| w.get()) {
            Some((p, t)) if p == pid => t,
            _ => {
                WATCHED.with(|w| w.set(Some((pid, now))));
                FOREIGN.with(|f| f.set(None));
                now
            }
        };
        let interval = if now.duration_since(first) < Duration::from_secs(60) { Duration::from_secs(2) } else { Duration::from_secs(15) };
        if LAST_SWEEP.with(|l| l.get()).is_some_and(|t| now.duration_since(t) < interval) {
            return;
        }
        LAST_SWEEP.with(|l| l.set(Some(now)));
        if !is_uxplay(pid) {
            return;
        }
        if let Some((hwnd, id)) = FOREIGN.with(|f| f.get()) {
            let mut owner = 0u32;
            GetWindowThreadProcessId(HWND(hwnd as _), Some(&mut owner));
            if owner == pid {
                let mut d = data(HWND(hwnd as _));
                d.uID = id;
                let _ = Shell_NotifyIconW(NIM_DELETE, &d);
                return;
            }
            FOREIGN.with(|f| f.set(None));
        }
        for hwnd in process_windows(pid) {
            for id in 0..=64u32 {
                let mut d = data(hwnd);
                d.uID = id;
                if Shell_NotifyIconW(NIM_DELETE, &d).as_bool() {
                    util::log("UxPlay'in bildirim simgesi gizlendi.");
                    FOREIGN.with(|f| f.set(Some((hwnd.0 as isize, id))));
                    return;
                }
            }
        }
    }
}

/// Sürecin üst düzey ve yalnız-ileti pencereleri (Qt simge penceresi yalnız-ileti olabilir).
unsafe fn process_windows(pid: u32) -> Vec<HWND> {
    let mut out = Vec::new();
    for parent in [HWND::default(), HWND_MESSAGE] {
        let mut after = HWND::default();
        loop {
            let Ok(h) = FindWindowExW(parent, after, PCWSTR::null(), PCWSTR::null()) else { break };
            if h.is_invalid() {
                break;
            }
            let mut owner = 0u32;
            GetWindowThreadProcessId(h, Some(&mut owner));
            // Yalnız Qt pencereleri (uxplay-windows bir Qt uygulaması; simge penceresi Qt sınıfında).
            let mut class = [0u16; 64];
            let n = GetClassNameW(h, &mut class) as usize;
            if owner == pid && String::from_utf16_lossy(&class[..n]).starts_with('Q') {
                out.push(h);
            }
            after = h;
        }
    }
    out
}
