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

## Riskler
- `CGVirtualDisplay` belgelenmemiş API: macOS sürümünde değişebilir; açılamazsa mod hata verir, Mac
  ekranı kipine dönülür.
- iPhone Mirroring kimlik doğrulaması "her seferinde sor" ise açılışta Mac parolası ister (görünmez ekranda
  kalır): ayarlardan "otomatik" seçilmeli.
- Fare imleci iPhone kipinde sanal ekrana gider; Mac başında biri varsa imleci ana ekranda görmez.
