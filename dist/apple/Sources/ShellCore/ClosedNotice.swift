import Foundation
import UserNotifications

/// What a push from the person's host carries in the open: the name the host gave the thing
/// (a name only this device can tie to it), and the part sealed for this device alone.
public struct ClosedPush: Equatable, Sendable {
    /// What a button of a push is kept under in the system's answer: its id follows.
    public static let actionPrefix = "tilo.told."

    public let name: String
    /// The thing waits no more: its notice is to be taken away, and nothing is shown.
    public let isGone: Bool
    /// The part sealed for this device, where the push carries one.
    public let sealed: Data?

    /// Nil for a notification that is no push of Tilo's.
    public init?(_ fields: [AnyHashable: Any]) {
        guard let name = fields["told"] as? String, !name.isEmpty else { return nil }
        self.name = name
        isGone = (fields["gone"] as? NSNumber)?.boolValue ?? false
        sealed = (fields["enc"] as? String).flatMap { Data(base64Encoded: $0) }
    }

    /// What the notice says once this device opened the sealed part: who told it, the Important
    /// mark, the host's words and buttons. Nil where it carries no sealed part, does not open
    /// with this key, or says nothing: the line the push carries in the open then stands.
    public func opened(with key: ClosedKey) -> Opened? {
        guard !isGone, let sealed, let plain = key.open(sealed),
              let said = try? JSONDecoder().decode(Said.self, from: plain)
        else { return nil }
        let words = said.words.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !words.isEmpty else { return nil }
        let from = (said.from ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let actions = (said.actions ?? []).map { Opened.Action(id: $0.id, label: $0.label) }
        return Opened(title: from.isEmpty ? "Tilo" : from,
                      subtitle: said.important == true ? Words.important.said : nil,
                      words: words.split(whereSeparator: \.isNewline)
                          .map { String($0).trimmingCharacters(in: .whitespaces) }
                          .filter { !$0.isEmpty }.joined(separator: " "),
                      category: ClosedPush.category(of: actions), actions: actions)
    }

    /// What an opened notice shows.
    public struct Opened: Equatable, Sendable {
        public let title: String
        public let subtitle: String?
        public let words: String
        /// The name the system keeps the notice's buttons under.
        public let category: String
        public let actions: [Action]

        public struct Action: Equatable, Sendable {
            public let id: String
            public let label: String

            public init(id: String, label: String) {
                self.id = id
                self.label = label
            }
        }
    }

    /// The sealed part as the host writes it (`apple_push.rs`): who told it, the words, whether
    /// it is Important, and the buttons by label. What each invokes is in no push, so a button
    /// answers through the log's own action.
    private struct Said: Decodable {
        let from: String?
        let words: String
        let important: Bool?
        let actions: [Button]?

        struct Button: Decodable {
            let id: String
            let label: String
        }
    }

    public static func isName(_ tag: String) -> Bool {
        tag.count == 32 && tag.allSatisfy(\.isHexDigit)
    }

    /// The category a notice's buttons are kept under.
    public static func category(of actions: [Opened.Action]) -> String {
        guard !actions.isEmpty else { return "tilo.told" }
        return "tilo.told." + actions.map { "\($0.id):\($0.label)" }.joined(separator: "|")
    }

    /// Take away what a push showed for the thing called `name`.
    public static func withdraw(_ name: String) {
        UNUserNotificationCenter.current().getDeliveredNotifications { delivered in
            let ids = delivered.filter { ($0.request.content.userInfo["told"] as? String) == name }
                .map(\.request.identifier)
            guard !ids.isEmpty else { return }
            UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: ids)
        }
    }

    /// What the person is shown for an opened push named `named`.
    public static func shown(_ content: UNNotificationContent, opened: Opened,
                             named: String) -> UNNotificationContent {
        guard let changed = content.mutableCopy() as? UNMutableNotificationContent else { return content }
        changed.title = opened.title
        changed.subtitle = opened.subtitle ?? ""
        changed.body = opened.words
        changed.categoryIdentifier = opened.category
        changed.threadIdentifier = named
        changed.userInfo["enc"] = nil
        return changed
    }
}
