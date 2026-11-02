//! Görev çubuklarında Başlat düğmesinin soluna yerleşen cihaz ikonları: iPhone ve (bağlıyken) Mac.
//!
//! Windows 11 uygulamaların Başlat'ın soluna sabitlenmesine izin vermez. Her ikon, sahibi bir
//! görev çubuğu (`Shell_TrayWnd` ya da ikinci monitörlerdeki `Shell_SecondaryTrayWnd`) olan
//! katmanlı (layered) bir açılır pencere: sahiplik onu görev çubuğunun hep üstünde tutar, konumu
//! UI Automation ile okunan o görev çubuğundaki `StartButton` sınırlarından hesaplanır. Sıra
//! soldan sağa [Mac] [iPhone] [Başlat]. UI Automation, Explorer'a eşzamanlı COM çağrısı yaptığı
//! için ayrı bir iş parçacığında çalışır: Explorer yavaşlarsa yansıtma ve girdi iletimi donmaz.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{w, BSTR, PCWSTR, VARIANT};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{DragAcceptFiles, HDROP};

use windows::Win32::UI::WindowsAndMessaging::*;

use crate::mirror::{self, WM_APP_PLACE, WM_APP_SHOW, WM_APP_TOGGLE};
use crate::transfer;
use crate::util;

const WM_MOUSELEAVE: u32 = 0x02A3;
const CLASS: PCWSTR = w!("iPhoneAyna.Ikon");
const TIMER_POSITION: usize = 1;
const MENU_TOGGLE: usize = 1;
const MENU_AUTOSTART: usize = 2;
const MENU_EXIT: usize = 3;
const MENU_HUB: usize = 4;
const MENU_DISCONNECT: usize = 5;

/// Cihazın hedef penceresine "bağlantıyı kes" isteği (Mac ikonunun menüsünden).
pub const WM_APP_DISCONNECT: u32 = WM_APP + 21;
/// Ana uygulama penceresini aç.
pub const WM_APP_HUB: u32 = WM_APP + 22;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Device {
    IPhone = 0,
    Mac = 1,
}

const DEVICES: [Device; 2] = [Device::IPhone, Device::Mac];

struct Icon {
    hwnd: HWND,
    device: Device,
    /// Sahibi olan görev çubuğu.
    tray: HWND,
    hover: bool,
    pressed: bool,
    bounds: RECT,
    shown: bool,
    drawn_key: Option<(i32, i32, bool, bool, bool, u8)>,
}

