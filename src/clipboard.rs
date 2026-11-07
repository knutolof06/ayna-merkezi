//! Mac ile Windows arasında pano: metin ve resim, iki yönde, bağlıyken kopyalanır kopyalanmaz.
//!
//! Resim iki tarafta da BMP dosyası olarak taşınır: Windows panosunun DIB'i ile arasında yalnız 14
//! baytlık dosya başlığı fark eder, macOS da BMP'yi doğrudan açar. Dosyalar (Gezgin/Finder'da kopyala)
//! taşınmaz, onlar için sürükle-bırak var. Parola yöneticilerinin "izleme / geçmişe alma" diye
//! işaretlediği içerik gönderilmez.

use std::hash::{Hash, Hasher};
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
    SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};

/// Ayar: Mac ile pano paylaşımı (varsayılan açık).
const SETTING: PCWSTR = w!("PanoPaylas");

pub fn enabled() -> bool {
    crate::util::read_setting(SETTING) != Some(0)
}

pub fn set_enabled(on: bool) {
    crate::util::write_setting(SETTING, u32::from(on));
}

pub const KIND_TEXT: u8 = 1;
pub const KIND_BMP: u8 = 2;
const MAX_TEXT: usize = 4 << 20;
/// Mac'ten gelen resim tek iletidir (Windows çerçeve sınırı 16 MB); Mac'e giden parçalıdır, 4K ekran
/// görüntüsü (32 bit ~33 MB) de sığar.
const MAX_IMAGE_IN: usize = 15 << 20;
const MAX_IMAGE_OUT: usize = 40 << 20;

const CF_DIB: u32 = 8;
const CF_HDROP: u32 = 15;
const CF_UNICODETEXT: u32 = 13;
const CF_DIBV5: u32 = 17;

#[derive(Clone, PartialEq, Debug)]
pub enum Clip {
    Text(String),
    /// BMP dosyası (dosya başlığıyla).
    Image(Vec<u8>),
}

impl Clip {
    pub fn kind(&self) -> u8 {
        match self {
            Clip::Text(_) => KIND_TEXT,
            Clip::Image(_) => KIND_BMP,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        match self {
            Clip::Text(s) => s.as_bytes(),
            Clip::Image(b) => b,
        }
    }

    /// Mac'ten gelen içerik (sınırları aşan ya da bozuk olan alınmaz). Metin "\n" satır sonuyla taşınır.
    pub fn decode(kind: u8, data: Vec<u8>) -> Option<Clip> {
        match kind {
            KIND_TEXT if data.len() <= MAX_TEXT => String::from_utf8(data).ok().map(Clip::Text),
            KIND_BMP if data.len() <= MAX_IMAGE_IN && dib_from_bmp(&data).is_some() => Some(Clip::Image(data)),
            _ => None,
        }
    }

    /// Aynı içerik bir daha gönderilmesin / geri yansımasın diye.
    pub fn digest(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.kind().hash(&mut h);
        self.bytes().hash(&mut h);
        h.finish()
    }
}

/// Kabul edilen DIB: tek düzlem, olağan bit derinliği, sıkıştırmasız ya da maskeli, makul boyut ve
/// pikseller veriye sığıyor. Karşı taraftan gelen bozuk bir resim panoyu okuyan uygulamalara ulaşmasın.
/// Piksellerin DIB içindeki yerini verir.
fn valid_dib(dib: &[u8]) -> Option<usize> {
    let u32_at = |i: usize| dib.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let u16_at = |i: usize| dib.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let header = u32_at(0)? as usize;
    if !(40..=124).contains(&header) {
        return None;
    }
    let (width, height) = (u32_at(4)? as i32, u32_at(8)? as i32);
    let (planes, bits) = (u16_at(12)?, u16_at(14)? as u64);
    let (compression, used) = (u32_at(16)?, u32_at(32)? as usize);
    if planes != 1 || ![1, 4, 8, 16, 24, 32].contains(&bits) || !(compression == 0 || (compression == 3 && bits >= 16)) {
        return None;
    }
    if width <= 0 || width > 16384 || height == 0 || height.unsigned_abs() > 16384 {
        return None;
    }
    if used > if bits <= 8 { 1 << bits } else { 256 } {
        return None;
    }
    // Renk tablosu ve (eski başlıkta) BI_BITFIELDS maskeleri piksellerden önce gelir.
    let colors = if used > 0 { used } else if bits <= 8 { 1 << bits } else { 0 };
    let masks = if header == 40 && compression == 3 { 12 } else { 0 };
    let offset = header + masks + colors * 4;
    let stride = (width as u64 * bits).div_ceil(32) * 4;
    let need = offset as u64 + stride * height.unsigned_abs() as u64;
    (need <= dib.len() as u64).then_some(offset)
}

/// Windows DIB'ine (BITMAPINFOHEADER ve sonrası) 14 baytlık BMP dosya başlığı ekler.
pub fn bmp_from_dib(dib: &[u8]) -> Option<Vec<u8>> {
    let offset = 14 + valid_dib(dib)?;
    let size = 14 + dib.len();
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(offset as u32).to_le_bytes());
    out.extend_from_slice(dib);
    Some(out)
}

