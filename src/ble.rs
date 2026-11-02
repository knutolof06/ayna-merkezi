//! PC'nin kendi Bluetooth LE radyosunu iPhone'a HID dokunmatik ekran olarak tanıtır.
//!
//! GATT kurulumu sakx7/iOSBleDigitizer (MIT) `windows/Program.cs` ile aynıdır (koruma
//! seviyeleri dahil): HID Information, Report Map, Control Point ve Protocol Mode açık; giriş
//! ve feature raporları şifreli. iPhone dokunuşları imleçsiz, AssistiveTouch olmadan işler.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use windows::core::{Result, GUID};
use windows::Devices::Bluetooth::GenericAttributeProfile::*;
use windows::Devices::Bluetooth::{
    BluetoothAdapter, BluetoothError, BluetoothLEDevice, BluetoothLEPreferredConnectionParameters,
    BluetoothLEPreferredConnectionParametersRequest,
};
use windows::Foundation::TypedEventHandler;
use windows::Storage::Streams::{DataWriter, IBuffer};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::hid;
use crate::util;

/// Durum değiştiğinde bildirim penceresine gönderilen mesaj.
pub const WM_APP_BLE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Starting,
    Advertising,
    Connected,
    Failed,
}

impl Status {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Status::Advertising,
            2 => Status::Connected,
            3 => Status::Failed,
            _ => Status::Starting,
        }
    }
}

/// Jestin bir adımı: parmak durumu ve konumu, ardından beklenecek süre.
#[derive(Clone, Copy)]
pub struct Step {
    pub down: bool,
    pub x: u16,
    pub y: u16,
    pub wait_ms: u64,
}

enum Cmd {
    Touch { down: bool, x: u16, y: u16 },
    Gesture(Vec<Step>),
    /// PC klavyesinin anlık durumu (basılı tuşlar).
    Keys([u8; hid::KEYBOARD_REPORT_LEN]),
    /// Metin yazdırma: (değiştiriciler, usage) vuruşları sırayla basılıp bırakılır.
    Type(Vec<(u8, u8)>),
    /// Kısa tuş dizisi: (değiştiriciler, usage, sonra beklenecek ms). Uyandırma, uyku engelleme,
    /// kısayollar.
    KeySeq(Vec<(u8, u8, u64)>),
    /// Tüketici tuşu (ses, güç) bas-bırak.
    Consumer(u16),
}

struct Shared {
    status: AtomicU8,
    error: Mutex<String>,
    notify_hwnd: AtomicIsize,
    /// Etkin "hızlı bağlantı" isteği; tek yuva, bağlantı kopunca kapatılır.
    link: Mutex<Option<(BluetoothLEDevice, BluetoothLEPreferredConnectionParametersRequest)>>,
    link_busy: AtomicBool,
    /// Süren metin yazdırmayı durdurur (Esc).
    typing_cancel: AtomicBool,
    /// Metin yazdırma sürüyor; bu sırada yeni yapıştırma ve tuşlar kabul edilmez.
    typing: AtomicBool,
    /// iPhone klavye raporuna abone (eski eşleşmede önbellekteki tanımda klavye yoksa değildir).
    keyboard_subscribed: AtomicBool,
    /// iPhone tüketici (ses/güç) raporuna abone.
    consumer_subscribed: AtomicBool,
}

impl Shared {
    fn drop_link(&self) {
        if let Some((device, request)) = self.link.lock().unwrap().take() {
            let _ = request.Close();
            let _ = device.Close();
        }
    }
}

impl Shared {
    fn set_status(&self, s: Status) {
        if self.status.swap(s as u8, Ordering::SeqCst) != s as u8 {
            self.poke();
        }
    }

    fn fail(&self, msg: String) {
        *self.error.lock().unwrap() = msg;
        self.set_status(Status::Failed);
    }

