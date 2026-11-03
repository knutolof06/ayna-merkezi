// Ayna Ajan: Mac'in menü çubuğunda duran ve Mac'i Windows'taki Ayna Merkezi'ne açan program.
// Başka program gerekmez: ekran yakalama (ScreenCaptureKit), donanım H.264 kodlama (VideoToolbox),
// fare/klavye (CGEvent) ve şifreli bağlantı (CryptoKit) burada.
//
// Protokol (AYNA2), Windows tarafı src/mac.rs ile birebir:
// - Bulma, UDP 21130:
//     "AYNA1 FIND <kod> <cihaz> <nonce>" → "AYNA1 HERE <kod> <port> <imza|-> <ad>".
//   Eşleşilmiş bilgisayara giden yanıt, onun anahtarıyla imzalanır:
//     HMAC(anahtar, "here|kod|nonce|port|ad").
// - El sıkışma, TCP 21130 (satırlar):
//     Windows → "AYNA2 COMMIT <cihaz> <SHA256(win-açık|cihaz)>"   (anahtara önceden bağlanma)
//     Mac → "AYNA2 HI <nonce> <mac-açık>"
//     Windows → "AYNA2 HELLO <cihaz> <imza|-> <win-açık> <ad>"   (Mac özeti doğrular)
//   Geçici ECDH P-256 ile ortak sır elde edilir; T = "AYNA2|nonce|mac-açık|win-açık|cihaz|ad".
//   Windows anahtarını Mac'inkini görmeden bağladığı için aradaki biri doğrulama kodunu
//   denenerek eşitleyemez; Mac de el sıkışmaları sınırlar.
//   HKDF-SHA256 (tuz yok) şunları türetir:
//     "keys|T"  → 64 bayt: [0..32] Windows→Mac, [32..64] Mac→Windows (AES-256-GCM)
//     "sas|T"   → 4 bayt: 6 haneli doğrulama kodu
//     "token|T" → 32 bayt: yeni eşleşme anahtarı
//   Eşleşilmiş bilgisayar HELLO'yu HMAC(anahtar, "hello|T") ile imzalar.
//   Mac yanıtı: "AYNA2 WAIT" … "AYNA2 OK NEW|SAME <HMAC(anahtar, "ok|T")>" | DENY | OFF | BUSY.
// - Şifreli çerçeveler: u32 uzunluk || şifreli metin || etiket.
//   nonce = 4 sıfır bayt || u64 sayaç (yön başına).
// - İmleç: Windows → Mac 0x1A LOCAL_CURSOR (Windows imleci kendisi çizer); bundan sonra Mac imleci
//   görüntüye çizilmez ve Mac → Windows 0x06 CURSOR [sıcak x][sıcak y][gen][yük][BGRA önceden
//   çarpılmış, görüntü pikseli ölçeğinde] gider; gen 0 = Windows kendi okunu göstersin. Bunu
//   bildirmeyen eski Windows için imleç görüntüde kalır.
// - Tanı: Mac → Windows 0x07 DIAG [ana iş parçacığında en uzun bekleme ms u16][bayraklar] her 10 sn;
//   bayrak 1 = sistem imleci okunamadı. Windows günlüğe yazar (takılmaların nedeni görülsün).
// - Sürükleme önizlemesi: Windows → Mac 0x25 HOVER [x][y][sayı][klasör mü][ad uz.][ad], 0x26 HOVER_END:
//   Windows'tan sürüklenen dosyanın simgesi ve adı Mac'te imlecin altında görünür.
// - iPhone kipi: Windows → Mac 0x18 MODE [0 Mac ekranı | 1 iPhone]; Mac → Windows 0x05 STATE
//   [kip][başarılı][metin]. iPhone kipinde görüntü ve fare, görünmez bir sanal ekranda açılan
//   iPhone Mirroring penceresine aittir.
// - Dosyalar (Windows → Mac): 0x20 PUT_BEGIN [toplu][no][boy u64][x][y][ad uz.][göreli ad],
//   0x21 PUT_DATA [no][veri], 0x22 PUT_END [no]; Mac her dosyadan sonra 0x35 PUT_DONE
//   [toplu][biten sayısı][hedef klasör]. Hedef, bırakılan noktadaki Finder penceresi ya da Masaüstü.
// - Dosyalar (Mac → Windows, sürükleyip pencereden çıkarma): 0x23 DRAG_QUERY → 0x30 DRAG_LIST
//   [sayı]{[boy u64][ad uz.][göreli ad]}; Windows her biri için 0x24 GET [sıra] ister, Mac 0x31
//   GET_BEGIN [sıra][boy], 0x32 GET_DATA [sıra][veri], 0x33 GET_END | 0x34 GET_ERROR [sıra] yollar.

import AppKit
import CoreMedia
import CryptoKit
import Network
import ScreenCaptureKit
import Security
import ServiceManagement
import VideoToolbox

let agentPort: UInt16 = 21130
/// Görüntünün en uzun kenarı (bant genişliği ve gecikme dengesi).
let maxVideoSide = 1920

// MARK: - Yardımcılar

func hex<S: Sequence>(_ bytes: S) -> String where S.Element == UInt8 { bytes.map { String(format: "%02x", $0) }.joined() }

func unhex(_ s: String) -> Data? {
    guard s.count % 2 == 0 else { return nil }
    var out = Data()
    var i = s.startIndex
    while i < s.endIndex {
        let j = s.index(i, offsetBy: 2)
        guard let b = UInt8(s[i..<j], radix: 16) else { return nil }
        out.append(b)
        i = j
    }
    return out
}

func isHex(_ s: String, length: Int) -> Bool { s.count == length && s.allSatisfy(\.isHexDigit) }

func randomBytes(_ count: Int) -> Data {
    var g = SystemRandomNumberGenerator()
    return Data((0..<count).map { _ in UInt8.random(in: 0...255, using: &g) })
}

func hmacHex(key: Data, message: String) -> String {
    hex(HMAC<SHA256>.authenticationCode(for: Data(message.utf8), using: SymmetricKey(data: key)))
}

func hkdf(_ ikm: Data, _ info: String, _ count: Int) -> Data {
    HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: ikm), info: Data(info.utf8), outputByteCount: count)
        .withUnsafeBytes { Data($0) }
}

/// Sabit sürede karşılaştırma.
func sameProof(_ a: String, _ b: String) -> Bool {
    let x = Array(a.utf8), y = Array(b.utf8)
    guard x.count == y.count else { return false }
    return zip(x, y).reduce(0) { $0 | ($1.0 ^ $1.1) } == 0
}

/// Ekranda gösterilen adlardan denetim, görünmez ve yön değiştiren karakterler atılır.
func cleanName(_ s: String) -> String {
    let scalars = s.unicodeScalars.filter { sc in
        switch sc.properties.generalCategory {
        case .control, .format, .lineSeparator, .paragraphSeparator: return false
        default: return true
        }
    }
    let cleaned = String(String(String.UnicodeScalarView(scalars)).prefix(64)).trimmingCharacters(in: .whitespaces)
    return cleaned.isEmpty ? "Windows" : cleaned
}

func be16(_ v: UInt16) -> [UInt8] { [UInt8(v >> 8), UInt8(v & 0xFF)] }

func readBE16(_ d: Data, _ at: Int) -> UInt16 {
    let b = [UInt8](d)
    return UInt16(b[at]) << 8 | UInt16(b[at + 1])
}

func be32(_ v: UInt32) -> [UInt8] { [UInt8(v >> 24), UInt8((v >> 16) & 0xFF), UInt8((v >> 8) & 0xFF), UInt8(v & 0xFF)] }

func be64(_ v: UInt64) -> [UInt8] { (0..<8).map { UInt8((v >> (56 - 8 * UInt64($0))) & 0xFF) } }

func readBE32(_ b: [UInt8], _ at: Int) -> UInt32 {
    UInt32(b[at]) << 24 | UInt32(b[at + 1]) << 16 | UInt32(b[at + 2]) << 8 | UInt32(b[at + 3])
}

func readBE64(_ b: [UInt8], _ at: Int) -> UInt64 {
    (0..<8).reduce(UInt64(0)) { $0 << 8 | UInt64(b[at + $1]) }
}

// MARK: - Kalıcı ayarlar

struct TrustedDevice: Codable, Equatable {
    var id: String
    var name: String
    var ip: String
    /// Bu bilgisayarla eşleşmenin anahtarı (64 onaltılık).
    var token: String
}

enum Store {
    static let defaults = UserDefaults.standard

    /// Bu Mac'e özel, değişmeyen 9 haneli kod.
    static var code: String {
        if let c = defaults.string(forKey: "code"), c.count == 9, c.allSatisfy(\.isNumber) { return c }
        var g = SystemRandomNumberGenerator()
        let c = String(UInt32.random(in: 100_000_000...999_999_999, using: &g))
        defaults.set(c, forKey: "code")
        return c
    }

    static var trusted: [TrustedDevice] {
        get {
            guard let data = defaults.data(forKey: "trusted2"),
                  let list = try? JSONDecoder().decode([TrustedDevice].self, from: data) else { return [] }
            return list
        }
        set { defaults.set(try? JSONEncoder().encode(newValue), forKey: "trusted2") }
    }

    static var accepting: Bool {
        get { defaults.object(forKey: "accepting") as? Bool ?? true }
        set { defaults.set(newValue, forKey: "accepting") }
    }
}

// MARK: - Yerel ağ

func ipValue(_ ip: String) -> UInt32? {
    var a = in_addr()
    guard inet_pton(AF_INET, ip, &a) == 1 else { return nil }
    return UInt32(bigEndian: a.s_addr)
}

/// Bu Mac'in IPv4 alt ağları (ağ adresi, maske).
func localSubnets() -> [(UInt32, UInt32)] {
    var out: [(UInt32, UInt32)] = []
    var ifap: UnsafeMutablePointer<ifaddrs>?
    guard getifaddrs(&ifap) == 0, let first = ifap else { return out }
    defer { freeifaddrs(ifap) }
    var p: UnsafeMutablePointer<ifaddrs>? = first
    while let cur = p {
        let i = cur.pointee
        if let a = i.ifa_addr, a.pointee.sa_family == sa_family_t(AF_INET), let m = i.ifa_netmask {
            let addr = a.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { UInt32(bigEndian: $0.pointee.sin_addr.s_addr) }
            let mask = m.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { UInt32(bigEndian: $0.pointee.sin_addr.s_addr) }
            if addr >> 24 != 127 && mask != 0 {
                out.append((addr & mask, mask))
            }
        }
        p = i.ifa_next
    }
    return out
}

/// Yalnız bu Mac'in kendi alt ağlarındaki özel adresler kabul edilir.
func isLocalPeer(_ ip: String) -> Bool {
    guard let v = ipValue(ip) else { return false }
    let o = v >> 24, o2 = (v >> 16) & 0xFF
    let privateRange = o == 10 || (o == 172 && (16...31).contains(o2)) || (o == 192 && o2 == 168) || (o == 169 && o2 == 254)
    return privateRange && localSubnets().contains { v & $0.1 == $0.0 }
}

func ipv4String(_ endpoint: NWEndpoint) -> String? {
    guard case let .hostPort(host, _) = endpoint else { return nil }
    switch host {
    case let .ipv4(a):
        return "\(a)".split(separator: "%").first.map(String.init)
    case let .ipv6(a):
        guard let v4 = a.asIPv4 else { return nil }
        return "\(v4)".split(separator: "%").first.map(String.init)
    default:
        return nil
    }
}

/// UDP bulma yanıtlayıcısı.
final class Discovery {
    private var fd: Int32 = -1
    private var source: DispatchSourceRead?
    private let reply: (String, String) -> String?

    init?(port: UInt16, reply: @escaping (_ message: String, _ fromIP: String) -> String?) {
        self.reply = reply
        fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)
        guard fd >= 0 else { return nil }
        var yes: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout<Int32>.size))
        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = port.bigEndian
        addr.sin_addr = in_addr(s_addr: INADDR_ANY)
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0 else { close(fd); return nil }
        let s = DispatchSource.makeReadSource(fileDescriptor: fd, queue: .main)
        s.setEventHandler { [weak self] in self?.receive() }
        s.resume()
        source = s
    }

    private func receive() {
        let capacity = 512
        var buf = [UInt8](repeating: 0, count: capacity)
        var from = sockaddr_in()
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        let n = withUnsafeMutablePointer(to: &from) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { recvfrom(fd, &buf, capacity, 0, $0, &len) }
        }
        guard n > 0 else { return }
        let message = String(decoding: buf[0..<n], as: UTF8.self)
        let ip = String(cString: inet_ntoa(from.sin_addr))
        // Önce ucuz ayrıştırma (yanıt yoksa ağ arayüzleri hiç taranmaz).
        guard let answer = reply(message, ip), isLocalPeer(ip) else { return }
        let bytes = Array(answer.utf8)
        _ = withUnsafePointer(to: &from) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { sendto(fd, bytes, bytes.count, 0, $0, len) }
        }
    }
}

// MARK: - Ekran yakalama + H.264

/// Ana ekranı yakalar, donanımla H.264'e kodlar; her kareyi Annex-B olarak verir.
final class Capturer: NSObject, SCStreamOutput, SCStreamDelegate {
    /// Yalnız `queue` üzerinde okunur/yazılır.
    private var scStream: SCStream?
    private var stopped = false
    private var encoder: VTCompressionSession?
    private let queue = DispatchQueue(label: "ayna.capture")
    private var paused = false
    private var forceKey = true
    private var lastBuffer: CVPixelBuffer?
    private var frameIndex: Int64 = 0
    let onFrame: (Data, Bool) -> Void
    let onSize: (Int, Int) -> Void
    let onError: (String) -> Void
    /// Verilirse ana ekran yerine bu ekranın bu bölümü (nokta, ekrana göre) yakalanır: iPhone kipinde
    /// görünmez ekrandaki iPhone Mirroring penceresi.
    let region: (display: SCDisplay, rect: CGRect, scale: CGFloat)?
    /// Durdurulduktan sonra (kodlayıcıda kalan kareler dahil) hiçbir şey dışarı verilmez: yeni
    /// yakalayıcının boyut iletisinden sonra eski boyutta kare gitmesin.
    private let deadLock = NSLock()
    private var dead = false
    private var isDead: Bool {
        deadLock.lock()
        defer { deadLock.unlock() }
        return dead
    }

