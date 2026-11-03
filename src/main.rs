#![windows_subsystem = "windows"]

mod ble;
mod buttons;
mod corners;
mod crypto;
mod dragout;
mod ghost;
mod gst;
mod hid;
mod hub;
mod mac;
mod macview;
mod mirror;
mod taskbar;
mod toast;
mod transfer;
mod util;
mod update;
mod uxplay;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Windows ile başlarken kullanılan bayrak: pencere açılmaz, yalnız görev çubuğu ikonu gelir.
pub const BACKGROUND_FLAG: &str = "--arka-plan";

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // Güncellemeden sonra: eski sürüm kapanana dek beklenir (tek kopya kilidi ondan alınır).
        update::on_startup();

        // Tek kopya: ikinci açılış mevcut pencereyi öne getirir.
        let _mutex = CreateMutexW(None, true, w!("Local\\iPhoneAyna.TekKopya"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(existing) = FindWindowW(hub::CLASS, PCWSTR::null()) {
                // Ön plandaki bu kopya, çalışan kopyanın penceresini öne getirmesine izin verir;
                // yoksa Windows'un ön plan kilidi pencereyi diğerlerinin arkasında bırakır.
                let mut pid = 0u32;
                GetWindowThreadProcessId(existing, Some(&mut pid));
                if pid != 0 {
                    let _ = AllowSetForegroundWindow(pid);
                }
                let _ = PostMessageW(existing, taskbar::WM_APP_HUB, WPARAM(0), LPARAM(0));
            }
            return;
        }

        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        // Sürükle-bırak (OLE) için.
        let _ = windows::Win32::System::Ole::OleInitialize(None);

        let ble = ble::Ble::start();
        let engine = uxplay::Engine::new();
        let Some(hwnd) = mirror::create(ble, engine) else { return };
        let Some(hub) = hub::create() else { return };
        macview::create();
        taskbar::ensure(hwnd);
        update::start(hub);
        if update::just_updated() {
            toast::show(hwnd, &format!("Ayna Merkezi güncellendi (sürüm {}).", update::build()));
        }

        // Uygulamayı açmak ana pencereyi açar; iPhone ekranı görev çubuğundaki ikondan.
        if !std::env::args().any(|a| a == BACKGROUND_FLAG) {
            hub::show(hub);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