    fn poke(&self) {
        let h = self.notify_hwnd.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(HWND(h as _), WM_APP_BLE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// UI iş parçacığından kullanılan tutamaç. Gönderimler kuyruğa girer, BLE iş parçacığı yollar.
pub struct Ble {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
}

impl Ble {
    pub fn start() -> Ble {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            status: AtomicU8::new(Status::Starting as u8),
            error: Mutex::new(String::new()),
            notify_hwnd: AtomicIsize::new(0),
            link: Mutex::new(None),
            link_busy: AtomicBool::new(false),
            typing_cancel: AtomicBool::new(false),
            typing: AtomicBool::new(false),
            keyboard_subscribed: AtomicBool::new(false),
            consumer_subscribed: AtomicBool::new(false),
        });
        let worker = shared.clone();
        thread::spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            // Oturum açılışında radyo henüz hazır olmayabilir ya da kullanıcı Bluetooth'u
            // kapatıp açabilir: kurulamazsa 5 sn sonra yeniden denenir.
            loop {
                match Peripheral::create(worker.clone()) {
                    Ok(p) => {
                        p.run(&rx);
                        return;
                    }
                    Err(e) => {
                        worker.fail(e);
                        thread::sleep(Duration::from_secs(5));
                        while rx.try_recv().is_ok() {}
                    }
                }
            }
        });
        Ble { tx, shared }
    }

    pub fn set_notify_window(&self, hwnd: HWND) {
        self.shared.notify_hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
    }

    pub fn status(&self) -> Status {
        Status::from_u8(self.shared.status.load(Ordering::SeqCst))
    }

    pub fn error(&self) -> String {
        self.shared.error.lock().unwrap().clone()
    }

    pub fn connected(&self) -> bool {
        self.status() == Status::Connected
    }

    /// Parmak (x, y) noktasında basılı (`down`) ya da oradan kalkıyor.
    pub fn touch(&self, down: bool, x: u16, y: u16) {
        let _ = self.tx.send(Cmd::Touch { down, x, y });
    }

    /// Zamanlaması önemli dokunma dizisi (ana ekran kaydırması, uzun basma).
    pub fn gesture(&self, steps: Vec<Step>) {
        let _ = self.tx.send(Cmd::Gesture(steps));
    }

    pub fn keys(&self, report: [u8; hid::KEYBOARD_REPORT_LEN]) {
        let _ = self.tx.send(Cmd::Keys(report));
    }

    /// Tuş vuruşlarını sırayla yazar (Windows panosunu iPhone'a yapıştırmak için). Bir yazma
    /// sürerken çağrılırsa yok sayılır ve false döner.
    pub fn type_keys(&self, strokes: Vec<(u8, u8)>) -> bool {
        // Bayrak burada (UI iş parçacığında) kurulur: Esc'den hemen sonraki yeni yapıştırma,
        // henüz durmamış eski yazmanın durdurma isteğini silemesin.
        if self.shared.typing.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.shared.typing_cancel.store(false, Ordering::SeqCst);
        let _ = self.tx.send(Cmd::Type(strokes));
        true
    }

    /// iPhone'un ses ya da güç tuşuna basıp bırakır.
    pub fn consumer(&self, usage: u16) {
        let _ = self.tx.send(Cmd::Consumer(usage));
    }

    /// Kısa bir tuş dizisi gönderir (her vuruş basılıp bırakılır, ardından beklenir).
    pub fn key_sequence(&self, strokes: Vec<(u8, u8, u64)>) {
        let _ = self.tx.send(Cmd::KeySeq(strokes));
    }

    pub fn typing(&self) -> bool {
        self.shared.typing.load(Ordering::SeqCst)
    }

    /// iPhone klavye raporunu dinliyor mu? Dinlemiyorsa iPhone eski (klavyesiz) cihaz tanımını
    /// önbellekte tutuyordur: Bluetooth'tan bu bilgisayarı unutup yeniden eşlemek gerekir.
    pub fn keyboard_ready(&self) -> bool {
        self.shared.keyboard_subscribed.load(Ordering::SeqCst)
    }

    /// iPhone ses/güç raporunu dinliyor mu? Dinlemiyorsa eski cihaz tanımı önbellektedir.
    pub fn consumer_ready(&self) -> bool {
        self.shared.consumer_subscribed.load(Ordering::SeqCst)
    }

    pub fn cancel_typing(&self) {
        self.shared.typing_cancel.store(true, Ordering::SeqCst);
    }
}

