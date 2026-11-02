//! HID dokunmatik ekran (digitizer) rapor tanımı ve rapor üreticisi.
//!
//! Descriptor, sakx7/iOSBleDigitizer (MIT) `windows/Program.cs` ile aynıdır: iPhone bu cihazı
//! fare değil parmak dokunuşu olarak işler, AssistiveTouch ve imleç gerekmez. iOS dokunuşları
//! yalnız Erişilebilirlik > Büyütme açıkken alır (Büyütme'nin olay süzgeci digitizer'ı geçirir).
//!
//! Dokunmatik ekranın ardına ayrı bir uygulama koleksiyonu olarak klavye eklenir (rapor 3):
//! PC klavyesiyle yazmak ve Windows panosunu iPhone'a "yazarak" yapıştırmak için.

pub const REPORT_TOUCH: u8 = 1;
pub const REPORT_FEATURE: u8 = 2;
pub const REPORT_KEYBOARD: u8 = 3;
pub const REPORT_CONSUMER: u8 = 4;

/// Dokunma koordinatlarının mantıksal aralığı (x ve y için 0..=TOUCH_MAX).
pub const TOUCH_MAX: u16 = 10000;

/// Rapordaki temas yuvası sayısı. Kaynak projede ayarlanabilir (varsayılan 10); burada 5: tek parmak
/// kullanılıyor ve klavyeyle birlikte rapor tanımı GATT öznitelik sınırı olan 512 baytın altında kalmalı.
pub const CONTACT_SLOTS: usize = 5;
const CONTACT_BYTES: usize = 6;
pub const TOUCH_REPORT_LEN: usize = CONTACT_SLOTS * CONTACT_BYTES + 1;

/// bcdHID 1.11, ülke kodu 0, bayraklar = RemoteWake.
pub const HID_INFORMATION: [u8; 4] = [0x11, 0x01, 0x00, 0x01];

#[rustfmt::skip]
const HEADER: [u8; 24] = [
    0x05, 0x0D,                         // Usage Page (Digitizer)
    0x09, 0x04,                         // Usage (Touch Screen)
    0xA1, 0x01,                         // Collection (Application)
    0x85, REPORT_TOUCH,                 // Report ID (1)
    0x15, 0x00, 0x25, 0x01,             // Logical Min 0, Max 1
    0x35, 0x00, 0x45, 0x00,             // Physical Min 0, Max 0
    0x55, 0x00, 0x65, 0x00,             // Unit Exponent 0, Unit None
    0x75, 0x01, 0x95, 0x02,             // Report Size 1, Count 2
];

// Confidence şart: onsuz iOS parmağı "el" sayar ve kenardan kaydırma jestleri (ana ekran,
// Denetim Merkezi) çalışmaz.
#[rustfmt::skip]
const CONTACT: [u8; 47] = [
    0xA4,                               // Push
    0x09, 0x22, 0xA1, 0x02,             // Finger, Logical Collection
    0x09, 0x42, 0x09, 0x47, 0x81, 0x02, // Tip Switch + Confidence, Input
    0x95, 0x06, 0x81, 0x03,             // 6 bit dolgu
    0x09, 0x51, 0x25, 0x7F,             // Contact ID, Max 127
    0x75, 0x08, 0x95, 0x01, 0x81, 0x02, // 8 bit, Input
    0x05, 0x01, 0x09, 0x01, 0xA1, 0x00, // Generic Desktop, Pointer, Physical Collection
    0x09, 0x30, 0x09, 0x31,             // X, Y
    0x26, 0x10, 0x27,                   // Logical Max 10000
    0x75, 0x10, 0x95, 0x02, 0x81, 0x02, // 16 bit x 2, Input
    0xC0,                               // End Pointer
    0xB4, 0xC0,                         // Pop, End Finger
];

#[rustfmt::skip]
const TAIL: [u8; 17] = [
    0x09, 0x54,                         // Usage (Contact Count)
    0x25, CONTACT_SLOTS as u8,          // Logical Max
    0x75, 0x08, 0x95, 0x01, 0x81, 0x02, // 8 bit, Input
    0x85, REPORT_FEATURE,               // Report ID (2)
    0x09, 0x55,                         // Usage (Contact Count Maximum)
    0xB1, 0x02,                         // Feature
    0xC0,                               // End Application
];