thread_local! {
    static ICONS: RefCell<Vec<Icon>> = const { RefCell::new(Vec::new()) };
    static LAST_SYNC: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Cihaz başına: tıklamaların gönderildiği pencere (iPhone: yansıtma, Mac: ana uygulama).
    static TARGETS: Cell<[isize; 2]> = const { Cell::new([0; 2]) };
    /// Cihaz başına: alt göstergenin izlediği pencere (açık mı, önde mi).
    static VIEWS: Cell<[isize; 2]> = const { Cell::new([0; 2]) };
    static ENABLED: Cell<[bool; 2]> = const { Cell::new([true, false]) };
    /// Cihaz başına: en son tıklanan ikonun görev çubuğu; pencere o monitörde açılır.
    static ANCHORS: Cell<[isize; 2]> = const { Cell::new([0; 2]) };
    static HUB: Cell<isize> = const { Cell::new(0) };
    /// İlk kurulum zamanı: Başlat düğmesi okunamazsa bu kadar sonra yedek konuma geçilir.
    static FIRST_SYNC: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Arka plan iş parçacığının en son okuduğu Başlat düğmesi sınırları, görev çubuğu başına
/// (fiziksel piksel).
static START_RECTS: Mutex<Vec<(isize, RECT)>> = Mutex::new(Vec::new());
static WATCHER: std::sync::Once = std::sync::Once::new();

/// iPhone ikonunun yansıtma penceresini kaydeder ve ikonları kurar.
pub fn ensure(mirror: HWND) {
    set_target(Device::IPhone, mirror);
    sync(false);
}

/// Cihaz ikonuna tıklanınca `WM_APP_TOGGLE`/`WM_APP_SHOW` alacak pencere.
pub fn set_target(device: Device, hwnd: HWND) {
    TARGETS.with(|t| {
        let mut a = t.get();
        a[device as usize] = hwnd.0 as isize;
        t.set(a);
    });
}

/// İkon altındaki göstergenin durumunu izlediği pencere (ör. Mac oturum penceresi).
pub fn set_view(device: Device, hwnd: HWND) {
    VIEWS.with(|t| {
        let mut a = t.get();
        a[device as usize] = hwnd.0 as isize;
        t.set(a);
    });
    refresh();
}

pub fn set_hub(hwnd: HWND) {
    HUB.with(|h| h.set(hwnd.0 as isize));
}

/// İkonu gösterir/gizler (iPhone ana uygulamada kapatıldı, Mac bağlandı ya da koptu).
pub fn set_enabled(device: Device, on: bool) {
    let changed = ENABLED.with(|e| {
        let mut a = e.get();
        let changed = a[device as usize] != on;
        a[device as usize] = on;
        e.set(a);
        changed
    });
    if changed {
        refresh();
    }
}

pub fn enabled(device: Device) -> bool {
    ENABLED.with(|e| e.get()[device as usize])
}

fn target(device: Device) -> HWND {
    HWND(TARGETS.with(|t| t.get()[device as usize]) as _)
}

fn view(device: Device) -> HWND {
    let v = VIEWS.with(|t| t.get()[device as usize]);
    if v != 0 {
        HWND(v as _)
    } else {
        target(device)
    }
}

/// Explorer yeniden başladığında ("TaskbarCreated") eski ikonlar artık sahipsizdir.
pub fn recreate(_mirror: HWND) {
    let old: Vec<HWND> = ICONS.with(|c| c.borrow_mut().drain(..).map(|i| i.hwnd).collect());
    for h in old {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
    sync(true);
}

/// Pencere açılıp kapandığında alt göstergeleri günceller.
pub fn refresh() {
    let hwnds: Vec<HWND> = ICONS.with(|c| c.try_borrow().map(|v| v.iter().map(|i| i.hwnd).collect()).unwrap_or_default());
    for h in hwnds {
        unsafe {
            let _ = PostMessageW(h, WM_TIMER, WPARAM(TIMER_POSITION), LPARAM(0));
        }
    }
}

/// Cihazın en son tıklanan ikonunun ekrandaki yeri (görünürse; yoksa ana görev çubuğundaki ya da
/// herhangi bir görünür ikonu). Cihaz penceresi bunun tam üstünde açılır.
pub fn icon_rect_for(device: Device) -> Option<RECT> {
    let anchor = ANCHORS.with(|a| a.get()[device as usize]);
    let primary = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).unwrap_or_default() };
    ICONS.with(|c| {
        let v = c.try_borrow().ok()?;
        let shown = |i: &&Icon| i.device == device && i.shown;
        v.iter()
            .filter(shown)
            .find(|i| i.tray.0 as isize == anchor)
            .or_else(|| v.iter().filter(shown).find(|i| i.tray == primary))
            .or_else(|| v.iter().find(shown))
            .map(|i| i.bounds)
    })
}

/// iPhone ikonunun yeri (yansıtma penceresi ve bildirimler bunu kullanır).
pub fn icon_rect() -> Option<RECT> {
    icon_rect_for(Device::IPhone)
}