struct Peripheral {
    provider: GattServiceProvider,
    shared: Arc<Shared>,
    input: GattLocalCharacteristic,
    keyboard: GattLocalCharacteristic,
    consumer: GattLocalCharacteristic,
    _battery: Option<GattServiceProvider>,
}

/// iOS bağlantıyı varsayılan olarak 15-30 ms aralıkla kurar; her dokunma raporu bir aralık
/// bekler. Windows'tan iPhone'a daha kısa bağlantı aralığı istenir (Windows 11 22H2+; iPhone
/// reddedebilir). Gerçekleşen aralık sorun bildirimi için günlüğe yazılır.
fn request_fast_link(shared: &Shared, ch: &GattLocalCharacteristic) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    shared.drop_link();
    let Ok(clients) = ch.SubscribedClients() else { return };
    // Yalnız ilk istemci: bu uygulama tek iPhone'a bağlanır.
    if let Some(client) = clients.into_iter().next() {
        let Ok(id) = client.Session().and_then(|s| s.DeviceId()).and_then(|d| d.Id()) else { return };
        let Ok(device) = BluetoothLEDevice::FromIdAsync(&id).and_then(|op| op.get()) else { return };
        let interval = |d: &BluetoothLEDevice| {
            d.GetConnectionParameters().map(|p| p.ConnectionInterval().unwrap_or(0) as f32 * 1.25).unwrap_or(0.0)
        };
        let before = interval(&device);
        let request = BluetoothLEPreferredConnectionParameters::ThroughputOptimized()
            .and_then(|p| device.RequestPreferredConnectionParameters(&p));
        match &request {
            Ok(r) => {
                thread::sleep(Duration::from_millis(1500));
                util::log(&format!(
                    "BLE bağlantı aralığı: {before:.2} ms -> {:.2} ms (istek durumu {})",
                    interval(&device),
                    r.Status().map(|s| s.0).unwrap_or(-1)
                ));
            }
            Err(e) => util::log(&format!("BLE bağlantı aralığı {before:.2} ms; hız isteği yapılamadı: {}", e.message())),
        }
        // İstek nesnesi açık kaldığı sürece tercih geçerlidir; bağlantı kopunca kapatılır.
        if let Ok(r) = request {
            *shared.link.lock().unwrap() = Some((device, r));
        }
    }
}

fn start_advertising(provider: &GattServiceProvider) -> Result<()> {
    let params = GattServiceProviderAdvertisingParameters::new()?;
    params.SetIsConnectable(true)?;
    params.SetIsDiscoverable(true)?;
    provider.StartAdvertisingWithParameters(&params)
}

fn uuid16(id: u16) -> GUID {
    GUID::from_values(id as u32, 0x0000, 0x1000, [0x80, 0x00, 0x00, 0x80, 0x5F, 0x9B, 0x34, 0xFB])
}

fn buffer(bytes: &[u8]) -> Result<IBuffer> {
    let w = DataWriter::new()?;
    w.WriteBytes(bytes)?;
    w.DetachBuffer()
}

const ENCRYPTED: GattProtectionLevel = GattProtectionLevel::EncryptionRequired;
const PLAIN: GattProtectionLevel = GattProtectionLevel::Plain;

fn check(step: &str, err: BluetoothError) -> std::result::Result<(), String> {
    if err == BluetoothError::Success {
        Ok(())
    } else {
        Err(format!("{step}: Bluetooth hatası {}", err.0))
    }
}

fn win(step: &str) -> impl Fn(windows::core::Error) -> String + '_ {
    move |e| format!("{step}: {}", e.message())
}