/// Klavye: değiştirici baytı, ayrılmış bayt, 6 tuş (esp32-iphone-agent'ın iPhone'da çalışan
/// klavye koleksiyonu; LED çıkışı yok).
#[rustfmt::skip]
const KEYBOARD: [u8; 49] = [
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, REPORT_KEYBOARD,
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02,
    0x95, 0x01, 0x75, 0x08, 0x81, 0x01,
    0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x05, 0x07, 0x19, 0x00, 0x2A, 0xFF, 0x00, 0x81, 0x00,
    0xC0,
];

pub const KEYBOARD_REPORT_LEN: usize = 8;

/// Tüketici kontrolü (medya tuşları): tek 16 bitlik alanda usage (0..0x3FF). iPhone'un ses ve
/// güç tuşları.
#[rustfmt::skip]
const CONSUMER: [u8; 25] = [
    0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x85, REPORT_CONSUMER,
    0x15, 0x00, 0x26, 0xFF, 0x03, 0x19, 0x00, 0x2A, 0xFF, 0x03, 0x75, 0x10, 0x95, 0x01, 0x81, 0x00,
    0xC0,
];

pub const CONSUMER_REPORT_LEN: usize = 2;
/// Güç (yan) tuşu: ekran açıkken kilitler, kapalıyken uyandırır.
pub const CONSUMER_POWER: u16 = 0x0030;
pub const CONSUMER_VOLUME_UP: u16 = 0x00E9;
pub const CONSUMER_VOLUME_DOWN: u16 = 0x00EA;
/// AL Terminal Lock/Screensaver.
pub const CONSUMER_LOCK: u16 = 0x019E;
pub const CONSUMER_SLEEP: u16 = 0x0032;
pub const CONSUMER_SLEEP_MODE: u16 = 0x0034;
/// Klavye sayfasındaki güç tuşu.
pub const USAGE_KEYBOARD_POWER: u8 = 0x66;
pub const USAGE_Q: u8 = 0x14;
pub const MOD_LEFT_CTRL: u8 = 0x01;

pub fn consumer_report(usage: u16) -> [u8; CONSUMER_REPORT_LEN] {
    usage.to_le_bytes()
}

pub const MOD_LEFT_ALT: u8 = 0x04;
pub const MOD_LEFT_SHIFT: u8 = 0x02;
pub const MOD_LEFT_GUI: u8 = 0x08;
pub const USAGE_8: u8 = 0x25;
pub const MOD_RIGHT_ALT: u8 = 0x40;
pub const USAGE_ENTER: u8 = 0x28;
pub const USAGE_TAB: u8 = 0x2B;
pub const USAGE_LEFT_CTRL: u8 = 0xE0;
pub const USAGE_RIGHT_CTRL: u8 = 0xE4;
pub const USAGE_LEFT_GUI: u8 = 0xE3;
pub const USAGE_RIGHT_GUI: u8 = 0xE7;

/// Basılı tuşları tutar ve 8 baytlık klavye raporunu üretir.
#[derive(Default)]
pub struct Keyboard {
    modifiers: u8,
    keys: Vec<u8>,
}

impl Keyboard {
    /// Rapor değiştiyse true döner (tekrar eden WM_KEYDOWN'lar rapor üretmez).
    pub fn press(&mut self, usage: u8) -> bool {
        if (0xE0..=0xE7).contains(&usage) {
            let bit = 1 << (usage - 0xE0);
            let changed = self.modifiers & bit == 0;
            self.modifiers |= bit;
            return changed;
        }
        if self.keys.contains(&usage) || self.keys.len() >= 6 {
            return false;
        }
        self.keys.push(usage);
        true
    }