/// Açık görev çubuklarını bulur; eksik ikonları kurar, görev çubuğu kapanmış (monitör çıkarıldı)
/// ikonları kaldırır. Saniyede bir kez; `force` beklemeden.
fn sync(force: bool) {
    let recent = LAST_SYNC.with(|t| t.get().is_some_and(|t| t.elapsed() < Duration::from_secs(1)));
    if recent && !force {
        return;
    }
    LAST_SYNC.with(|t| t.set(Some(Instant::now())));
    FIRST_SYNC.with(|t| {
        if t.get().is_none() {
            t.set(Some(Instant::now()));
        }
    });
    WATCHER.call_once(|| {
        std::thread::spawn(watch_start_buttons);
    });
    let trays = unsafe { taskbars() };
    let stale: Vec<HWND> = ICONS.with(|c| {
        let Ok(mut v) = c.try_borrow_mut() else { return Vec::new() };
        let mut stale = Vec::new();
        v.retain(|i| {
            let keep = trays.contains(&i.tray) && unsafe { IsWindow(i.hwnd).as_bool() };
            if !keep {
                stale.push(i.hwnd);
            }
            keep
        });
        stale
    });
    for h in stale {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
    for &tray in &trays {
        for device in DEVICES {
            let exists = ICONS.with(|c| {
                c.try_borrow().map(|v| v.iter().any(|i| i.tray == tray && i.device == device)).unwrap_or(true)
            });
            if !exists {
                unsafe { create(tray, device) };
            }
        }
    }
}

/// Ana görev çubuğu ve diğer monitörlerdeki görev çubukları.
pub(crate) unsafe fn taskbars() -> Vec<HWND> {
    let mut out: Vec<HWND> = Vec::new();
    if let Ok(primary) = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) {
        out.push(primary);
    }
    let mut after = HWND::default();
    while let Ok(h) = FindWindowExW(HWND::default(), after, w!("Shell_SecondaryTrayWnd"), PCWSTR::null()) {
        if h.0.is_null() {
            break;
        }
        out.push(h);
        after = h;
    }
    out
}