impl Peripheral {
    fn create(shared: Arc<Shared>) -> std::result::Result<Peripheral, String> {
        let adapter = BluetoothAdapter::GetDefaultAsync()
            .and_then(|op| op.get())
            .map_err(|_| "Bluetooth adaptörü bulunamadı veya kapalı.".to_string())?;
        if !adapter.IsLowEnergySupported().unwrap_or(false)
            || !adapter.IsPeripheralRoleSupported().unwrap_or(false)
        {
            return Err("Bu Bluetooth adaptörü çevre birimi (peripheral) rolünü desteklemiyor.".into());
        }

        let created = GattServiceProvider::CreateAsync(uuid16(0x1812))
            .and_then(|op| op.get())
            .map_err(win("HID servisi"))?;
        check("HID servisi", created.Error().map_err(win("HID servisi"))?)?;
        let provider = created.ServiceProvider().map_err(win("HID servisi"))?;
        let service = provider.Service().map_err(win("HID servisi"))?;

        readable(&service, 0x2A4A, hid::HID_INFORMATION.to_vec()).map_err(win("HID Information"))?;
        readable(&service, 0x2A4B, hid::report_map()).map_err(win("Report Map"))?;
        control_point(&service).map_err(win("HID Control Point"))?;
        protocol_mode(&service).map_err(win("Protocol Mode"))?;
        let input = input_report(&service, hid::REPORT_TOUCH, hid::TOUCH_REPORT_LEN).map_err(win("Dokunma raporu"))?;
        feature_report(&service).map_err(win("Feature raporu"))?;
        let keyboard = input_report(&service, hid::REPORT_KEYBOARD, hid::KEYBOARD_REPORT_LEN)
            .map_err(win("Klavye raporu"))?;
        let consumer = input_report(&service, hid::REPORT_CONSUMER, hid::CONSUMER_REPORT_LEN)
            .map_err(win("Tüketici raporu"))?;
        let cs_shared = shared.clone();
        consumer
            .SubscribedClientsChanged(&TypedEventHandler::new(
                move |sender: &Option<GattLocalCharacteristic>, _| {
                    let n = sender.as_ref().map_or(0, subscribers);
                    cs_shared.consumer_subscribed.store(n > 0, Ordering::SeqCst);
                    Ok(())
                },
            ))
            .map_err(win("Tüketici abonelik olayı"))?;
        let kb_shared = shared.clone();
        keyboard
            .SubscribedClientsChanged(&TypedEventHandler::new(
                move |sender: &Option<GattLocalCharacteristic>, _| {
                    let n = sender.as_ref().map_or(0, subscribers);
                    kb_shared.keyboard_subscribed.store(n > 0, Ordering::SeqCst);
                    Ok(())
                },
            ))
            .map_err(win("Klavye abonelik olayı"))?;
        let battery = battery_service().ok();

        // iPhone abone olduğunda (eşleşip bağlandığında) durum "bağlı" olur.
        let sub_shared = shared.clone();
        input
            .SubscribedClientsChanged(&TypedEventHandler::new(
                move |sender: &Option<GattLocalCharacteristic>, _| {
                    let subscribed = sender
                        .as_ref()
                        .is_some_and(|c| c.SubscribedClients().and_then(|l| l.Size()).unwrap_or(0) > 0);
                    let was = sub_shared.status.load(Ordering::SeqCst) == Status::Connected as u8;
                    if was != subscribed {
                        util::log(if subscribed { "iPhone Bluetooth ile bağlandı." } else { "iPhone Bluetooth bağlantısı koptu." });
                    }
                    sub_shared.set_status(if subscribed { Status::Connected } else { Status::Advertising });
                    if !subscribed {
                        sub_shared.drop_link();
                    } else if let Some(ch) = sender.clone() {
                        // Bağlantı başına tek istek: aynı anda yalnız bir iş parçacığı.
                        if !sub_shared.link_busy.swap(true, Ordering::SeqCst) {
                            let shared = sub_shared.clone();
                            thread::spawn(move || {
                                request_fast_link(&shared, &ch);
                                shared.link_busy.store(false, Ordering::SeqCst);
                            });
                        }
                    }
                    Ok(())
                },
            ))
            .map_err(win("Abonelik olayı"))?;

        let adv_shared = shared.clone();
        provider
            .AdvertisementStatusChanged(&TypedEventHandler::new(
                move |_, args: &Option<GattServiceProviderAdvertisementStatusChangedEventArgs>| {
                    if let Some(args) = args {
                        let st = args.Status()?;
                        if st == GattServiceProviderAdvertisementStatus::Started
                            && adv_shared.status.load(Ordering::SeqCst) != Status::Connected as u8
                        {
                            adv_shared.set_status(Status::Advertising);
                        }
                    }
                    Ok(())
                },
            ))
            .map_err(win("Yayın olayı"))?;

        start_advertising(&provider).map_err(win("Yayın başlatma"))?;
        if provider.AdvertisementStatus().ok() == Some(GattServiceProviderAdvertisementStatus::Started) {
            shared.set_status(Status::Advertising);
        }

        Ok(Peripheral { provider, shared, input, keyboard, consumer, _battery: battery })
    }

