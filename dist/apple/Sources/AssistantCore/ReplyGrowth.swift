import Foundation

/// What was drawn of a conversation's replies while they grew. A reply's model writes it over
/// seconds and its words arrive change by change; each time the conversation is drawn, whoever
/// draws it says so here. So a program with no window can count what a person at the window
/// would have seen: when a reply's words first stood there, when they last changed, and how
/// many times they were drawn anew.
public struct ReplyGrowth: Equatable, Sendable {
    /// What was drawn of one reply.
    public struct Seen: Equatable, Sendable {
        /// When its words were first drawn.
        public var first: Date
        /// When it was last drawn with other words than before.
        public var last: Date
        /// How many times it was drawn with other words than before, the first time included.
        public var drew: Int
        /// How many characters it held when it was last drawn.
        public var letters: Int

        public init(first: Date, last: Date, drew: Int, letters: Int) {
            self.first = first
            self.last = last
            self.drew = drew
            self.letters = letters
        }
    }

    /// The words each reply was last drawn with. Nil until the conversation was drawn loaded.
    private var words: [Item.ID: String]?
    /// The replies whose words came or changed after the conversation was first drawn.
    public private(set) var seen: [Item.ID: Seen] = [:]

    public init() {}

    /// The conversation is drawn as it stands at `moment`. What it held when it was first drawn
    /// is no news and is not counted. A reply that holds no word yet shows nothing and is not
    /// counted either, and one drawn again with the words it had is not drawn anew.
    public mutating func drew(_ conversation: Conversation, at moment: Date) {
        guard conversation.isLoaded else { return }
        var now: [Item.ID: String] = [:]
        for item in conversation.items {
            guard case .message(let message) = item.content, message.author == .assistant else { continue }
            now[item.id] = message.text
        }
        defer { words = now }
        guard let before = words else { return }
        for (id, text) in now where before[id] != text && !text.isEmpty {
            if var known = seen[id] {
                known.last = moment
                known.drew += 1
                known.letters = text.count
                seen[id] = known
            } else {
                seen[id] = Seen(first: moment, last: moment, drew: 1, letters: text.count)
            }
        }
    }
}