unsafe fn create(tray: HWND, device: Device) {
    let instance: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
        lpszClassName: CLASS,
        ..Default::default()
    };
    RegisterClassExW(&wc);

    let title = match device {
        Device::IPhone => w!("iPhone"),
        Device::Mac => w!("Mac"),
    };
    let Ok(hwnd) = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
        CLASS,
        title,
        WS_POPUP,
        0,
        0,
        1,
        1,
        tray,
        HMENU::default(),
        instance,
        None,
    ) else {
        util::log(&format!("görev çubuğu ikonu oluşturulamadı: {}", windows::core::Error::from_win32().message()));
        return;
    };

    ICONS.with(|c| {
        c.borrow_mut().push(Icon {
            hwnd,
            device,
            tray,
            hover: false,
            pressed: false,
            bounds: RECT::default(),
            shown: false,
            drawn_key: None,
        })
    });
    // iPhone ikonunun üzerine bırakılan dosyalar da iPhone'a gönderilir.
    if device == Device::IPhone {
        DragAcceptFiles(hwnd, true);
    }
    SetTimer(hwnd, TIMER_POSITION, 500, None);
    update(hwnd);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            update(hwnd);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let start_tracking = with(hwnd, |i| {
                let first = !i.hover;
                i.hover = true;
                first
            });
            if start_tracking == Some(true) {
                let mut t = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut t);
                update(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with(hwnd, |i| {
                i.hover = false;
                i.pressed = false;
            });
            update(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            with(hwnd, |i| i.pressed = true);
            update(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let clicked = with(hwnd, |i| {
                let was = i.pressed;
                i.pressed = false;
                was.then_some((i.device, i.tray))
            })
            .flatten();
            if let Some((device, tray)) = clicked {
                // Başka monitördeki ikona tıklandıysa açık pencere gizlenmez, o monitöre taşınır.
                let moved = set_anchor(device, tray);
                let t = target(device);
                let open = IsWindowVisible(view(device)).as_bool();
                let msg = if moved && open { WM_APP_SHOW } else { WM_APP_TOGGLE };
                let _ = PostMessageW(t, msg, WPARAM(0), LPARAM(0));
            }
            update(hwnd);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            // Mac ikonunda sağ tık boyut penceresini açar (menü onun "Ayna Merkezi" bağlantısında).
            match with(hwnd, |i| (i.device, i.tray)) {
                Some((Device::Mac, tray)) => {
                    set_anchor(Device::Mac, tray);
                    let _ = PostMessageW(target(Device::Mac), crate::macview::WM_APP_SIZER, WPARAM(0), LPARAM(0));
                }
                _ => show_menu(hwnd),
            }
            LRESULT(0)
        }
        WM_DROPFILES => {
            let paths = transfer::dropped_paths(HDROP(wp.0 as _));
            mirror::send_dropped(target(Device::IPhone), paths);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(hwnd, TIMER_POSITION);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Cihaz penceresinin açılacağı görev çubuğunu kaydeder; öncekinden farklıysa true. Henüz hiç
/// tıklanmadıysa ana görev çubuğu sayılır.
unsafe fn set_anchor(device: Device, tray: HWND) -> bool {
    let primary = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).unwrap_or_default();
    ANCHORS.with(|a| {
        let mut all = a.get();
        let previous = if all[device as usize] == 0 { primary.0 as isize } else { all[device as usize] };
        all[device as usize] = tray.0 as isize;
        a.set(all);
        previous != tray.0 as isize
    })
}

fn with<R>(hwnd: HWND, f: impl FnOnce(&mut Icon) -> R) -> Option<R> {
    ICONS.with(|c| c.try_borrow_mut().ok().and_then(|mut v| v.iter_mut().find(|i| i.hwnd == hwnd).map(f)))
}

unsafe fn show_menu(hwnd: HWND) {
    let Some((device, tray)) = with(hwnd, |i| (i.device, i.tray)) else { return };
    let Ok(menu) = CreatePopupMenu() else { return };
    let v = view(device);
    let open = IsWindowVisible(v).as_bool() && !IsIconic(v).as_bool();
    let label = match (device, open) {
        (Device::IPhone, true) => w!("iPhone ekranını gizle"),
        (Device::IPhone, false) => w!("iPhone ekranını aç"),
        (Device::Mac, true) => w!("Mac ekranını gizle"),
        (Device::Mac, false) => w!("Mac ekranını göster"),
    };
    let _ = AppendMenuW(menu, MF_STRING, MENU_TOGGLE, label);
    if device == Device::Mac {
        let _ = AppendMenuW(menu, MF_STRING, MENU_DISCONNECT, w!("Mac bağlantısını kes"));
    }
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, MENU_HUB, w!("Ayna Merkezi"));
    let auto = if util::autostart_enabled() { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, auto, MENU_AUTOSTART, w!("Windows ile başlat"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Çıkış"));

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        None,
    );
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);

    match cmd.0 as usize {
        MENU_TOGGLE => {
            set_anchor(device, tray);
            let _ = PostMessageW(target(device), WM_APP_TOGGLE, WPARAM(0), LPARAM(0));
        }
        MENU_DISCONNECT => {
            let _ = PostMessageW(target(device), WM_APP_DISCONNECT, WPARAM(0), LPARAM(0));
        }
        MENU_HUB => {
            let hub = HWND(HUB.with(|h| h.get()) as _);
            let _ = PostMessageW(hub, WM_APP_HUB, WPARAM(0), LPARAM(0));
        }
        MENU_AUTOSTART => util::set_autostart(!util::autostart_enabled()),
        MENU_EXIT => {
            let _ = DestroyWindow(target(Device::IPhone));
        }
        _ => {}
    }
}

/// Konumu yeniden hesaplar, gerekirse yeniden çizer.
unsafe fn update(hwnd: HWND) {
    let Some((device, tray)) = with(hwnd, |i| (i.device, i.tray)) else { return };
    if !IsWindow(tray).as_bool() {
        let _ = ShowWindow(hwnd, SW_HIDE);
        return;
    }
    if GetWindow(hwnd, GW_OWNER).unwrap_or_default() != tray {
        SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, tray.0 as isize);
    }

    let mut bar = RECT::default();
    let _ = GetWindowRect(tray, &mut bar);
    let start = START_RECTS.lock().unwrap().iter().find(|(t, _)| *t == tray.0 as isize).map(|(_, r)| *r);

    let scale = util::scale(tray);
    let w = (44.0 * scale).round() as i32;
    let h = util::height(&bar);
    // Mac, iPhone ikonunun solunda; iPhone kapalıysa onun yerinde.
    let slot = match device {
        Device::IPhone => 0,
        Device::Mac => i32::from(enabled(Device::IPhone)),
    };
    let count = DEVICES.iter().filter(|&&d| enabled(d)).count() as i32;
    let x = match start {
        // Görev çubuğu sola hizalı (Başlat'ın solunda bütün grup için yer yok): grup Başlat'ın
        // sağına, aynı sırayla.
        Some(s) if s.left - w * count < bar.left + 2 => s.right + w * (count - 1 - slot),
        Some(s) => s.left - w * (slot + 1),
        None => bar.left + w * slot,
    };
    let y = bar.top;

    // Başlat düğmesi henüz okunmadıysa ikon yanlış yerde görünüp sonra zıplamasın; okunamıyorsa
    // birkaç saniye sonra yedek konumda görünür.
    let located =
        start.is_some() || FIRST_SYNC.with(|t| t.get().is_some_and(|t| t.elapsed() > Duration::from_secs(8)));
    let visible = enabled(device)
        && located
        && IsWindowVisible(tray).as_bool()
        && taskbar_on_screen(&bar)
        && !fullscreen_app(&bar);
    let v = view(device);
    let indicator = if !IsWindowVisible(v).as_bool() || IsIconic(v).as_bool() {
        0
    } else if GetForegroundWindow() == v {
        2
    } else {
        1
    };

    let Some((hover, pressed, changed, moved)) = with(hwnd, |i| {
        let key = (x, y, i.hover, i.pressed, util::light_taskbar(), indicator);
        let moved = i.bounds != RECT { left: x, top: y, right: x + w, bottom: y + h };
        let changed = moved || i.drawn_key != Some(key) || i.shown != visible;
        i.drawn_key = Some(key);
        i.bounds = RECT { left: x, top: y, right: x + w, bottom: y + h };
        i.shown = visible;
        (i.hover, i.pressed, changed, moved)
    }) else {
        return;
    };
    // Orta hizalı görev çubuğu kaydıkça (uygulama açılıp kapandıkça) açık iPhone penceresi
    // ikonun üstünde kalsın.
    let anchored = ANCHORS.with(|a| a.get()[device as usize]) == tray.0 as isize
        || FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).unwrap_or_default() == tray;
    if device == Device::IPhone && moved && visible && anchored && IsWindowVisible(target(device)).as_bool() {
        let _ = PostMessageW(target(device), WM_APP_PLACE, WPARAM(0), LPARAM(0));
    }

    if !visible {
        let _ = ShowWindow(hwnd, SW_HIDE);
        return;
    }
    if changed && h > 0 {
        let pixels = render_taskbar_icon(device, w, h, scale, util::light_taskbar(), hover, pressed, indicator);
        present(hwnd, x, y, w, h, &pixels);
    }
    if !IsWindowVisible(hwnd).as_bool() {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    // Explorer görev çubuğunu yeniden en üste aldığında (ör. bir pencere ekranı kaplayınca)
    // başka sürece ait sahipli ikon her zaman onunla yükselmiyor ve altında kalıyor. İkonun
    // ortası başka pencereye denk geliyorsa yeniden en üste alınır.
    // Yalnız örten görev çubuğunun kendisiyse: kullanıcının oraya sürüklediği başka bir
    // her-zaman-üstte pencereyle (ekran klavyesi, resim içinde resim) z-sırası kavgası çıkmasın.
    let center = POINT { x: x + w / 2, y: y + h / 2 };
    let cover = GetAncestor(WindowFromPoint(center), GA_ROOT);
    if cover != hwnd && is_taskbar(cover) {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

unsafe fn is_taskbar(hwnd: HWND) -> bool {
    let mut class = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut class) as usize;
    matches!(String::from_utf16_lossy(&class[..n]).as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

/// Her görev çubuğundaki Başlat düğmesinin konumunu 400 ms'de bir okuyup `START_RECTS`'e yazar.
fn watch_start_buttons() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let uia: Option<IUIAutomation> = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok();
        let Some(uia) = uia else { return };
        let mut elements: HashMap<isize, IUIAutomationElement> = HashMap::new();
        let mut last_rect: HashMap<isize, RECT> = HashMap::new();
        // Görev çubuğu başına: sonraki arama zamanı ve art arda başarısız arama sayısı.
        let mut next_search: HashMap<isize, (Instant, u32)> = HashMap::new();
        loop {
            let trays = taskbars();
            let alive = |k: &isize| trays.iter().any(|t| t.0 as isize == *k);
            elements.retain(|k, _| alive(k));
            last_rect.retain(|k, _| alive(k));
            next_search.retain(|k, _| alive(k));
            let mut rects = Vec::new();
            for &tray in &trays {
                let key = tray.0 as isize;
                let mut rect =
                    elements.get(&key).and_then(|e| e.CurrentBoundingRectangle().ok()).filter(|r| util::width(r) > 0);
                if rect.is_none() {
                    elements.remove(&key);
                    // Tam ağaç taraması pahalı: hiç bulunamayan görev çubuğunda aralık 5 sn'den
                    // 60 sn'ye kadar uzar. Daha önce bulunmuş düğme (Explorer ağacı yeniledi) hemen
                    // yeniden aranır; bulunana dek son konum kullanılır, ikonlar zıplamaz.
                    let (due, fails) = next_search.get(&key).copied().unwrap_or((Instant::now(), 0));
                    if Instant::now() >= due || last_rect.contains_key(&key) {
                        match find_start_button(&uia, tray) {
                            Some(e) => {
                                rect = e.CurrentBoundingRectangle().ok().filter(|r| util::width(r) > 0);
                                elements.insert(key, e);
                                next_search.remove(&key);
                            }
                            None => {
                                let wait = Duration::from_secs((5u64 << fails.min(4)).min(60));
                                next_search.insert(key, (Instant::now() + wait, fails + 1));
                                last_rect.remove(&key);
                            }
                        }
                    }
                }
                if let Some(r) = rect.or_else(|| last_rect.get(&key).copied()) {
                    last_rect.insert(key, r);
                    rects.push((key, r));
                }
            }
            *START_RECTS.lock().unwrap() = rects;
            std::thread::sleep(Duration::from_millis(400));
        }
    }
}

unsafe fn find_start_button(uia: &IUIAutomation, tray: HWND) -> Option<IUIAutomationElement> {
    let root = uia.ElementFromHandle(tray).ok()?;
    let id = VARIANT::from(BSTR::from("StartButton"));
    let cond = uia.CreatePropertyCondition(UIA_AutomationIdPropertyId, &id).ok()?;
    root.FindFirst(TreeScope_Descendants, &cond).ok()
}

/// Otomatik gizlenen görev çubuğu kenara kaydığında ikon da gizlenir.
unsafe fn taskbar_on_screen(bar: &RECT) -> bool {
    let mon = MonitorFromRect(bar, MONITOR_DEFAULTTONEAREST);
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !GetMonitorInfoW(mon, &mut info).as_bool() {
        return true;
    }
    let m = info.rcMonitor;
    let visible = bar.bottom.min(m.bottom) - bar.top.max(m.top);
    visible as f32 >= util::height(bar) as f32 * 0.6
}

/// Görev çubuğunun bulunduğu monitörü ön plandaki bir pencere tamamen kaplıyorsa (oyun, video)
/// ikon da çekilir. Sistem geneli "meşgul" durumu kullanılmaz: başka monitördeki tam ekran
/// pencere de onu tetikliyor.
unsafe fn fullscreen_app(bar: &RECT) -> bool {
    let fg = GetForegroundWindow();
    if fg.0.is_null() {
        return false;
    }
    let mut class = [0u16; 64];
    let n = GetClassNameW(fg, &mut class) as usize;
    let class = String::from_utf16_lossy(&class[..n]);
    if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
        return false;
    }
    let mon = MonitorFromRect(bar, MONITOR_DEFAULTTONEAREST);
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let mut r = RECT::default();
    if !GetMonitorInfoW(mon, &mut info).as_bool() || GetWindowRect(fg, &mut r).is_err() {
        return false;
    }
    let m = info.rcMonitor;
    r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
}