    pub fn release(&mut self, usage: u8) -> bool {
        if (0xE0..=0xE7).contains(&usage) {
            let bit = 1 << (usage - 0xE0);
            let changed = self.modifiers & bit != 0;
            self.modifiers &= !bit;
            return changed;
        }
        let before = self.keys.len();
        self.keys.retain(|&k| k != usage);
        before != self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.modifiers == 0 && self.keys.is_empty()
    }

    pub fn clear(&mut self) -> bool {
        let changed = self.modifiers != 0 || !self.keys.is_empty();
        self.modifiers = 0;
        self.keys.clear();
        changed
    }

    pub fn report(&self) -> [u8; KEYBOARD_REPORT_LEN] {
        let mut r = [0u8; KEYBOARD_REPORT_LEN];
        r[0] = self.modifiers;
        for (slot, &k) in r[2..].iter_mut().zip(&self.keys) {
            *slot = k;
        }
        r
    }
}

/// Tek tuş vuruşu raporu (değiştiricilerle birlikte).
pub fn key_report(modifiers: u8, usage: u8) -> [u8; KEYBOARD_REPORT_LEN] {
    [modifiers, 0, usage, 0, 0, 0, 0, 0]
}

/// Set 1 tarama kodunu HID klavye usage'ına çevirir. Tarama kodu fiziksel tuş konumudur;
/// karakteri iPhone'un donanım klavyesi düzeni belirler (Windows'takiyle aynı seçilmeli).
pub fn scancode_to_usage(scancode: u32, extended: bool) -> Option<u8> {
    let u = if extended {
        match scancode {
            0x1C => 0x58,
            0x1D => 0xE4,
            0x35 => 0x54,
            0x38 => 0xE6,
            0x45 => 0x53, // Num Lock (Windows genişletilmiş olarak bildirir)
            0x47 => 0x4A,
            0x48 => 0x52,
            0x49 => 0x4B,
            0x4B => 0x50,
            0x4D => 0x4F,
            0x4F => 0x4D,
            0x50 => 0x51,
            0x51 => 0x4E,
            0x52 => 0x49,
            0x53 => 0x4C,
            0x5B => 0xE3,
            0x5C => 0xE7,
            0x5D => 0x65,
            _ => return None,
        }
    } else {
        match scancode {
            0x01 => 0x29,
            0x02..=0x0B => 0x1E + (scancode - 0x02) as u8,
            0x0C => 0x2D,
            0x0D => 0x2E,
            0x0E => 0x2A,
            0x0F => 0x2B,
            0x10 => 0x14,
            0x11 => 0x1A,
            0x12 => 0x08,
            0x13 => 0x15,
            0x14 => 0x17,
            0x15 => 0x1C,
            0x16 => 0x18,
            0x17 => 0x0C,
            0x18 => 0x12,
            0x19 => 0x13,
            0x1A => 0x2F,
            0x1B => 0x30,
            0x1C => 0x28,
            0x1D => 0xE0,
            0x1E => 0x04,
            0x1F => 0x16,
            0x20 => 0x07,
            0x21 => 0x09,
            0x22 => 0x0A,
            0x23 => 0x0B,
            0x24 => 0x0D,
            0x25 => 0x0E,
            0x26 => 0x0F,
            0x27 => 0x33,
            0x28 => 0x34,
            0x29 => 0x35,
            0x2A => 0xE1,
            0x2B => 0x31,
            0x2C => 0x1D,
            0x2D => 0x1B,
            0x2E => 0x06,
            0x2F => 0x19,
            0x30 => 0x05,
            0x31 => 0x11,
            0x32 => 0x10,
            0x33 => 0x36,
            0x34 => 0x37,
            0x35 => 0x38,
            0x36 => 0xE5,
            0x37 => 0x55,
            0x38 => 0xE2,
            0x39 => 0x2C,
            0x3A => 0x39,
            0x3B..=0x44 => 0x3A + (scancode - 0x3B) as u8,
            0x45 => 0x48, // Pause
            0x46 => 0x47,
            0x47 => 0x5F,
            0x48 => 0x60,
            0x49 => 0x61,
            0x4A => 0x56,
            0x4B => 0x5C,
            0x4C => 0x5D,
            0x4D => 0x5E,
            0x4E => 0x57,
            0x4F => 0x59,
            0x50 => 0x5A,
            0x51 => 0x5B,
            0x52 => 0x62,
            0x53 => 0x63,
            0x56 => 0x64, // ISO < > tuşu
            0x57 => 0x44,
            0x58 => 0x45,
            _ => return None,
        }
    };
    Some(u)
}