    /// Ana ekran yakalamasında imleç çizilsin mi (Windows imleci kendisi çiziyorsa hayır).
    private let cursor: Bool
    /// Yalnız `queue` üzerinde: çalışan akışın yapılandırması (imleç sonradan değiştirilebilir).
    private var config: SCStreamConfiguration?

    init(region: (display: SCDisplay, rect: CGRect, scale: CGFloat)? = nil, paused: Bool = false, cursor: Bool = true,
         onFrame: @escaping (Data, Bool) -> Void, onSize: @escaping (Int, Int) -> Void, onError: @escaping (String) -> Void) {
        self.region = region
        self.paused = paused
        self.cursor = cursor
        self.onFrame = onFrame
        self.onSize = onSize
        self.onError = onError
    }

    func start() {
        if let region {
            let pixels = (Int(region.rect.width * region.scale), Int(region.rect.height * region.scale))
            begin(filter: SCContentFilter(display: region.display, excludingWindows: []), pixels: pixels, cursor: false,
                  source: region.rect)
            return
        }
        SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) { [weak self] content, _ in
            guard let self else { return }
            guard let content,
                  let display = content.displays.first(where: { $0.displayID == CGMainDisplayID() }) ?? content.displays.first
            else {
                self.onError("Ekran Kaydı izni gerekli (Sistem Ayarları > Gizlilik ve Güvenlik > Ekran Kaydı).")
                return
            }
            // Gerçek piksel boyutu (Retina), en uzun kenar sınırına ölçeklenir; kodlayıcı çift sayı ister.
            var pw = display.width, ph = display.height
            if let mode = CGDisplayCopyDisplayMode(display.displayID) {
                pw = mode.pixelWidth
                ph = mode.pixelHeight
            }
            self.begin(filter: SCContentFilter(display: display, excludingWindows: []), pixels: (pw, ph), cursor: self.cursor)
        }
    }

    private func begin(filter: SCContentFilter, pixels: (Int, Int), cursor: Bool, source: CGRect? = nil) {
        let (pw, ph) = (max(pixels.0, 2), max(pixels.1, 2))
        let scale = min(1.0, Double(maxVideoSide) / Double(max(pw, ph)))
        let w = Int(Double(pw) * scale) & ~1, h = Int(Double(ph) * scale) & ~1
        let config = SCStreamConfiguration()
        config.width = w
        config.height = h
        config.minimumFrameInterval = CMTime(value: 1, timescale: 60)
        config.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        config.showsCursor = cursor
        config.queueDepth = 5
        if let source { config.sourceRect = source }
        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        do {
            try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: self.queue)
        } catch {
            self.onError("Ekran yakalanamadı: \(error.localizedDescription)")
            return
        }
        // Oturum bu arada kapandıysa yakalama hiç başlamaz (sahipsiz yakalama kalmasın).
        let proceed: Bool = self.queue.sync {
            guard !self.stopped else { return false }
            self.makeEncoder(width: w, height: h)
            self.scStream = stream
            self.config = config
            return true
        }
        guard proceed else { return }
        stream.startCapture { [weak self] error in
            guard let self else { return }
            if let error {
                self.onError("Ekran yakalanamadı: \(error.localizedDescription)")
                return
            }
            // Başlarken durdurulduysa hemen kapat.
            self.queue.async {
                if self.stopped { stream.stopCapture(completionHandler: nil) }
            }
        }
        if !isDead { self.onSize(w, h) }
    }

    func stop() {
        deadLock.lock()
        dead = true
        deadLock.unlock()
        queue.async {
            self.stopped = true
            self.scStream?.stopCapture(completionHandler: nil)
            self.scStream = nil
            if let e = self.encoder {
                VTCompressionSessionCompleteFrames(e, untilPresentationTimeStamp: .invalid)
                VTCompressionSessionInvalidate(e)
            }
            self.encoder = nil
            self.lastBuffer = nil
        }
    }

    /// Düşük gecikmeli kodlayıcı: kare biriktirmez (MaxFrameDelayCount = 0), önce Apple'ın düşük
    /// gecikme kipi denenir; açılmazsa olağan gerçek zamanlı kip.
    private func makeEncoder(width: Int, height: Int) {
        func create(lowLatency: Bool) -> VTCompressionSession? {
            var session: VTCompressionSession?
            let spec: CFDictionary? = lowLatency
                ? [kVTVideoEncoderSpecification_EnableLowLatencyRateControl: true] as CFDictionary : nil
            let status = VTCompressionSessionCreate(
                allocator: kCFAllocatorDefault, width: Int32(width), height: Int32(height),
                codecType: kCMVideoCodecType_H264, encoderSpecification: spec, imageBufferAttributes: nil,
                compressedDataAllocator: nil, outputCallback: nil, refcon: nil, compressionSessionOut: &session)
            return status == noErr ? session : nil
        }
        let lowLatency = create(lowLatency: true)
        guard let s = lowLatency ?? create(lowLatency: false) else {
            onError("Görüntü kodlayıcı açılamadı.")
            return
        }
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_MaxFrameDelayCount, value: NSNumber(value: 0))
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ProfileLevel,
                             value: lowLatency != nil ? kVTProfileLevel_H264_ConstrainedHigh_AutoLevel : kVTProfileLevel_H264_Main_AutoLevel)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: NSNumber(value: 15_000_000))
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: NSNumber(value: 60))
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_MaxKeyFrameInterval, value: NSNumber(value: 600))
        VTCompressionSessionPrepareToEncodeFrames(s)
        encoder = s
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sampleBuffer.isValid, let pb = sampleBuffer.imageBuffer else { return }
        if let arr = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
           let raw = arr.first?[.status] as? Int, let status = SCFrameStatus(rawValue: raw), status != .complete {
            return
        }
        lastBuffer = pb
        encode(pb)
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        guard !isDead else { return }
        onError("Ekran yakalama durdu: \(error.localizedDescription)")
    }

    /// Kuyrukta çağrılır.
    private func encode(_ pb: CVPixelBuffer) {
        guard !paused, let e = encoder else { return }
        frameIndex += 1
        var props: CFDictionary?
        if forceKey {
            props = [kVTEncodeFrameOptionKey_ForceKeyFrame: kCFBooleanTrue] as CFDictionary
            forceKey = false
        }
        VTCompressionSessionEncodeFrame(e, imageBuffer: pb, presentationTimeStamp: CMTime(value: frameIndex, timescale: 60),
                                        duration: .invalid, frameProperties: props, infoFlagsOut: nil) { [weak self] status, _, sbuf in
            guard let self, !self.isDead, status == noErr, let sbuf, let frame = Capturer.annexB(sbuf) else { return }
            self.onFrame(frame.0, frame.1)
        }
    }

    /// Anahtar kare: ekran değişmiyorsa son kare yeniden kodlanır.
    func requestKeyframe() {
        queue.async {
            self.forceKey = true
            if let pb = self.lastBuffer { self.encode(pb) }
        }
    }

    /// İmleci görüntüye çizmeyi açar/kapatır (akış yeniden başlamaz).
    func setCursorShown(_ shown: Bool) {
        queue.async {
            guard let stream = self.scStream, let config = self.config, config.showsCursor != shown else { return }
            config.showsCursor = shown
            stream.updateConfiguration(config) { _ in }
        }
    }

    func setPaused(_ p: Bool) {
        queue.async {
            self.paused = p
            if !p {
                self.forceKey = true
                if let pb = self.lastBuffer { self.encode(pb) }
            }
        }
    }

    /// VideoToolbox çıktısı (uzunluk önekli NAL'lar) → Annex-B; anahtar karede SPS/PPS başa eklenir.
    static func annexB(_ sb: CMSampleBuffer) -> (Data, Bool)? {
        guard let block = CMSampleBufferGetDataBuffer(sb) else { return nil }
        var key = true
        if let arr = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as NSArray?,
           let d = arr.firstObject as? NSDictionary {
            key = !((d[kCMSampleAttachmentKey_NotSync] as? Bool) ?? false)
        }
        let start: [UInt8] = [0, 0, 0, 1]
        var out = Data()
        if key, let fmt = CMSampleBufferGetFormatDescription(sb) {
            var count = 0
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fmt, parameterSetIndex: 0, parameterSetPointerOut: nil,
                                                               parameterSetSizeOut: nil, parameterSetCountOut: &count,
                                                               nalUnitHeaderLengthOut: nil)
            for i in 0..<count {
                var p: UnsafePointer<UInt8>?
                var n = 0
                if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fmt, parameterSetIndex: i, parameterSetPointerOut: &p,
                                                                      parameterSetSizeOut: &n, parameterSetCountOut: nil,
                                                                      nalUnitHeaderLengthOut: nil) == noErr, let p {
                    out.append(contentsOf: start)
                    out.append(p, count: n)
                }
            }
        }
        var total = 0
        var ptr: UnsafeMutablePointer<CChar>?
        guard CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &total,
                                          dataPointerOut: &ptr) == kCMBlockBufferNoErr, let base = ptr else { return nil }
        let bytes = UnsafeRawPointer(base).assumingMemoryBound(to: UInt8.self)
        var off = 0
        while off + 4 <= total {
            let n = Int(bytes[off]) << 24 | Int(bytes[off + 1]) << 16 | Int(bytes[off + 2]) << 8 | Int(bytes[off + 3])
            off += 4
            guard n > 0, off + n <= total else { break }
            out.append(contentsOf: start)
            out.append(bytes + off, count: n)
            off += n
        }
        return (out, key)
    }
}

// MARK: - Fare ve klavye

final class Injector {
    private let source = CGEventSource(stateID: .hidSystemState)
    private var capsLock = false
    private var buttons: Set<UInt8> = []
    private var keys: Set<UInt16> = []
    private var lastClick = (time: Date.distantPast, button: UInt8(0), count: Int64(0), point: CGPoint.zero)
    /// Düğme başına, basışta verilen tıklama sayısı (bırakış aynısını taşır).
    private var clickCount: [UInt8: Int64] = [:]
    /// Windows kısayolunun Mac karşılığı: basışta seçilen çeviri bırakışta da aynen kullanılır.
    private var mapped: [UInt16: Mapping] = [:]
    /// Finder'da Ctrl+X: dosyalar kopyalanır, ardından gelen Ctrl+V taşır (Cmd+Opt+V). Kesme anındaki
    /// pano sayacı: arada başka bir şey kopyalandıysa Ctrl+V yalnız yapıştırır.
    private var finderCut: Int?

    var leftDown: Bool { buttons.contains(0) }
    /// iPhone kipi: koordinatlar bu alana (iPhone Mirroring penceresi, genel koordinat) eşlenir.
    var area: (() -> CGRect?)?

    /// Basılı değiştirici tuşlardan (sol/sağ ayrı) bayraklar.
    private var flags: CGEventFlags {
        var f: CGEventFlags = capsLock ? [.maskAlphaShift] : []
        for k in keys { if let m = Injector.modifier(k) { f.insert(m) } }
        return f
    }

    /// iPhone kipinde pencerenin yeri bilinmiyorsa (açılıyor, taşınıyor) nil: fare Mac masaüstüne gitmez.
    private func point(_ x: UInt16, _ y: UInt16) -> CGPoint? {
        let b: CGRect
        if let area {
            guard let r = area() else { return nil }
            b = r
        } else {
            b = CGDisplayBounds(CGMainDisplayID())
        }
        return CGPoint(x: min(b.minX + CGFloat(x) / 65535 * b.width, b.maxX - 1),
                       y: min(b.minY + CGFloat(y) / 65535 * b.height, b.maxY - 1))
    }

