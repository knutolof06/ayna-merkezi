# iPhone'u Mac üzerinden kontrol — Plan

**Tarih:** 2026-11-03
**Durum:** Aşama 1 uygulanıyor

## Amaç
Windows'tan iPhone'u, Mac mini'deki iPhone Mirroring üzerinden kontrol etmek. Kullanıcı şartı: Mac'in ana
ekranında iPhone görüntüsü sürekli açık durmasın.

## Karar
- Mac ajanı görünmez bir sanal ekran oluşturur (CoreGraphics'in belgelenmemiş `CGVirtualDisplay`
  sınıfı; DeskPad, BetterDisplay gibi uygulamalar da kullanır). iPhone Mirroring o ekranda açılır,
  penceresi Erişilebilirlik ile oraya taşınır; ScreenCaptureKit yalnız o pencereyi yakalar.
- Fare koordinatları pencereye eşlenir (CGEvent, genel koordinat). Klavye için iPhone Mirroring öne alınır.
- Mod bitince iPhone Mirroring kapatılır, kapandıktan sonra sanal ekran kaldırılır (pencere ana ekrana
  düşmesin).
- Protokol: Windows → Mac 0x18 MODE [0 Mac ekranı | 1 iPhone]; Mac → Windows 0x05 STATE
  [mod][başarılı][metin]. Görüntü aynı 0x01/0x02 akışı (tek oturum, tek yakalayıcı).

## Aşamalar
1. Deneme: Mac ikonuna sağ tık → "iPhone'u Mac üzerinden göster"; görüntü Mac penceresinde (dikey oran).
   Gerçek Mac'te doğrulanacaklar: sanal ekran açılıyor mu, pencere taşınıyor mu, yakalama ve tıklama
   çalışıyor mu, kapanışta pencere ana ekrana düşüyor mu.
2. iPhone ikonuna taşıma: iPhone penceresi Mac yolunu kullanır (BLE/AirPlay yerine ya da yedek olarak).

## Aşama 2: iPhone ayrı pencerede (2026-11-05, kullanıcı onayladı, kod yazıldı: src/phoneview.rs + Session)
Değişen: TARGET iki baytlı [fare][klavye]; fare imlecin üstündeki pencereyi, klavye etkin pencereyi izler.
2026-11-05 (v21 sonrası, kullanıcı): yeni pencere istemiyor. Menü yalnız Mac'te uygulamayı başlatır; görüntü
iPhone ikonuyla aynı yerde/boyutta açılır (phoneview artık çerçevesiz, mirror::frame), gizliyken 0x1D ile durur.
v22 günlüğü (23:30:40): iPhone hazır olunca "Mac ekranı: 900×1000" = ana yakalama görünmez ekrana geçti (normali
1920×1080). Mac penceresinin küçülmesi ve "siyah kare" bundan. Olası neden: Mac mini monitörsüz, macOS'un sahte
1920×1080 ekranı sanal ekran eklenince kalkıyor, sanal ekran ana ekran oluyor. Kullanıcı doğruladı: monitör yok.
Çözüm: önce özgün boyutta yedek sanal ekran (PhoneMirror.standIn), özgün ekran kalkarsa kalır, kalkmazsa 2 sn'de
kapanır; sonra görünmez ekran. Doğrulanacak: günlükte kip açılınca "Mac ekranı: 900×1000" görünmemeli.
Kullanıcı kipi gerçek Mac'te açtı, iPhone görüntüsü geldi. Şikâyet: görüntü Mac penceresinin yerini alıyor,
Mac kullanılamıyor. Öneri:
- Mac ajanı iki yakalayıcıyı aynı anda çalıştırır: ana ekran (0x01/0x02) + iPhone bölgesi. Yeni iletiler:
  Mac → Windows 0x09 PHONE_VIDEO, 0x0A PHONE_SCREEN; Windows → Mac 0x19 TARGET [0 Mac | 1 iPhone],
  0x1C PHONE_KEYFRAME; 0x18 MODE [2] = ayrı pencere (eski ajan 2'yi kapalı sayar, Windows uyarır).
- Windows: ikinci GStreamer oynatıcılı yeni pencere (src/phoneview.rs). Fare hangi pencerenin üstündeyse
  TARGET ona çevrilir; injector.area buna göre iPhone penceresi ya da ana ekran olur.
- Klavye iPhone penceresindeyken iPhone Mirroring öne alınır; Mac penceresine dönünce önceki uygulama öne gelir.
Sınırlar: Mac'te tek imleç ve tek ön plan uygulaması var, ikisi aynı anda değil sırayla kullanılır. Doğrulanmamış:
iPhone Mirroring öne alınmamışken görüntü üretmeye devam ediyor mu.

## Riskler
- `CGVirtualDisplay` belgelenmemiş API: macOS sürümünde değişebilir; açılamazsa mod hata verir, Mac
  ekranı kipine dönülür.
- iPhone Mirroring kimlik doğrulaması "her seferinde sor" ise açılışta Mac parolası ister (görünmez ekranda
  kalır): ayarlardan "otomatik" seçilmeli.
- Fare imleci iPhone kipinde sanal ekrana gider; Mac başında biri varsa imleci ana ekranda görmez.