    fn run(&self, rx: &Receiver<Cmd>) {
        let mut last_down = false;
        loop {
            let first = match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(c) => c,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.keep_advertising();
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
            let mut batch = vec![first];
            while let Ok(c) = rx.try_recv() {
                batch.push(c);
            }
            if subscribers(&self.input) == 0 {
                last_down = false;
            }
            // Bildirim beklerken biriken sürükleme konumları seyreltilir. Yalnız parmak zaten
            // basılıyken ardından yine basılı bir konum gelen raporlar düşer: basma ve kaldırma
            // her zaman kendi konumlarıyla gider, yoksa dokunma iPhone'da kaydırmaya dönüşürdü.
            for i in 0..batch.len() {
                match &batch[i] {
                    Cmd::Touch { down, x, y } => {
                        let next_down = matches!(batch.get(i + 1), Some(Cmd::Touch { down: true, .. }));
                        if *down && last_down && next_down {
                            continue;
                        }
                        last_down = *down;
                        self.report(*down, *x, *y);
                    }
                    Cmd::Gesture(steps) => {
                        self.play(steps);
                        last_down = false;
                    }
                    Cmd::Keys(report) => {
                        notify(&self.keyboard, report);
                    }
                    Cmd::Type(strokes) => {
                        self.type_strokes(strokes, rx);
                        last_down = false;
                    }
                    Cmd::Consumer(usage) => self.press_consumer(*usage),
                    Cmd::KeySeq(strokes) => {
                        for &(modifiers, usage, wait_ms) in strokes {
                            // Bir vuruş iletilemezse dizinin kalanı da gönderilmez: yarım kalan
                            // aç/kapa çifti bir ayarı ters durumda bırakmasın.
                            if !notify(&self.keyboard, &hid::key_report(modifiers, usage)) {
                                break;
                            }
                            notify(&self.keyboard, &[0; hid::KEYBOARD_REPORT_LEN]);
                            if wait_ms > 0 {
                                thread::sleep(Duration::from_millis(wait_ms));
                            }
                        }
                    }
                }
            }
        }
    }

    fn press_consumer(&self, usage: u16) {
        if notify(&self.consumer, &hid::consumer_report(usage)) {
            thread::sleep(Duration::from_millis(30));
        }
        // Bırakma her durumda gönderilir: tuş iPhone'da basılı kalmasın.
        notify(&self.consumer, &hid::consumer_report(0));
    }

    fn play(&self, steps: &[Step]) {
        for st in steps {
            self.report(st.down, st.x, st.y);
            if st.wait_ms > 0 {
                thread::sleep(Duration::from_millis(st.wait_ms));
            }
        }
    }