    func move(_ x: UInt16, _ y: UInt16) {
        let type: CGEventType, button: CGMouseButton
        if buttons.contains(0) { (type, button) = (.leftMouseDragged, .left) }
        else if buttons.contains(1) { (type, button) = (.rightMouseDragged, .right) }
        else if buttons.contains(2) { (type, button) = (.otherMouseDragged, .center) }
        else { (type, button) = (.mouseMoved, .left) }
        guard let p = point(x, y) else { return }
        let e = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: p, mouseButton: button)
        e?.flags = flags
        e?.post(tap: .cghidEventTap)
    }

    func button(_ b: UInt8, down: Bool, _ x: UInt16, _ y: UInt16) {
        // Bırakış hiç düşürülmez (düğme basılı kalmasın): yer bilinmiyorsa imlecin olduğu yerde.
        guard let p = point(x, y) ?? (down ? nil : CGEvent(source: nil)?.location) else { return }
        let (downType, upType, mb): (CGEventType, CGEventType, CGMouseButton) = switch b {
        case 0: (.leftMouseDown, .leftMouseUp, .left)
        case 1: (.rightMouseDown, .rightMouseUp, .right)
        default: (.otherMouseDown, .otherMouseUp, .center)
        }
        if down {
            // Çift tıklama: aynı düğme, kısa süre ve yakın nokta.
            let near = abs(p.x - lastClick.point.x) < 5 && abs(p.y - lastClick.point.y) < 5
            let count: Int64 = (lastClick.button == b && near && Date().timeIntervalSince(lastClick.time) < NSEvent.doubleClickInterval)
                ? lastClick.count + 1 : 1
            lastClick = (Date(), b, count, p)
            clickCount[b] = count
            buttons.insert(b)
        } else {
            buttons.remove(b)
        }
        let e = CGEvent(mouseEventSource: source, mouseType: down ? downType : upType, mouseCursorPosition: p, mouseButton: mb)
        e?.setIntegerValueField(.mouseEventClickState, value: clickCount[b] ?? 1)
        e?.flags = flags
        e?.post(tap: .cghidEventTap)
    }

    func scroll(dx: Int16, dy: Int16) {
        // CG'de yatay tekerleğin pozitif yönü sola; protokolde pozitif dx sağa.
        let e = CGEvent(scrollWheelEvent2Source: source, units: .pixel, wheelCount: 2, wheel1: Int32(dy), wheel2: Int32(-Int(dx)), wheel3: 0)
        e?.flags = flags
        e?.post(tap: .cghidEventTap)
    }

    private static func modifier(_ code: UInt16) -> CGEventFlags? {
        switch code {
        case 0x37, 0x36: return .maskCommand
        case 0x38, 0x3C: return .maskShift
        case 0x3A, 0x3D: return .maskAlternate
        case 0x3B, 0x3E: return .maskControl
        default: return nil
        }
    }

    func key(_ code: UInt16, down: Bool) {
        if Injector.modifier(code) != nil {
            if down { keys.insert(code) } else { keys.remove(code) }
            let e = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down)
            e?.type = .flagsChanged
            e?.flags = flags
            e?.post(tap: .cghidEventTap)
            return
        }
        if code == 0x39 { // Caps Lock: basışta durum değişir, flagsChanged olarak gider
            if down { capsLock.toggle() }
            let e = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down)
            e?.type = .flagsChanged
            e?.flags = flags
            e?.post(tap: .cghidEventTap)
            return
        }
        if down { keys.insert(code) } else { keys.remove(code) }
        let m: Mapping
        if down {
            // Basılı tutulup yinelenen tuş ilk basıştaki çeviriyi korur.
            m = mapped[code] ?? translate(code)
            mapped[code] = m
        } else {
            m = mapped.removeValue(forKey: code) ?? Mapping(code)
        }
        var f = flags
        f.subtract(m.remove)
        f.formUnion(m.add)
        let e = CGEvent(keyboardEventSource: source, virtualKey: m.code, keyDown: down)
        e?.flags = f
        e?.post(tap: .cghidEventTap)
    }

    struct Mapping {
        var code: UInt16
        var add: CGEventFlags = []
        var remove: CGEventFlags = []
        init(_ code: UInt16, add: CGEventFlags = [], remove: CGEventFlags = []) {
            self.code = code
            self.add = add
            self.remove = remove
        }
    }

    // Mac tuş kodları (kVK_*)
    private static let kReturn: UInt16 = 0x24, kEnter: UInt16 = 0x4C, kTab: UInt16 = 0x30, kBackspace: UInt16 = 0x33
    private static let kDelete: UInt16 = 0x75, kHome: UInt16 = 0x73, kEnd: UInt16 = 0x77, kF2: UInt16 = 0x78, kF4: UInt16 = 0x76
    private static let kLeft: UInt16 = 0x7B, kRight: UInt16 = 0x7C, kDown: UInt16 = 0x7D, kUp: UInt16 = 0x7E
    private static let kC: UInt16 = 0x08, kD: UInt16 = 0x02, kI: UInt16 = 0x22, kV: UInt16 = 0x09, kW: UInt16 = 0x0D
    private static let kX: UInt16 = 0x07, kY: UInt16 = 0x10, kZ: UInt16 = 0x06, kOpenBracket: UInt16 = 0x21, kCloseBracket: UInt16 = 0x1E
    /// Finder'da dosya işlemine çevrilen tuşlar (öbür tuşlarda odak sorulmaz).
    private static let finderKeys: Set<UInt16> = [kDelete, kD, kReturn, kEnter, kF2, kBackspace, kUp, kX, kC, kV]

    /// Windows alışkanlığı → Mac karşılığı. Windows'ta Ctrl Mac'e Cmd, Alt Option, Windows tuşu
    /// Control olarak gelir; burada Windows'taki anlamı korunacak biçimde tuş/değiştirici değişir.
    /// Finder'da dosya işlemleri (Delete çöpe atar, Enter açar, F2 yeniden adlandırır), yazı
    /// alanlarında satır başı/sonu, her yerde kelime atlama ve geri al/yinele.
    private func translate(_ code: UInt16) -> Mapping {
        typealias K = Injector
        let f = flags
        let cmd = f.contains(.maskCommand), opt = f.contains(.maskAlternate), shift = f.contains(.maskShift)
        if f.contains(.maskControl) { return Mapping(code) } // Windows tuşuyla basılanlar olduğu gibi
        // Odak yalnız gerektiğinde sorulur (Erişilebilirlik çağrısı).
        var textCache: Bool?
        func text() -> Bool {
            if textCache == nil { textCache = Injector.focusIsText() }
            return textCache!
        }
        let finder = NSWorkspace.shared.frontmostApplication?.bundleIdentifier == "com.apple.finder"
        if finder && K.finderKeys.contains(code) && Injector.finderFileViewFocused() {
            switch code {
            case K.kDelete where !cmd && !opt:
                // Delete: çöpe at; Shift+Delete: kalıcı sil (Finder yine onay sorar).
                return shift ? Mapping(K.kBackspace, add: [.maskCommand, .maskAlternate], remove: [.maskShift])
                             : Mapping(K.kBackspace, add: [.maskCommand])
            case K.kD where cmd && !opt && !shift:
                return Mapping(K.kBackspace)
            case K.kReturn where !cmd && !opt && !shift, K.kEnter where !cmd && !opt && !shift:
                return Mapping(K.kDown, add: [.maskCommand])
            case K.kReturn where opt && !cmd, K.kEnter where opt && !cmd:
                return Mapping(K.kI, add: [.maskCommand], remove: [.maskAlternate])
            case K.kF2 where !cmd && !opt && !shift:
                return Mapping(K.kReturn)
            case K.kBackspace where !cmd && !opt && !shift:
                return Mapping(K.kOpenBracket, add: [.maskCommand])
            case K.kUp where opt && !cmd:
                return Mapping(K.kUp, add: [.maskCommand], remove: [.maskAlternate])
            case K.kX where cmd && !opt && !shift:
                finderCut = NSPasteboard.general.changeCount
                return Mapping(K.kC)
            case K.kC where cmd && !opt:
                finderCut = nil
            case K.kV where cmd && !opt && !shift:
                // Yalnız kesmenin kopyası panodaysa (sayaç bir arttı) taşınır.
                defer { finderCut = nil }
                if let base = finderCut, NSPasteboard.general.changeCount == base + 1 {
                    return Mapping(K.kV, add: [.maskAlternate])
                }
            default:
                break
            }
        }
        switch code {
        case K.kLeft where cmd && !opt, K.kRight where cmd && !opt,
             K.kBackspace where cmd && !opt, K.kDelete where cmd && !opt:
            // Ctrl+ok / Ctrl+Backspace / Ctrl+Delete: kelime kelime.
            return Mapping(code, add: [.maskAlternate], remove: [.maskCommand])
        case K.kY where cmd && !opt && !shift:
            return Mapping(K.kZ, add: [.maskShift])
        case K.kTab where cmd && !opt:
            return Mapping(K.kTab, add: [.maskControl], remove: [.maskCommand])
        case K.kHome where cmd && !opt, K.kEnd where cmd && !opt:
            // Ctrl+Home/End: metnin başı/sonu.
            return text() ? Mapping(code == K.kHome ? K.kUp : K.kDown) : Mapping(code, remove: [.maskCommand])
        case K.kHome where !opt, K.kEnd where !opt:
            return text() ? Mapping(code == K.kHome ? K.kLeft : K.kRight, add: [.maskCommand]) : Mapping(code)
        case K.kLeft where opt && !cmd, K.kRight where opt && !cmd:
            // Alt+ok: geri/ileri (yazı alanında Option+ok kelime atlar, dokunulmaz).
            return text() ? Mapping(code)
                          : Mapping(code == K.kLeft ? K.kOpenBracket : K.kCloseBracket, add: [.maskCommand], remove: [.maskAlternate])
        case K.kF4 where opt && !cmd:
            return Mapping(K.kW, add: [.maskCommand], remove: [.maskAlternate])
        case K.kF4 where cmd && !opt:
            return Mapping(K.kW)
        default:
            return Mapping(code)
        }
    }

    /// Finder'da odak bir dosya görünümünde mi (simge, liste, sütun görünümü ya da Masaüstü)? Yalnız
    /// bilinen roller ve olağan bir Finder penceresi (diyalog/sayfa değil) kabul edilir; herhangi bir
    /// Erişilebilirlik çağrısı başarısızsa hayır: Delete dosya silmeye, Enter açmaya dönmez.
    private static func finderFileViewFocused() -> Bool {
        let system = AXUIElementCreateSystemWide()
        AXUIElementSetMessagingTimeout(system, 0.15)
        var focused: CFTypeRef?
        guard AXUIElementCopyAttributeValue(system, kAXFocusedUIElementAttribute as CFString, &focused) == .success,
              let f = focused, CFGetTypeID(f) == AXUIElementGetTypeID() else { return false }
        let element = f as! AXUIElement
        var role: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, kAXRoleAttribute as CFString, &role) == .success,
              let r = role as? String,
              ["AXList", "AXOutline", "AXBrowser", "AXTable", "AXScrollArea", "AXGroup"].contains(r) else { return false }
        var window: CFTypeRef?
        let ws = AXUIElementCopyAttributeValue(element, kAXWindowAttribute as CFString, &window)
        // Masaüstü simgelerinin bir penceresi yoktur.
        if ws == .noValue || ws == .attributeUnsupported { return true }
        guard ws == .success, let w = window, CFGetTypeID(w) == AXUIElementGetTypeID() else { return false }
        var subrole: CFTypeRef?
        guard AXUIElementCopyAttributeValue(w as! AXUIElement, kAXSubroleAttribute as CFString, &subrole) == .success else { return false }
        return (subrole as? String) == (kAXStandardWindowSubrole as String)
    }

    /// Klavye odağı yazı yazılan bir alanda mı? Odak öğrenilemezse (izin yok, yanıt yok) evet
    /// sayılır: dosya silme gibi çeviriler yanlışlıkla yazı alanında çalışmasın.
    private static func focusIsText() -> Bool {
        let system = AXUIElementCreateSystemWide()
        AXUIElementSetMessagingTimeout(system, 0.15)
        var focused: CFTypeRef?
        let status = AXUIElementCopyAttributeValue(system, kAXFocusedUIElementAttribute as CFString, &focused)
        if status == .noValue { return false }
        guard status == .success, let f = focused, CFGetTypeID(f) == AXUIElementGetTypeID() else { return true }
        let element = f as! AXUIElement
        var role: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, kAXRoleAttribute as CFString, &role) == .success else { return true }
        if let r = role as? String, ["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"].contains(r) { return true }
        var subrole: CFTypeRef?
        AXUIElementCopyAttributeValue(element, kAXSubroleAttribute as CFString, &subrole)
        if (subrole as? String) == "AXSearchField" { return true }
        var settable: DarwinBoolean = false
        var range: CFTypeRef?
        return AXUIElementCopyAttributeValue(element, kAXSelectedTextRangeAttribute as CFString, &range) == .success
            && AXUIElementIsAttributeSettable(element, kAXValueAttribute as CFString, &settable) == .success
            && settable.boolValue
    }

    /// Oturum biterken basılı kalan her şey bırakılır.
    func releaseAll() {
        for k in keys { key(k, down: false) }
        for b in buttons {
            let p = CGEvent(source: nil)?.location ?? .zero
            let t: CGEventType = b == 0 ? .leftMouseUp : b == 1 ? .rightMouseUp : .otherMouseUp
            CGEvent(mouseEventSource: source, mouseType: t, mouseCursorPosition: p, mouseButton: b == 0 ? .left : b == 1 ? .right : .center)?
                .post(tap: .cghidEventTap)
        }
        buttons.removeAll()
        keys.removeAll()
        mapped.removeAll()
    }
}

// MARK: - Dosya aktarımı

/// Windows'tan gelmekte olan bir dosya: önce geçici dosyaya yazılır, bitince yerine taşınır.
final class IncomingFile {
    let batch: UInt32
    let temp: URL
    let target: URL?
    let handle: FileHandle?
    var remaining: UInt64

    init(batch: UInt32, temp: URL, target: URL?, handle: FileHandle?, remaining: UInt64) {
        self.batch = batch
        self.temp = temp
        self.target = target
        self.handle = handle
        self.remaining = remaining
    }
}

struct IncomingBatch {
    var dest: URL
    /// Gelen en üst düzey ad → hedefte çakışmayan ad ("Rapor 2.pdf" gibi).
    var names: [String: String] = [:]
    var done: UInt16 = 0
}

enum Files {
    static let maxItems = 2000
    static let chunk = 60_000

    static var desktop: URL { FileManager.default.urls(for: .desktopDirectory, in: .userDomainMask)[0] }

    /// Windows'tan gelen göreli adın parçaları; "..", boş parça, mutlak yol kabul edilmez.
    static func safeParts(_ name: String) -> [String]? {
        let parts = name.split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        guard !parts.isEmpty, parts.count <= 33 else { return nil }
        for p in parts where p.isEmpty || p == "." || p == ".." || p.contains(":") || p.unicodeScalars.contains(where: { $0.value < 0x20 }) {
            return nil
        }
        return parts
    }

    /// Windows dosya adlarında olamayan karakterler "_" olur.
    static func windowsSafe(_ part: String) -> String {
        let bad = Set("\\:<>\"|?*")
        let s = String(part.map { bad.contains($0) || ($0.unicodeScalars.first?.value ?? 32) < 0x20 ? "_" : $0 })
        let trimmed = s.replacingOccurrences(of: "[ .]+$", with: "", options: .regularExpression)
        if trimmed.isEmpty { return "_" }
        let stem = (trimmed.split(separator: ".").first.map(String.init) ?? "").uppercased()
        let reserved = ["CON", "PRN", "AUX", "NUL"].contains(stem)
            || (stem.count == 4 && (stem.hasPrefix("COM") || stem.hasPrefix("LPT")) && stem.last!.isNumber)
        return reserved ? "_" + trimmed : trimmed
    }

    /// `dir` içinde `name` boştaysa kendisi, doluysa Finder gibi "ad 2.uzantı".
    static func unique(_ name: String, in dir: URL) -> String {
        let fm = FileManager.default
        if !fm.fileExists(atPath: dir.appendingPathComponent(name).path) { return name }
        let ext = (name as NSString).pathExtension
        let stem = (name as NSString).deletingPathExtension
        for i in 2..<10_000 {
            let candidate = ext.isEmpty ? "\(stem) \(i)" : "\(stem) \(i).\(ext)"
            if !fm.fileExists(atPath: dir.appendingPathComponent(candidate).path) { return candidate }
        }
        return name
    }

