# Ayna Merkezi — Plan (iPhone + Mac mini)

**Tarih:** 2026-11-02
**Durum:** uygulandı (2026-11-02) — Mac tarafı gerçek Mac'te doğrulanmadı

## Uygulama sırasında değişen kararlar
- Arayüz Tauri yerine mevcut Win32/Rust kodu üzerinde kendi çizilen pencere (hub.rs): geçiş gerilemesi
  riski yok, görünüm mevcut Apple tarzıyla aynı. Mac'te ayrı Swift menü çubuğu yardımcısı (mac-agent/).
- iPhone ekranı bugünkü yuvarlak pencerede kaldı (ikonun üstünde); ana uygulama yönetim penceresi.
- RustDesk oturum penceresi gömülmedi (Flutter alt penceresi, aynı süreçte); görev çubuğu düğmesi
  kaldırılıp Mac ikonuyla yönetiliyor.
- Bulma Bonjour yerine UDP yayını (21130). Güven: ilk onayda cihaza özel anahtar (TOFU), sonra
  HMAC-SHA256 ile karşılıklı kanıt; Mac'te RustDesk IP beyaz listesi.
- Parola Windows'ta saklanmıyor: RustDesk'in kendi parola penceresi soruyor.

## Amaç

Tek bir uygulama ("Ayna Merkezi"):

1. iPhone ekranını kendi içinde gösterir; boyut gibi ayarlar uygulamanın içinden yapılır.
2. Aynı ağdaki Mac mini'ye, AnyDesk gibi **cihaza özel kodla** bağlanır; ilk bağlantıda **güven onayı**,
   sonrasında **parola** ister; Mac'in ekranını gösterir ve **kontrol eder**.
3. Görev çubuğunda Başlat'ın solunda iPhone ikonu, Mac bağlıyken onun yanında Mac ikonu durur;
   ikonlar **tüm monitörlerin görev çubuklarında** çıkar.
4. iPhone kapanınca/bağlantı kopunca iPhone ikonu kaybolur, açılınca geri gelir; Mac için de aynısı.
5. Mac sürümü GitHub Actions'ta derlenir.

## Alınan kararlar

| Konu | Karar |
| --- | --- |
| Ağ | Şimdilik yalnız aynı yerel ağ (sunucu yok) |
| Uzak bağlantı motoru | RustDesk tabanlı (ekran kodlama, şifreli bağlantı, kontrol hazır) |
| Kontrol yönü | Windows → Mac (Mac'te yalnız ajan) |
| Depo | Yeni **özel** GitHub deposu; Actions ile Windows + macOS derlemesi |

Lisans notu: RustDesk AGPL-3.0. Kişisel kullanımda sorun yok; uygulama başkalarına dağıtılırsa kaynak
kodu açılmalı.

## Mimari

```
Windows (Ayna Merkezi)                               Mac mini (Ayna Ajan)
┌──────────────────────────────────────┐            ┌──────────────────────────────┐
│ Ana pencere (Tauri 2 / WebView2)     │            │ Menü çubuğu uygulaması       │
│  ├ iPhone paneli  ← UxPlay videosu   │            │  ├ cihaz kodu (ör. 418 227 095)│
│  │                 (yerel çocuk HWND)│  Bonjour   │  ├ güven onayı / güvenilenler │
│  ├ Mac paneli: kod gir, bağlan ──────┼──_ayna._tcp─▶  ├ katılımsız erişim parolası │
│  └ Ayarlar: boyut, başlangıç, …      │            │  └ RustDesk hizmeti (host)    │
│ Görev çubuğu ikonları (tüm monitörler)│  RustDesk  │      ekran + giriş (izinli)   │
│ iPhone: BLE HID + UxPlay (mevcut)    │◀═══════════▶                              │
│ Mac: RustDesk istemcisi (doğrudan IP)│  şifreli   └──────────────────────────────┘
└──────────────────────────────────────┘
```

- **Arayüz:** Tauri 2 (Rust çekirdek + HTML/CSS arayüz). Aynı kod Windows'ta ana uygulama, Mac'te ajan
  olarak derlenir. Mevcut Rust modülleri (BLE, UxPlay, görev çubuğu, iCloud aktarımı) çekirdeğe taşınır.
  Video ve RustDesk pencereleri, WebView'in üstüne yerel çocuk pencere olarak gömülür (UxPlay'de
  bugün yaptığımız gibi).
