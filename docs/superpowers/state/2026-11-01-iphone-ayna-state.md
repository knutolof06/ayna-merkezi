# iPhone Ayna — Aktif State

**Başlangıç:** 2026-11-01
**Durum:** implementation

## Karar Geçmişi
- 2026-11-01: Eski AetherCast kodu "saçma" bulundu, baştan yazılacak. Eski kod `_eski_aethercast/` altına taşındı (silinmedi).
- 2026-11-01: Kontrol için ESP32 yerine PC'nin kendi Bluetooth'u (WinRT GattServiceProvider ile HOGP) kullanılacak. Adaptör `Peripheral=True`.
- 2026-11-01: İşaretçi mutlak (0..32767) — jinwon-int/esp32-iphone-agent #104'te iPhone'da BLE üzerinden doğrulanmış.
- 2026-11-01: Yansıtma motoru UxPlay (`bin/uxplay/uxplay-windows.exe`), video penceresi bizim pencereye gömülür.
- 2026-11-01: Görev çubuğu ikonu: Shell_TrayWnd'e sahiplenmiş layered popup, UIA ile `StartButton` konumunun soluna.

## Bağlam Pinleri
- abhishek-raj/windows-ble-hid `src/BleHid.Core/BleHidPeripheral.cs` — GATT kurulum referansı (MIT)
- jinwon-int/esp32-iphone-agent `hardware/hid_bridge/ble_hid.h` — mutlak işaretçi report map'i
- UxPlay ayarları `%APPDATA%\leapbtw\uxplay-windows\arguments.txt` dosyasından okunur

- 2026-11-01: Otomatik "Windows ile başlat" kaldırıldı (onaysız kalıcılık) — yalnız ikon menüsünden açılır.
- 2026-11-01: UxPlay Türkçe karakterli yolda exit 1 veriyordu (eski projenin yansıtmasının çalışmama nedeni) — 8.3 kısa yol ile çözüldü.
- 2026-11-01: Reviewer (sonnet) 12 bulgu; 10'u düzeltildi (#9 asılı pencere bekçisi, #10 CREATE_SUSPENDED atlandı).