    /// Başka bilgisayardan gelen dosya: Gatekeeper açılışta indirilmiş dosya gibi denetler.
    static func quarantine(_ url: URL) {
        let value = String(format: "0081;%08lx;Ayna Ajan;", Int(Date().timeIntervalSince1970))
        _ = value.withCString { setxattr(url.path, "com.apple.quarantine", $0, strlen($0), 0, 0) }
    }

    /// Ekrandaki bir noktaya bırakılan dosyaların klasörü: noktadaki en üst pencere bir Finder
    /// penceresiyse onun klasörü, değilse Masaüstü. Finder'a sormak Otomasyon izni ister (bir kez).
    static func dropFolder(at point: CGPoint) -> URL {
        guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]]
        else { return desktop }
        var topIsFinder = false
        for w in list {
            guard (w[kCGWindowLayer as String] as? Int) == 0,
                  let bounds = w[kCGWindowBounds as String],
                  let r = CGRect(dictionaryRepresentation: bounds as! CFDictionary), r.contains(point) else { continue }
            let pid = (w[kCGWindowOwnerPID as String] as? Int).map { pid_t($0) } ?? 0
            topIsFinder = NSRunningApplication(processIdentifier: pid)?.bundleIdentifier == "com.apple.finder"
            break
        }
        guard topIsFinder else { return desktop }
        let script = """
        tell application "Finder"
            set out to ""
            repeat with w in (every Finder window)
                try
                    set b to bounds of w
                    set out to out & ((item 1 of b) as text) & "," & ((item 2 of b) as text) & "," & ((item 3 of b) as text) & "," & ((item 4 of b) as text) & "," & (POSIX path of (target of w as alias)) & linefeed
                end try
            end repeat
            return out
        end tell
        """
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/osascript")
        proc.arguments = ["-e", script]
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = FileHandle.nullDevice
        guard (try? proc.run()) != nil else { return desktop }
        // İzin sorusu yanıtsız kalırsa beklemek sonsuza dek sürmesin.
        DispatchQueue.global().asyncAfter(deadline: .now() + 30) { if proc.isRunning { proc.terminate() } }
        let out = String(decoding: pipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        proc.waitUntilExit()
        // Finder pencereleri önden arkaya sıralı gelir; noktayı içeren ilk pencere.
        for line in out.split(separator: "\n") {
            let f = line.split(separator: ",", maxSplits: 4).map(String.init)
            guard f.count == 5, let l = Double(f[0]), let t = Double(f[1]), let r = Double(f[2]), let b = Double(f[3]),
                  CGRect(x: l, y: t, width: r - l, height: b - t).contains(point) else { continue }
            var isDir: ObjCBool = false
            if FileManager.default.fileExists(atPath: f[4], isDirectory: &isDir), isDir.boolValue {
                return URL(fileURLWithPath: f[4], isDirectory: true)
            }
            break
        }
        return desktop
    }

    /// Sürüklenen öğeler: dosyalar ve klasörlerin içindeki dosyalar (bağlantılar atlanır),
    /// göreli adlarla ("Klasör/alt/dosya.txt").
    static func expand(_ urls: [URL]) -> [(url: URL, name: String, size: UInt64)] {
        var out: [(url: URL, name: String, size: UInt64)] = []
        let fm = FileManager.default
        let keys: [URLResourceKey] = [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey]
        for u in urls {
            let top = windowsSafe(u.lastPathComponent)
            guard let v = try? u.resourceValues(forKeys: Set(keys + [.isDirectoryKey])), v.isSymbolicLink != true else { continue }
            if v.isRegularFile == true {
                out.append((u, top, UInt64(v.fileSize ?? 0)))
            } else if v.isDirectory == true, let en = fm.enumerator(at: u, includingPropertiesForKeys: keys) {
                let base = u.standardizedFileURL.path
                for case let f as URL in en {
                    guard out.count < maxItems else { return out }
                    guard let fv = try? f.resourceValues(forKeys: Set(keys)), fv.isRegularFile == true, fv.isSymbolicLink != true,
                          f.lastPathComponent != ".DS_Store" else { continue }
                    let path = f.standardizedFileURL.path
                    guard path.hasPrefix(base + "/") else { continue }
                    let rel = path.dropFirst(base.count + 1).split(separator: "/").map { windowsSafe(String($0)) }
                    out.append((f, ([top] + rel).joined(separator: "/"), UInt64(fv.fileSize ?? 0)))
                }
            }
            if out.count >= maxItems { break }
        }
        return Array(out.prefix(maxItems))
    }
}

// MARK: - iPhone Mirroring (görünmez ekranda)

/// iPhone Mirroring'i Mac'in hiçbir monitöründe görünmeyen bir sanal ekranda açar; yalnız o ekranın
/// pencereye düşen bölümü Windows'a gönderilir. Bitince uygulama kapatılır; sanal ekran ancak uygulama
/// gerçekten kapandıktan sonra kaldırılır (önce kalksa pencere ana ekrana düşerdi).
final class PhoneMirror {
    static let bundleID = "com.apple.ScreenContinuity"
    /// Uygulamanın kapanmasını bekleyen sanal ekranlar.
    private static var parked: [CGVirtualDisplay] = []
    private var display: CGVirtualDisplay?
    private var timer: Timer?
    private let started = Date()
    /// Açılış isteği sonuçlandı (o zamana dek uygulama listede görünmeyebilir).
    private var openFinished = false
    private var seenRunning = false
    private var moveStarted: Date?
    private var windowID: CGWindowID?
    /// En son bildirilen bölge (ekrana göre) ve iki ölçümdür aynı kalan aday (boyut animasyonu biter).
    private var reported = CGRect.null
    private var candidate = CGRect.null
    private var lookingUp = false
    private var finished = false
    /// Pencerenin şu anki yeri (genel koordinat); fare buna eşlenir. Bilinmiyorsa nil.
    private(set) var frame: CGRect?
    /// Sanal ekranın piksel/nokta oranı.
    private var scale: CGFloat = 2
    let onReady: (SCDisplay, CGRect, CGFloat) -> Void
    let onError: (String) -> Void

    init(onReady: @escaping (SCDisplay, CGRect, CGFloat) -> Void, onError: @escaping (String) -> Void) {
        self.onReady = onReady
        self.onError = onError
    }

    func start() {
        guard AXIsProcessTrusted() else {
            fail("Erişilebilirlik izni gerekli (Sistem Ayarları > Gizlilik ve Güvenlik > Erişilebilirlik > Ayna Ajan).")
            return
        }
        guard let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: Self.bundleID) else {
            fail("Bu Mac'te iPhone Mirroring yok (macOS 15 Sequoia ve Apple çipli Mac gerekir).")
            return
        }
        guard let d = Self.makeDisplay() else {
            fail("Görünmez ekran açılamadı.")
            return
        }
        display = d
        if let mode = CGDisplayCopyDisplayMode(d.displayID), mode.width > 0 {
            scale = CGFloat(mode.pixelWidth) / CGFloat(mode.width)
        }
        let config = NSWorkspace.OpenConfiguration()
        config.activates = false
        NSWorkspace.shared.openApplication(at: url, configuration: config) { [weak self] _, error in
            DispatchQueue.main.async {
                self?.openFinished = true
                if let error { self?.fail("iPhone Mirroring açılamadı: \(error.localizedDescription)") }
            }
        }
        timer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in self?.tick() }
    }

    /// Uygulamayı kapatır; sanal ekran uygulama kapanınca kaldırılır.
    func stop() {
        guard !finished else { return }
        finished = true
        frame = nil
        timer?.invalidate()
        timer = nil
        guard let d = display else { return }
        display = nil
        Self.parked.append(d)
        let begun = Date()
        let openFinished = { [weak self] in self?.openFinished ?? true }
        func check() {
            let waited = Date().timeIntervalSince(begun)
            let apps = NSRunningApplication.runningApplications(withBundleIdentifier: PhoneMirror.bundleID).filter { !$0.isTerminated }
            for app in apps { _ = waited > 3 ? app.forceTerminate() : app.terminate() }
            // Açılış isteği sonuçlanmadıysa uygulama birazdan belirebilir: o da kapatılana dek beklenir.
            if (apps.isEmpty && (openFinished() || waited > 8)) || waited > 20 {
                PhoneMirror.parked.removeAll { $0 === d }
                return
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) { check() }
        }
        check()
    }

    /// Ayna Ajan kapanırken (sanal ekran süreçle birlikte gider): önce iPhone Mirroring kapatılır.
    static func shutdown() {
        guard !parked.isEmpty else { return }
        let deadline = Date().addingTimeInterval(3)
        while Date() < deadline {
            let apps = NSRunningApplication.runningApplications(withBundleIdentifier: bundleID).filter { !$0.isTerminated }
            if apps.isEmpty { return }
            apps.forEach { $0.terminate() }
            RunLoop.current.run(until: Date().addingTimeInterval(0.1))
        }
        NSRunningApplication.runningApplications(withBundleIdentifier: bundleID).forEach { $0.forceTerminate() }
    }

    /// Klavye iPhone Mirroring'e gitsin. Öndeyse true; değilse Erişilebilirlikle öne alınır (false).
    func focus() -> Bool {
        guard let app = NSRunningApplication.runningApplications(withBundleIdentifier: Self.bundleID).first else { return false }
        if NSWorkspace.shared.frontmostApplication?.processIdentifier == app.processIdentifier { return true }
        AXUIElementSetAttributeValue(AXUIElementCreateApplication(app.processIdentifier), kAXFrontmostAttribute as CFString,
                                     kCFBooleanTrue)
        return false
    }

    private func fail(_ message: String) {
        guard !finished else { return }
        onError(message)
    }

    private func tick() {
        guard let display, !finished else { return }
        let id = display.displayID
        let elapsed = Date().timeIntervalSince(started)
        let bounds = CGDisplayBounds(id)
        // Sanal ekran hazır olmadan taşınırsa pencere ana ekranda bir yere gider.
        guard CGDisplayIsActive(id) != 0, bounds.width >= 300, bounds.height >= 300 else {
            if elapsed > 10 { fail("Görünmez ekran hazır olmadı.") }
            return
        }
        guard let app = NSRunningApplication.runningApplications(withBundleIdentifier: Self.bundleID).first(where: { !$0.isTerminated })
        else {
            if seenRunning { fail("iPhone Mirroring kapandı.") } else if elapsed > 20 { fail("iPhone Mirroring açılmadı.") }
            return
        }
        seenRunning = true
        guard let found = Self.mainWindow(pid: app.processIdentifier) else {
            if windowID == nil && elapsed > 30 { fail("iPhone Mirroring penceresi bulunamadı.") }
            return
        }
        let (wid, rect) = found
        // Pencere görünmez ekranda değilse oraya taşınır (açılışta ve sistem/kullanıcı taşırsa).
        if !bounds.contains(CGPoint(x: rect.midX, y: rect.midY)) {
            frame = nil
            let since = moveStarted ?? Date()
            moveStarted = since
            if Date().timeIntervalSince(since) > 10 {
                fail("iPhone Mirroring penceresi görünmez ekrana taşınamadı.")
            } else {
                Self.move(pid: app.processIdentifier, to: CGPoint(x: bounds.minX + 40, y: bounds.minY + 40))
            }
            return
        }
        moveStarted = nil
        frame = rect
        let local = rect.offsetBy(dx: -bounds.minX, dy: -bounds.minY)
        guard wid != windowID || local != reported else {
            candidate = .null
            return
        }
        // Yeni yer/boyut iki ölçümde aynı kalınca bildirilir (boyut animasyonunda yakalama yeniden başlamasın).
        guard candidate == local else {
            candidate = local
            return
        }
        guard !lookingUp else { return }
        lookingUp = true
        SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) { content, _ in
            let found = content?.displays.first(where: { $0.displayID == id })
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                self.lookingUp = false
                // Bulunamadıysa bir sonraki ölçümde yeniden denenir.
                guard !self.finished, let found else { return }
                self.windowID = wid
                self.reported = local
                self.candidate = .null
                self.onReady(found, local, self.scale)
            }
        }
    }

    /// Uygulamanın en büyük olağan penceresi (katman 0).
    private static func mainWindow(pid: pid_t) -> (CGWindowID, CGRect)? {
        guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]]
        else { return nil }
        var best: (CGWindowID, CGRect)?
        for w in list {
            guard (w[kCGWindowOwnerPID as String] as? Int).map({ pid_t($0) }) == pid,
                  (w[kCGWindowLayer as String] as? Int) == 0,
                  let id = (w[kCGWindowNumber as String] as? Int).map({ CGWindowID($0) }),
                  let b = w[kCGWindowBounds as String], let r = CGRect(dictionaryRepresentation: b as! CFDictionary),
                  r.width > 100, r.height > 100 else { continue }
            if best == nil || r.width * r.height > best!.1.width * best!.1.height { best = (id, r) }
        }
        return best
    }

    private static func move(pid: pid_t, to point: CGPoint) {
        let app = AXUIElementCreateApplication(pid)
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &value) == .success,
              let windows = value as? [AXUIElement] else { return }
        var p = point
        guard let position = AXValueCreate(.cgPoint, &p) else { return }
        for w in windows {
            AXUIElementSetAttributeValue(w, kAXPositionAttribute as CFString, position)
        }
    }

    /// 900×1000 noktalık Retina sanal ekran (iPhone penceresine yeter).
    private static func makeDisplay() -> CGVirtualDisplay? {
        guard NSClassFromString("CGVirtualDisplay") != nil else { return nil }
        let descriptor = CGVirtualDisplayDescriptor()
        if descriptor.responds(to: NSSelectorFromString("setDispatchQueue:")) {
            descriptor.setDispatchQueue(DispatchQueue.main)
        } else if descriptor.responds(to: NSSelectorFromString("setQueue:")) {
            descriptor.queue = DispatchQueue.main
        } else {
            return nil
        }
        descriptor.name = "Ayna iPhone"
        descriptor.maxPixelsWide = 1800
        descriptor.maxPixelsHigh = 2000
        descriptor.sizeInMillimeters = CGSize(width: 300, height: 333)
        descriptor.vendorID = 0x4159
        descriptor.productID = 0x0001
        descriptor.serialNum = 0x0001
        guard let display = CGVirtualDisplay(descriptor: descriptor) else { return nil }
        let settings = CGVirtualDisplaySettings()
        settings.hiDPI = 1
        settings.modes = [CGVirtualDisplayMode(width: 1800, height: 2000, refreshRate: 60)]
        guard display.apply(settings), display.displayID != 0 else { return nil }
        return display
    }
}

