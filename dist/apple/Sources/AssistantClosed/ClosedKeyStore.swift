import AssistantCore
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
    public let service: PushAddress.Service
    /// The keychain group the app shares with its notice extension.
    public let group: String
    /// The bundle identifier named in `Info.plist`, when present.
    public let app: String

    /// From a bundle's `Info.plist`: `TiloPush`, and the group from `TiloTeam` and `TiloTold`.
    public init?(info: [String: Any]?) {
        guard let service = (info?["TiloPush"] as? String).flatMap(PushAddress.Service.init(rawValue:)),
              let kept = KeychainClosedKeys(info: info)
        else { return nil }
        self.service = service
        group = kept.group
        app = (info?["CFBundleIdentifier"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    }

    /// The store of the device's key in this build.
    public var keys: any ClosedKeyStore { KeychainClosedKeys(group: group) }

    /// The APNs topic for this app's live activity updates (`<bundle>.push-type.liveactivity`).
    public func activityTopic(bundleID: String = "") -> String {
        let base = bundleID.isEmpty ? app : bundleID
        return base.isEmpty ? "" : "\(base).push-type.liveactivity"
    }

    /// Build a `PushAddress` for this build's APNs service and the appropriate topic for `use`.
    public func address(for use: PushAddress.Use, token: Data, bundleID: String = "") -> PushAddress {
        let base = bundleID.isEmpty ? app : bundleID
        let topic: String = switch use {
        case .notices: base
        case .activityStart, .activity: activityTopic(bundleID: base)
        }
        return PushAddress(use: use, token: token, service: service, app: topic)
    }
}

/// Where a notification — local or remote APNs — routes when the person taps it.
///
/// Only non-sensitive routing identifiers are read from `userInfo`: sealed payloads (`enc`),
/// alert text (`aps`), and credentials are filtered out. A lock-screen privacy-filtered push
/// carries only an opaque device-scoped `told` tag (`ClosedKey.name(of:)`) and its `kind`,
/// which the unlocked app resolves locally against `Item.toldAs`.
public struct ClosedRoute: Equatable, Sendable {
    public enum Target: Equatable, Sendable {
        /// A specific conversation piece (`item` / `message`).
        case item(Item.ID, chat: Chat)
        /// A specific waiting decision (`decision` / `question`).
        case decision(Question.ID, chat: Chat)
        /// A specific task (`task`).
        case task(AssistantTask.ID)
        /// A privacy-filtered remote push identified only by its opaque `told` tag.
        case told(String, kind: String)
    }

    public enum Resolved: Equatable, Sendable {
        /// Matched conversation piece and its chat.
        case item(Item.ID, chat: Chat)
        /// Matched task with its optional conversation piece and current state.
        case task(AssistantTask.ID, item: Item.ID?, state: AssistantTask.State?)
    }

    public let target: Target
    /// The opaque device-scoped push tag (`told`), when present.
    public let told: String?
    /// The kind reported by the push (`question`, `decision`, `reminder`, `brief`, `task`, `news`).
    public let kind: String?
    /// Explicit question and choice identifiers for inline notification answer actions.
    public let question: Question.ID?
    public let choice: Choice.ID?

    /// True when the notification payload carried no cleartext conversation or task identifier
    /// and routes solely through the opaque privacy-preserving `told` tag.
    public var isPrivacyFiltered: Bool {
        if case .told = target { return true }
        return false
    }

    /// Keys that must never be inspected as routing identifiers.
    private static let privateKeys: Set<String> = [
        "aps", "enc", "secret", "password", "token", "auth", "p256dh", "key", "body", "words", "text",
    ]
    /// Nested dictionary containers that may carry non-sensitive routing identifiers.
    private static let routeContainers = ["tilo", "route", "target", "data"]
    private static let maxIdentifierBytes = 256

    /// Parse a route from a notification's `userInfo` (local or remote APNs).
    /// Returns `nil` for withdrawn pushes (`gone: true`) or payloads carrying no Tilo route.
    public init?(userInfo: [AnyHashable: Any], requestIdentifier: String? = nil) {
        if Self.flag("gone", in: userInfo) { return nil }

        let kind = Self.identifier(["kind", "notice_kind", "noticeKind"], in: userInfo)
        let told = Self.identifier(["told", "told_as", "toldAs"], in: userInfo)
            ?? Self.toldFromRequest(requestIdentifier, kind: kind)
        let chat = Self.chat(in: userInfo)
        let questionID = Self.identifier(["question", "question_id", "questionId"], in: userInfo)
        let choiceID = Self.identifier(["choice", "choice_id", "choiceId"], in: userInfo)
        let itemID = Self.identifier(["item", "item_id", "itemId", "message", "message_id", "messageId"], in: userInfo)
        let decisionID = Self.identifier(["decision", "decision_id", "decisionId"], in: userInfo) ?? questionID
        let taskID = Self.identifier(["task", "task_id", "taskId"], in: userInfo)

        self.told = told
        self.kind = kind
        self.question = questionID.map { Question.ID($0) }
        self.choice = choiceID.map { Choice.ID($0) }

        if let itemID {
            self.target = .item(Item.ID(itemID), chat: chat ?? .main)
        } else if let decisionID {
            self.target = .decision(Question.ID(decisionID), chat: chat ?? .main)
        } else if let taskID {
            self.target = .task(AssistantTask.ID(taskID))
        } else if let told {
            self.target = .told(told, kind: kind ?? "")
        } else {
            return nil
        }
    }

    /// Resolve this route against the conversations and tasks currently held by the app.
    /// Returns `nil` when the target has not synced into local state yet (for example on a cold launch).
    public func resolve(conversations: [Chat: [Item]], tasks: [AssistantTask]) -> Resolved? {
        switch target {
        case .item(let id, let preferredChat):
            if let found = Self.findItem(id, in: conversations, preferring: preferredChat) {
                return found
            }
            if let told { return Self.findTold(told, in: conversations) }
            return nil

        case .decision(let id, let preferredChat):
            if let found = Self.findDecision(id, in: conversations, preferring: preferredChat) {
                return found
            }
            if let told { return Self.findTold(told, in: conversations) }
            return nil

        case .task(let id):
            if let task = tasks.first(where: { $0.id == id }) {
                return .task(task.id, item: task.item, state: task.state)
            }
            for chat in Self.orderedChats(of: conversations, preferring: .main) {
                if let item = conversations[chat]?.first(where: { $0.task?.task == id }) {
                    return .item(item.id, chat: chat)
                }
            }
            return nil

        case .told(let name, _):
            return Self.findTold(name, in: conversations)
        }
    }

    /// Parse a chat name (`main` or `sub-agent:<id>`).
    public static func parseChat(_ name: String?) -> Chat? {
        guard let name = cleanIdentifier(name) else { return nil }
        if name == "main" { return .main }
        guard name.hasPrefix("sub-agent:") else { return nil }
        let agent = String(name.dropFirst("sub-agent:".count))
        guard !agent.isEmpty else { return nil }
        return .subAgent(SubAgent.ID(agent))
    }

    private static func orderedChats(of conversations: [Chat: [Item]], preferring preferred: Chat) -> [Chat] {
        var chats: [Chat] = []
        if conversations[preferred] != nil { chats.append(preferred) }
        if preferred != .main, conversations[.main] != nil { chats.append(.main) }
        let others = conversations.keys
            .filter { $0 != preferred && $0 != .main }
            .sorted { chatKey($0) < chatKey($1) }
        chats.append(contentsOf: others)
        return chats
    }

    private static func chatKey(_ chat: Chat) -> String {
        switch chat {
        case .main: "main"
        case .subAgent(let id): "sub-agent:\(id.value)"
        }
    }

    private static func shortName(of id: String) -> String {
        id.split(separator: "/").last.map(String.init) ?? id
    }

    private static func findItem(_ id: Item.ID, in conversations: [Chat: [Item]], preferring preferred: Chat) -> Resolved? {
        let chats = orderedChats(of: conversations, preferring: preferred)
        for chat in chats {
            if let matched = conversations[chat]?.first(where: { $0.id == id }) {
                return .item(matched.id, chat: chat)
            }
        }
        let wanted = shortName(of: id.value)
        for chat in chats {
            if let matched = conversations[chat]?.first(where: { shortName(of: $0.id.value) == wanted }) {
                return .item(matched.id, chat: chat)
            }
        }
        return nil
    }

    private static func findDecision(_ id: Question.ID, in conversations: [Chat: [Item]], preferring preferred: Chat) -> Resolved? {
        let chats = orderedChats(of: conversations, preferring: preferred)
        let wanted = shortName(of: id.value)
        for chat in chats {
            if let matched = conversations[chat]?.first(where: { matchesDecision($0, exact: id) }) {
                return .item(matched.id, chat: chat)
            }
        }
        for chat in chats {
            if let matched = conversations[chat]?.first(where: { matchesDecisionShortName($0, short: wanted) }) {
                return .item(matched.id, chat: chat)
            }
        }
        return nil
    }

    private static func matchesDecision(_ item: Item, exact id: Question.ID) -> Bool {
        if item.id.value == id.value { return true }
        switch item.content {
        case .question(let asked): return asked.id == id
        case .secret(let asked): return asked.id.value == id.value
        case .connection(let offered): return offered.id.value == id.value
        default: return false
        }
    }

    private static func matchesDecisionShortName(_ item: Item, short: String) -> Bool {
        if shortName(of: item.id.value) == short { return true }
        switch item.content {
        case .question(let asked): return shortName(of: asked.id.value) == short
        case .secret(let asked): return shortName(of: asked.id.value) == short
        case .connection(let offered): return shortName(of: offered.id.value) == short
        default: return false
        }
    }

    private static func findTold(_ name: String, in conversations: [Chat: [Item]]) -> Resolved? {
        for chat in orderedChats(of: conversations, preferring: .main) {
            if let matched = conversations[chat]?.first(where: { $0.toldAs == name }) {
                return .item(matched.id, chat: chat)
            }
        }
        return nil
    }

    private static func chat(in userInfo: [AnyHashable: Any]) -> Chat? {
        if let raw = identifier(["chat"], in: userInfo), let parsed = parseChat(raw) {
            return parsed
        }
        if let agent = identifier(["sub_agent", "subAgent", "sub_agent_id", "subAgentId"], in: userInfo) {
            return .subAgent(SubAgent.ID(agent))
        }
        return nil
    }

    private static func toldFromRequest(_ requestIdentifier: String?, kind: String?) -> String? {
        guard kind != nil, let request = cleanIdentifier(requestIdentifier), !request.hasPrefix("assistant-") else {
            return nil
        }
        return request
    }

    private static func flag(_ key: String, in userInfo: [AnyHashable: Any]) -> Bool {
        for map in candidateMaps(in: userInfo) {
            if let bool = map[key] as? Bool { return bool }
            if let number = map[key] as? NSNumber { return number.boolValue }
        }
        return false
    }

    private static func identifier(_ keys: [String], in userInfo: [AnyHashable: Any]) -> String? {
        for map in candidateMaps(in: userInfo) {
            for key in keys where !privateKeys.contains(key.lowercased()) {
                if let value = cleanIdentifier(map[key]) { return value }
            }
        }
        return nil
    }

    private static func candidateMaps(in userInfo: [AnyHashable: Any]) -> [[AnyHashable: Any]] {
        var maps: [[AnyHashable: Any]] = [userInfo]
        for container in routeContainers {
            if let nested = userInfo[container] as? [AnyHashable: Any] {
                maps.append(nested)
            }
        }
        return maps
    }

    private static func cleanIdentifier(_ raw: Any?) -> String? {
        guard let text = raw as? String else { return nil }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, trimmed.utf8.count <= maxIdentifierBytes else { return nil }
        guard !trimmed.unicodeScalars.contains(where: { $0.value < 0x20 || $0.value == 0x7F }) else { return nil }
        return trimmed
    }
}
