import Foundation
import Security

/// The relay's private key and the paired computers' public keys, in the Keychain. Items can be
/// read only while the phone is unlocked and never leave it: not in backups, not to another device.
enum Keychain {
    struct Failure: Error, CustomStringConvertible {
        let status: OSStatus
        var description: String { "Keychain error \(status)" }
    }

    private static let service = "io.github.heidaridev.kite"
    private static let keyLength = 32

    /// The relay's key, made and stored on first use.
    static func privateKey() throws -> Data {
        if let key = try read("relay-key") { return key }
        let key = try generateKey()
        try write("relay-key", key)
        return key
    }

    static func peers() throws -> [Data] {
        // A blob of the wrong size is not ours to guess at; start without paired computers.
        guard let data = try read("peers"), data.count % keyLength == 0 else { return [] }
        return stride(from: 0, to: data.count, by: keyLength).map { data.subdata(in: $0..<$0 + keyLength) }
    }

    static func savePeers(_ peers: [Data]) throws {
        try write("peers", peers.reduce(Data(), +))
    }

    private static func query(_ account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    private static func read(_ account: String) throws -> Data? {
        var query = query(account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = item as? Data else { throw Failure(status: status) }
        return data
    }

    private static func write(_ account: String, _ data: Data) throws {
        let status = SecItemUpdate(query(account) as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecItemNotFound {
            var item = query(account)
            item[kSecValueData as String] = data
            item[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
            let added = SecItemAdd(item as CFDictionary, nil)
            guard added == errSecSuccess else { throw Failure(status: added) }
        } else if status != errSecSuccess {
            throw Failure(status: status)
        }
    }
}
