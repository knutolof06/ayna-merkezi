// Ayna Ajan: Mac'in menü çubuğunda duran yardımcı.
//
// - Mac'in kodunu (RustDesk kimliği) gösterir; Windows'taki Ayna Merkezi bu kodla Mac'i yerel ağda
//   bulur (UDP 21130, "AYNA1 FIND <kod> <cihaz> <nonce>" → "AYNA1 HERE <kod> <port> <imza|-> <ad>").
// - Bir Windows bilgisayarı ilk kez bağlanmak istediğinde "İzin ver / Reddet" sorar (TCP 21130:
//   "AYNA1 NONCE <n>" → "AYNA1 HELLO <cihaz> <imza|-> <ad>" → "AYNA1 OK <anahtar|->"). İzin verilen
//   bilgisayara özel bir anahtar verilir; sonraki bağlantılarda hem Mac'in yanıtı hem bilgisayarın
//   kimliği bu anahtarla (HMAC-SHA256) kanıtlanır. İzin verilen adres RustDesk'in IP beyaz
//   listesine yazılır; RustDesk listede olmayan adresten (internet üzerinden gelenler dahil)
//   bağlantı kabul etmez.
// - RustDesk'i doğrudan IP ve kalıcı parola ile çalışacak biçimde ayarlar (yönetici onayıyla).
//
// Uzak masaüstünün kendisi (görüntü, kontrol, şifreleme) RustDesk'tir; bu uygulama parolayı
// saklamaz, yalnız RustDesk'e komut satırından iletir.

import AppKit
import CryptoKit
import Network
import ServiceManagement

let agentPort: UInt16 = 21130
let rustDeskPort = 21118
let rustDeskApp = "/Applications/RustDesk.app"
let rustDeskBinary = rustDeskApp + "/Contents/MacOS/RustDesk"
/// Beyaz liste boşken RustDesk herkesi kabul eder; hiçbir bilgisayara güvenilmiyorsa yalnız bu Mac.
let closedWhitelist = "127.0.0.1"
let minPasswordLength = 10

struct TrustedDevice: Codable, Equatable {
    var id: String
    var name: String
    var ip: String
    /// Bu bilgisayara verilen anahtar (64 onaltılık).
    var token: String?
}

// MARK: - Yardımcılar

func hex(_ bytes: [UInt8]) -> String { bytes.map { String(format: "%02x", $0) }.joined() }