- 2026-11-01: Kullanıcı: yansıtma bağlantısı çalışıyor ama kontrol yok; AssistiveTouch istemiyor. → HID fare yerine BLE dokunmatik ekran (digitizer, sakx7/iOSBleDigitizer descriptor'u bayt bayt). iPhone'da Büyütme açık olmalı (1x). Klavye iletimi kaldırıldı.
- 2026-11-01: Kullanıcı: iki telefon simgesi → pencere gizli STATIC sahibine bağlandı (görev çubuğu düğmesi yok), küçültme kaldırıldı, sistem menüsüne "Çıkış" eklendi.

- 2026-11-01: Kullanıcı: kontrol gecikmeli; pencere ikonun üstünde çerçevesiz, yuvarlak köşeli olsun; sürükle-bırak dosya iPhone'a. → Pencere WS_POPUP + SetWindowRgn, ikonun üstünde ekran boyu. UxPlay -vd d3d11h264dec -vc d3d11convert. BLE ölçümü: aralık zaten 15 ms (iOS alt sınırı), hızlandırma isteği "Success" ama değişmiyor → gecikme BLE kaynaklı değil.
- 2026-11-01: Dosya aktarımı: kullanıcı iCloud Drive seçti (Bluetooth ile iOS dosya almıyor). Bırakılan dosya %USERPROFILE%\iCloudDrive\iPhone Ayna'ya kopyalanır; iCloud yoksa MessageBox + Store sayfası (9PKTQ5699M62).

- 2026-11-01: Kullanıcı: yansıtma bağlanıp anında kopuyor → kök neden `-vc d3d11convert` (UxPlay hattı `converter ! videoscale ! sink`, videoscale yalnız sistem belleği). Varsayılan çözücüye dönüldü. UxPlay çıktısı artık %TEMP%\iPhoneAyna-uxplay.log.
- 2026-11-01: Kullanıcı: iCloud yükledi, aktarım yok → bizim kopya doğruydu, iCloud ilk eşitlemede "beklemede"ydi. CfGetPlaceholderInfo ile yükleme izleyicisi eklendi; uçtan uca test: kopya → 6 sn'de yüklendi.

- 2026-11-01: Kullanıcı: pencere çok büyük, siyah bar, tırtıklı köşe; Windows panosu → iPhone. Kök neden (bar): oran d3d penceresinin ilk boyutundan alınıyordu; UxPlay `-d 1` geometri satırı (iPhone17,1: 1178x2560, dolgusuz) ile düzeltildi. Boy %78 + Ctrl+tekerlek (HKCU\Software\iPhoneAyna\BoyutYuzde). Köşeler: 4 küçük katmanlı köşe penceresi (DirectFlip'i bozmamak için tam boy değil). Pano: report map'e klavye (rapor 3) + Ctrl+V tuş tuş yazma (ToUnicodeEx tablosu). Temas yuvası 10→5 (rapor tanımı 512 bayt GATT sınırı altında: 325 bayt).
- 2026-11-01: UxPlay günlüğünde sık "client feedback 3-6 sn gecikti" hataları → Wi-Fi tarafı gecikmesi (gecikme şikâyetinin olası kaynağı, ayrı incelenecek).

- 2026-11-02: Kullanıcı: ekran kapanınca 5-10 sn'de kopuyor (ikisi de: otomatik kilit + yan tuş); her seferinde BT yeniden bağlama + Büyütme aç-kapa zahmetli; Windows'tan uyandırma istiyor; iPhone uygulaması yerine "daha kolay çözüm, GitHub araştır". iOS 27+. Kullanıcı CoreDevice yolu yerine **Bluetooth yapısını iyileştir**'i seçti.
- 2026-11-02: UxPlay günlüğü arabellekte kalıyordu (kopma nedeni kayıp) → ConPTY + `cmd /c start /wait` ile çalıştırılıyor, satırlar anında. Uyku engelleme (20 sn Shift), tıklayınca uyandırma, BT bağlanınca ⌥⌘8×2 Büyütme yenileme (Klavye Kısayolları açık olmalı; doğrulanmadı).
- 2026-11-02: CoreDevice Universal HID (iOS 27, go-ios#849 / pymobiledevice3) kayıtlı alternatif: BT/Büyütme/AirPlay'siz, bir kerelik Geliştirici Modu + USB + tünel.

- 2026-11-02: Review (8 bulgu) uygulandı: stop_running önce TerminateJobObject + ClosePseudoConsole ayrı iş parçacığında; VT imleç hareketleri satır sonu; spawn RAII; Büyütme yenileme yalnız yansıtma açıkken ve ikinci vuruş UI'da zamanlanıyor. Doğrulandı: UxPlay öldürülünce donmadan yeniden başlıyor, prev.log içerikli, BT kendiliğinden yeniden bağlanıyor.

- 2026-11-02: Kullanıcı: Büyütme yenileme çalıştı. İstek: iPhone'daki yerlerinde güç + ses tuşları; ekran kapanınca hâlâ kopuyor. → HID'e tüketici koleksiyonu (rapor 4: güç 0x30, ses 0xE9/0xEA; harita 350 bayt), buttons.rs kenar tuşları. Günlükten kök neden: UxPlay ekran kapanınca kapanıyor, yeniden başlatma "Erişim engellendi" (sonlanan iş nesnesine ekleme) → her çalıştırmaya yeni iş nesnesi; prev.log yalnız doluysa döndürülüyor. iPhone'un yeniden eşlenmesi gerekiyor (rapor haritası değişti). Güç tuşunun iOS'ta Consumer 0x30 ile çalıştığı doğrulanmadı.

- 2026-11-02: Kullanıcı yeni kapsam istedi: ana uygulama (iPhone ekranı içinde, boyut ayarı), Mac mini'ye AnyDesk benzeri kodla bağlantı (güven + parola + kontrol), Win+Mac, ikonlar tüm monitörlerde ve bağlantıya göre görünür. Kararlar: yalnız aynı ağ, RustDesk tabanlı, Windows→Mac, yeni özel GitHub deposu. Plan: docs/superpowers/plans/2026-11-02-ayna-merkezi-plan.md (onay bekliyor).

- 2026-11-02: Kullanıcı: ses tuşları çalışıyor, güç tuşu çalışmıyor; "geri kalan için her şeyi yap onaylıyorum". Güç tuşu: 6 kilitleme yöntemi sırayla denenir, yansıtma kesilmesiyle doğrulanıp hatırlanır (GucYontemi). Plan uygulandı: tüm monitörlerde ikonlar (iPhone + Mac), hub.rs (Ayna Merkezi), mac.rs (UDP bulma + TOFU/HMAC güven + RustDesk --connect), mac-agent (Swift). İki review (doğruluk + güvenlik) bulguları uygulandı. Windows zinciri sahte ajanla uçtan uca doğrulandı (eşleşme, imzalı yeniden bağlanma, sahte Mac reddi).

## Sonraki Adım
Kullanıcı: (1) iPhone'da güç tuşunu birkaç kez dene (yöntem öğrenilir). (2) Mac mini'de RustDesk 1.5.1 + Actions'tan AynaAjan.dmg kur, kodla bağlan. Mac tarafı gerçek cihazda henüz test edilmedi.

## Açık Sorular / Bekleyen Onaylar
- [x] Yerel doğrulama: derleme, birim testler, ikon konumu/aç-kapa, sağ tık menüsü, temiz çıkış, devre dışı çocuk pencere fare yönlendirmesi, BLE HID yayını başladı
- [ ] iPhone yatay moddayken mutlak koordinatların yönü (test edilmedi)
- [ ] iPhone ile gerçek eşleşme testi (kullanıcı)
- [ ] Gerçek video penceresinin gömülmesi (iPhone yansıtması başlatılınca doğrulanacak)
