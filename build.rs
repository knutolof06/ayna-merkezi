//! Uygulama simgesini (assets/icon.res, tools/make_icon.py üretir) exe'ye gömer: Gezgin, görev
//! yöneticisi ve kısayollar logoyu gösterir. MSVC bağlayıcısı .res dosyasını doğrudan alır (sürümler
//! CI'da MSVC ile derlenir); GNU ld almadığı için yerel GNU derlemesi simgesiz kalır. Pencere ve
//! bildirim alanı simgesi her durumda çalışma anında aynı logodan yüklenir (icon.ico gömülü).

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.res");
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let res = std::path::Path::new(&dir).join("assets").join("icon.res");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc && res.exists() {
        println!("cargo:rustc-link-arg-bins={}", res.display());
    }
}
