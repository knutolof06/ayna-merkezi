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

import AppKit
import CoreMedia
import CryptoKit
import Network
import ScreenCaptureKit
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

    init(onFrame: @escaping (Data, Bool) -> Void, onSize: @escaping (Int, Int) -> Void, onError: @escaping (String) -> Void) {
        self.onFrame = onFrame
        self.onSize = onSize
        self.onError = onError
    }

    func start() {
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
            let scale = min(1.0, Double(maxVideoSide) / Double(max(pw, ph)))
            let w = Int(Double(pw) * scale) & ~1, h = Int(Double(ph) * scale) & ~1
            let config = SCStreamConfiguration()
            config.width = w
            config.height = h
            config.minimumFrameInterval = CMTime(value: 1, timescale: 60)
            config.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
            config.showsCursor = true
            config.queueDepth = 5
            let filter = SCContentFilter(display: display, excludingWindows: [])
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
            self.onSize(w, h)
        }
    }

    func stop() {
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

    private func makeEncoder(width: Int, height: Int) {
        var session: VTCompressionSession?
        let status = VTCompressionSessionCreate(
            allocator: kCFAllocatorDefault, width: Int32(width), height: Int32(height),
            codecType: kCMVideoCodecType_H264, encoderSpecification: nil, imageBufferAttributes: nil,
            compressedDataAllocator: nil, outputCallback: nil, refcon: nil, compressionSessionOut: &session)
        guard status == noErr, let s = session else {
            onError("Görüntü kodlayıcı açılamadı (\(status)).")
            return
        }
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ProfileLevel, value: kVTProfileLevel_H264_Main_AutoLevel)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: NSNumber(value: 12_000_000))
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
            guard status == noErr, let sbuf, let frame = Capturer.annexB(sbuf) else { return }
            self?.onFrame(frame.0, frame.1)
        }
    }

    /// Anahtar kare: ekran değişmiyorsa son kare yeniden kodlanır.
    func requestKeyframe() {
        queue.async {
            self.forceKey = true
            if let pb = self.lastBuffer { self.encode(pb) }
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

    /// Basılı değiştirici tuşlardan (sol/sağ ayrı) bayraklar.
    private var flags: CGEventFlags {
        var f: CGEventFlags = capsLock ? [.maskAlphaShift] : []
        for k in keys { if let m = Injector.modifier(k) { f.insert(m) } }
        return f
    }

    private func point(_ x: UInt16, _ y: UInt16) -> CGPoint {
        let b = CGDisplayBounds(CGMainDisplayID())
        return CGPoint(x: b.minX + CGFloat(x) / 65535 * b.width, y: b.minY + CGFloat(y) / 65535 * b.height)
    }

    func move(_ x: UInt16, _ y: UInt16) {
        let type: CGEventType, button: CGMouseButton
        if buttons.contains(0) { (type, button) = (.leftMouseDragged, .left) }
        else if buttons.contains(1) { (type, button) = (.rightMouseDragged, .right) }
        else if buttons.contains(2) { (type, button) = (.otherMouseDragged, .center) }
        else { (type, button) = (.mouseMoved, .left) }
        let e = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: point(x, y), mouseButton: button)
        e?.flags = flags
        e?.post(tap: .cghidEventTap)
    }

    func button(_ b: UInt8, down: Bool, _ x: UInt16, _ y: UInt16) {
        let p = point(x, y)
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
        let e = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down)
        e?.flags = flags
        e?.post(tap: .cghidEventTap)
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
    private let injector = Injector()
    private var lastData = Date()
    private var watchdog: Timer?
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
                if self.stage == .approval || self.buffer.count + data.count > 70_000 {
                    self.close()
                    return
                }
                self.buffer.append(data)
                self.lastData = Date()
                self.process()
            }
            if complete || error != nil { self.close() } else { self.receive() }
        }
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
                guard buffer.count >= 4 else { return }
                let b = [UInt8](buffer.prefix(4))
                let n = Int(b[0]) << 24 | Int(b[1]) << 16 | Int(b[2]) << 8 | Int(b[3])
                guard n >= 17, n <= 65_536 else { close(); return }
                guard buffer.count >= 4 + n else { return }
                _ = take(4)
                let sealed = take(n)
                guard let plain = open(sealed) else { close(); return }
                handle(plain)
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

    private func sendNow(_ plain: Data) {
        guard let key = txKey, let box = try? AES.GCM.seal(plain, using: key, nonce: nonceData(txCounter)) else { return }
        txCounter += 1
        var frame = Data()
        let sealed = box.ciphertext + box.tag
        let n = UInt32(sealed.count)
        frame.append(contentsOf: [UInt8(n >> 24), UInt8((n >> 16) & 0xFF), UInt8((n >> 8) & 0xFF), UInt8(n & 0xFF)])
        frame.append(sealed)
        let size = frame.count
        inFlight += size
        conn.send(content: frame, completion: .contentProcessed { [weak self] _ in
            self?.sendQueue.async { self?.inFlight -= size }
        })
    }

    /// Ağ yavaşsa kareler biriktirilmez: bekleyen veri çoksa kare atlanır, sonra anahtar kareyle sürer.
    private func sendVideo(_ data: Data, key: Bool) {
        sendQueue.async { [weak self] in
            guard let self else { return }
            if self.inFlight > 4_000_000 {
                self.needKey = true
                return
            }
            if self.needKey && !key {
                if self.inFlight < 1_000_000 {
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
        let cap = Capturer(
            onFrame: { [weak self] data, key in self?.sendVideo(data, key: key) },
            onSize: { [weak self] w, h in
                self?.send(Data([0x02] + be16(UInt16(clamping: w)) + be16(UInt16(clamping: h))))
            },
            onError: { [weak self] message in
                DispatchQueue.main.async { self?.app?.show(error: message) }
            })
        capturer = cap
        cap.start()
        process()
    }

    // --- Oturum mesajları

    private func handle(_ m: Data) {
        guard let type = m.first else { return }
        let b = [UInt8](m)
        switch type {
        case 0x10 where b.count >= 5:
            injector.move(readBE16(m, 1), readBE16(m, 3))
        case 0x11 where b.count >= 7:
            injector.button(b[1], down: b[2] == 1, readBE16(m, 3), readBE16(m, 5))
        case 0x12 where b.count >= 5:
            injector.scroll(dx: Int16(bitPattern: readBE16(m, 1)), dy: Int16(bitPattern: readBE16(m, 3)))
        case 0x13 where b.count >= 4:
            injector.key(readBE16(m, 1), down: b[3] == 1)
        case 0x14:
            capturer?.requestKeyframe()
        case 0x15 where b.count >= 9:
            send(Data([0x03]) + m.dropFirst().prefix(8))
        case 0x16 where b.count >= 2:
            capturer?.setPaused(b[1] == 1)
        case 0x17:
            close()
        default:
            break
        }
    }

    func close() {
        guard stage != .closed else { return }
        let wasStreaming = stage == .streaming
        stage = .closed
        watchdog?.invalidate()
        capturer?.stop()
        capturer = nil
        injector.releaseAll()
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

/// GitHub'daki son sürümü denetler; yeni sürümü indirir, SHA-256 özetini ve imzasını (bu projenin
/// sabit imza anahtarı) doğrular, uygulamayı değiştirip yeniden başlatır. Sürüm numarası GitHub
/// Actions çalıştırma numarasıdır (CFBundleVersion); 0 olan yerel derlemelerde kapalıdır.
enum Updater {
    static let latest = URL(string: "https://api.github.com/repos/knutolof06/ayna-merkezi/releases/latest")!
    static let downloadPrefix = "https://github.com/knutolof06/ayna-merkezi/releases/download/"
    static let asset = "AynaAjan.zip"
    /// Yalnız bu kimlik ve bu sertifikayla imzalanmış uygulama kabul edilir.
    static let requirement =
        "identifier \"tr.ayna.ajan\" and certificate leaf = H\"c7251ef44643fc0dcf58efe33fd4d5e02a215fa0\""

    static var build: Int { Int(Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "") ?? 0 }

    private struct Release: Decodable {
        let tag_name: String
        let body: String?
        let assets: [Asset]
    }

    private struct Asset: Decodable {
        let name: String
        let browser_download_url: String
    }

    /// Yeni sürüm hazırlanırsa (indirilip doğrulandıysa) ana kuyrukta sürüm ve açılmış uygulama yolu.
    static func check(completion: @escaping (Int, URL) -> Void, failed: @escaping (String) -> Void) {
        guard build > 0 else { return }
        var req = URLRequest(url: latest)
        req.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        URLSession.shared.dataTask(with: req) { data, _, error in
            guard let data, error == nil, let release = try? JSONDecoder().decode(Release.self, from: data),
                  let version = Int(release.tag_name.dropFirst()), version > build else { return }
            guard let a = release.assets.first(where: { $0.name == asset }),
                  a.browser_download_url.hasPrefix(downloadPrefix), let url = URL(string: a.browser_download_url),
                  let expected = sha256(in: release.body ?? "", for: asset) else {
                failed("Sürüm \(version) eksik yayınlanmış.")
                return
            }
            URLSession.shared.dataTask(with: url) { zip, _, error in
                guard let zip, error == nil, hex(SHA256.hash(data: zip)) == expected else {
                    failed("İndirilen güncellemenin özeti tutmuyor.")
                    return
                }
                let dir = FileManager.default.temporaryDirectory.appendingPathComponent("AynaAjan-\(version)")
                try? FileManager.default.removeItem(at: dir)
                try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                let zipURL = dir.appendingPathComponent(asset)
                let app = dir.appendingPathComponent("Ayna Ajan.app")
                guard (try? zip.write(to: zipURL)) != nil,
                      run("/usr/bin/ditto", ["-x", "-k", zipURL.path, dir.path]),
                      run("/usr/bin/codesign", ["--verify", "--deep", "--strict", "-R=\(requirement)", app.path]) else {
                    failed("Güncelleme doğrulanamadı (imza).")
                    return
                }
                DispatchQueue.main.async { completion(version, app) }
            }.resume()
        }.resume()
    }

    /// Yeni uygulamayı eskisinin yerine koyar ve yeniden başlatır.
    static func install(_ app: URL) -> Bool {
        let current = Bundle.main.bundleURL
        do {
            _ = try FileManager.default.replaceItemAt(current, withItemAt: app)
        } catch {
            return false
        }
        let path = current.path.replacingOccurrences(of: "'", with: "'\\''")
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = ["-c", "sleep 1; /usr/bin/open '\(path)'"]
        try? p.run()
        return true
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
            let params = NWParameters.tcp
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