- **Cihaz kodu:** Mac'in donanım kimliğinden türetilen, değişmeyen 9 haneli kod. Ajan bunu yerel ağda
  Bonjour (`_ayna._tcp`, TXT kaydında kod + açık anahtar parmak izi) ile duyurur. Windows kodu girer,
  Bonjour'dan IP'yi bulur; sunucu gerekmez.
- **Bağlantı:** RustDesk istemcisi Mac'e doğrudan IP ile bağlanır (Mac'te "Doğrudan IP erişimi" açık).
  Görüntü, kontrol ve şifreleme RustDesk'ten gelir.
- **Güven ve parola (AnyDesk akışı):**
  1. İlk bağlantı: Mac'te "BURHAN-PC bağlanmak istiyor — İzin ver / Her zaman güven" penceresi
     çıkar; Windows'ta Mac'in anahtar parmak izi gösterilir.
  2. "Her zaman güven" sonrası Windows cihaz anahtarı Mac'in güvenilenler listesine yazılır.
  3. Sonraki bağlantılar: katılımsız erişim parolası (Mac ajanında belirlenir).
  4. Parola Windows'ta düz metin tutulmaz (Windows Kimlik Bilgisi Yöneticisi / DPAPI).
  5. Hatalı parola denemelerine bekleme süresi; güvenilenler listesi ajandan yönetilir/silinir.
- **Görev çubuğu ikonları:** Her görev çubuğu (`Shell_TrayWnd` + her `Shell_SecondaryTrayWnd`) için
  ayrı ikon seti; her setin Başlat düğmesi UI Automation ile bulunur. Sıra: [Mac] [iPhone] [Başlat].
- **Görünürlük kuralı:** iPhone ikonu, iPhone bağlıyken (yansıtma oturumu ya da Bluetooth bağlantısı)
  görünür; ikisi de koptuğunda gizlenir. Mac ikonu Mac oturumu açıkken görünür. Ana uygulamaya
  her zaman bildirim alanındaki (saatin yanındaki) uygulama simgesinden ulaşılır.

## Fazlar

Her faz kendi başına çalışır durumda biter; faz sonunda commit (push yalnız onayla).

### F0 — Ön denemeler (kısa)
Riskli varsayımlar kod yazılmadan denenir:
- [ ] RustDesk istemcisi komut satırından Mac'e doğrudan IP + parola ile bağlanabiliyor mu
      (`--connect`), penceresi bizim pencereye gömülebiliyor mu?
- [ ] Mac'te RustDesk hizmeti: kalıcı parola ve doğrudan IP erişimi komut satırından ayarlanabiliyor mu?
- [ ] Tauri penceresinin üstüne UxPlay video çocuğunu gömme (WebView2 + yerel çocuk HWND).
- [ ] İkinci monitör görev çubuğunda Başlat düğmesi UI Automation ile bulunuyor mu?

Çıktı: kısa bulgu notu; bir varsayım tutmazsa plan güncellenir (ör. gömme olmazsa RustDesk ayrı
pencerede, bizim ikonla açılıp kapanır).

### F1 — Depo ve otomatik derleme
- [ ] Özel GitHub deposu (`gh repo create --private`), mevcut kod ve `bin\uxplay` (LFS ya da ayrı indirme).
- [ ] Çalışma alanı: `core` (paylaşılan), `app-windows`, `agent-mac`.
- [ ] GitHub Actions: Windows derlemesi + macOS derlemesi (Apple Silicon + Intel), sonuçlar artefakt.
- Not: İmzasız Mac uygulaması; ilk açılışta "Sağ tık → Aç" gerekir (Apple geliştirici hesabı ile
  imzalanabilir, şart değil).

### F2 — Tüm monitörlerde ikon + bağlantıya bağlı görünürlük (iPhone)
- [ ] Görev çubuğu modülü çoklu görev çubuğuna; her biri için ikon, Explorer yeniden başlayınca
      yeniden kurulum.
- [ ] iPhone bağlantı durumu (yansıtma + BLE) ikonun görünürlüğünü yönetir.
- Bu faz mevcut uygulamada yapılabilir; kullanıcıya en hızlı görünen kazanç.