// MARK: - Sürükleme önizlemesi

/// Windows'tan Mac penceresine sürüklenen dosyanın simgesi ve adı: Mac'te imlecin altında, Finder'ın
/// sürükleme görüntüsü gibi. Fareyi yakalamaz, görüntüyle birlikte Windows'a gider.
final class DragPreview {
    private let panel: NSPanel
    private let icon = NSImageView()
    private let label = NSTextField(labelWithString: "")
    private let badge = NSTextField(labelWithString: "")
    private static let size = NSSize(width: 200, height: 100)

    init() {
        panel = NSPanel(contentRect: NSRect(origin: .zero, size: DragPreview.size), styleMask: [.borderless, .nonactivatingPanel],
                        backing: .buffered, defer: false)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.ignoresMouseEvents = true
        panel.level = .screenSaver
        panel.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary]
        let content = NSView(frame: NSRect(origin: .zero, size: DragPreview.size))
        icon.frame = NSRect(x: (DragPreview.size.width - 64) / 2, y: 32, width: 64, height: 64)
        icon.imageScaling = .scaleProportionallyUpOrDown
        icon.alphaValue = 0.85
        label.frame = NSRect(x: 4, y: 6, width: DragPreview.size.width - 8, height: 22)
        label.alignment = .center
        label.lineBreakMode = .byTruncatingMiddle
        label.font = .systemFont(ofSize: 12, weight: .medium)
        label.textColor = .white
        label.wantsLayer = true
        label.layer?.backgroundColor = NSColor.controlAccentColor.withAlphaComponent(0.85).cgColor
        label.layer?.cornerRadius = 5
        badge.frame = NSRect(x: DragPreview.size.width / 2 + 18, y: 74, width: 26, height: 20)
        badge.alignment = .center
        badge.font = .systemFont(ofSize: 12, weight: .bold)
        badge.textColor = .white
        badge.wantsLayer = true
        badge.layer?.backgroundColor = NSColor.systemRed.cgColor
        badge.layer?.cornerRadius = 10
        content.addSubview(icon)
        content.addSubview(label)
        content.addSubview(badge)
        panel.contentView = content
    }

    /// `point`: genel koordinat (sol üst köşe başlangıç, Mac'in fare koordinatları gibi).
    func show(at point: CGPoint, name: String, count: Int, folder: Bool) {
        if label.stringValue != name {
            label.stringValue = name
            let ext = (name as NSString).pathExtension
            icon.image = folder ? NSImage(named: NSImage.folderName)
                : NSWorkspace.shared.icon(forFileType: ext.isEmpty ? "public.data" : ext)
            // Etiket, ada göre daralır (Finder'daki gibi).
            let width = min(DragPreview.size.width - 8, (name as NSString).size(withAttributes: [.font: label.font!]).width + 16)
            label.frame = NSRect(x: (DragPreview.size.width - width) / 2, y: 6, width: width, height: 22)
        }
        badge.isHidden = count < 2
        badge.stringValue = "\(count)"
        // Cocoa ekran koordinatı: başlangıç ana ekranın sol alt köşesi. İmleç simgenin ortasında.
        let primary = NSScreen.screens.first?.frame.height ?? CGDisplayBounds(CGMainDisplayID()).height
        let origin = NSPoint(x: point.x - DragPreview.size.width / 2, y: primary - point.y - 64)
        panel.setFrameOrigin(origin)
        if !panel.isVisible { panel.orderFrontRegardless() }
    }

    func hide() {
        panel.orderOut(nil)
        label.stringValue = ""
    }
}

// MARK: - Oturum

final class Session {
    let conn: NWConnection
    let ip: String
    private weak var app: AppDelegate?
    private var buffer = Data()
    private enum Stage { case commit, hello, approval, streaming, closed }
    private var stage = Stage.commit
    private let started = Date()
    private var approvalStarted = Date()
    private var commitID = ""
    private var commitHash = ""
    private let nonce = hex(randomBytes(16))
    private let ecdh = P256.KeyAgreement.PrivateKey()
    private lazy var macPub = hex(ecdh.publicKey.x963Representation)
    private var txKey: SymmetricKey?
    private var rxKey: SymmetricKey?
    private var txCounter: UInt64 = 0
    private var rxCounter: UInt64 = 0
    private let sendQueue = DispatchQueue(label: "ayna.send")
    private var inFlight = 0
    private var needKey = false
    private var capturer: Capturer?
    /// iPhone kipindeyken görünmez ekrandaki iPhone Mirroring.
    private var phone: PhoneMirror?
    private var phoneAnnounced = false
    /// Windows imleci kendisi çiziyor: Mac imleci görüntüye çizilmez, şekli 0x06 ile gider.
    private var localCursor = false
    /// Tanı: ana iş parçacığının en uzun gecikmesi (10 sn'lik pencerede) ve imleç okunamadı mı.
    private var diagTimer: Timer?
    private var diagLastTick = Date()
    private var diagMaxStall: TimeInterval = 0
    private var diagWindowStart = Date()
    private var cursorUnreadable = false
    /// Mac imlecinin şeklini Windows'a taşır (görüntüde imleç yok).
    private var cursorTimer: Timer?
    private var lastCursor = Data()
    private var dragPreview: DragPreview?
    /// Windows penceresi gizli: görüntü durur (yeni yakalayıcıya da geçer).
    private var videoPaused = false
    private let injector = Injector()
    private var lastData = Date()
    private var watchdog: Timer?
    // Dosya aktarımı: dosya okuma/yazma kendi kuyruğunda (görüntü ve girdi beklemesin).
    private let fileQueue = DispatchQueue(label: "ayna.files")
    /// Mac'ten Windows'a dosya okuma ayrı kuyrukta (Finder'a sorarken yazma beklese de okuma sürer).
    private let readQueue = DispatchQueue(label: "ayna.files.read")
    /// Ana kuyrukta: dosya kuyruğuna verilmiş ama diske yazılmamış bayt. Çoksa okuma bekletilir
    /// (Windows TCP'de durur, bellek şişmez).
    private var pendingWrite = 0
    /// Gönderim kuyruğunda: gönderilmeyi bekleyen dosya verisi ve oturum kapandı mı.
    private var fileInFlight = 0
    private var filesCancelled = false
    /// Sol tuşa basıldığındaki sürükleme panosu sayacı: değiştiyse Mac'te bir şey sürükleniyor.
    private var dragBaseline = NSPasteboard(name: .drag).changeCount
    private var dragFiles: [URL] = []
    /// Dosya kuyruğunda.
    private var incoming: [UInt32: IncomingFile] = [:]
    private var batches: [UInt32: IncomingBatch] = [:]
    private(set) var name = "Windows"
    private(set) var deviceID = ""

    init(conn: NWConnection, ip: String, app: AppDelegate) {
        self.conn = conn
        self.ip = ip
        self.app = app
    }