func unhex(_ s: String) -> [UInt8]? {
    guard s.count % 2 == 0 else { return nil }
    var out: [UInt8] = []
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

func randomHex(_ count: Int) -> String {
    var g = SystemRandomNumberGenerator()
    return hex((0..<count).map { _ in UInt8.random(in: 0...255, using: &g) })
}

func hmacHex(key: String, message: String) -> String? {
    guard let k = unhex(key) else { return nil }
    let mac = HMAC<SHA256>.authenticationCode(for: Data(message.utf8), using: SymmetricKey(data: k))
    return hex(Array(mac))
}

/// Sabit sürede karşılaştırma.
func sameProof(_ a: String, _ b: String) -> Bool {
    let x = Array(a.utf8), y = Array(b.utf8)
    guard x.count == y.count else { return false }
    return zip(x, y).reduce(0) { $0 | ($1.0 ^ $1.1) } == 0
}

/// Ekranda gösterilen adlardan denetim, görünmez ve yön değiştiren karakterler atılır (ad sahteciliği).
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

// MARK: - Kalıcı ayarlar

enum Store {
    static let defaults = UserDefaults.standard

    static var trusted: [TrustedDevice] {
        get {
            guard let data = defaults.data(forKey: "trusted"),
                  let list = try? JSONDecoder().decode([TrustedDevice].self, from: data) else { return [] }
            return list
        }
        set { defaults.set(try? JSONEncoder().encode(newValue), forKey: "trusted") }
    }

    static var accepting: Bool {
        get { defaults.object(forKey: "accepting") as? Bool ?? true }
        set { defaults.set(newValue, forKey: "accepting") }
    }

    static var configured: Bool {
        get { defaults.bool(forKey: "configured") }
        set { defaults.set(newValue, forKey: "configured") }
    }

    static func whitelist(_ list: [TrustedDevice]) -> String {
        let ips = Array(Set(list.map(\.ip))).sorted()
        return ips.isEmpty ? closedWhitelist : ips.joined(separator: ",")
    }
}

// MARK: - RustDesk komut satırı

enum RustDesk {
    static var installed: Bool { FileManager.default.isExecutableFile(atPath: rustDeskBinary) }

    /// Yönetici gerektirmeyen `--get-id`.
    static func id(completion: @escaping (String?) -> Void) {
        guard installed else { completion(nil); return }
        DispatchQueue.global().async {
            let p = Process()
            p.executableURL = URL(fileURLWithPath: rustDeskBinary)
            p.arguments = ["--get-id"]
            let out = Pipe()
            p.standardOutput = out
            p.standardError = FileHandle.nullDevice
            do { try p.run() } catch { DispatchQueue.main.async { completion(nil) }; return }
            DispatchQueue.global().asyncAfter(deadline: .now() + 10) { if p.isRunning { p.terminate() } }
            let data = out.fileHandleForReading.readDataToEndOfFile()
            p.waitUntilExit()
            let text = String(decoding: data, as: UTF8.self)
            let id = text.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }
                .last { !$0.isEmpty && $0.allSatisfy { $0.isLetter || $0.isNumber || $0 == "-" || $0 == "_" } }
            DispatchQueue.main.async { completion(id) }
        }
    }

    /// RustDesk ayar komutları yönetici (root) ister; hepsi tek parola sorusunda çalışır. RustDesk
    /// hatayı çıkış koduyla bildirmediği için sonuç, çıktının son satırının `expected` olmasıyla
    /// doğrulanır (betik, ayarları geri okuyup karşılaştırarak bu satırı yazar).
    static func runAsAdmin(_ commands: [[String]], expected: String? = nil) -> (Bool, String) {
        runShellAsAdmin(commands.map { $0.map(shellQuote).joined(separator: " ") }.joined(separator: " ; "), expected: expected)
    }

    static func runShellAsAdmin(_ shell: String, expected: String?) -> (Bool, String) {
        let source = "do shell script \"\(appleScriptEscape(shell))\" with administrator privileges"
        var error: NSDictionary?
        guard let script = NSAppleScript(source: source) else { return (false, "Komut hazırlanamadı.") }
        let result = script.executeAndReturnError(&error)
        if let error {
            let message = error[NSAppleScript.errorMessage] as? String ?? "İptal edildi."
            return (false, message)
        }
        let output = result.stringValue ?? ""
        let lines = output.components(separatedBy: CharacterSet.newlines).map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        if lines.contains(where: { $0.contains("required") || $0.contains("disabled") }) {
            return (false, output)
        }
        if let expected, lines.last != expected {
            return (false, "RustDesk ayarı doğrulanamadı (okunan: \(lines.last ?? "boş")).")
        }
        return (true, output)
    }

    static func option(_ key: String, _ value: String) -> [String] {
        [rustDeskBinary, "--option", key, value]
    }

    static func read(_ key: String) -> [String] {
        [rustDeskBinary, "--option", key]
    }

    static func shellQuote(_ s: String) -> String {
        "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    static func appleScriptEscape(_ s: String) -> String {
        s.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
    }

    static var bin: String { shellQuote(rustDeskBinary) }

    /// Beyaz listeyi yazar ve geri okuyarak doğrular; tutmazsa eski değeri geri yazar (RustDesk'te
    /// izinli olup listede görünmeyen adres kalmasın).
    static func setWhitelist(_ list: [TrustedDevice], previous: [TrustedDevice]) -> (Bool, String) {
        let new = shellQuote(Store.whitelist(list)), old = shellQuote(Store.whitelist(previous))
        let shell = """
            \(bin) --option whitelist \(new); \
            if [ "$(\(bin) --option whitelist)" = \(new) ]; then echo AYNA_OK; \
            else \(bin) --option whitelist \(old); echo AYNA_FAIL; fi
            """
        return runShellAsAdmin(shell, expected: "AYNA_OK")
    }

    /// İlk kurulum: önce beyaz liste yazılıp doğrulanır; ancak tuttuysa doğrudan IP erişimi ve parola
    /// açılır. Beyaz liste tutmazsa RustDesk her adresten parolayla erişilebilir hâle gelmez.
    static func configure(whitelist: [TrustedDevice], password: String) -> (Bool, String) {
        let w = shellQuote(Store.whitelist(whitelist))
        let options = [
            option("direct-access-port", String(rustDeskPort)),
            option("approve-mode", "password"),
            option("verification-method", "use-permanent-password"),
            option("enable-lan-discovery", "N"),
            [rustDeskBinary, "--password", password],
            option("direct-server", "Y"),
        ].map { $0.map(shellQuote).joined(separator: " ") }.joined(separator: " ; ")
        let shell = """
            \(bin) --option whitelist \(w); \
            if [ "$(\(bin) --option whitelist)" = \(w) ]; then \(options); \
            if [ "$(\(bin) --option direct-server)" = Y ]; then echo AYNA_OK; else echo AYNA_FAIL; fi; \
            else echo AYNA_FAIL; fi
            """
        return runShellAsAdmin(shell, expected: "AYNA_OK")
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

/// Yalnız bu Mac'in kendi alt ağlarındaki özel adresler kabul edilir (yönlendirilmiş/VPN ağları değil).
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
        guard isLocalPeer(ip), let answer = reply(message, ip) else { return }
        let bytes = Array(answer.utf8)
        _ = withUnsafePointer(to: &from) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { sendto(fd, bytes, bytes.count, 0, $0, len) }
        }
    }
}