### F3 — Ayna Merkezi ana uygulaması (Windows)
- [ ] Tauri 2 kabuğu, bildirim alanı simgesi, tek kopya.
- [ ] iPhone paneli: canlı görüntü uygulamanın içinde; "ayır" ile bugünkü yuvarlak pencereye dönüş.
- [ ] Ayarlar: boyut (kaydırıcı), Windows ile başlat, Büyütme yenileme, iCloud klasörü.
- [ ] Mevcut özellikler korunur: dokunma, klavye, Ctrl+V, kenar tuşları, sürükle-bırak, uyku engelleme.

### F4 — Mac ajanı (konsey kapısından sonra)
- [ ] Menü çubuğu uygulaması: cihaz kodu, katılımsız parola belirleme, güvenilenler listesi.
- [ ] RustDesk hizmetinin kurulumu/yapılandırması (doğrudan IP, kalıcı parola) ve macOS izinleri
      (Ekran Kaydı, Erişilebilirlik) için yönlendirme ekranı.
- [ ] Bonjour duyurusu (`_ayna._tcp`: kod, parmak izi, port).
- [ ] İlk bağlantıda onay penceresi.

### F5 — Windows'tan Mac'e bağlantı (konsey kapısından sonra)
- [ ] Mac paneli: kod gir → Bonjour'da bul → parmak izi göster → onay bekle → parola.
- [ ] RustDesk oturumu; pencere panele gömülü ya da ayrı (F0 sonucuna göre).
- [ ] Oturum açıkken Mac ikonu tüm görev çubuklarında, iPhone ikonunun yanında; kapanınca kaybolur.
- [ ] Parola Kimlik Bilgisi Yöneticisi'nde; "beni hatırla" isteğe bağlı.

### F6 — Güvenlik sertleştirme ve tam inceleme
- [ ] Bağlantı yalnız yerel ağ adreslerinden; Bonjour duyurusunda parola/sır yok.
- [ ] Hatalı parola için artan bekleme; güvenilen cihaz iptali.
- [ ] security-audit rehberlik modu + tam review boru hattı.

### F7 — Paketleme
- [ ] Windows: tek klasör/kurulum (uygulama + `bin\uxplay` + RustDesk istemcisi).
- [ ] Mac: `.dmg`; ilk açılış ve izin rehberi.

## Riskler

| Risk | Etki | Önlem |
| --- | --- | --- |
| RustDesk penceresi gömülemez | Mac görüntüsü panelde değil ayrı pencerede | F0'da dene; olmazsa ayrı pencereyi ikonla yönet |
| RustDesk CLI parola/ayar desteği sürüme göre değişir | Otomatik kurulum zorlaşır | Sürümü sabitle; F0'da doğrula |
| macOS izinleri (Ekran Kaydı, Erişilebilirlik) | İlk kurulumda elle onay şart | Ajanda adım adım rehber |
| İmzasız Mac uygulaması | Gatekeeper uyarısı | "Sağ tık → Aç" rehberi; istenirse geliştirici hesabıyla imza |
| Tauri'ye geçiş mevcut Win32 kodunu etkiler | Gerileme riski | F2'yi geçişten önce bitir; özellikleri adım adım taşı, her adımda test |
| AGPL | Dağıtımda kaynak açma yükümlülüğü | Kişisel kullanım; dağıtım kararı ayrıca |

## Süreç kapıları (ATAR)

- F4–F5 kritik alan (kimlik doğrulama, parola, güven) → uygulamadan önce **karar konseyi**.
- Her faz sonunda review (F4–F6 tam boru hattı + güvenlik adımı).
- Commit faz sonlarında otomatik; **push yalnız açık onayla**.

## Açık sorular

- [ ] Mac mini'nin macOS sürümü ve işlemcisi (Apple Silicon / Intel)?
- [ ] iPhone paneli varsayılan olarak ana pencerenin içinde mi, bugünkü gibi ayrı yuvarlak pencerede mi
      açılsın? (Plan: içinde; "ayır" seçeneği var.)
- [ ] Mac bağlantısında ses ve dosya aktarımı da istenir mi? (RustDesk destekliyor; kapsam kararı.)