/// BMP dosyasından Windows DIB'i (dosya başlığı atılır) ve pano biçimi. Yalnız CF_DIB'in (40 bayt) ve
/// CF_DIBV5'in (124 bayt) başlıkları kabul edilir; pikseller başlığın hemen ardından başlamalı.
pub fn dib_from_bmp(bmp: &[u8]) -> Option<(&[u8], u32)> {
    if bmp.get(0..2)? != b"BM" {
        return None;
    }
    let dib = bmp.get(14..)?;
    let offset = valid_dib(dib)?;
    if u32::from_le_bytes(bmp.get(10..14)?.try_into().ok()?) as usize != 14 + offset {
        return None;
    }
    match u32::from_le_bytes(dib.get(0..4)?.try_into().ok()?) {
        40 => Some((dib, CF_DIB)),
        124 => Some((dib, CF_DIBV5)),
        _ => None,
    }
}

/// 32 bit, sıkıştırmasız DIB'de alfa çoğu uygulamada boş (0) bırakılır; Mac'te resim saydam görünmesin.
fn make_opaque(dib: &mut [u8]) {
    let Some(offset) = valid_dib(dib) else { return };
    let bits = u16::from_le_bytes([dib[14], dib[15]]);
    let compression = u32::from_le_bytes([dib[16], dib[17], dib[18], dib[19]]);
    if bits != 32 || compression != 0 {
        return;
    }
    for px in dib[offset..].chunks_exact_mut(4) {
        px[3] = 0xFF;
    }
}

/// Windows satır sonu ("\r\n") ile Mac'inki ("\n") arasında.
fn to_mac_newlines(s: &str) -> String {
    s.replace("\r\n", "\n")
}

fn to_windows_newlines(s: &str) -> String {
    to_mac_newlines(s).replace('\n', "\r\n")
}

