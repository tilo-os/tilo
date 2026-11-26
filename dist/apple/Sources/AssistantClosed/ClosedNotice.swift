import AssistantCore
import Foundation

/// What a push from the person's server carries in the open, as Apple's service hands it to
/// the device: a name for the thing only this device can tie to it, where the thing stands in
/// the server's log, its kind, and the part sealed for this device alone. No word of the
/// thing is in the open.
public struct ClosedPush: Equatable, Sendable {
    /// The name the thing has in a push to this device (`ClosedKey.name(of:)`), which is also
    /// what the system keeps its notice under.
    public let name: String
    /// The thing's place in the server's log, or 0 where the push names none.
    public let place: Int
    public let kind: String
    /// The thing waits no more: its notice is to be taken away, and nothing is shown.
    public let isGone: Bool
    /// The part sealed for this device, where the push carries one.
    public let sealed: Data?

    /// From a notification's own fields. Nil for a notification that is no push of Tilo's.
    public init?(_ fields: [AnyHashable: Any]) {
        guard let name = fields["told"] as? String, !name.isEmpty else { return nil }
        self.name = name
        place = (fields["seq"] as? NSNumber)?.intValue ?? 0
        kind = fields["kind"] as? String ?? ""
        isGone = (fields["gone"] as? NSNumber)?.boolValue ?? false
        sealed = (fields["enc"] as? String).flatMap { Data(base64Encoded: $0) }
    }

    /// What the notice says once this device opened the sealed part: the agent's name over
    /// it, the Important mark in the app's own word, the server's words, and the kind's own
    /// category. Nil where the push carries no sealed part, where it does not open with this
    /// key, or where it says nothing: the line the push carries in the open then stands.
    ///
    /// The buttons a thing offers are not put under such a notice. Whether one may be pressed
    /// from under a notification is the app's to judge by what the button would invoke, which
    /// no push carries; the notice opens the app at the thing, where its buttons stand.
    public func opened(with key: ClosedKey) -> Opened? {
        guard !isGone, let sealed, let plain = key.open(sealed),
              let said = try? JSONDecoder().decode(Said.self, from: plain)
        else { return nil }
        let words = said.words.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !words.isEmpty else { return nil }
        let from = said.from.trimmingCharacters(in: .whitespacesAndNewlines)
        return Opened(title: from.isEmpty ? AssistantWords.assistant.said : from,
                      subtitle: nil,
                      words: Markdown.plain(words), category: "assistant.\(ClosedPush.noticeKind(of: kind).rawValue)")
    }

    /// What an opened notice shows.
    public struct Opened: Equatable, Sendable {
        public let title: String
        public let subtitle: String?
        public let words: String
        /// The name the app keeps the notice's buttons under: its kind's own, which the app
        /// gives at every start.
        public let category: String
    }

    /// The sealed part as the person's server writes it. Its buttons are read past.
    private struct Said: Decodable {
        let from: String
        let words: String
    }

    /// The kind of notice a thing of this kind is, as the app's own notices name kinds.
    static func noticeKind(of kind: String) -> Notice.Kind {
        switch kind {
        case "question": .decision
        case "reminder": .reminder
        case "brief": .brief
        default: .important
        }
    }
}