    /// Her karakter basılıp bırakılır; aynı harf art arda gelse de ayrı vuruş olur. Uzun metin
    /// dakikalar sürebilir: araya giren dokunma ve jestler hemen işlenir, tuşlar ve yeni
    /// yapıştırmalar (UI zaten göndermez) yok sayılır.
    fn type_strokes(&self, strokes: &[(u8, u8)], rx: &Receiver<Cmd>) {
        for &(modifiers, usage) in strokes {
            if self.shared.typing_cancel.load(Ordering::SeqCst) {
                break;
            }
            notify(&self.keyboard, &hid::key_report(modifiers, usage));
            notify(&self.keyboard, &[0; hid::KEYBOARD_REPORT_LEN]);
            while let Ok(cmd) = rx.try_recv() {
                match cmd {
                    Cmd::Touch { down, x, y } => self.report(down, x, y),
                    Cmd::Gesture(steps) => self.play(&steps),
                    // Güç tuşu iPhone'u kilitler, kalan vuruşlar kilit ekranına giderdi.
                    Cmd::Keys(_) | Cmd::Type(_) | Cmd::KeySeq(_) | Cmd::Consumer(_) => {}
                }
            }
        }
        notify(&self.keyboard, &[0; hid::KEYBOARD_REPORT_LEN]);
        self.shared.typing.store(false, Ordering::SeqCst);
    }

    /// Uyku dönüşü ya da radyo kapat-aç sonrası durmuş yayını yeniden başlatır.
    fn keep_advertising(&self) {
        if self.shared.status.load(Ordering::SeqCst) == Status::Connected as u8 {
            return;
        }
        match self.provider.AdvertisementStatus() {
            Ok(GattServiceProviderAdvertisementStatus::Started) => self.shared.set_status(Status::Advertising),
            Ok(_) => {
                let _ = start_advertising(&self.provider);
            }
            Err(_) => {}
        }
    }

    fn report(&self, down: bool, x: u16, y: u16) {
        let data = hid::touch_report(down, x, y);
        if !notify(&self.input, &data) && !down {
            thread::sleep(Duration::from_millis(15));
            notify(&self.input, &data);
        }
    }
}

/// Bildirim, raporu bağlı istemcilere ulaştırana kadar bekler; bu da gönderim hızını
/// bağlantı aralığına doğal olarak eşitler.
/// Tüm abonelere ulaştıysa true döner.
fn notify(ch: &GattLocalCharacteristic, data: &[u8]) -> bool {
    if subscribers(ch) == 0 {
        return false;
    }
    let Ok(results) = buffer(data).and_then(|buf| ch.NotifyValueAsync(&buf)).and_then(|op| op.get()) else {
        return false;
    };
    results
        .into_iter()
        .all(|r| r.Status().is_ok_and(|s| s == windows::Devices::Bluetooth::GenericAttributeProfile::GattCommunicationStatus::Success))
}

fn subscribers(ch: &GattLocalCharacteristic) -> u32 {
    ch.SubscribedClients().and_then(|l| l.Size()).unwrap_or(0)
}

fn characteristic(
    service: &GattLocalService,
    uuid: u16,
    params: &GattLocalCharacteristicParameters,
) -> Result<GattLocalCharacteristic> {
    let result = service.CreateCharacteristicAsync(uuid16(uuid), params)?.get()?;
    let err = result.Error()?;
    if err != BluetoothError::Success {
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            format!("0x{uuid:04X} oluşturulamadı (Bluetooth hatası {})", err.0),
        ));
    }
    result.Characteristic()
}

fn readable(service: &GattLocalService, uuid: u16, value: Vec<u8>) -> Result<()> {
    let p = GattLocalCharacteristicParameters::new()?;
    p.SetCharacteristicProperties(GattCharacteristicProperties::Read)?;
    p.SetReadProtectionLevel(PLAIN)?;
    let ch = characteristic(service, uuid, &p)?;
    respond_to_reads(&ch, value)
}

fn respond_to_writes(ch: &GattLocalCharacteristic) -> Result<()> {
    ch.WriteRequested(&TypedEventHandler::new(
        |_, args: &Option<GattWriteRequestedEventArgs>| {
            if let Some(args) = args {
                let deferral = args.GetDeferral()?;
                let request = args.GetRequestAsync()?.get()?;
                if request.Option()? == GattWriteOption::WriteWithResponse {
                    request.Respond()?;
                }
                deferral.Complete()?;
            }
            Ok(())
        },
    ))?;
    Ok(())
}

