# Mac çoklu ekran — Plan

**Tarih:** 2026-11-23
**Durum:** implementation tamam (2026-11-23), gerçek cihazda doğrulama bekleniyor

## İstek (kullanıcı)
Mac bağlanınca Windows'taki her monitör ayrı bir Mac ekranı olsun. Ayarla seçilebilsin:
- **Ayrı ekranlar (genişletilmiş):** her Windows monitörü Mac'in ayrı bir ekranı; Mac masaüstü üç ekrana yayılır.
- **Aynı ekran (yansıt):** her monitörde aynı Mac ekranı.
- (Şimdiki gibi tek pencere de kalabilir.)

## Durum tespiti
- Windows: 3 × 1920×1080, yan yana: DISPLAY2 (-1920,0) | DISPLAY6 birincil (0,0) | DISPLAY1 (1920,0).
- Mac mini monitörsüz; ajan zaten özgün boyutta sanal ekran açabiliyor (`PhoneMirror.standIn`, `makeDisplay`).
  Aynı yöntemle Windows monitörü başına bir sanal ekran açılabilir.
- Bugün tek akış: Mac → 0x01 VIDEO / 0x02 SCREEN, tek `Capturer`; Windows'ta tek `macview` penceresi ve tek
  GStreamer `Player`. Fare `Injector.point()` ana ekrana eşlenir.

## Tasarım
### Kip 1: Aynı ekran (yalnız Windows, düşük risk)
- Tek akış, her monitörde bir Mac penceresi (tam ekran). Gelen kareler her pencerenin oynatıcısına itilir.
- Fare/klavye hangi penceredeyse oradan aynı Mac ekranına gider (koordinat pencereye göre).
- Mac ajanında değişiklik gerekmez.

### Kip 2: Ayrı ekranlar (Mac + Windows)
- Windows bağlanınca monitör düzenini gönderir: yeni ileti `0x1E LAYOUT [n] ([x i16][y i16][w u16][h u16])×n`.
- Mac ajanı her monitör için o boyutta sanal ekran açar ve Windows'taki diziliş gibi yerleştirir
  (`CGConfigureDisplayOrigin`; Windows'un birincil monitörü Mac'in ana ekranı = menü çubuğu ve Dock).
  Monitörsüz Mac'te macOS'un sahte ekranı kalkar (bugünkü yedek ekranla aynı davranış).
- Her sanal ekran için ayrı `Capturer` + kodlayıcı; yeni iletiler `0x0B VIDEO_N [ekran][bayrak][veri]`,
  `0x0C SCREEN_N [ekran][g][y]`. Ekran 0 eski 0x01/0x02'yi kullanmaya devam eder (eski istemci uyumu).
- Fare: `0x1F MOVE_N [ekran][x][y]` (ya da TARGET gibi ekran seçimi + eski 0x10). Mac imleci ekranlar arasında
  Windows'taki gibi geçer; tıklama/sürükleme hangi monitördeyse o Mac ekranına.
- Windows: monitör başına bir Mac penceresi; `macview` tek pencere için yazılmış (thread_local tek `View`),
  ek ekranlar için `phoneview` benzeri hafif bir görüntü penceresi (`src/macscreens.rs`) ve ortak girdi kodu.
- Bağlantı kesilince/kip kapanınca sanal ekranlar kaldırılır; Mac pencereleri ana ekrana döner (macOS yapar).

### Ayar
Ayarlar › Mac bölümüne: **Ekranlar: Tek pencere | Aynı ekran | Ayrı ekranlar**. Mac simgesinin sağ tık menüsünde de.
Değişince bağlantı yeniden kurulmadan uygulanır (Mac sanal ekranları ekler/kaldırır).

## Aşamalar
1. Kip 1 (aynı ekran): Windows'ta çoklu pencere + tek akışı çoğaltma, tam ekran, girdi. Mac değişmez.
2. Kip 2 protokolü + Mac: LAYOUT, sanal ekranlar, çoklu Capturer, VIDEO_N/SCREEN_N, ekranlı fare.
3. Kip 2 Windows: monitör başına pencere/oynatıcı, girdi yönlendirme, imleç (her pencerede Mac imleci şekli).
4. Ayar arayüzü (Ayarlar + sağ tık), README, sürüm.
Her aşama ayrı sürüm olarak yayınlanabilir; 1. aşama tek başına kullanılabilir.

## Riskler
- Bant ve yük: 3 × 1080p akış ≈ 3 kat veri ve Mac'te 3 kodlayıcı (Apple çipi kaldırır; Wi-Fi'de gecikme artabilir).
- Belgelenmemiş `CGVirtualDisplay`: macOS sürümüyle değişebilir; açılamazsa tek ekrana dönülür.
- Ekran düzeni değişince macOS pencereleri taşır; kip kapanınca Mac pencereleri tek ekrana toplanır.
- iPhone kipi de sanal ekran kullanıyor: ikisi birlikte açıkken ekran sırası ve ana ekran korunmalı.
- Oyun fare kilidi çoklu ekranda yalnız bulunduğu ekranda.

## Kararlar (2026-11-23, kullanıcı)
- Görünüm: bugünkü Mac penceresi boyutu kalır (tam ekran değil); her monitörde o monitörün Mac simgesinin
  yanında, bugünkü yerleşim ve boyut ayarıyla.
- Ekran sayısı: Windows'ta kaç monitör varsa o kadar Mac ekranı eklenebilir; kullanıcı istediğini kaldırabilir
  (monitör başına aç/kapa; Ayarlar'da ve o monitördeki Mac simgesinin sağ tık menüsünde).
- Sıra: iki kip birlikte geliştirilir (tek sürüm).
- Varsayılan değişmez: bugünkü gibi ekranda tek Mac penceresi ("Tek pencere" kipi). Diğer kipler yalnız ayardan
  seçilince; o zaman da her monitörde bugünkü gibi tek pencere, o monitördeki Mac simgesiyle açılır (hepsi
  kendiliğinden açılmaz).

## Açık sorular
- [ ] Diğer istekler (mesajda "bir kaç istek" deniyor; şimdilik biri yazıldı).
- [ ] Uygulamaya başlama onayı.

## Sonraki Adım
Kod yazıldı (src/macscreens.rs, MainKeeper + ExtraScreen main.swift'te), iki review uygulandı. LAYOUT başlığında
birincil monitör boyutu var. Kalan: push onayı → CI → kullanıcı denemesi (aynı ekran, ayrı ekranlar, monitör kaldırma).