    func start() {
        conn.stateUpdateHandler = { [weak self] state in
            switch state {
            case .failed, .cancelled: self?.close()
            default: break
            }
        }
        conn.start(queue: .main)
        receive()
        // El sıkışma 10 sn'de biter (veri gelse de uzamaz); onay en çok 2 dk sürer; oturum 10 sn
        // sessiz kalırsa kapanır.
        watchdog = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            guard let self else { return }
            let now = Date()
            switch self.stage {
            case .commit, .hello: if now.timeIntervalSince(self.started) > 10 { self.close() }
            case .approval: if now.timeIntervalSince(self.approvalStarted) > 120 { self.close() }
            case .streaming: if now.timeIntervalSince(self.lastData) > 10 { self.close() }
            case .closed: break
            }
        }
    }

    private func sendLine(_ s: String) {
        conn.send(content: Data((s + "\n").utf8), completion: .contentProcessed { _ in })
    }

    private func receive() {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, complete, error in
            guard let self, self.stage != .closed else { return }
            if let data, !data.isEmpty {
                // Onay beklenirken Windows bir şey göndermez; gelen veri bağlantıyı kapatır.
                if self.stage == .approval || self.buffer.count + data.count > 140_000 {
                    self.close()
                    return
                }
                self.buffer.append(data)
                self.lastData = Date()
                self.process()
            }
            if complete || error != nil { self.close() } else { self.receiveWhenReady() }
        }
    }

    private func receiveWhenReady() {
        guard stage != .closed else { return }
        if pendingWrite > 8_000_000 {
            // Bilerek okunmuyor: sessizlik bekçisi bunu kopma saymasın.
            lastData = Date()
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.01) { [weak self] in self?.receiveWhenReady() }
            return
        }
        receive()
    }

    private func take(_ n: Int) -> Data {
        let d = Data(buffer.prefix(n))
        buffer = Data(buffer.dropFirst(n))
        return d
    }

    private func process() {
        while stage != .closed {
            switch stage {
            case .commit, .hello:
                guard let nl = buffer.firstIndex(of: 0x0A) else {
                    if buffer.count > 1024 { close() }
                    return
                }
                let line = String(decoding: take(nl - buffer.startIndex + 1).dropLast(), as: UTF8.self)
                if stage == .commit { commit(line) } else { hello(line) }
            case .approval:
                return
            case .streaming:
                // Gelen tüm tam çerçeveler çözülür; art arda fare hareketlerinden yalnız sonuncusu
                // uygulanır (her biri sisteme ayrı olay olarak işlenip ana iş parçacığını tıkamasın).
                var messages: [Data] = []
                while buffer.count >= 4 {
                    let b = [UInt8](buffer.prefix(4))
                    let n = Int(b[0]) << 24 | Int(b[1]) << 16 | Int(b[2]) << 8 | Int(b[3])
                    guard n >= 17, n <= 65_536 else { close(); return }
                    guard buffer.count >= 4 + n else { break }
                    _ = take(4)
                    let sealed = take(n)
                    guard let plain = open(sealed) else { close(); return }
                    messages.append(plain)
                }
                for (i, message) in messages.enumerated() {
                    if message.first == 0x10, i + 1 < messages.count, messages[i + 1].first == 0x10 { continue }
                    handle(message)
                    if stage == .closed { return }
                }
                return
            case .closed:
                return
            }
        }
    }

    private func nonceData(_ c: UInt64) -> AES.GCM.Nonce {
        var b = [UInt8](repeating: 0, count: 12)
        for i in 0..<8 { b[4 + i] = UInt8((c >> (56 - 8 * UInt64(i))) & 0xFF) }
        return try! AES.GCM.Nonce(data: b)
    }

    private func open(_ sealed: Data) -> Data? {
        guard let key = rxKey, sealed.count >= 16 else { return nil }
        let n = nonceData(rxCounter)
        rxCounter += 1
        guard let box = try? AES.GCM.SealedBox(nonce: n, ciphertext: sealed.prefix(sealed.count - 16), tag: sealed.suffix(16)) else {
            return nil
        }
        return try? AES.GCM.open(box, using: key)
    }

    /// Şifreleyip gönderir (gönderim kuyruğunda; sayaç sırası korunur).
    private func send(_ plain: Data) {
        sendQueue.async { [weak self] in self?.sendNow(plain) }
    }

    private func sendNow(_ plain: Data, file: Bool = false) {
        guard let key = txKey, let box = try? AES.GCM.seal(plain, using: key, nonce: nonceData(txCounter)) else { return }
        txCounter += 1
        var frame = Data()
        let sealed = box.ciphertext + box.tag
        let n = UInt32(sealed.count)
        frame.append(contentsOf: [UInt8(n >> 24), UInt8((n >> 16) & 0xFF), UInt8((n >> 8) & 0xFF), UInt8(n & 0xFF)])
        frame.append(sealed)
        let size = frame.count
        inFlight += size
        if file { fileInFlight += size }
        conn.send(content: frame, completion: .contentProcessed { [weak self] _ in
            self?.sendQueue.async {
                self?.inFlight -= size
                if file { self?.fileInFlight -= size }
            }
        })
    }

    /// Ağ yavaşsa kareler biriktirilmez: bekleyen veri çoksa kare atlanır, sonra anahtar kareyle sürer.
    private func sendVideo(_ data: Data, key: Bool) {
        sendQueue.async { [weak self] in
            guard let self else { return }
            // Gönderilmeyi bekleyen veri az tutulur (~0,3 sn): birikirse gecikme büyür.
            // Dosya aktarımı kendi sınırıyla bekler; görüntü sınırı yalnız görüntü verisine bakar.
            let pending = self.inFlight - self.fileInFlight
            if pending > 512_000 {
                self.needKey = true
                return
            }
            if self.needKey && !key {
                if pending < 128_000 {
                    DispatchQueue.main.async { self.capturer?.requestKeyframe() }
                }
                return
            }
            if key { self.needKey = false }
            self.sendNow(Data([0x01, key ? 1 : 0]) + data)
        }
    }

    // --- El sıkışma

    /// `AYNA2 COMMIT <cihaz> <özet>`: Windows açık anahtarına bağlanır; ancak bundan sonra Mac'in
    /// anahtarı gönderilir.
    private func commit(_ line: String) {
        guard let app else { close(); return }
        let parts = line.split(separator: " ").map(String.init)
        guard parts.count == 4, parts[0] == "AYNA2", parts[1] == "COMMIT", isHex(parts[2], length: 32),
              isHex(parts[3], length: 64) else {
            sendLine("AYNA2 DENY")
            close()
            return
        }
        guard app.allowHandshake(ip: ip) else {
            sendLine("AYNA2 BUSY")
            close()
            return
        }
        commitID = parts[2]
        commitHash = parts[3]
        stage = .hello
        sendLine("AYNA2 HI \(nonce) \(macPub)")
    }

    private func hello(_ line: String) {
        guard let app else { close(); return }
        let parts = line.split(separator: " ", maxSplits: 5).map(String.init)
        guard parts.count >= 5, parts[0] == "AYNA2", parts[1] == "HELLO", isHex(parts[2], length: 32),
              isHex(parts[4], length: 130), let winPubData = unhex(parts[4]),
              let peerKey = try? P256.KeyAgreement.PublicKey(x963Representation: winPubData),
              let secret = try? ecdh.sharedSecretFromKeyAgreement(with: peerKey)
        else {
            sendLine("AYNA2 DENY")
            close()
            return
        }
        let id = parts[2], proof = parts[3], winPub = parts[4]
        let rawName = parts.count > 5 ? parts[5] : ""
        // Açılan anahtar, el sıkışmanın başında bağlanılanla aynı olmalı.
        guard id == commitID, sameProof(hex(SHA256.hash(data: Data("\(winPub)|\(id)".utf8))), commitHash) else {
            sendLine("AYNA2 DENY")
            close()
            return
        }
        name = cleanName(rawName.isEmpty ? "Windows" : rawName)
        let shared = secret.withUnsafeBytes { Data($0) }
        let t = "AYNA2|\(nonce)|\(macPub)|\(winPub)|\(id)|\(rawName)"
        let keys = hkdf(shared, "keys|" + t, 64)
        let sasBytes = [UInt8](hkdf(shared, "sas|" + t, 4))
        let sasN = (UInt32(sasBytes[0]) << 24 | UInt32(sasBytes[1]) << 16 | UInt32(sasBytes[2]) << 8 | UInt32(sasBytes[3])) % 1_000_000
        let sas = String(format: "%03d %03d", Int(sasN / 1000), Int(sasN % 1000))
        rxKey = SymmetricKey(data: keys.prefix(32))
        txKey = SymmetricKey(data: keys.suffix(32))

        guard Store.accepting else { sendLine("AYNA2 OFF"); close(); return }
        let known = Store.trusted.first { $0.id == id }
        let knownToken = known.flatMap { unhex($0.token) }
        let authenticated = knownToken.map { sameProof(hmacHex(key: $0, message: "hello|" + t), proof) } ?? false
        deviceID = id
        // Aynı anda tek oturum; ancak kimliğini kanıtlayan aynı bilgisayar (ağ kopup geri geldi)
        // eski oturumu devralır.
        if !app.mayStart(deviceID: id, authenticated: authenticated) {
            sendLine("AYNA2 BUSY")
            close()
            return
        }

        if authenticated, let known, let token = knownToken, known.ip == ip {
            approve(token: token, kind: "SAME", t: t)
            return
        }
        stage = .approval
        approvalStarted = Date()
        app.ask(session: self, name: name, ip: ip, id: id, sas: sas, known: known, authenticated: authenticated) { [weak self] allowed in
            guard let self, self.stage == .approval else { return }
            guard allowed else {
                self.sendLine("AYNA2 DENY")
                self.close()
                return
            }
            let token = authenticated ? (knownToken ?? Data()) : hkdf(shared, "token|" + t, 32)
            var list = Store.trusted.filter { $0.id != id }
            list.append(TrustedDevice(id: id, name: self.name, ip: self.ip, token: hex(token)))
            Store.trusted = list
            self.approve(token: token, kind: authenticated ? "SAME" : "NEW", t: t)
        }
        if stage == .approval {
            sendLine("AYNA2 WAIT")
        }
    }

    private func approve(token: Data, kind: String, t: String) {
        sendLine("AYNA2 OK \(kind) \(hmacHex(key: token, message: "ok|" + t))")
        stage = .streaming
        lastData = Date()
        app?.sessionStarted(self)
        startCapture(region: nil)
        diagLastTick = Date()
        diagWindowStart = Date()
        // Ortak kip: menü açıkken de ölçülsün (yoksa menü süresi takılma sanılır).
        let diag = Timer(timeInterval: 0.1, repeats: true) { [weak self] _ in self?.diagTick() }
        diag.tolerance = 0
        RunLoop.main.add(diag, forMode: .common)
        diagTimer = diag
        process()
    }

    /// 0,1 sn'lik zamanlayıcının gecikmesi ana iş parçacığının ne kadar meşgul kaldığını gösterir.
    private func diagTick() {
        let now = Date()
        diagMaxStall = max(diagMaxStall, now.timeIntervalSince(diagLastTick) - 0.1)
        diagLastTick = now
        guard now.timeIntervalSince(diagWindowStart) >= 10 else { return }
        let ms = UInt16(clamping: Int(max(0, diagMaxStall) * 1000))
        var report: [UInt8] = [0x07]
        report += be16(ms)
        report.append(cursorUnreadable ? 1 : 0)
        send(Data(report))
        diagMaxStall = 0
        diagWindowStart = now
    }

    /// Mac imlecinin şekli değiştiyse resmini (nokta başına 2 piksel) Windows'a gönderir.
    private func sendCursor() {
        guard stage == .streaming else { return }
        var message = Data([0x06, 0, 0, 0, 0, 0, 0, 0, 0])
        let current = phone == nil ? NSCursor.currentSystem : nil
        cursorUnreadable = phone == nil && current == nil
        if let cursor = current {
            // Sabit ölçek: Windows imleci pencere boyutundan bağımsız, olağan imleç boyutunda gösterir.
            let scale: CGFloat = 2
            let size = cursor.image.size
            let w = Int((size.width * scale).rounded()), h = Int((size.height * scale).rounded())
            if (1...256).contains(w), (1...256).contains(h) {
                var pixels = [UInt8](repeating: 0, count: w * h * 4)
                let drawn: Bool = pixels.withUnsafeMutableBytes { raw in
                    guard let ctx = CGContext(data: raw.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                                              space: CGColorSpaceCreateDeviceRGB(),
                                              bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue)
                    else { return false }
                    NSGraphicsContext.saveGraphicsState()
                    NSGraphicsContext.current = NSGraphicsContext(cgContext: ctx, flipped: false)
                    // En yüksek çözünürlüklü temsil (Retina imleci küçültülerek keskin kalır).
                    let rect = NSRect(x: 0, y: 0, width: w, height: h)
                    if let best = cursor.image.representations.max(by: { $0.pixelsWide < $1.pixelsWide }) {
                        best.draw(in: rect)
                    } else {
                        cursor.image.draw(in: rect)
                    }
                    NSGraphicsContext.restoreGraphicsState()
                    return true
                }
                if drawn {
                    let hot = cursor.hotSpot
                    let hx = UInt16(clamping: Int(hot.x * scale))
                    let hy = UInt16(clamping: Int(hot.y * scale))
                    var head: [UInt8] = [0x06]
                    head += be16(hx)
                    head += be16(hy)
                    head += be16(UInt16(w))
                    head += be16(UInt16(h))
                    message = Data(head)
                    message.append(contentsOf: pixels)
                }
            }
        }
        guard message != lastCursor else { return }
        lastCursor = message
        send(message)
    }

    /// Ana ekranı (ya da iPhone kipinde görünmez ekrandaki iPhone Mirroring penceresini) yakalamaya başlar.
    private func startCapture(region: (display: SCDisplay, rect: CGRect, scale: CGFloat)?) {
        capturer?.stop()
        let cap = Capturer(
            region: region,
            paused: videoPaused,
            cursor: !localCursor,
            onFrame: { [weak self] data, key in self?.sendVideo(data, key: key) },
            onSize: { [weak self] w, h in
                self?.send(Data([0x02] + be16(UInt16(clamping: w)) + be16(UInt16(clamping: h))))
                // Yeni boyut iletisinden sonra imleç yeniden gönderilsin.
                DispatchQueue.main.async { self?.lastCursor = Data() }
            },
            onError: { [weak self] message in
                DispatchQueue.main.async {
                    // iPhone kipinde pencere kapanması zaten iPhone Mirroring izlenerek bildirilir; Mac
                    // ekranında uyarı açılmaz.
                    guard let self, self.phone == nil else { return }
                    self.app?.show(error: message)
                }
            })
        capturer = cap
        cap.start()
    }

    // --- iPhone kipi

    private func setPhoneMode(_ on: Bool) {
        guard on != (phone != nil) else {
            // Yinelenen istek de yanıtlanır (Windows yanıt bekler).
            sendState(mode: on ? 1 : 0, ok: true, "")
            return
        }
        if on {
            let p = PhoneMirror(
                onReady: { [weak self] display, rect, scale in self?.phoneReady(display, rect, scale) },
                onError: { [weak self] message in self?.phoneFailed(message) })
            phone = p
            phoneAnnounced = false
            injector.area = { [weak p] in p?.frame }
            sendState(mode: 1, ok: true, "iPhone Mirroring açılıyor…")
            p.start()
        } else {
            phone?.stop()
            phone = nil
            injector.area = nil
            startCapture(region: nil)
            sendState(mode: 0, ok: true, "")
        }
    }

    private func phoneReady(_ display: SCDisplay, _ rect: CGRect, _ scale: CGFloat) {
        guard let phone, stage == .streaming else { return }
        startCapture(region: (display, rect, scale))
        _ = phone.focus()
        if !phoneAnnounced {
            phoneAnnounced = true
            sendState(mode: 1, ok: true, "iPhone hazır")
        }
    }

    private func phoneFailed(_ message: String) {
        guard phone != nil else { return }
        phone?.stop()
        phone = nil
        injector.area = nil
        if stage == .streaming { startCapture(region: nil) }
        sendState(mode: 0, ok: false, message)
    }

    private func sendState(mode: UInt8, ok: Bool, _ text: String) {
        send(Data([0x05, mode, ok ? 1 : 0]) + Data(text.utf8))
    }

    // --- Oturum mesajları

    private func handle(_ m: Data) {
        guard let type = m.first else { return }
        let b = [UInt8](m)
        switch type {
        case 0x10 where b.count >= 5:
            injector.move(readBE16(m, 1), readBE16(m, 3))
        case 0x11 where b.count >= 7:
            if b[1] == 0 && b[2] == 1 {
                // Yeni basış: önceki sürüklemenin dosyaları artık istenemez.
                dragBaseline = NSPasteboard(name: .drag).changeCount
                dragFiles = []
            }
            injector.button(b[1], down: b[2] == 1, readBE16(m, 3), readBE16(m, 5))
        case 0x12 where b.count >= 5:
            injector.scroll(dx: Int16(bitPattern: readBE16(m, 1)), dy: Int16(bitPattern: readBE16(m, 3)))
        case 0x13 where b.count >= 4:
            // iPhone kipinde tuş basışı yalnız iPhone Mirroring öndeyken gider (yoksa öne alınır, basış
            // düşer); bırakış her zaman gider (tuş basılı kalmasın).
            if b[3] == 1, let phone, !phone.focus() { return }
            injector.key(readBE16(m, 1), down: b[3] == 1)
        case 0x14:
            capturer?.requestKeyframe()
        case 0x15 where b.count >= 9:
            send(Data([0x03]) + m.dropFirst().prefix(8))
        case 0x16 where b.count >= 2:
            videoPaused = b[1] == 1
            capturer?.setPaused(videoPaused)
        case 0x17:
            close()
        case 0x18 where b.count >= 2:
            setPhoneMode(b[1] == 1)
        case 0x20 where b.count >= 23:
            putBegin(b)
        case 0x21 where b.count >= 5:
            let id = readBE32(b, 1)
            let data = Data(b[5...])
            pendingWrite += data.count
            fileQueue.async { [weak self] in
                self?.putData(id, data)
                DispatchQueue.main.async { self?.pendingWrite -= data.count }
            }
        case 0x22 where b.count >= 5:
            let id = readBE32(b, 1)
            fileQueue.async { [weak self] in self?.putEnd(id) }
        case 0x23:
            dragQuery()
        case 0x24 where b.count >= 5:
            get(readBE32(b, 1))
        case 0x25 where b.count >= 10:
            let (x, y) = (readBE16(m, 1), readBE16(m, 3))
            let count = Int(readBE16(m, 5)), folder = b[7] == 1
            let len = Int(UInt16(b[8]) << 8 | UInt16(b[9]))
            guard phone == nil, b.count >= 10 + len else { break }
            let name = String(decoding: b[10..<(10 + len)], as: UTF8.self)
            injector.move(x, y)
            let screen = CGDisplayBounds(CGMainDisplayID())
            let point = CGPoint(x: screen.minX + CGFloat(x) / 65535 * screen.width, y: screen.minY + CGFloat(y) / 65535 * screen.height)
            let preview = dragPreview ?? DragPreview()
            dragPreview = preview
            preview.show(at: point, name: name, count: count, folder: folder)
        case 0x26:
            dragPreview?.hide()
        case 0x1A:
            // Windows imleci kendisi çiziyor: Mac imleci görüntüden çıkar, şekli ayrıca gönderilir.
            guard !localCursor else { break }
            localCursor = true
            if phone == nil { capturer?.setCursorShown(false) }
            cursorTimer = Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { [weak self] _ in self?.sendCursor() }
        default:
            break
        }
    }

    // --- Dosyalar: Windows → Mac

    private func putBegin(_ b: [UInt8]) {
        let batch = readBE32(b, 1), id = readBE32(b, 5), size = readBE64(b, 9)
        let x = UInt16(b[17]) << 8 | UInt16(b[18]), y = UInt16(b[19]) << 8 | UInt16(b[20])
        let len = Int(UInt16(b[21]) << 8 | UInt16(b[22]))
        guard b.count >= 23 + len else { return }
        let name = String(decoding: b[23..<(23 + len)], as: UTF8.self)
        let screen = CGDisplayBounds(CGMainDisplayID())
        let point = CGPoint(x: screen.minX + CGFloat(x) / 65535 * screen.width, y: screen.minY + CGFloat(y) / 65535 * screen.height)
        fileQueue.async { [weak self] in self?.putOpen(batch: batch, id: id, size: size, point: point, name: name) }
    }

    private func putOpen(batch: UInt32, id: UInt32, size: UInt64, point: CGPoint, name: String) {
        if let old = incoming.removeValue(forKey: id) { discard(old) }
        if batches[batch] == nil {
            if batches.count > 64 { batches.removeAll() }
            batches[batch] = IncomingBatch(dest: Files.dropFolder(at: point))
        }
        let fm = FileManager.default
        let temp = fm.temporaryDirectory.appendingPathComponent("ayna-\(UUID().uuidString)")
        guard incoming.count < 8 else {
            // Aynı anda çok dosya açılmaz; yine de "bitti" sayılır.
            incoming[id] = IncomingFile(batch: batch, temp: temp, target: nil, handle: nil, remaining: size)
            return
        }
        var target: URL?
        var handle: FileHandle?
        // Uygunsuz ad atlanır ama yine de "bitti" sayılır (Windows'taki sayaç tamamlansın).
        if var b = batches[batch], let parts = Files.safeParts(name) {
            let top = b.names[parts[0]] ?? Files.unique(parts[0], in: b.dest)
            b.names[parts[0]] = top
            batches[batch] = b
            target = parts.dropFirst().reduce(b.dest.appendingPathComponent(top)) { $0.appendingPathComponent($1) }
            if fm.createFile(atPath: temp.path, contents: nil) {
                handle = try? FileHandle(forWritingTo: temp)
            }
        }
        incoming[id] = IncomingFile(batch: batch, temp: temp, target: target, handle: handle, remaining: size)
    }

    private func putData(_ id: UInt32, _ data: Data) {
        guard let f = incoming[id], let h = f.handle else { return }
        let n = Int(min(UInt64(data.count), f.remaining))
        guard n > 0 else { return }
        if (try? h.write(contentsOf: data.prefix(n))) == nil {
            discard(f)
            incoming[id] = IncomingFile(batch: f.batch, temp: f.temp, target: nil, handle: nil, remaining: 0)
            return
        }
        f.remaining -= UInt64(n)
    }

    private func putEnd(_ id: UInt32) {
        guard let f = incoming.removeValue(forKey: id) else { return }
        try? f.handle?.close()
        // Bildirilen boyut gelmediyse dosya eksiktir, yerine konmaz.
        if let target = f.target, f.handle != nil, f.remaining == 0 {
            let fm = FileManager.default
            let dir = target.deletingLastPathComponent()
            try? fm.createDirectory(at: dir, withIntermediateDirectories: true)
            let final = dir.appendingPathComponent(Files.unique(target.lastPathComponent, in: dir))
            if (try? fm.moveItem(at: f.temp, to: final)) == nil {
                try? fm.removeItem(at: f.temp)
            } else {
                Files.quarantine(final)
            }
        } else {
            try? FileManager.default.removeItem(at: f.temp)
        }
        guard var b = batches[f.batch] else { return }
        b.done &+= 1
        batches[f.batch] = b
        send(Data([0x35] + be32(f.batch) + be16(b.done)) + Data(b.dest.path.utf8))
    }

    private func discard(_ f: IncomingFile) {
        try? f.handle?.close()
        try? FileManager.default.removeItem(at: f.temp)
    }

    // --- Dosyalar: Mac → Windows

    /// Windows sorar: fare basılıyken pencereden çıkıldı; Mac'te dosya sürükleniyor mu?
    private func dragQuery() {
        let pb = NSPasteboard(name: .drag)
        var items: [(url: URL, name: String, size: UInt64)] = []
        if injector.leftDown && pb.changeCount != dragBaseline,
           let urls = pb.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] {
            items = Files.expand(urls)
        }
        // Liste tek çerçeveye sığmalı.
        var body = Data()
        var count = 0
        for item in items {
            let name = Data(item.name.utf8)
            guard name.count <= 4096, body.count + 10 + name.count < 60_000 else { break }
            body.append(contentsOf: be64(item.size) + be16(UInt16(name.count)))
            body.append(name)
            count += 1
        }
        dragFiles = items.prefix(count).map { $0.url }
        send(Data([0x30] + be16(UInt16(count))) + body)
    }

    private func get(_ index: UInt32) {
        guard Int(index) < dragFiles.count else {
            send(Data([0x34] + be32(index)))
            return
        }
        let url = dragFiles[Int(index)]
        readQueue.async { [weak self] in self?.stream(url, index) }
    }

    private func stream(_ url: URL, _ index: UInt32) {
        // Listeden sonra bağlantıya dönüştürülmüş yol izlenmez; yalnız olağan dosya okunur.
        let fd = Darwin.open(url.path, O_RDONLY | O_NOFOLLOW)
        var st = stat()
        guard fd >= 0, fstat(fd, &st) == 0, (Int(st.st_mode) & Int(S_IFMT)) == Int(S_IFREG) else {
            if fd >= 0 { Darwin.close(fd) }
            send(Data([0x34] + be32(index)))
            return
        }
        let h = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
        defer { try? h.close() }
        let size = UInt64(st.st_size)
        send(Data([0x31] + be32(index) + be64(size)))
        while true {
            // Geri basınç: bekleyen dosya verisi ~1 MB'ı geçmez (görüntü ve girdi akmaya devam eder).
            while true {
                let (cancelled, pending) = sendQueue.sync { (filesCancelled, fileInFlight) }
                if cancelled { return }
                if pending < 1_000_000 { break }
                usleep(2_000)
            }
            guard let chunk = try? h.read(upToCount: Files.chunk), !chunk.isEmpty else { break }
            let message = Data([0x32] + be32(index)) + chunk
            sendQueue.async { [weak self] in self?.sendNow(message, file: true) }
        }
        send(Data([0x33] + be32(index)))
    }

    func close() {
        guard stage != .closed else { return }
        let wasStreaming = stage == .streaming
        stage = .closed
        watchdog?.invalidate()
        cursorTimer?.invalidate()
        cursorTimer = nil
        diagTimer?.invalidate()
        diagTimer = nil
        dragPreview?.hide()
        capturer?.stop()
        capturer = nil
        phone?.stop()
        phone = nil
        injector.area = nil
        injector.releaseAll()
        sendQueue.async { [self] in filesCancelled = true }
        fileQueue.async { [self] in
            for f in incoming.values { discard(f) }
            incoming.removeAll()
        }
        if wasStreaming {
            // Güçlü yakalama: oturum listeden çıksa da veda iletisi gidip bağlantı kapanana dek yaşar.
            sendQueue.async { [self] in
                sendNow(Data([0x04]))
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { self.conn.cancel() }
            }
        } else {
            conn.cancel()
        }
        app?.sessionEnded(self)
    }
}

