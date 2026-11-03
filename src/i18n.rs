//! Arayüz dili: Türkçe ya da İngilizce. Ayar ("Dil": "tr" | "en") yoksa Windows'un arayüz diline göre
//! (Türkçe değilse İngilizce). Metinler kullanıldıkları yerde iki dilde yazılır: `t("…", "…")`.

use std::sync::atomic::{AtomicU8, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Globalization::GetUserDefaultUILanguage;

use crate::util;

const SETTING: PCWSTR = w!("Dil");
/// 0: henüz okunmadı, 1: Türkçe, 2: İngilizce.
static LANG: AtomicU8 = AtomicU8::new(0);

pub fn english() -> bool {
    match LANG.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let en = match util::read_text_setting(SETTING).as_deref() {
                Some("tr") => false,
                Some("en") => true,
                // Birincil dil kimliği (alt 10 bit): 0x1F Türkçe.
                _ => (unsafe { GetUserDefaultUILanguage() } & 0x3FF) != 0x1F,
            };
            LANG.store(if en { 2 } else { 1 }, Ordering::Relaxed);
            en
        }
    }
}

pub fn set_english(on: bool) {
    let _ = util::write_text_setting(SETTING, if on { "en" } else { "tr" });
    LANG.store(if on { 2 } else { 1 }, Ordering::Relaxed);
}

/// Seçili dildeki metin.
pub fn t<'a>(turkish: &'a str, in_english: &'a str) -> &'a str {
    if english() {
        in_english
    } else {
        turkish
    }
}

/// Seçili dildeki biçimli metin: şablondaki her `{}` sırayla `args` ile değiştirilir.
pub fn tf(turkish: &str, in_english: &str, args: &[&dyn std::fmt::Display]) -> String {
    let template = t(turkish, in_english);
    let mut out = String::with_capacity(template.len() + 16);
    let mut it = args.iter();
    let mut rest = template;
    while let Some(i) = rest.find("{}") {
        out.push_str(&rest[..i]);
        if let Some(a) = it.next() {
            out.push_str(&a.to_string());
        }
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}