fn respond_to_reads(ch: &GattLocalCharacteristic, value: Vec<u8>) -> Result<()> {
    ch.ReadRequested(&TypedEventHandler::new(
        move |_, args: &Option<GattReadRequestedEventArgs>| {
            if let Some(args) = args {
                let deferral = args.GetDeferral()?;
                let request = args.GetRequestAsync()?.get()?;
                request.RespondWithValue(&buffer(&value)?)?;
                deferral.Complete()?;
            }
            Ok(())
        },
    ))?;
    Ok(())
}

fn control_point(service: &GattLocalService) -> Result<()> {
    let p = GattLocalCharacteristicParameters::new()?;
    p.SetCharacteristicProperties(GattCharacteristicProperties::WriteWithoutResponse)?;
    let ch = characteristic(service, 0x2A4C, &p)?;
    respond_to_writes(&ch)
}

fn protocol_mode(service: &GattLocalService) -> Result<()> {
    let p = GattLocalCharacteristicParameters::new()?;
    p.SetCharacteristicProperties(
        GattCharacteristicProperties::Read | GattCharacteristicProperties::WriteWithoutResponse,
    )?;
    let ch = characteristic(service, 0x2A4E, &p)?;
    respond_to_reads(&ch, vec![1])?; // report protocol
    respond_to_writes(&ch)
}

fn report_reference(ch: &GattLocalCharacteristic, report_id: u8, kind: u8) -> Result<()> {
    let d = GattLocalDescriptorParameters::new()?;
    d.SetReadProtectionLevel(ENCRYPTED)?;
    d.SetStaticValue(&buffer(&[report_id, kind])?)?;
    let r = ch.CreateDescriptorAsync(uuid16(0x2908), &d)?.get()?;
    if r.Error()? != BluetoothError::Success {
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            format!("Report Reference {report_id} oluşturulamadı"),
        ));
    }
    Ok(())
}

fn input_report(service: &GattLocalService, report_id: u8, len: usize) -> Result<GattLocalCharacteristic> {
    let p = GattLocalCharacteristicParameters::new()?;
    p.SetCharacteristicProperties(GattCharacteristicProperties::Read | GattCharacteristicProperties::Notify)?;
    p.SetReadProtectionLevel(ENCRYPTED)?;
    let ch = characteristic(service, 0x2A4D, &p)?;
    report_reference(&ch, report_id, 0x01)?; // tür: giriş
    respond_to_reads(&ch, vec![0; len])?;
    Ok(ch)
}

/// En fazla temas sayısını bildiren feature raporu (Contact Count Maximum).
fn feature_report(service: &GattLocalService) -> Result<()> {
    let p = GattLocalCharacteristicParameters::new()?;
    p.SetCharacteristicProperties(GattCharacteristicProperties::Read | GattCharacteristicProperties::Write)?;
    p.SetReadProtectionLevel(ENCRYPTED)?;
    p.SetWriteProtectionLevel(ENCRYPTED)?;
    let ch = characteristic(service, 0x2A4D, &p)?;
    report_reference(&ch, hid::REPORT_FEATURE, 0x03)?; // tür: feature
    respond_to_reads(&ch, vec![hid::CONTACT_SLOTS as u8])?;
    respond_to_writes(&ch)
}

/// Ayrı sağlayıcıda pil servisi; görünür yayın yapmaz, yalnız GATT veritabanına eklenir.
fn battery_service() -> Result<GattServiceProvider> {
    let created = GattServiceProvider::CreateAsync(uuid16(0x180F))?.get()?;
    let provider = created.ServiceProvider()?;
    let p = GattLocalCharacteristicParameters::new()?;
    p.SetCharacteristicProperties(GattCharacteristicProperties::Read)?;
    p.SetReadProtectionLevel(PLAIN)?;
    p.SetStaticValue(&buffer(&[100])?)?;
    characteristic(&provider.Service()?, 0x2A19, &p)?;
    let params = GattServiceProviderAdvertisingParameters::new()?;
    params.SetIsConnectable(false)?;
    params.SetIsDiscoverable(false)?;
    provider.StartAdvertisingWithParameters(&params)?;
    Ok(provider)
}