pub(crate) unsafe fn present(hwnd: HWND, x: i32, y: i32, w: i32, h: i32, pixels: &[u32]) {
    let screen = GetDC(HWND::default());
    let mem = CreateCompatibleDC(screen);
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
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    if let Ok(bmp) = CreateDIBSection(mem, &info, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) {
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, pixels.len());
        let old = SelectObject(mem, bmp);
        let dst = POINT { x, y };
        let size = SIZE { cx: w, cy: h };
        let src = POINT::default();
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            screen,
            Some(&dst),
            Some(&size),
            mem,
            Some(&src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
    }
    let _ = DeleteDC(mem);
    ReleaseDC(HWND::default(), screen);
}

// ---------------------------------------------------------------------------------------------
// Çizim: işaretli uzaklık alanlarıyla (SDF) kenar yumuşatmalı küçük bir yazılım rasterleyici.

pub(crate) struct Canvas {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>, // önceden çarpılmış RGBA
}

impl Canvas {
    pub(crate) fn new(w: usize, h: usize) -> Canvas {
        Canvas { w, h, px: vec![[0.0; 4]; w * h] }
    }

    pub(crate) fn fill_background(&mut self, rgb: [f32; 3], a: f32) {
        for p in &mut self.px {
            *p = [rgb[0] * a, rgb[1] * a, rgb[2] * a, a];
        }
    }

