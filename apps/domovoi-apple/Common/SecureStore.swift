import Foundation
import Security
import Shared

/// The core's `Store` effect plus the device's iroh key.
///
/// iOS and macOS release builds keep everything in the Keychain. macOS debug
/// builds are signed ad hoc, so their signature changes with every build and
/// Keychain items would ask for permission each time; there the values live
/// in a file only the user can read
/// (`~/Library/Application Support/Domovoi/store.json`, mode 0600). A release
/// build moves an existing file into the Keychain once.
/// `DOMOVOI_STORE=memory` keeps nothing (smoke runs and snapshots).
final class SecureStore: @unchecked Sendable {
    private let memoryOnly = ProcessInfo.processInfo.environment["DOMOVOI_STORE"] == "memory"
    private var memory: [String: String] = [:]
    private let service = "com.crayen.domovoi.client"
    private let lock = NSLock()

    #if os(macOS) && DEBUG
    private let useFile = true
    #else
    private let useFile = false
    #endif

    /// What a Keychain read found.
    enum Lookup {
        case found(String)
        case missing
        /// The Keychain could not be read (locked before the first unlock,
        /// or another error): not the same as "nothing saved".
        case unavailable(OSStatus)
    }

    init() {
        #if os(macOS) && !DEBUG
        if !memoryOnly { migrateFileToKeychain() }
        #endif
    }

    func load(_ key: String) -> String? {
        if case .found(let value) = lookup(key) { return value }
        return nil
    }

    func lookup(_ key: String) -> Lookup {
        lock.lock()
        defer { lock.unlock() }
        if memoryOnly { return memory[key].map(Lookup.found) ?? .missing }
        if let file = Self.cacheFile(key) {
            return (try? String(contentsOf: file, encoding: .utf8)).map(Lookup.found) ?? .missing
        }
        if useFile {
            switch readFile() {
            case .success(let all): return all[key].map(Lookup.found) ?? .missing
            case .failure: return .unavailable(errSecDecode)
            }
        }
        return keychainRead(key)
    }

    @discardableResult
    func save(_ key: String, _ value: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if memoryOnly {
            memory[key] = value
            return true
        }
        if let file = Self.cacheFile(key) {
            try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            return (try? value.write(to: file, atomically: true, encoding: .utf8)) != nil
        }
        if useFile { return updateFile { $0[key] = value } }
        return keychainWrite(key, value)
    }

    @discardableResult
    func delete(_ key: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if memoryOnly {
            memory[key] = nil
            return true
        }
        if let file = Self.cacheFile(key) {
            try? FileManager.default.removeItem(at: file)
            return true
        }
        if useFile { return updateFile { $0[key] = nil } }
        let status = SecItemDelete(query(key) as CFDictionary)
        return status == errSecSuccess || status == errSecItemNotFound
    }

    /// Where a `cache:` value lives: a file in the caches folder, not the
    /// Keychain (an offline copy of a thread list is large and not secret
    /// enough to need it; the system may clear it).
    private static func cacheFile(_ key: String) -> URL? {
        guard key.hasPrefix("cache:"),
              let dir = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first else { return nil }
        let name = String(key.unicodeScalars.map { CharacterSet.alphanumerics.contains($0) || $0 == "-" ? Character($0) : "_" })
        return dir.appendingPathComponent("Domovoi", isDirectory: true).appendingPathComponent(name + ".json")
    }

    /// This device's iroh secret key (its identity to remote servers),
    /// created on first use. `nil` when the Keychain cannot be read or a new
    /// key cannot be kept: a key that is not kept would make a new identity
    /// on every start, and every server would ask to pair again.
    func deviceKey() -> String? {
        switch lookup("deviceKey") {
        case .found(let key) where key.count == 64:
            return key
        case .unavailable(let status):
            NSLog("Domovoi: the device key could not be read (OSStatus %d)", status)
            return nil
        default:
            let key = tunnelGenerateKey()
            guard save("deviceKey", key), load("deviceKey") == key else {
                NSLog("Domovoi: a new device key could not be saved")
                return nil
            }
            return key
        }
    }

    // MARK: macOS file (debug builds)

    private var fileURL: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("Domovoi", isDirectory: true).appendingPathComponent("store.json")
    }

    private struct Unreadable: Error {}

    /// The saved values; empty when there is no file, a failure when there is
    /// one that cannot be parsed (which is then never overwritten).
    private func readFile() -> Result<[String: String], Error> {
        guard let data = try? Data(contentsOf: fileURL) else { return .success([:]) }
        guard let values = try? JSONDecoder().decode([String: String].self, from: data) else { return .failure(Unreadable()) }
        return .success(values)
    }

    private func updateFile(_ change: (inout [String: String]) -> Void) -> Bool {
        guard case .success(var values) = readFile() else {
            NSLog("Domovoi: %@ cannot be read; not overwriting it", fileURL.path)
            return false
        }
        change(&values)
        return writeFile(values)
    }

    /// Writes a new file readable only by the user, then moves it into place.
    private func writeFile(_ values: [String: String]) -> Bool {
        let url = fileURL
        let dir = url.deletingLastPathComponent()
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
            let data = try JSONEncoder().encode(values)
            let temp = dir.appendingPathComponent(".store-\(UUID().uuidString).json")
            guard FileManager.default.createFile(atPath: temp.path, contents: data, attributes: [.posixPermissions: 0o600]) else { return false }
            if rename(temp.path, url.path) != 0 {
                try? FileManager.default.removeItem(at: temp)
                return false
            }
            return true
        } catch {
            NSLog("Domovoi: saving %@ failed: %@", url.path, "\(error)")
            return false
        }
    }

    #if os(macOS) && !DEBUG
    /// A release build picks up what a debug build saved, once.
    private func migrateFileToKeychain() {
        guard case .success(let values) = readFile(), !values.isEmpty else { return }
        for (key, value) in values where !keychainWrite(key, value) { return }
        try? FileManager.default.removeItem(at: fileURL)
    }
    #endif

    // MARK: Keychain

    private func query(_ key: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: key]
    }

    private func keychainRead(_ key: String) -> Lookup {
        var q = query(key)
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var out: AnyObject?
        let status = SecItemCopyMatching(q as CFDictionary, &out)
        switch status {
        case errSecSuccess:
            guard let data = out as? Data, let value = String(data: data, encoding: .utf8) else { return .unavailable(errSecDecode) }
            return .found(value)
        case errSecItemNotFound:
            return .missing
        default:
            return .unavailable(status)
        }
    }

    /// Readable after the first unlock (a reconnect in the background works),
    /// and carried over in an encrypted backup to a new phone.
    private func keychainWrite(_ key: String, _ value: String) -> Bool {
        let data = Data(value.utf8)
        let attributes: [String: Any] = [kSecValueData as String: data, kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlock]
        var status = SecItemUpdate(query(key) as CFDictionary, attributes as CFDictionary)
        if status == errSecItemNotFound {
            var q = query(key)
            q.merge(attributes) { _, new in new }
            status = SecItemAdd(q as CFDictionary, nil)
        }
        if status != errSecSuccess { NSLog("Domovoi: saving %@ in the Keychain failed (OSStatus %d)", key, status) }
        return status == errSecSuccess
    }
}
