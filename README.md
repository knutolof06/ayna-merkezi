# Ayna Merkezi (iPhone + Mac)

iPhone ekranını Windows'a yansıtır ve bilgisayarın fare/klavyesiyle iPhone'u kontrol etmeyi sağlar.
Aynı ağdaki bir Mac'e (ör. Mac mini) koduyla bağlanıp ekranını görmeyi ve kontrol etmeyi sağlar.
iPhone için ek donanım ya da uygulama gerekmez; Mac'te küçük bir menü çubuğu yardımcısı (Ayna Ajan) çalışır.

- **Ana uygulama (Ayna Merkezi):** `iPhoneAyna.exe`'yi açınca gelen pencere. iPhone'u açıp kapatma
  (kapalıyken yansıtma alıcısı durur, ikon kalkar), ekran boyutu, Mac bağlantısı, "Windows ile başlat" ve
  "Bağlanınca Büyütme'yi yenile". Ayrıca görev çubuğu ikonlarının sağ tık menüsündeki **Ayna Merkezi**'nden
  ya da uygulama çalışırken exe'yi yeniden açarak gelir.
- **Görev çubuğu ikonları tüm monitörlerde:** Her görev çubuğunda Başlat'ın solunda iPhone ikonu; Mac
  bağlıyken onun da solunda Mac ikonu. Pencere, tıklanan ikonun bulunduğu monitörde açılır.

- **Yansıtma:** AirPlay (UxPlay alıcısı). Görüntü, görev çubuğundaki ikonun tam üstünde açılan çerçevesiz,
  köşeleri iPhone gibi yuvarlak (kenar yumuşatmalı) bir pencerede gösterilir. Pencere oranı iPhone'un bildirdiği
  kare geometrisinden alınır; siyah bar kalmaz. Boy **Ctrl + tekerlek** ile değişir ve hatırlanır.
- **Klavye ve pano:** Pencere odaktayken yazdıklarınız iPhone'a gider. **Ctrl+V**, Windows panosundaki metni
  iPhone'da imlecin olduğu yere yazar (Esc durdurur).
- **Kontrol:** PC'nin kendi Bluetooth'u iPhone'a **dokunmatik ekran** olarak bağlanır. Pencerede tıkladığınız
  nokta iPhone'da aynı noktaya parmakla dokunulmuş gibi işlenir. İmleç yok, AssistiveTouch gerekmez.
- **Görev çubuğu:** iPhone ekranının girişi Başlat düğmesinin solundaki telefon ikonu (pencerenin ayrı görev
  çubuğu düğmesi yok). Tıklayınca pencere açılır/gizlenir; sağ tık menüsünde "Ayna Merkezi", "Windows ile
  başlat" ve "Çıkış" var.

## Kullanım

1. `iPhoneAyna.exe`'yi çalıştırın (yanında `bin\uxplay` klasörü olmalı).
2. **Yansıtma:** iPhone ile PC aynı Wi-Fi'de olsun. iPhone'da Denetim Merkezi → Ekran Yansıtma → **Ayna**.
3. **Kontrol (bir kez):**
   - iPhone Ayarlar → Bluetooth → bilgisayarın adını (ör. `BURHAN`) eşleyin. Daha önce eşlediyseniz önce
     **Bu Aygıtı Unut** deyip yeniden eşleyin (iPhone eski cihaz tanımını önbellekte tutar). Windows bir eşleşme
     onayı gösterirse kabul edin.
   - iPhone Ayarlar → Erişilebilirlik → **Büyütme: Açık**. iOS harici dokunmatik ekran girdisini yalnız Büyütme
     açıkken işler. Açınca ekran büyürse **üç parmakla çift dokunarak** normale döndürün; ayar açık kalır,
     ekranda hiçbir şey değişmez.
   - Görüntü gelmeden önce pencerede "Bluetooth: bağlı" yazıyorsa tıklama ve sürüklemeler iPhone'a gider.
   - Türkçe karakterlerin doğru yazılması için iPhone'da Ayarlar → Genel → Klavye → **Donanım Klavyesi**
     düzenini Windows'taki ile aynı seçin (ör. ikisi de Türkçe Q). Bağlı bir donanım klavyesi varken iOS ekran
     klavyesini gizler; yazı PC klavyesinden gelir.