// MARK: - Otomatik güncelleme

/// GitHub'daki son sürümü denetler; yeni sürümü indirip doğrular, uygulamayı değiştirip yeniden
/// başlatır. Sürüm numarası GitHub Actions çalıştırma numarasıdır (CFBundleVersion); 0 olan yerel
/// derlemelerde kapalıdır.
///
/// Güven: CI her sürümde bir bildirge ("AYNA-SURUM <n>" + dosya özetleri) üretip projenin imza
/// anahtarıyla (bu uygulamayı imzalayan, yalnız GitHub gizli değişkenlerinde duran RSA anahtarı)
/// imzalar. Güncelleme yalnız imzası doğrulanan, sürümü etiketle aynı ve bu sürümden yeni olan
/// bildirgedeki özetle eşleşirse; açılan uygulama da aynı anahtarla imzalı ve içindeki sürüm
/// numarası aynıysa kurulur.
enum Updater {
    static let latest = URL(string: "https://api.github.com/repos/knutolof06/ayna-merkezi/releases/latest")!
    static let downloadPrefix = "https://github.com/knutolof06/ayna-merkezi/releases/download/"
    static let asset = "AynaAjan.zip"
    /// Yalnız bu kimlik ve bu sertifikayla imzalanmış uygulama kabul edilir.
    static let requirement =
        "identifier \"tr.ayna.ajan\" and certificate leaf = H\"c7251ef44643fc0dcf58efe33fd4d5e02a215fa0\""
    /// Sürüm imza anahtarının açık kısmı (PKCS#1, RSA-2048).
    static let releaseKey = Data(base64Encoded: "MIIBCgKCAQEAtl0vDk4BMuQsHY1T/SwwsojdabRdGzAmQh/hEvarWYtYbHbcpE/nBbqZfLYAW3rchGwtgPfoqWwukaw0CZMFL1LmAQTQSu1WjCBiU4tOfvgRVEy0qzDbvOEFUYu9DyHKum+GetVAeK611XQuGDPOmarnFpiqsEB6knSoa0Jnz+UsKTy6TVbNSiy7GLPOUOgz4VN+ax4Z2/defvuPG7aXdIgcUioE987Hru6OheByjbWFpHOIBWcOgspfaavq0zBC+nyolCsWjcC9xepqaXsSc6IKr4KhTsVbAtQytA5YicGyz16qiSUAzDsI4dnMRMp5Ld8nApkearU7XA7IQ3LEOwIDAQAB")!

    static var build: Int { Int(Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "") ?? 0 }

    private struct Release: Decodable {
        let tag_name: String
        let assets: [Asset]
    }

    private struct Asset: Decodable {
        let name: String
        let browser_download_url: String
    }

    /// Yeni sürüm hazırlanırsa (indirilip doğrulandıysa) ana kuyrukta sürüm ve açılmış uygulama yolu.
    static func check(completion: @escaping (Int, URL) -> Void, failed: @escaping (String) -> Void) {
        guard build > 0 else { return }
        DispatchQueue.global().async {
            do {
                var req = URLRequest(url: latest)
                req.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
                let release = try JSONDecoder().decode(Release.self, from: try fetch(req))
                guard let version = Int(release.tag_name.dropFirst()), version > build else { return }
                func download(_ name: String) throws -> Data {
                    guard let a = release.assets.first(where: { $0.name == name }),
                          a.browser_download_url.hasPrefix(downloadPrefix), let url = URL(string: a.browser_download_url)
                    else { throw UpdateError("Sürüm \(version) eksik yayınlanmış (\(name)).") }
                    return try fetch(URLRequest(url: url))
                }
                let manifest = try download("manifest.txt")
                let sigText = String(decoding: try download("manifest.sig"), as: UTF8.self)
                guard let sig = unhex(sigText.trimmingCharacters(in: .whitespacesAndNewlines)), verify(manifest, sig) else {
                    throw UpdateError("Güncellemenin imzası doğrulanamadı.")
                }
                let text = String(decoding: manifest, as: UTF8.self)
                guard text.split(whereSeparator: \.isNewline).first.map(String.init) == "AYNA-SURUM \(version)",
                      let expected = sha256(in: text, for: asset) else {
                    throw UpdateError("Güncelleme bildirgesi sürümle uyuşmuyor.")
                }
                let zip = try download(asset)
                guard hex(SHA256.hash(data: zip)) == expected else { throw UpdateError("İndirilen güncellemenin özeti tutmuyor.") }
                let dir = FileManager.default.temporaryDirectory.appendingPathComponent("AynaAjan-\(version)")
                try? FileManager.default.removeItem(at: dir)
                try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                let zipURL = dir.appendingPathComponent(asset)
                let app = dir.appendingPathComponent("Ayna Ajan.app")
                try zip.write(to: zipURL)
                guard run("/usr/bin/ditto", ["-x", "-k", zipURL.path, dir.path]), signatureValid(app),
                      bundleVersion(app) == version else {
                    throw UpdateError("Güncelleme doğrulanamadı (imza ya da sürüm).")
                }
                DispatchQueue.main.async { completion(version, app) }
            } catch let e as UpdateError {
                failed(e.message)
            } catch {
                // Ağ yok, GitHub'a ulaşılamadı: sessizce sonraki denemeye kalır.
            }
        }
    }

    /// Yeni uygulamayı eskisinin yerine koyar (hemen önce imzası yeniden doğrulanır) ve bu süreç
    /// kapandıktan sonra yeniden açar.
    static func install(_ app: URL) -> Bool {
        guard signatureValid(app) else { return false }
        let current = Bundle.main.bundleURL
        do {
            _ = try FileManager.default.replaceItemAt(current, withItemAt: app)
        } catch {
            return false
        }
        let path = current.path.replacingOccurrences(of: "'", with: "'\\''")
        let pid = ProcessInfo.processInfo.processIdentifier
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = ["-c", "while kill -0 \(pid) 2>/dev/null; do sleep 0.2; done; /usr/bin/open '\(path)'"]
        try? p.run()
        return true
    }

    private struct UpdateError: Error {
        let message: String
        init(_ m: String) { message = m }
    }

    private static func fetch(_ req: URLRequest) throws -> Data {
        var result: Result<Data, Error> = .failure(URLError(.unknown))
        let done = DispatchSemaphore(value: 0)
        URLSession.shared.dataTask(with: req) { data, response, error in
            if let data, (response as? HTTPURLResponse)?.statusCode == 200 {
                result = .success(data)
            } else {
                result = .failure(error ?? URLError(.badServerResponse))
            }
            done.signal()
        }.resume()
        done.wait()
        return try result.get()
    }

    private static func verify(_ manifest: Data, _ sig: Data) -> Bool {
        let attrs: [String: Any] = [
            kSecAttrKeyType as String: kSecAttrKeyTypeRSA,
            kSecAttrKeyClass as String: kSecAttrKeyClassPublic,
            kSecAttrKeySizeInBits as String: 2048,
        ]
        guard let key = SecKeyCreateWithData(releaseKey as CFData, attrs as CFDictionary, nil) else { return false }
        return SecKeyVerifySignature(key, .rsaSignatureMessagePKCS1v15SHA256, manifest as CFData, sig as CFData, nil)
    }

    private static func signatureValid(_ app: URL) -> Bool {
        run("/usr/bin/codesign", ["--verify", "--deep", "--strict", "-R=\(requirement)", app.path])
    }

    private static func bundleVersion(_ app: URL) -> Int? {
        let plist = app.appendingPathComponent("Contents/Info.plist")
        guard let dict = NSDictionary(contentsOf: plist), let v = dict["CFBundleVersion"] as? String else { return nil }
        return Int(v)
    }

    private static func sha256(in body: String, for name: String) -> String? {
        for line in body.split(whereSeparator: \.isNewline) {
            let parts = line.split(separator: " ", omittingEmptySubsequences: true)
            if parts.count >= 2, parts[1].trimmingCharacters(in: CharacterSet(charactersIn: "*")) == name,
               isHex(String(parts[0]), length: 64) {
                return parts[0].lowercased()
            }
        }
        return nil
    }

