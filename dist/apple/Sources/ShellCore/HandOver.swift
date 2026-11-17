import Foundation
import Security

/// What the share sheet hands to the app: words, a link, or both.
public struct SharedItem: Codable, Equatable, Sendable {
    /// The most the shell puts into a message at once, in characters. The page takes far
    /// more in one insert; a message does not.
    public static let limit = 100_000

    public var text: String?
    public var link: String?
    public var at: Date

    public init(text: String? = nil, link: String? = nil, at: Date = Date()) {
        self.text = text.flatMap { $0.isEmpty ? nil : $0 }
        self.link = link.flatMap { $0.isEmpty ? nil : $0 }
        self.at = at
    }

    /// What goes into the message field: the words, then the link on a line of its own
    /// unless the words already hold it. `nil` when there is nothing, or too much.
    public var words: String? {
        let parts = [text, link.flatMap { link in text?.contains(link) == true ? nil : link }]
        let words = parts.compactMap(\.self).joined(separator: "\n")
        return words.isEmpty || words.count > SharedItem.limit ? nil : words
    }
}

/// Where the share extension leaves an item and the app picks it up. They are two
/// programs, so it is a place both are entitled to and nothing else is.
public protocol HandOver: Sendable {
    func leave(_ item: SharedItem) throws
    /// Every item left, oldest first. Each is handed over once.
    func take() throws -> [SharedItem]
}

public enum HandOverError: Error, Equatable {
    /// The build holds no place the app and its extension share.
    case noSharedPlace
    case keychain(OSStatus)
}

/// A folder in the container the app and its extension share (an app group), one file an item.
public struct FolderHandOver: HandOver {
    public let folder: URL

    public init(folder: URL) {
        self.folder = folder
    }

    /// The hand-over in app group `group`, or `nil` where this build is not entitled to it.
    public init?(group: String) {
        guard let container = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group)
        else { return nil }
        folder = container.appendingPathComponent("Shared", isDirectory: true)
    }

    public func leave(_ item: SharedItem) throws {
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        // Named so that a listing in name order is the order they were left in.
        let stamp = String(format: "%017.0f", item.at.timeIntervalSince1970 * 1000)
        let file = folder.appendingPathComponent("\(stamp)-\(UUID().uuidString).json")
        try JSONEncoder().encode(item).write(to: file, options: .atomic)
    }

    public func take() throws -> [SharedItem] {
        let manager = FileManager.default
        guard manager.fileExists(atPath: folder.path) else { return [] }
        let files = try manager.contentsOfDirectory(at: folder, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" }
            .sorted { $0.lastPathComponent < $1.lastPathComponent }
        return files.compactMap { file in
            defer { try? manager.removeItem(at: file) }
            // A file that is no item (half written by a build that crashed) is dropped.
            return (try? Data(contentsOf: file)).flatMap { try? JSONDecoder().decode(SharedItem.self, from: $0) }
        }
    }
}

/// Items kept in a keychain group the app and its extension share. A development profile
/// that covers every app of a team grants such a group and no app group, so a trial build
/// on a phone hands over this way.
public struct KeychainHandOver: HandOver {
    public let group: String
    /// The name the items are kept under: the place's own.
    public let service: String

    public init(group: String, service: String) {
        self.group = group
        self.service = service
    }

    private var all: [CFString: Any] {
        [kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccessGroup: group,
         kSecUseDataProtectionKeychain: true]
    }

    public func leave(_ item: SharedItem) throws {
        var entry = all
        entry[kSecAttrAccount] = UUID().uuidString
        entry[kSecAttrAccessible] = kSecAttrAccessibleAfterFirstUnlock
        entry[kSecValueData] = try JSONEncoder().encode(item)
        let status = SecItemAdd(entry as CFDictionary, nil)
        guard status == errSecSuccess else { throw HandOverError.keychain(status) }
    }

    public func take() throws -> [SharedItem] {
        var query = all
        query[kSecMatchLimit] = kSecMatchLimitAll
        query[kSecReturnData] = true
        query[kSecReturnAttributes] = true
        var found: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &found)
        if status == errSecItemNotFound { return [] }
        guard status == errSecSuccess else { throw HandOverError.keychain(status) }
        let items = (found as? [[CFString: Any]] ?? [])
            .compactMap { $0[kSecValueData] as? Data }
            .compactMap { try? JSONDecoder().decode(SharedItem.self, from: $0) }
            .sorted { $0.at < $1.at }
        let removed = SecItemDelete(all as CFDictionary)
        guard removed == errSecSuccess || removed == errSecItemNotFound else { throw HandOverError.keychain(removed) }
        return items
    }
}

/// The one place an app shares with its own extension, and with no other app. The build
/// writes its two names into the `Info.plist` of both: the place's own, which is the app's
/// identifier with `.shared` (`TiloShared`), and the team that signed the build (`TiloTeam`).
/// An unsigned build has no team, and so no place.
public enum SharedPlace {
    public static let teamKey = "TiloTeam"
    public static let nameKey = "TiloShared"

    /// The app group (Mac) and keychain group (iPhone) of this build: the place's name under
    /// the team that signed it.
    public static func group(info: [String: Any]? = Bundle.main.infoDictionary) -> String? {
        guard let team = info?[teamKey] as? String, !team.isEmpty,
              let name = info?[nameKey] as? String, !name.isEmpty
        else { return nil }
        return "\(team).\(name)"
    }

    /// The hand-over this build can use, or `nil` when it is entitled to none.
    public static func handOver(info: [String: Any]? = Bundle.main.infoDictionary) -> (any HandOver)? {
        guard let group = group(info: info), let name = info?[nameKey] as? String else { return nil }
        #if os(iOS)
        return KeychainHandOver(group: group, service: name)
        #else
        return FolderHandOver(group: group)
        #endif
    }
}