    /// `coverage(x, y)` 0..1 döner; renk "üstüne" kipinde karıştırılır.
    pub(crate) fn draw(&mut self, rgb: [f32; 3], alpha: f32, coverage: impl Fn(f32, f32) -> f32) {
        for y in 0..self.h {
            for x in 0..self.w {
                let a = coverage(x as f32 + 0.5, y as f32 + 0.5) * alpha;
                if a <= 0.0 {
                    continue;
                }
                let p = &mut self.px[y * self.w + x];
                for c in 0..3 {
                    p[c] = rgb[c] * a + p[c] * (1.0 - a);
                }
                p[3] = a + p[3] * (1.0 - a);
            }
        }
    }

    /// `draw` gibi, ama yalnız (x0, y0)-(x1, y1) kutusunun içini tarar: büyük tuvalde küçük şekiller.
    pub(crate) fn draw_in(&mut self, bx: [f32; 4], rgb: [f32; 3], alpha: f32, coverage: impl Fn(f32, f32) -> f32) {
        let x0 = (bx[0].floor().max(0.0) as usize).min(self.w);
        let y0 = (bx[1].floor().max(0.0) as usize).min(self.h);
        let x1 = (bx[2].ceil().max(0.0) as usize).min(self.w);
        let y1 = (bx[3].ceil().max(0.0) as usize).min(self.h);
        for y in y0..y1 {
            for x in x0..x1 {
                let a = coverage(x as f32 + 0.5, y as f32 + 0.5) * alpha;
                if a <= 0.0 {
                    continue;
                }
                let p = &mut self.px[y * self.w + x];
                for c in 0..3 {
                    p[c] = rgb[c] * a + p[c] * (1.0 - a);
                }
                p[3] = a + p[3] * (1.0 - a);
            }
        }
    }