unsafe fn open(owner: HWND) -> bool {
    // Başka bir uygulama panoyu kısa süre açık tutabilir.
    for _ in 0..10 {
        if OpenClipboard(owner).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

unsafe fn available(format: u32) -> bool {
    format != 0 && IsClipboardFormatAvailable(format).is_ok()
}

/// Parola yöneticileri ve benzerleri panoyu izleyen programlara görünmemesini istedi mi?
unsafe fn sensitive() -> bool {
    if available(RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")))
        || available(RegisterClipboardFormatW(w!("Clipboard Viewer Ignore")))
    {
        return true;
    }
    let history = RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory"));
    available(history) && read_global(history).is_some_and(|d| d.len() >= 4 && d[..4] == [0, 0, 0, 0])
}

unsafe fn read_global(format: u32) -> Option<Vec<u8>> {
    let data = GetClipboardData(format).ok()?;
    let global = HGLOBAL(data.0);
    let size = GlobalSize(global);
    let ptr = GlobalLock(global) as *const u8;
    if ptr.is_null() {
        return None;
    }
    let out = std::slice::from_raw_parts(ptr, size).to_vec();
    let _ = GlobalUnlock(global);
    Some(out)
}

pub enum Read {
    Clip(Clip),
    /// Pano başka bir uygulamada açık: biraz sonra yeniden denenir.
    Busy,
    /// Gönderilecek bir şey yok (dosya, gizli içerik, desteklenmeyen biçim).
    Nothing,
}

/// Panodaki metin ya da resim.
pub fn read() -> Read {
    unsafe {
        if !open(HWND::default()) {
            return Read::Busy;
        }
        let clip = (|| {
            if sensitive() || available(CF_HDROP) {
                return None;
            }
            if available(CF_UNICODETEXT) {
                let raw = read_global(CF_UNICODETEXT)?;
                let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
                let text = to_mac_newlines(&String::from_utf16_lossy(&units));
                return (!text.is_empty() && text.len() <= MAX_TEXT).then_some(Clip::Text(text));
            }
            if available(CF_DIB) {
                let mut dib = read_global(CF_DIB)?;
                make_opaque(&mut dib);
                let bmp = bmp_from_dib(&dib)?;
                return (bmp.len() <= MAX_IMAGE_OUT).then_some(Clip::Image(bmp));
            }
            None
        })();
        let _ = CloseClipboard();
        clip.map_or(Read::Nothing, Read::Clip)
    }
}

/// Mac'ten gelen içeriği panoya yazar. Yazılan değişikliğin sıra numarasını verir (pano açıkken
/// okunur: arada başka bir uygulamanın kopyası bizimki sanılmasın).
pub fn write(owner: HWND, clip: &Clip) -> Option<u32> {
    let (format, bytes): (u32, Vec<u8>) = match clip {
        Clip::Text(s) => (
            CF_UNICODETEXT,
            to_windows_newlines(s).encode_utf16().chain(std::iter::once(0)).flat_map(|u| u.to_le_bytes()).collect(),
        ),
        Clip::Image(bmp) => match dib_from_bmp(bmp) {
            Some((dib, format)) => (format, dib.to_vec()),
            None => return None,
        },
    };
    unsafe {
        if !open(owner) {
            return None;
        }
        let ok = (|| {
            EmptyClipboard().ok()?;
            let global = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).ok()?;
            let ptr = GlobalLock(global) as *mut u8;
            if ptr.is_null() {
                let _ = GlobalFree(global);
                return None;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            let _ = GlobalUnlock(global);
            // Başarılı olursa belleğin sahibi sistemdir.
            if SetClipboardData(format, HANDLE(global.0)).is_err() {
                let _ = GlobalFree(global);
                return None;
            }
            Some(windows::Win32::System::DataExchange::GetClipboardSequenceNumber())
        })();
        let _ = CloseClipboard();
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dib(header: u32, bits: u16, compression: u32, extra: usize, pixels: usize) -> Vec<u8> {
        let mut d = vec![0u8; header as usize + extra + pixels];
        d[0..4].copy_from_slice(&header.to_le_bytes());
        d[4..8].copy_from_slice(&2i32.to_le_bytes());
        d[8..12].copy_from_slice(&2i32.to_le_bytes());
        d[12..14].copy_from_slice(&1u16.to_le_bytes());
        d[14..16].copy_from_slice(&bits.to_le_bytes());
        d[16..20].copy_from_slice(&compression.to_le_bytes());
        d
    }

    #[test]
    fn bmp_round_trip_keeps_the_dib() {
        let d = dib(40, 32, 0, 0, 16);
        let bmp = bmp_from_dib(&d).unwrap();
        assert_eq!(&bmp[0..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()) as usize, bmp.len());
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 54);
        let (back, format) = dib_from_bmp(&bmp).unwrap();
        assert_eq!(back, &d[..]);
        assert_eq!(format, CF_DIB);
    }

    #[test]
    fn pixel_offset_skips_masks_and_palette() {
        // BI_BITFIELDS, eski başlık: üç maske.
        let bmp = bmp_from_dib(&dib(40, 32, 3, 12, 16)).unwrap();
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 14 + 40 + 12);
        // V4 başlığı panoya yazılmaz (CF_DIB 40, CF_DIBV5 124 bayt ister).
        assert!(dib_from_bmp(&bmp_from_dib(&dib(108, 32, 3, 0, 16)).unwrap()).is_none());
        // 8 bit: 256 renklik tablo (2 piksellik satır 4 bayta yuvarlanır).
        let bmp = bmp_from_dib(&dib(40, 8, 0, 1024, 8)).unwrap();
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 14 + 40 + 1024);
        // V5 başlığı CF_DIBV5 olarak yazılır.
        let bmp = bmp_from_dib(&dib(124, 32, 3, 0, 16)).unwrap();
        assert_eq!(dib_from_bmp(&bmp).unwrap().1, CF_DIBV5);
    }

    #[test]
    fn rejects_malformed_images() {
        // Pikseller veriye sığmıyor.
        assert!(bmp_from_dib(&dib(40, 32, 0, 0, 8)).is_none());
        // RLE / JPEG / PNG sıkıştırması ya da olağan dışı bit derinliği.
        assert!(bmp_from_dib(&dib(40, 8, 1, 1024, 8)).is_none());
        assert!(bmp_from_dib(&dib(40, 32, 4, 0, 16)).is_none());
        assert!(bmp_from_dib(&dib(40, 7, 0, 0, 16)).is_none());
        // Dev boyut.
        let mut huge = dib(40, 32, 0, 0, 16);
        huge[4..8].copy_from_slice(&100_000i32.to_le_bytes());
        assert!(bmp_from_dib(&huge).is_none());
        // Bozuk BMP dosyası Windows panosuna yazılmaz.
        let mut bmp = bmp_from_dib(&dib(40, 32, 0, 0, 16)).unwrap();
        bmp.truncate(bmp.len() - 4);
        assert!(dib_from_bmp(&bmp).is_none());
    }

    #[test]
    fn newlines_and_alpha() {
        assert_eq!(to_mac_newlines("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(to_windows_newlines("a\nb\r\nc"), "a\r\nb\r\nc");
        let mut d = dib(40, 32, 0, 0, 16);
        make_opaque(&mut d);
        assert!(d[40..].chunks(4).all(|p| p[3] == 0xFF));
        // Dosya başlığındaki piksel yeri tutmazsa reddedilir.
        let mut bmp = bmp_from_dib(&dib(40, 32, 0, 0, 16)).unwrap();
        bmp[10] += 4;
        assert!(dib_from_bmp(&bmp).is_none());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(bmp_from_dib(&[1, 2, 3]).is_none());
        assert!(dib_from_bmp(b"PNG").is_none());
        assert!(Clip::decode(KIND_TEXT, vec![0xFF, 0xFE]).is_none());
        assert!(Clip::decode(9, b"x".to_vec()).is_none());
        assert_eq!(Clip::decode(KIND_TEXT, "çay".as_bytes().to_vec()), Some(Clip::Text("çay".into())));
    }
}
