//! Mac görüntüsünün çözülüp pencereye çizilmesi: UxPlay paketindeki GStreamer kütüphaneleri çalışma
//! anında yüklenir (`bin\uxplay`). Hat: appsrc (H.264 Annex-B, erişim birimi başına) → h264parse →
//! d3d11h264dec (donanım çözücü) → d3d11videosink (verilen pencerenin içine çizer).
//!
//! Gecikme: çözücü `compliance=flexible` ile kareyi DPB'de bekletmeden hemen verir (akışta B-kare
//! yok; Mac ekranı yalnız değişince kare gönderdiği için bekletilen kare bir sonraki değişikliğe
//! dek ekrana çıkmazdı), çizici saate göre beklemez (`sync=false`).

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::OnceLock;

use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH};

use crate::util;

type Ptr = *mut c_void;

#[allow(clippy::type_complexity)]
struct Api {
    parse_launch: unsafe extern "C" fn(*const c_char, *mut Ptr) -> Ptr,
    bin_get_by_name: unsafe extern "C" fn(Ptr, *const c_char) -> Ptr,
    set_state: unsafe extern "C" fn(Ptr, c_int) -> c_int,
    unref: unsafe extern "C" fn(Ptr),
    buffer_new_allocate: unsafe extern "C" fn(Ptr, usize, Ptr) -> Ptr,
    buffer_fill: unsafe extern "C" fn(Ptr, usize, *const c_void, usize) -> usize,
    push_buffer: unsafe extern "C" fn(Ptr, Ptr) -> c_int,
    set_window_handle: unsafe extern "C" fn(Ptr, usize),
}

// İşlev işaretçileri iş parçacığından bağımsızdır.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

static API: OnceLock<Option<Api>> = OnceLock::new();

const GST_STATE_NULL: c_int = 1;
const GST_STATE_PLAYING: c_int = 4;
const GST_FLOW_OK: c_int = 0;

fn api() -> Option<&'static Api> {
    API.get_or_init(|| unsafe { load() }).as_ref()
}

/// GStreamer yüklenebiliyor mu (Mac görüntüsü gösterilebilir mi)?
pub fn available() -> bool {
    api().is_some()
}

unsafe fn load() -> Option<Api> {
    let dir = crate::uxplay::short_path(&util::find_uxplay_dir()?);
    let dir_s = dir.to_string_lossy().to_string();
    // Eklentiler bağımlı DLL'leri PATH'te arar; UxPlay'in kendi kaydı bozulmasın diye ayrı kayıt.
    let path = std::env::var("PATH").unwrap_or_default();
    if !path.split(';').any(|p| p.eq_ignore_ascii_case(&dir_s)) {
        std::env::set_var("PATH", format!("{dir_s};{path}"));
    }
    std::env::set_var("GST_PLUGIN_SYSTEM_PATH_1_0", dir.join("lib").join("gstreamer-1.0"));
    std::env::set_var("GST_PLUGIN_PATH_1_0", "");
    std::env::set_var("GST_REGISTRY_1_0", std::env::temp_dir().join("iPhoneAyna-gst.bin"));

    let open = |name: &str| -> Option<HMODULE> {
        let p = util::wide(&dir.join(name).to_string_lossy());
        LoadLibraryExW(PCWSTR(p.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH).ok()
    };
    let (core, app, video) = (open("libgstreamer-1.0-0.dll")?, open("libgstapp-1.0-0.dll")?, open("libgstvideo-1.0-0.dll")?);
    let sym = |m: HMODULE, name: &str| -> Option<unsafe extern "system" fn() -> isize> {
        let c = CString::new(name).ok()?;
        GetProcAddress(m, PCSTR(c.as_ptr() as *const u8))
    };
    macro_rules! f {
        ($m:expr, $name:expr) => {
            std::mem::transmute(sym($m, $name)?)
        };
    }
    let init: unsafe extern "C" fn(*mut c_int, *mut c_void) = f!(core, "gst_init");
    let api = Api {
        parse_launch: f!(core, "gst_parse_launch"),
        bin_get_by_name: f!(core, "gst_bin_get_by_name"),
        set_state: f!(core, "gst_element_set_state"),
        unref: f!(core, "gst_object_unref"),
        buffer_new_allocate: f!(core, "gst_buffer_new_allocate"),
        buffer_fill: f!(core, "gst_buffer_fill"),
        push_buffer: f!(app, "gst_app_src_push_buffer"),
        set_window_handle: f!(video, "gst_video_overlay_set_window_handle"),
    };
    init(std::ptr::null_mut(), std::ptr::null_mut());
    // Kütüphaneler süreç boyunca açık kalır (GStreamer kapanışta boşaltılmayı desteklemiyor).
    Some(api)
}

/// Bir pencereye çizen H.264 oynatıcısı.
pub struct Player {
    pipeline: Ptr,
    src: Ptr,
}

// GStreamer nesneleri iş parçacığı güvenli; ağ iş parçacığı kareleri doğrudan iter.
unsafe impl Send for Player {}
unsafe impl Sync for Player {}

impl Player {
    pub fn new(hwnd: HWND) -> Option<Player> {
        let api = api()?;
        let desc = CString::new(
            "appsrc name=src is-live=true do-timestamp=true format=time \
             caps=video/x-h264,stream-format=byte-stream,alignment=au \
             ! h264parse ! d3d11h264dec compliance=flexible ! d3d11videosink name=sink sync=false",
        )
        .ok()?;
        unsafe {
            let mut err: Ptr = std::ptr::null_mut();
            let pipeline = (api.parse_launch)(desc.as_ptr(), &mut err);
            if pipeline.is_null() {
                util::log("Mac görüntü hattı kurulamadı (GStreamer).");
                return None;
            }
            let name = |s: &str| CString::new(s).unwrap();
            let sink = (api.bin_get_by_name)(pipeline, name("sink").as_ptr());
            let src = (api.bin_get_by_name)(pipeline, name("src").as_ptr());
            if sink.is_null() || src.is_null() {
                (api.unref)(pipeline);
                return None;
            }
            (api.set_window_handle)(sink, hwnd.0 as usize);
            (api.unref)(sink);
            (api.set_state)(pipeline, GST_STATE_PLAYING);
            Some(Player { pipeline, src })
        }
    }

    /// Bir erişim birimi (Annex-B) iter.
    pub fn push(&self, data: &[u8]) -> bool {
        let Some(api) = api() else { return false };
        unsafe {
            let buf = (api.buffer_new_allocate)(std::ptr::null_mut(), data.len(), std::ptr::null_mut());
            if buf.is_null() {
                return false;
            }
            (api.buffer_fill)(buf, 0, data.as_ptr() as *const c_void, data.len());
            // Tampon sahipliği appsrc'ye geçer.
            (api.push_buffer)(self.src, buf) == GST_FLOW_OK
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        if let Some(api) = api() {
            unsafe {
                (api.set_state)(self.pipeline, GST_STATE_NULL);
                (api.unref)(self.src);
                (api.unref)(self.pipeline);
            }
        }
    }
}