| PC'de | iPhone'da |
| --- | --- |
| Sol tık | Dokunma |
| Basılı tutup sürükle | Parmakla kaydırma / sürükleme |
| Tekerlek | Sayfayı kaydırma |
| Sağ tık | Uzun basma |
| Orta tık (tekerleğe basma) | Ana ekran |
| Klavye | Yazı (Ctrl, iPhone'da Cmd olur: Ctrl+C/X/A/Z çalışır) |
| Ctrl+V | Windows panosundaki metni yazar |
| Ctrl + tekerlek | Pencere boyu |
| Esc | Pencereyi gizler (süren yapıştırmayı durdurur) |

Görüntü yokken sağ tık pencerenin kendi menüsünü (Gizle / Çıkış) açar.

### Kenar tuşları

Pencerenin sol kenarında **ses +/−**, sağ kenarında **güç (yan) tuşu** iPhone 16 Pro'daki yerlerinde durur. Tıklamak
iPhone'a Bluetooth üzerinden aynı tuşu gönderir. Tuşlar cihaz tanımına eklendiği için ilk seferde iPhone'da
**Bu Aygıtı Unut** deyip yeniden eşlemek gerekir.

Güç tuşu: görüntü yokken iPhone'u uyandırır, varken kilitler. iOS'un kilitleme için hangi HID kodunu kabul ettiği
modele ve sürüme göre değişiyor (standart "Power" kodu bazı iPhone'larda çalışmıyor). Uygulama birkaç yöntemi
sırayla dener: kilitlenen iPhone yansıtmayı kestiği için işe yarayan anlaşılır ve hatırlanır. Tutmazsa
"başka yöntem denenecek" bildirimi çıkar; tuşa yeniden basmak yeterli.

### Uyku, uyandırma ve Büyütme

- **Uyku engelleme:** Yansıtma sürerken uygulama 20 saniyede bir Bluetooth klavyesinden zararsız bir Shift
  vuruşu gönderir; iPhone kendiliğinden kilitlenip yansıtmayı kesmez.
- **Uyandırma:** Görüntü yokken (iPhone kilitli/ekranı kapalı) pencereye tıklamak iPhone'u uyandırır. Kilidi açıp
  Ekran Yansıtma'dan "Ayna"yı seçmek gerekir: iOS, ekran kapanınca yansıtmayı kendisi sonlandırır.
- **Büyütme'nin yenilenmesi:** iOS harici dokunmatik ekranı yalnız Büyütme açılırken bağlı olan cihazlara uygular.
  Bluetooth her (yeniden) bağlandığında uygulama Büyütme'yi klavye kısayoluyla (⌥⌘8) kapatıp açar. Bunun için
  iPhone'da **Ayarlar → Erişilebilirlik → Büyütme → Klavye Kısayolları** açık olmalı. Kapatmak için
  `HKCU\Software\iPhoneAyna\BuyutmeYenile` = 0.

## Dosya gönderme

Dosyayı (ya da klasörü) iPhone penceresine veya görev çubuğundaki telefon ikonuna sürükleyip bırakın.
Dosya iCloud Drive'daki **iPhone Ayna** klasörüne kopyalanır ve iPhone'da **Dosyalar > iCloud Drive > iPhone Ayna**
altında görünür. iPhone Bluetooth ile dosya kabul etmediği için yol bu. Önce "iCloud'a yükleniyor…", iCloud
yüklemeyi bitirince "iCloud'a yüklendi" bildirimi çıkar (iCloud'un ilk kurulumunda yükleme birkaç dakika sürebilir).

Bu bilgisayarda ücretsiz **iCloud for Windows** gerekir. Kurulu değilse ya da iCloud Drive açık değilse, ilk
bırakmada ne yapılacağı anlatılır ve Microsoft Store sayfası açılır. Kurulumdan sonra Apple kimliğiyle giriş
yapıp "iCloud Drive" kutusunu işaretlemek yeterli.

## Mac'e bağlanma

Uzak masaüstünün kendisi (görüntü, kontrol, şifreleme) [RustDesk](https://github.com/rustdesk/rustdesk)'tir
(1.5.1 ve üstü). Şimdilik yalnız **aynı yerel ağ**: sunucu ya da internet gerekmez.

**Mac'te (bir kez):**
1. RustDesk'i kurun (`/Applications/RustDesk.app`), Sistem Ayarları → Gizlilik ve Güvenlik'te RustDesk'e
   **Ekran Kaydı** ve **Erişilebilirlik** izni verin.
2. GitHub Actions'ın ürettiği `AynaAjan.dmg`'den **Ayna Ajan**'ı Uygulamalar'a sürükleyin. İmzasız olduğu için
   ilk açılışta sağ tık → **Aç** deyin.
3. İlk açılışta Ayna Ajan bağlantı parolasını sorar (en az 10 karakter) ve RustDesk'i ayarlar (macOS yönetici
   parolası istenir): doğrudan IP erişimi, yalnız kalıcı parola, yalnız güvenilen bilgisayarların adresleri
   (IP beyaz listesi; internet üzerinden gelen bağlantılar da bu yüzden reddedilir).
4. Menü çubuğundaki ekran simgesinde Mac'in **kodu** görünür (RustDesk kimliği; tıklayınca kopyalanır).

**Windows'ta:** Ayna Merkezi → Mac → kodu yazıp **Bağlan**.
1. Mac yerel ağda kodla bulunur.
2. İlk seferde Mac'te "… bu Mac'e bağlanmak istiyor — Reddet / İzin ver" sorusu çıkar (varsayılan Reddet).
   İzin verilince bu bilgisayar Mac'in güvenilenler listesine girer ve ikisi arasında bir anahtar paylaşılır:
   sonraki bağlantılarda Mac'in yanıtı ve bu bilgisayarın kimliği bu anahtarla doğrulanır, aynı ağdaki başka
   bir cihaz kendini Mac ya da bu bilgisayar gibi gösteremez.
3. RustDesk penceresi açılır ve parolayı sorar ("Parolayı hatırla" işaretlenebilir). Parola Ayna
   Merkezi'nden geçmez.

Bağlıyken görev çubuklarında Mac ikonu çıkar: tıklamak Mac penceresini gösterir/gizler, sağ tık menüsünde
"Mac bağlantısını kes" var. Pencere kapanınca ikon kalkar. RustDesk istemcisi uygulamanın yanındaki
`bin\rustdesk\rustdesk.exe`'den (resmî 1.5.1 sürümü) ya da kurulu RustDesk'ten çalışır.

Güvenilen bilgisayarlar, parola değişikliği ve "Bağlantılara izin ver" anahtarı Ayna Ajan menüsündedir.

## Bilinen sınırlar

- AirPlay H.264 ile sıkıştırır, tam kayıpsız görüntü mümkün değil. Gecikmeyi düşürmek için ses/görüntü eşitlemesi
  kapalı (`-vsync no`), 60 fps.
- iPhone yatay moddayken dokunma koordinatlarının yönü doğrulanmadı.
- Gönderilen dosyalar internet üzerinden iCloud'a gider; büyük dosyaların iPhone'da görünmesi yükleme hızına
  bağlıdır (ücretsiz iCloud kotası 5 GB).
- Windows 11'de iPhone'dan daha kısa Bluetooth bağlantı aralığı istenir; iPhone'un kabul edip etmediği
  `%TEMP%\iPhoneAyna.log` dosyasına yazılır.
- Yapıştırma metni tuş tuş yazar (saniyede ~30 karakter, en fazla 5000). Klavye düzeninde tek tuşla
  yazılamayan karakterler (ör. ölü tuş isteyen é) atlanır ve bildirimde sayısı yazılır.
- UxPlay'in kendi küçük simgesi saatin yanındaki bildirim alanında görünür.
- Bluetooth adaptörünün "çevre birimi (peripheral)" rolünü desteklemesi gerekir. Desteklemiyorsa pencerede yazar.
- UxPlay, Türkçe karakterli klasör yollarında çalışmıyor. Uygulama bunu 8.3 kısa yolla aşıyor; kısa adlar diskte
  kapalıysa klasörü `C:\iPhoneAyna` gibi bir yola taşıyın.
- Günlükler: `%TEMP%\iPhoneAyna.log` (uygulama: Bluetooth bağlantısı, dosya gönderimi) ve
  `%TEMP%\iPhoneAyna-uxplay.log` (yansıtma motoru; bir önceki çalıştırmanınki `.prev.log`). UxPlay gizli bir
  sözde konsolda çalışır, çıktısı satır satır ve anında günlüğe düşer.
- Mac görüntüsü Ayna Merkezi'nin içine gömülü değil, RustDesk'in kendi penceresindedir (görev çubuğu düğmesi
  kaldırılır, Mac ikonuyla yönetilir).
- Mac bağlantısı yalnız aynı yerel ağda (aynı alt ağda) çalışır. Bilgisayarın IP adresi değişirse Mac'te
  "yeni bir adresten bağlanıyor" sorusu yeniden çıkar.
- Bağlantı parolası RustDesk'e komut satırından verilir; Mac'te ayar sırasında kısa süre süreç listesinde
  görünebilir.
- iOS 27+ için alternatif (henüz uygulanmadı): Apple'ın geliştirici kanalı (CoreDevice) Bluetooth ve Büyütme
  olmadan dokunma iletebiliyor; bir kerelik Geliştirici Modu + USB eşleme gerektirir
  ([go-ios#849](https://github.com/danielpaulus/go-ios/pull/849)).

## Geliştirme

```
cargo build --release
copy target\release\iphone-ayna.exe iPhoneAyna.exe
```

Rust + `windows` crate. Dosyalar: `hub.rs` (Ayna Merkezi penceresi), `mac.rs` (Mac'i bulma, güven, RustDesk
oturumu), `mirror.rs` (iPhone penceresi, video gömme, girdi), `ble.rs` (HID over GATT), `hid.rs` (dokunmatik
ekran + klavye + tüketici rapor tanımı), `taskbar.rs` (tüm görev çubuklarında cihaz ikonları), `buttons.rs`
(kenar tuşları), `uxplay.rs` (AirPlay motoru), `transfer.rs` (iCloud Drive'a dosya gönderme), `toast.rs`
(bildirim balonu), `corners.rs` (yumuşak köşe katmanı).

Mac yardımcısı: `mac-agent/` (Swift, AppKit + Network; `build.sh` evrensel .app ve .dmg üretir). Her `main`
gönderiminde GitHub Actions Windows exe'sini ve Mac .dmg'sini derleyip artefakt olarak yükler
(`.github/workflows/build.yml`). `bin\` (UxPlay ve RustDesk ikilileri) depoda değildir.

Yararlanılan açık kaynak çalışmalar:
[abhishek-raj/windows-ble-hid](https://github.com/abhishek-raj/windows-ble-hid) (WinRT ile HOGP),
[sakx7/iOSBleDigitizer](https://github.com/sakx7/iOSBleDigitizer) (AssistiveTouch'sız BLE dokunmatik ekran descriptor'u),
[leapbtw/uxplay-windows](https://github.com/leapbtw/uxplay-windows) (UxPlay Windows derlemesi).