// MARK: - Uygulama

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private var statusItem: NSStatusItem!
    private var code: String?
    private var discovery: Discovery?
    private var listener: NWListener?
    private var openConnections = 0
    /// Adres başına açık bağlantı (tek bir cihaz bütün yuvaları tutamasın).
    private var connectionsByIP: [String: Int] = [:]
    /// Aynı anda tek onay sorusu.
    private var asking = false
    /// Reddedilen adresler bir süre yeniden sorulmaz; ağdan gelen soru seli olmasın.
    private var deniedUntil: [String: Date] = [:]
    /// Son onay sorularının zamanları (genel sınır).
    private var recentPrompts: [Date] = []

    func applicationDidFinishLaunching(_ notification: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let button = statusItem.button {
            button.image = NSImage(systemSymbolName: "display", accessibilityDescription: "Ayna Ajan")
        }
        let menu = NSMenu()
        menu.delegate = self
        statusItem.menu = menu
        refreshCode()
        Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in self?.refreshCode() }
        startNetwork()
        if RustDesk.installed && !Store.configured {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1) { self.setup() }
        }
    }

    private func refreshCode() {
        RustDesk.id { [weak self] id in self?.code = id }
    }

    private var plainCode: String? { code?.filter { !$0.isWhitespace } }

    private func formatted(_ code: String) -> String {
        guard code.allSatisfy(\.isNumber) else { return code }
        var out = ""
        for (i, c) in code.enumerated() {
            if i > 0 && (code.count - i) % 3 == 0 { out.append(" ") }
            out.append(c)
        }
        return out
    }

    private func device(_ id: String) -> TrustedDevice? {
        Store.trusted.first { $0.id == id }
    }

    // MARK: Ağ

    private func startNetwork() {
        discovery = Discovery(port: agentPort) { [weak self] message, _ in
            guard let self, Store.accepting, let code = self.plainCode else { return nil }
            let parts = message.trimmingCharacters(in: .whitespacesAndNewlines).split(separator: " ").map(String.init)
            guard parts.count == 5, parts[0] == "AYNA1", parts[1] == "FIND", parts[2] == code,
                  isHex(parts[3], length: 32), isHex(parts[4], length: 32) else { return nil }
            // Daha önce eşleşilmiş bilgisayara yanıt, onun anahtarıyla imzalanır: aynı ağdaki başka bir
            // cihaz kendini bu Mac gibi gösteremez.
            let name = cleanName(Host.current().localizedName ?? "Mac")
            let signed = "here|\(code)|\(parts[4])|\(rustDeskPort)|\(name)"
            let proof = self.device(parts[3])?.token.flatMap { hmacHex(key: $0, message: signed) } ?? "-"
            return "AYNA1 HERE \(code) \(rustDeskPort) \(proof) \(name)"
        }
        do {
            let params = NWParameters.tcp
            params.allowLocalEndpointReuse = true
            let l = try NWListener(using: params, on: NWEndpoint.Port(rawValue: agentPort)!)
            l.newConnectionHandler = { [weak self] conn in self?.accept(conn) }
            l.start(queue: .main)
            listener = l
        } catch {
            NSLog("Ayna Ajan: TCP dinlenemedi: \(error)")
        }
    }

    private func accept(_ conn: NWConnection) {
        guard openConnections < 8, let ip = ipv4String(conn.endpoint), isLocalPeer(ip),
              connectionsByIP[ip, default: 0] < 2 else { conn.cancel(); return }
        openConnections += 1
        connectionsByIP[ip, default: 0] += 1
        // .failed ardından .cancelled da gelir: sayaç bir kez düşer.
        var counted = true
        conn.stateUpdateHandler = { [weak self] state in
            switch state {
            case .cancelled, .failed:
                if counted, let self {
                    counted = false
                    self.openConnections -= 1
                    self.connectionsByIP[ip, default: 1] -= 1
                    if self.connectionsByIP[ip] == 0 { self.connectionsByIP[ip] = nil }
                }
            default: break
            }
        }
        conn.start(queue: .main)
        let nonce = randomHex(16)
        send(conn, "AYNA1 NONCE \(nonce)")
        var greeted = false
        readLine(conn, buffer: Data()) { [weak self] line in
            greeted = true
            guard let self, let line else { conn.cancel(); return }
            self.hello(line, nonce: nonce, ip: ip, conn: conn)
        }
        // Selamlaşma 10 sn'de gelmezse bağlantı kapanır.
        DispatchQueue.main.asyncAfter(deadline: .now() + 10) { if !greeted { conn.cancel() } }
        // Yanıtsız kalan bağlantılar kapanır.
        DispatchQueue.main.asyncAfter(deadline: .now() + 120) { conn.cancel() }
    }

    private func readLine(_ conn: NWConnection, buffer: Data, done: @escaping (String?) -> Void) {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 512) { [weak self] data, _, complete, error in
            var buf = buffer
            if let data { buf.append(data) }
            if let nl = buf.firstIndex(of: 0x0A) {
                done(String(decoding: buf[..<nl], as: UTF8.self))
            } else if error != nil || complete || buf.count > 512 {
                done(nil)
            } else {
                self?.readLine(conn, buffer: buf, done: done)
            }
        }
    }

    private func send(_ conn: NWConnection, _ text: String, close: Bool = false) {
        conn.send(content: (text + "\n").data(using: .utf8), completion: .contentProcessed { _ in
            if close { conn.cancel() }
        })
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

    /// `AYNA1 HELLO <32 onaltılık kimlik> <imza|-> <ad…>`
    private func hello(_ line: String, nonce: String, ip: String, conn: NWConnection) {
        let parts = line.split(separator: " ", maxSplits: 4).map(String.init)
        guard parts.count >= 4, parts[0] == "AYNA1", parts[1] == "HELLO", isHex(parts[2], length: 32) else {
            send(conn, "AYNA1 DENY", close: true)
            return
        }
        let id = parts[2], proof = parts[3]
        let name = cleanName(parts.count > 4 ? parts[4] : "Windows")
        guard Store.accepting else { send(conn, "AYNA1 OFF", close: true); return }

        let known = device(id)
        let authenticated = known?.token.flatMap { hmacHex(key: $0, message: "hello|\(nonce)") }
            .map { sameProof($0, proof) } ?? false
        /// Onay satırı, kullanılan anahtarla imzalanır: Windows, eşleşilmiş Mac'in onayını doğrular.
        func approve(token: String, issue: Bool) {
            let proof = hmacHex(key: token, message: "ok|\(nonce)|\(id)") ?? "-"
            send(conn, "AYNA1 OK \(issue ? token : "-") \(proof)", close: true)
        }
        if authenticated, let known, let token = known.token, known.ip == ip {
            approve(token: token, issue: false)
            return
        }
        guard mayAsk(ip: ip) else { send(conn, "AYNA1 DENY", close: true); return }
        asking = true
        defer { asking = false }
        send(conn, "AYNA1 WAIT")

        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        let shortID = String(id.suffix(6)).uppercased()
        switch (known, authenticated) {
        case (.some(let k), true):
            alert.messageText = "“\(k.name)” yeni bir adresten bağlanıyor"
            alert.informativeText = "Güvenilen bu bilgisayar artık \(ip) adresinde (önceden \(k.ip))."
        case (.some(let k), false):
            alert.alertStyle = .critical
            alert.messageText = "“\(name)” kimliğini kanıtlayamadı"
            alert.informativeText = """
                Bu cihaz kendini güvendiğiniz “\(k.name)” gibi gösteriyor ama onun anahtarına sahip değil. \
                Kendi bilgisayarınızın Ayna Merkezi'ni yeniden kurmadıysanız reddedin.
                """
        default:
            alert.messageText = "“\(name)” bu Mac'e bağlanmak istiyor"
            alert.informativeText = """
                İzin verirseniz bu bilgisayar güvenilenler listesine eklenir ve Mac'in ekranını görüp \
                kontrol edebilir. Bağlantı için yine RustDesk parolası gerekir.
                """
        }
        alert.informativeText += "\n\nAdres: \(ip)   Cihaz: …\(shortID)"
        // Varsayılan (Enter) düğme "Reddet": o an yazı yazan kullanıcı yanlışlıkla izin vermesin.
        alert.addButton(withTitle: "Reddet")
        alert.addButton(withTitle: "İzin ver")
        // Bağlantı soru açılmadan koptuysa sorulmaz.
        guard case .ready = conn.state else { conn.cancel(); return }
        recentPrompts.append(Date())
        let allowed = alert.runModal() == .alertSecondButtonReturn
        // Yanıt geç verildiyse Windows çoktan vazgeçmiştir: hiçbir şey kaydedilmez.
        guard case .ready = conn.state else { conn.cancel(); return }
        guard allowed else {
            deniedUntil[ip] = Date().addingTimeInterval(600)
            send(conn, "AYNA1 DENY", close: true)
            return
        }
        let reuse = authenticated ? known?.token : nil
        let token = reuse ?? randomHex(32)
        let previous = Store.trusted
        var list = previous.filter { $0.id != id }
        list.append(TrustedDevice(id: id, name: name, ip: ip, token: token))
        if Store.whitelist(list) != Store.whitelist(previous) {
            let (ok, message) = RustDesk.setWhitelist(list, previous: previous)
            guard ok else {
                show(error: "Güvenilenler listesi RustDesk'e yazılamadı.", detail: message)
                send(conn, "AYNA1 DENY", close: true)
                return
            }
        }
        Store.trusted = list
        approve(token: token, issue: reuse == nil)
    }

    // MARK: Menü

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        guard RustDesk.installed else {
            menu.addItem(item("RustDesk kurulu değil", nil))
            menu.addItem(item("RustDesk'i indir…", #selector(downloadRustDesk)))
            menu.addItem(.separator())
            menu.addItem(item("Çıkış", #selector(quit)))
            return
        }
        if let code {
            let codeItem = item("Kod: \(formatted(code))", #selector(copyCode))
            codeItem.toolTip = "Kopyalamak için tıklayın"
            menu.addItem(codeItem)
        } else {
            menu.addItem(item("Kod okunuyor…", nil))
        }
        let accepting = item("Bağlantılara izin ver", #selector(toggleAccepting))
        accepting.state = Store.accepting ? .on : .off
        menu.addItem(accepting)
        menu.addItem(.separator())

        let trustedItem = item("Güvenilen bilgisayarlar", nil)
        let sub = NSMenu()
        let list = Store.trusted
        if list.isEmpty {
            sub.addItem(item("Henüz yok", nil))
        }
        for device in list {
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
        menu.addItem(item("Bağlantı parolasını değiştir…", #selector(changePassword)))
        menu.addItem(item("RustDesk'i Ayna için ayarla…", #selector(setupFromMenu)))
        menu.addItem(item("Ekran Kaydı ve Erişilebilirlik izinleri…", #selector(openPrivacy)))
        menu.addItem(.separator())
        let login = item("Oturum açınca başlat", #selector(toggleLogin))
        login.state = SMAppService.mainApp.status == .enabled ? .on : .off
        menu.addItem(login)
        menu.addItem(item("Çıkış", #selector(quit)))
    }

    private func item(_ title: String, _ action: Selector?) -> NSMenuItem {
        let i = NSMenuItem(title: title, action: action, keyEquivalent: "")
        i.target = self
        i.isEnabled = action != nil
        return i
    }

    @objc private func copyCode() {
        guard let code = plainCode else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(code, forType: .string)
    }

    @objc private func toggleAccepting() {
        Store.accepting.toggle()
    }

    /// Önce RustDesk'ten kaldırılır (doğrulanarak), sonra listeden: yazım başarısızsa adres RustDesk'te
    /// izinli kalırken listede görünmez olmasın.
    @objc private func removeTrusted(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        let previous = Store.trusted
        let list = previous.filter { $0.id != id }
        let (ok, message) = RustDesk.setWhitelist(list, previous: previous)
        guard ok else { show(error: "RustDesk ayarı değiştirilemedi; bilgisayar hâlâ güvenilir.", detail: message); return }
        Store.trusted = list
    }

    @objc private func setupFromMenu() { setup() }

    /// İlk kurulum: parola + RustDesk'i doğrudan IP, kalıcı parola ve beyaz listeyle çalıştırma.
    private func setup() {
        NSApp.activate(ignoringOtherApps: true)
        guard let password = askPassword(
            title: "Ayna için RustDesk ayarı",
            text: """
                Windows'tan bu Mac'e bağlanırken sorulacak parolayı belirleyin (en az \(minPasswordLength) karakter).
                RustDesk yalnız güvendiğiniz bilgisayarların yerel ağ adreslerinden ve bu parolayla \
                bağlantı kabul edecek biçimde ayarlanacak. Ardından macOS yönetici parolanızı soracak.
                """
        ) else { return }
        let (ok, message) = RustDesk.configure(whitelist: Store.trusted, password: password)
        guard ok else {
            show(error: "RustDesk ayarlanamadı.", detail: message)
            return
        }
        Store.configured = true
        let done = NSAlert()
        done.messageText = "Hazır"
        done.informativeText = """
            Windows'ta Ayna Merkezi'ne bu Mac'in kodunu yazın: \(code.map(formatted) ?? "menü çubuğundaki kod").
            RustDesk'in ekranı paylaşabilmesi için Sistem Ayarları > Gizlilik ve Güvenlik'te \
            Ekran Kaydı ve Erişilebilirlik izinlerinin RustDesk için açık olması gerekir.
            """
        done.runModal()
    }

    @objc private func changePassword() {
        NSApp.activate(ignoringOtherApps: true)
        guard let password = askPassword(
            title: "Bağlantı parolası",
            text: "Yeni parolayı yazın (en az \(minPasswordLength) karakter)."
        ) else { return }
        let (ok, message) = RustDesk.runAsAdmin([[rustDeskBinary, "--password", password]])
        if !ok { show(error: "Parola değiştirilemedi.", detail: message) }
    }

    private func askPassword(title: String, text: String) -> String? {
        while true {
            let alert = NSAlert()
            alert.messageText = title
            alert.informativeText = text
            let stack = NSStackView(frame: NSRect(x: 0, y: 0, width: 260, height: 56))
            stack.orientation = .vertical
            stack.spacing = 8
            let first = NSSecureTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
            first.placeholderString = "Parola"
            let second = NSSecureTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
            second.placeholderString = "Parola (tekrar)"
            stack.addArrangedSubview(first)
            stack.addArrangedSubview(second)
            alert.accessoryView = stack
            alert.addButton(withTitle: "Tamam")
            alert.addButton(withTitle: "Vazgeç")
            alert.window.initialFirstResponder = first
            guard alert.runModal() == .alertFirstButtonReturn else { return nil }
            let p = first.stringValue
            if p.count < minPasswordLength {
                show(error: "Parola en az \(minPasswordLength) karakter olmalı.", detail: "")
            } else if p != second.stringValue {
                show(error: "Parolalar aynı değil.", detail: "")
            } else if p.contains(where: { $0.isNewline }) {
                show(error: "Parola satır sonu içeremez.", detail: "")
            } else {
                return p
            }
        }
    }

    @objc private func openPrivacy() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") {
            NSWorkspace.shared.open(url)
        }
    }

    @objc private func downloadRustDesk() {
        if let url = URL(string: "https://github.com/rustdesk/rustdesk/releases/latest") {
            NSWorkspace.shared.open(url)
        }
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
        NSApp.terminate(nil)
    }

    private func show(error: String, detail: String) {
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = error
        alert.informativeText = detail
        alert.runModal()
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.accessory)
app.run()