pub fn report_map() -> Vec<u8> {
    let mut map = HEADER.to_vec();
    for _ in 0..CONTACT_SLOTS {
        map.extend_from_slice(&CONTACT);
    }
    map.extend_from_slice(&TAIL);
    map.extend_from_slice(&KEYBOARD);
    map.extend_from_slice(&CONSUMER);
    map
}

/// Tek parmaklı dokunma raporu. `down == false` parmağın kalktığı rapordur: temas kaydı
/// yalnız Confidence ile gönderilir, bir sonraki raporda hiç yer almaz.
pub fn touch_report(down: bool, x: u16, y: u16) -> [u8; TOUCH_REPORT_LEN] {
    let mut r = [0u8; TOUCH_REPORT_LEN];
    r[0] = if down { 0x03 } else { 0x02 };
    r[1] = 0; // contact id
    r[2..4].copy_from_slice(&x.min(TOUCH_MAX).to_le_bytes());
    r[4..6].copy_from_slice(&y.min(TOUCH_MAX).to_le_bytes());
    r[CONTACT_SLOTS * CONTACT_BYTES] = 1; // contact count
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_map_has_one_collection_per_slot() {
        let map = report_map();
        assert_eq!(map.len(), 24 + 47 * CONTACT_SLOTS + 17 + 49 + 25);
        let opens = map.iter().filter(|&&b| b == 0xA1).count();
        let closes = map.iter().filter(|&&b| b == 0xC0).count();
        assert_eq!(opens, 1 + 2 * CONTACT_SLOTS + 1 + 1); // dokunmatik ekran + klavye + tüketici
        assert_eq!(consumer_report(CONSUMER_VOLUME_UP), [0xE9, 0x00]);
        assert_eq!(closes, opens);
        // Dokunmatik ekran bölümü, iPhone'da doğrulanmış biçimle bayt bayt aynı kalır.
        assert_eq!(&map[..24], &HEADER);
        // Bluetooth bir öznitelik değerini en fazla 512 bayt taşır; aşan rapor tanımı kesilir.
        assert!(map.len() <= 512, "rapor tanımı {} bayt", map.len());
    }

    #[test]
    fn keyboard_tracks_modifiers_and_keys() {
        let mut k = Keyboard::default();
        assert!(k.press(USAGE_LEFT_GUI));
        assert!(k.press(0x06));
        assert!(!k.press(0x06), "tekrar eden basış rapor üretmemeli");
        assert_eq!(k.report(), [0x08, 0, 0x06, 0, 0, 0, 0, 0]);
        assert!(k.release(0x06));
        assert!(k.clear());
        assert_eq!(k.report(), [0; 8]);
        assert_eq!(key_report(MOD_LEFT_SHIFT, 0x04), [0x02, 0, 0x04, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn scancodes_map_to_usages() {
        assert_eq!(scancode_to_usage(0x1E, false), Some(0x04)); // A
        assert_eq!(scancode_to_usage(0x0B, false), Some(0x27)); // 0
        assert_eq!(scancode_to_usage(0x48, true), Some(0x52)); // yukarı ok
        assert_eq!(scancode_to_usage(0x45, true), Some(0x53)); // Num Lock
        assert_eq!(scancode_to_usage(0x7F, false), None);
    }

    #[test]
    fn touch_report_layout() {
        let r = touch_report(true, 0x1234, TOUCH_MAX + 7);
        assert_eq!(&r[..6], &[0x03, 0x00, 0x34, 0x12, 0x10, 0x27]);
        assert_eq!(r[TOUCH_REPORT_LEN - 1], 1);
        assert_eq!(touch_report(false, 0, 0)[0], 0x02);
    }
}
