# Uygulama cilası — Plan

**Tarih:** 2026-11-03
**Durum:** yayınlandı — v16 (305d580), CI üç iş başarılı, exe'de simge kaynağı doğrulandı; depo açıklaması İngilizce.
Gerçek kullanımda doğrulanmadı: Ctrl 10 sn sayacı (oyunda), Mac ajanının İngilizce metinleri.

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
v16 (cila) ve v17 (zip paketi) yayınlandı. 2026-11-04: kurulum programı — installer/AynaMerkezi.iss (Inno Setup,
kullanıcı başına %LOCALAPPDATA%\Programs\Ayna Merkezi, AppId 15E0CF97-…, Programlar ve Özellikler kaydı HKCU),
src/install.rs (DisplayVersion + autostart yolu), CI'da ISCC + sessiz kur/kaldır testi, README'ler.
Kalan: review → commit → push onayı → CI'da Setup testi geçti mi.