    pub(crate) fn to_premultiplied_bgra(&self) -> Vec<u32> {
        self.px
            .iter()
            .map(|p| {
                let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
                b(p[2]) | b(p[1]) << 8 | b(p[0]) << 16 | b(p[3]) << 24
            })
            .collect()
    }

    fn to_straight_bgra(&self) -> Vec<u32> {
        self.px
            .iter()
            .map(|p| {
                let a = p[3];
                let un = |v: f32| if a > 0.0 { v / a } else { 0.0 };
                let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
                b(un(p[2])) | b(un(p[1])) << 8 | b(un(p[0])) << 16 | b(a) << 24
            })
            .collect()
    }
}

pub(crate) fn round_rect_sdf(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - hw + r;
    let qy = (py - cy).abs() - hh + r;
    let (ox, oy) = (qx.max(0.0), qy.max(0.0));
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

pub(crate) fn fill(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

pub(crate) fn stroke(d: f32, width: f32) -> f32 {
    (0.5 - (d.abs() - width / 2.0)).clamp(0.0, 1.0)
}

pub(crate) fn hex(v: u32) -> [f32; 3] {
    [((v >> 16) & 0xFF) as f32 / 255.0, ((v >> 8) & 0xFF) as f32 / 255.0, (v & 0xFF) as f32 / 255.0]
}

/// Telefon gövdesi (çerçeve + dinamik ada), (cx, cy) merkezli, `s` ölçekli.
pub(crate) fn draw_phone(c: &mut Canvas, cx: f32, cy: f32, s: f32, rgb: [f32; 3]) {
    let (hw, hh, r, sw) = (6.0 * s, 10.0 * s, 2.6 * s, 1.4 * s);
    c.draw(rgb, 1.0, |x, y| stroke(round_rect_sdf(x, y, cx, cy, hw - sw / 2.0, hh - sw / 2.0, r), sw));
    let iy = cy - hh + 3.0 * s;
    c.draw(rgb, 1.0, |x, y| fill(round_rect_sdf(x, y, cx, iy, 1.9 * s, 0.65 * s, 0.65 * s)));
}

/// Mac ekranı: monitör çerçevesi ve ayak, (cx, cy) merkezli, `s` ölçekli.
pub(crate) fn draw_mac(c: &mut Canvas, cx: f32, cy: f32, s: f32, rgb: [f32; 3]) {
    let (hw, hh, r, sw) = (9.0 * s, 6.2 * s, 1.6 * s, 1.4 * s);
    let my = cy - 1.6 * s;
    c.draw(rgb, 1.0, |x, y| stroke(round_rect_sdf(x, y, cx, my, hw - sw / 2.0, hh - sw / 2.0, r), sw));
    let neck = my + hh + 1.4 * s;
    c.draw(rgb, 1.0, |x, y| fill(round_rect_sdf(x, y, cx, neck, 1.1 * s, 1.4 * s, 0.3 * s)));
    let foot = neck + 1.9 * s;
    c.draw(rgb, 1.0, |x, y| fill(round_rect_sdf(x, y, cx, foot, 4.2 * s, 0.7 * s, 0.7 * s)));
}

#[allow(clippy::too_many_arguments)]
fn render_taskbar_icon(
    device: Device,
    w: i32,
    h: i32,
    s: f32,
    light: bool,
    hover: bool,
    pressed: bool,
    indicator: u8,
) -> Vec<u32> {
    let (w, h) = (w.max(1) as usize, h.max(1) as usize);
    let mut c = Canvas::new(w, h);
    // Alfa 0 olan pikseller tıklamayı alttaki pencereye geçirir; neredeyse görünmez bir zemin
    // ikonun tüm alanını tıklanabilir yapar.
    c.fill_background([1.0, 1.0, 1.0], 1.0 / 255.0);

    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let box_hw = (w as f32 / 2.0 - 2.0 * s).min(20.0 * s);
    let box_hh = (h as f32 / 2.0 - 4.0 * s).min(20.0 * s);
    if hover {
        let (rgb, a) = if light { ([0.0, 0.0, 0.0], 0.05) } else { ([1.0, 1.0, 1.0], 0.085) };
        let a = if pressed { a * 0.6 } else { a };
        c.draw(rgb, a, |x, y| fill(round_rect_sdf(x, y, cx, cy, box_hw, box_hh, 4.0 * s)));
    }

    let fg = if light { hex(0x1A1A1A) } else { hex(0xFFFFFF) };
    let glyph_s = if pressed { s * 0.92 } else { s };
    match device {
        Device::IPhone => draw_phone(&mut c, cx, cy - 1.0 * s, glyph_s, fg),
        Device::Mac => draw_mac(&mut c, cx, cy - 1.0 * s, glyph_s, fg),
    }

    if indicator > 0 {
        let (half, rgb) = if indicator == 2 {
            (8.0 * s, if light { hex(0x005FB8) } else { hex(0x4CC2FF) })
        } else {
            (3.0 * s, if light { hex(0x616161) } else { hex(0x9A9A9A) })
        };
        let iy = cy + box_hh - 1.5 * s;
        c.draw(rgb, 1.0, |x, y| fill(round_rect_sdf(x, y, cx, iy, half, 1.5 * s, 1.5 * s)));
    }
    c.to_premultiplied_bgra()
}

/// Pencere başlığı ve Alt+Tab için mavi zeminli uygulama ikonu.
pub fn app_icon(size: i32) -> HICON {
    let n = size as usize;
    let mut c = Canvas::new(n, n);
    let s = size as f32 / 32.0;
    let half = n as f32 / 2.0;
    c.draw(hex(0x0A84FF), 1.0, |x, y| fill(round_rect_sdf(x, y, half, half, half - 0.5, half - 0.5, 7.0 * s)));
    draw_phone(&mut c, half, half, s * 1.15, [1.0, 1.0, 1.0]);
    let pixels = c.to_straight_bgra();

    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(HDC::default(), &info, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) else {
            return HICON::default();
        };
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, pixels.len());
        let mask = CreateBitmap(size, size, 1, 1, None);
        let ii = ICONINFO { fIcon: TRUE, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&ii).unwrap_or_default();
        let _ = DeleteObject(color);
        let _ = DeleteObject(mask);
        icon
    }
}
