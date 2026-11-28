import Foundation
import Security

/// Where the device's key for being told while the app is closed is kept. The app is handed
/// one and never names the keychain itself, so a test hands it one that keeps the key in
/// memory and touches nothing of the person's.
public protocol ClosedKeyStore: Sendable {
    /// The key kept, if one is.
    func read() -> ClosedKey?
    /// Keep this key in the place of any other.
    func keep(_ key: ClosedKey) throws
    /// Keep none.
    func forget()
}

/// A store that keeps the key while the program lives: for tests, and for a check.
public final class MemoryClosedKeys: ClosedKeyStore, @unchecked Sendable {
    private let lock = NSLock()
    private var key: ClosedKey?
    /// How often a key was put in: a test counts the keys that were made.
    public private(set) var kept = 0

    public init() {}

    public func read() -> ClosedKey? {
        lock.withLock { key }
    }

    public func keep(_ key: ClosedKey) throws {
        lock.withLock {
            self.key = key
            kept += 1
        }
    }

    public func forget() {
        lock.withLock { key = nil }
    }
}

/// The key in the one keychain group the app shares with its notice extension. It can be read
/// once the device was unlocked after its start, so a push that comes to a locked phone is
/// opened too; it stays on this device and is in no backup.
public struct KeychainClosedKeys: ClosedKeyStore {
    public let group: String

    public init(group: String) {
        self.group = group
    }

    /// The store a bundle names in its `Info.plist`: under the team that signed it (`TiloTeam`),
    /// the one place the app shares with its notice extension (`TiloTold`). Nil for a bundle
    /// that names none, which is every bundle of a build without the push right.
    public init?(info: [String: Any]?) {
        guard let team = info?["TiloTeam"] as? String, !team.isEmpty, let place = info?["TiloTold"] as? String, !place.isEmpty
        else { return nil }
        self.init(group: "\(team).\(place)")
    }

    private var entry: [CFString: Any] {
        [kSecClass: kSecClassGenericPassword, kSecAttrService: "tilo-told", kSecAttrAccount: "device",
         kSecAttrAccessGroup: group, kSecUseDataProtectionKeychain: true]
    }

    public func read() -> ClosedKey? {
        var query = entry
        query[kSecReturnData] = true
        query[kSecMatchLimit] = kSecMatchLimitOne
        var found: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &found) == errSecSuccess, let data = found as? Data else { return nil }
        return ClosedKey(kept: data)
    }

    public func keep(_ key: ClosedKey) throws {
        SecItemDelete(entry as CFDictionary)
        var made = entry
        made[kSecAttrAccessible] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        made[kSecValueData] = key.kept
        let status = SecItemAdd(made as CFDictionary, nil)
        guard status == errSecSuccess else { throw ClosedProblem.notKept(status) }
    }

    public func forget() {
        SecItemDelete(entry as CFDictionary)
    }
}

public enum ClosedProblem: Error, Equatable {
    /// The keychain did not keep the key; the number is the system's.
    case notKept(OSStatus)
}

/// What a build says of being told while the app is closed. `build.sh` writes it into a
/// bundle only when the build was told to include push and its profile grants the right, so
/// a build without it asks Apple's service for nothing and keeps no key.
public struct ClosedBuild: Equatable, Sendable {
    /// Which of Apple's two services reaches the build.
    public enum Service: String, Equatable, Sendable { case development, production }

    /// Which of Apple's two services reaches the build.
    public let service: Service
    /// The keychain group the app shares with its notice extension.
    public let group: String

    /// From a bundle's `Info.plist`: `TiloPush`, and the group from `TiloTeam` and `TiloTold`.
    public init?(info: [String: Any]?) {
        guard let service = (info?["TiloPush"] as? String).flatMap(Service.init(rawValue:)),
              let kept = KeychainClosedKeys(info: info)
        else { return nil }
        self.service = service
        group = kept.group
    }

    /// The store of the device's key in this build.
    public var keys: any ClosedKeyStore { KeychainClosedKeys(group: group) }
}