    @discardableResult
    private static func run(_ tool: String, _ args: [String]) -> Bool {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: tool)
        p.arguments = args
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        do { try p.run() } catch { return false }
        p.waitUntilExit()
        return p.terminationStatus == 0
    }
}

// MARK: - Uygulama

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private var statusItem: NSStatusItem!
    private var discovery: Discovery?
    private var listener: NWListener?
    private var active: Session?
    private var pending: [Session] = []
    private var connectionsByIP: [String: Int] = [:]
    private var asking = false
    /// Reddedilen adresler bir süre yeniden sorulmaz; ağdan gelen soru seli olmasın.
    private var deniedUntil: [String: Date] = [:]
    private var recentPrompts: [Date] = []
    private var handshakes: [(Date, String)] = []
    /// İndirilmiş, uygulanmayı bekleyen güncelleme (oturum yokken uygulanır).
    private var pendingUpdate: (Int, URL)?
    private var updateNote: String?
    /// Bilgisayar adı bir kez okunur (her bulma isteğinde ağ adı çözümlemesi yapılmasın).
    private lazy var macName = cleanName(Host.current().localizedName ?? "Mac")

    func applicationDidFinishLaunching(_ notification: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        updateIcon()
        let menu = NSMenu()
        menu.delegate = self
        statusItem.menu = menu
        startNetwork()
        requestPermissions()
        // Güncelleme: açılıştan 1 dk sonra, sonra 6 saatte bir; indirilen sürüm oturum yokken kurulur.
        DispatchQueue.main.asyncAfter(deadline: .now() + 60) { [weak self] in self?.checkForUpdate() }
        Timer.scheduledTimer(withTimeInterval: 6 * 3600, repeats: true) { [weak self] _ in self?.checkForUpdate() }
        Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in self?.installIfIdle() }
    }

    private func checkForUpdate() {
        guard pendingUpdate == nil else { return }
        Updater.check(completion: { [weak self] version, app in
            self?.pendingUpdate = (version, app)
            self?.updateNote = "Sürüm \(version) hazır; bağlantı yokken kurulacak."
            self?.installIfIdle()
        }, failed: { [weak self] message in
            DispatchQueue.main.async { self?.updateNote = message }
        })
    }

    private func installIfIdle() {
        guard let update = pendingUpdate, active == nil, pending.isEmpty else { return }
        let (version, app) = update
        pendingUpdate = nil
        if Updater.install(app) {
            NSApp.terminate(nil)
        } else {
            updateNote = "Sürüm \(version) kurulamadı (Uygulamalar klasörüne yazılamadı)."
        }
    }

    @objc private func checkNow() {
        updateNote = "Güncelleme denetleniyor…"
        checkForUpdate()
    }

    private func updateIcon() {
        let symbol = active == nil ? "display" : "display.and.arrow.down"
        statusItem.button?.image = NSImage(systemSymbolName: symbol, accessibilityDescription: "Ayna Ajan")
            ?? NSImage(systemSymbolName: "display", accessibilityDescription: "Ayna Ajan")
    }

    private func formatted(_ code: String) -> String {
        var out = ""
        for (i, c) in code.enumerated() {
            if i > 0 && (code.count - i) % 3 == 0 { out.append(" ") }
            out.append(c)
        }
        return out
    }

    // MARK: İzinler

    private var screenAllowed: Bool { CGPreflightScreenCaptureAccess() }
    private var inputAllowed: Bool { AXIsProcessTrusted() }

    private func requestPermissions() {
        if !screenAllowed { CGRequestScreenCaptureAccess() }
        if !inputAllowed {
            let options = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
            _ = AXIsProcessTrustedWithOptions(options)
        }
    }

    // MARK: Ağ

    private func startNetwork() {
        discovery = Discovery(port: agentPort) { [weak self] message, _ in
            guard let self, Store.accepting else { return nil }
            let code = Store.code
            let parts = message.trimmingCharacters(in: .whitespacesAndNewlines).split(separator: " ").map(String.init)
            guard parts.count == 5, parts[0] == "AYNA1", parts[1] == "FIND", parts[2] == code,
                  isHex(parts[3], length: 32), isHex(parts[4], length: 32) else { return nil }
            let name = self.macName
            let signed = "here|\(code)|\(parts[4])|\(agentPort)|\(name)"
            let proof = Store.trusted.first { $0.id == parts[3] }.flatMap { unhex($0.token) }
                .map { hmacHex(key: $0, message: signed) } ?? "-"
            return "AYNA1 HERE \(code) \(agentPort) \(proof) \(name)"
        }
        do {
            // Nagle kapalı: küçük paketler (fare, yoklama yanıtı) bekletilmeden gider.
            let tcp = NWProtocolTCP.Options()
            tcp.noDelay = true
            let params = NWParameters(tls: nil, tcp: tcp)
            params.allowLocalEndpointReuse = true
            let l = try NWListener(using: params, on: NWEndpoint.Port(rawValue: agentPort)!)
            l.newConnectionHandler = { [weak self] conn in self?.accept(conn) }
            l.start(queue: .main)
            listener = l
        } catch {
            show(error: "Bağlantı noktası \(agentPort) açılamadı: \(error.localizedDescription)")
        }
    }

    private func accept(_ conn: NWConnection) {
        guard let ip = ipv4String(conn.endpoint), isLocalPeer(ip), connectionsByIP[ip, default: 0] < 2,
              pending.count < 8 else { conn.cancel(); return }
        connectionsByIP[ip, default: 0] += 1
        let s = Session(conn: conn, ip: ip, app: self)
        pending.append(s)
        s.start()
    }

    /// Etkin oturum varken yeni oturum yalnız aynı (kimliğini kanıtlamış) bilgisayardan olabilir.
    func mayStart(deviceID: String, authenticated: Bool) -> Bool {
        guard let a = active else { return true }
        return authenticated && a.deviceID == deviceID
    }

    func sessionStarted(_ s: Session) {
        pending.removeAll { $0 === s }
        active?.close()
        active = s
        updateIcon()
    }

    func sessionEnded(_ s: Session) {
        pending.removeAll { $0 === s }
        connectionsByIP[s.ip, default: 1] -= 1
        if connectionsByIP[s.ip] == 0 { connectionsByIP[s.ip] = nil }
        if active === s {
            active = nil
            updateIcon()
        }
    }

    /// El sıkışma sınırı: adres başına dakikada 5, toplam dakikada 20. Doğrulama kodunu denenerek
    /// eşitlemeye (ve yük bindirmeye) karşı.
    func allowHandshake(ip: String) -> Bool {
        let now = Date()
        handshakes = handshakes.filter { now.timeIntervalSince($0.0) < 60 }
        guard handshakes.count < 20, handshakes.filter({ $0.1 == ip }).count < 5 else { return false }
        handshakes.append((now, ip))
        return true
    }

    /// Onay sorusu sınırı: adres başına reddedildikten sonra 10 dk; genel olarak 10 sn'de bir ve
    /// 10 dk'da en fazla 6 soru.
    private func mayAsk(ip: String) -> Bool {
        let now = Date()
        deniedUntil = deniedUntil.filter { $0.value > now }
        recentPrompts = recentPrompts.filter { now.timeIntervalSince($0) < 600 }
        if deniedUntil[ip] != nil || asking { return false }
        if let last = recentPrompts.last, now.timeIntervalSince(last) < 10 { return false }
        return recentPrompts.count < 6
    }

    /// Yeni (ya da kimliğini kanıtlayamayan, ya da adresi değişen) bilgisayar için onay sorusu.
    func ask(session: Session, name: String, ip: String, id: String, sas: String, known: TrustedDevice?,
             authenticated: Bool, completion: @escaping (Bool) -> Void) {
        guard mayAsk(ip: ip) else { completion(false); return }
        asking = true
        recentPrompts.append(Date())
        // Soru, GCD ana kuyruğunun dışında (çalışma döngüsünde) açılır: kipli pencere açıkken ağ
        // olayları (ana kuyruk) işlenmeye devam eder; "WAIT" de önce gönderilmiş olur.
        RunLoop.main.perform(inModes: [.common]) { [self] in
            defer { asking = false }
            NSApp.activate(ignoringOtherApps: true)
            let alert = NSAlert()
            switch (known, authenticated) {
            case (.some(let k), true):
                alert.messageText = "“\(k.name)” yeni bir adresten bağlanıyor"
                alert.informativeText = "Güvenilen bu bilgisayar artık \(ip) adresinde (önceden \(k.ip))."
            case (.some(let k), false):
                alert.alertStyle = .critical
                alert.messageText = "“\(name)” kimliğini kanıtlayamadı"
                alert.informativeText = """
                    Bu cihaz kendini güvendiğiniz “\(k.name)” gibi gösteriyor ama onun anahtarına sahip değil. \
                    Kendi bilgisayarınızın eşleşmesini sıfırlamadıysanız reddedin.
                    """
            default:
                alert.messageText = "“\(name)” bu Mac'e bağlanmak istiyor"
                alert.informativeText = """
                    İzin verirseniz bu bilgisayar Mac'in ekranını görüp fare ve klavyeyle kontrol edebilir \
                    ve güvenilenler listesine eklenir.
                    """
            }
            alert.informativeText += """


                Windows'taki Mac penceresinde görünen kod bununla aynı olmalı:
                \(sas)

                Adres: \(ip)   Cihaz: …\(id.suffix(6).uppercased())
                """
            // Varsayılan (Enter) düğme "Reddet": o an yazı yazan kullanıcı yanlışlıkla izin vermesin.
            alert.addButton(withTitle: "Reddet")
            alert.addButton(withTitle: "İzin ver")
            guard case .ready = session.conn.state else { completion(false); return }
            let allowed = alert.runModal() == .alertSecondButtonReturn
            // Yanıt geç verildiyse Windows çoktan vazgeçmiştir.
            guard case .ready = session.conn.state else { completion(false); return }
            if !allowed { deniedUntil[ip] = Date().addingTimeInterval(600) }
            completion(allowed)
        }
    }

    // MARK: Menü

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        let codeItem = item("Kod: \(formatted(Store.code))", #selector(copyCode))
        codeItem.toolTip = "Kopyalamak için tıklayın"
        menu.addItem(codeItem)
        if let s = active {
            menu.addItem(item("Bağlı: \(s.name) (\(s.ip))", nil))
            menu.addItem(item("Bağlantıyı kes", #selector(disconnect)))
        }
        let accepting = item("Bağlantılara izin ver", #selector(toggleAccepting))
        accepting.state = Store.accepting ? .on : .off
        menu.addItem(accepting)
        menu.addItem(.separator())

        let trustedItem = item("Güvenilen bilgisayarlar", nil)
        let sub = NSMenu()
        if Store.trusted.isEmpty { sub.addItem(item("Henüz yok", nil)) }
        for device in Store.trusted {
            let entry = NSMenuItem(title: "\(device.name) (\(device.ip))", action: nil, keyEquivalent: "")
            let actions = NSMenu()
            let remove = item("Güveni kaldır", #selector(removeTrusted(_:)))
            remove.representedObject = device.id
            actions.addItem(remove)
            entry.submenu = actions
            sub.addItem(entry)
        }
        trustedItem.submenu = sub
        menu.addItem(trustedItem)
        menu.addItem(.separator())
        menu.addItem(item(screenAllowed ? "Ekran Kaydı izni: verildi" : "Ekran Kaydı izni ver…", #selector(openScreenPrivacy)))
        menu.addItem(item(inputAllowed ? "Erişilebilirlik izni: verildi" : "Erişilebilirlik izni ver…", #selector(openInputPrivacy)))
        let version = Updater.build > 0 ? "Sürüm \(Updater.build)" : "Geliştirme sürümü"
        menu.addItem(item(updateNote.map { "\(version) · \($0)" } ?? version, nil))
        menu.addItem(item("Güncellemeleri denetle", #selector(checkNow)))
        let login = item("Oturum açınca başlat", #selector(toggleLogin))
        login.state = SMAppService.mainApp.status == .enabled ? .on : .off
        menu.addItem(login)
        menu.addItem(.separator())
        menu.addItem(item("Çıkış", #selector(quit)))
    }

    private func item(_ title: String, _ action: Selector?) -> NSMenuItem {
        let i = NSMenuItem(title: title, action: action, keyEquivalent: "")
        i.target = self
        i.isEnabled = action != nil
        return i
    }

    @objc private func copyCode() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(Store.code, forType: .string)
    }

    @objc private func disconnect() { active?.close() }

    @objc private func toggleAccepting() {
        Store.accepting.toggle()
        if !Store.accepting { active?.close() }
    }

    @objc private func removeTrusted(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        Store.trusted = Store.trusted.filter { $0.id != id }
    }

    @objc private func openScreenPrivacy() {
        if !screenAllowed { CGRequestScreenCaptureAccess() }
        open("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
    }

    @objc private func openInputPrivacy() {
        requestPermissions()
        open("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
    }

    private func open(_ url: String) {
        if let u = URL(string: url) { NSWorkspace.shared.open(u) }
    }

    @objc private func toggleLogin() {
        do {
            if SMAppService.mainApp.status == .enabled {
                try SMAppService.mainApp.unregister()
            } else {
                try SMAppService.mainApp.register()
            }
        } catch {
            show(error: "Oturum açılış ayarı değiştirilemedi.", detail: error.localizedDescription)
        }
    }

    @objc private func quit() {
        active?.close()
        NSApp.terminate(nil)
    }

    func applicationWillTerminate(_ notification: Notification) {
        PhoneMirror.shutdown()
    }

    /// Uyarı, GCD ana kuyruğunun dışında açılır (açıkken oturumun ağ olayları durmasın).
    func show(error: String, detail: String = "") {
        RunLoop.main.perform(inModes: [.common]) {
            NSApp.activate(ignoringOtherApps: true)
            let alert = NSAlert()
            alert.alertStyle = .warning
            alert.messageText = error
            alert.informativeText = detail
            alert.runModal()
        }
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.accessory)
app.run()
