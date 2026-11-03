# Uygulama cilası — Plan

**Tarih:** 2026-11-03
**Durum:** uygulanıyor

## İstekler (kullanıcı)
1. Ctrl 10 sn basılı → fare kilidinden çıkış; ekranda geri sayım. Oyunda hiçbir şey değişmemeli (Esc oyunun
   "bağlantıyı kes" menüsünü açıyor; çıkış yolu oyuna Esc/Alt göndermemeli). Ctrl oyuna normal gider (karar).
2. Gizli simgelerde uxplay-windows değil bizim uygulama görünsün; logo.
3. GitHub İngilizce (README, sürüm adları).
4. İngilizce seçeneği: Windows uygulamasında seçim, Mac ajanı Mac'in diline göre (karar).
5. Windows'ta "Uygulamayı güncelle" düğmesi.
6. Mac ve iPhone ikonlarına sağ tık: Çık, Ayarları aç, Bağlantıyı kes.

## Kararlar
- Kaçış: Ctrl (sol ya da sağ) 1 sn basılı kalınca Mac penceresinin ortasında sayaç (10→0), 10 sn'de kilit
  bırakılır; Ctrl+Alt kaldırılır (Alt oyuna gidiyordu). Sayaç penceresi katmanlı, tıklanamaz, Mac'e gitmez.
- uxplay-windows'un Qt bildirim simgesi: süreç pencereleri için NIM_DELETE (uID 0..64), UxPlay her
  başladığında ve 30 sn'de bir yinelenir. Bizim simge: Shell_NotifyIcon, sol tık Ayarlar, sağ tık menü.
- Logo: `assets/icon.ico` (Pillow ile üretilir, 16–256), exe kaynağı build.rs (winresource) ile; çalışma
  anında da aynı ico yüklenir.
- Dil: `i18n.rs` `tr(tr, en)`; ayar "Dil" (tr|en), yoksa Windows arayüz diline göre.

## Sonraki Adım
Yapıldı (commit yok): 1 Ctrl 10 sn + sayaç (countdown.rs), 6 sağ tık (Mac sizer'a Ayarlar/Çık; iPhone menüsüne
Bağlantıyı kes = mirror::disconnect, Ayarları aç, Güncellemeleri denetle, Çık), 5 hub'da Dil + Güncelleme satırı
(update::check_now/Status), 2 logo (tools/make_icon.py → assets/icon.ico/.res/logo.png; .res yalnız MSVC'de
gömülür) + tray.rs (kendi simgesi, uxplay simgesini NIM_DELETE), 4 Mac i18n (L()), Info.plist en + tr.lproj,
3 README İngilizce (+README.tr.md), iş akışı İngilizce + "Version N". Windows metinlerinin çevirisi ajanda.
Kalan: ajan sonucu → build/test → review → commit → push onayı (+ gh repo açıklaması İngilizce).
