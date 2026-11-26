import Foundation

/// What the system shows the person for one piece of a conversation, and what they can answer
/// right there.
public struct Notice: Equatable, Sendable {
    public var item: Item.ID
    public var chat: Chat
    public var kind: Kind
    public var title: String
    /// The Important mark, where the piece carries it.
    public var subtitle: String?
    public var body: String
    /// The answer offered as a button, where the piece asks something: the one the assistant
    /// expects first. Beside it the system offers Open.
    public var answer: Answer?
    /// What the person's Tilo offers under it, the expected one first. Where it offers
    /// something, these are the buttons, and `answer` is not shown beside them.
    public var offers: [Offer] = []
    /// The agent whose own telling carried the offers, by name: said with them.
    public var offeredBy: String?

    public enum Kind: String, Sendable, CaseIterable {
        /// Something waits for the person's yes: a question, a secret, a sign-in.
        case decision
        case reminder
        case brief
        /// Nothing waits, and the assistant judged it important for the person's day.
        case important
        /// It did not interrupt, and says why: listed without a sound and without lighting the screen.
        case quiet
    }

    public struct Answer: Equatable, Sendable {
        public var question: Question.ID
        public var choice: Choice.ID
        public var title: String

        public init(question: Question.ID, choice: Choice.ID, title: String) {
            self.question = question
            self.choice = choice
            self.title = title
        }
    }

    public var interrupts: Bool { kind != .quiet }

    /// What the system shows as the notice's words: its body, and under it who offers the
    /// buttons, where an agent does.
    public var words: String {
        offeredBy.map { "\(body)\n\(AssistantWords.offeredBy.said(["name": $0]))" } ?? body
    }
}

/// Which pieces are told to the person outside the window, and how.
///
/// The person's rule: a decision that waits, a reminder and a brief interrupt, and so does
/// whatever the assistant judged important. A report of what it did alone, and a sub-agent's
/// news, do not, unless they are important. What did not interrupt may say why.
public enum NoticePlan {
    /// The button that gives the offered answer, and the one that opens the app at the piece.
    public static let answerAction = "assistant.answer"
    public static let openAction = "assistant.open"
    /// What a button's name begins with when it is one of the piece's offers: the offer's id follows.
    public static let offerAction = "assistant.offer."
    /// The system shows a handful of buttons under a notification; Open is one of them.
    public static let mostOffers = 3

    public static func notice(for item: Item, in chat: Chat) -> Notice? {
        func made(_ kind: Notice.Kind, _ body: String, answer: Notice.Answer? = nil) -> Notice {
            // What interrupts says what the assistant chose to say there; what did not says why.
            let said = kind == .quiet ? body : item.notice.map(Markdown.plain) ?? body
            var notice = Notice(item: item.id, chat: chat, kind: kind, title: AssistantWords.assistant.said,
                                subtitle: item.isImportant ? AssistantWords.important.said : nil, body: said, answer: answer)
            // What did not interrupt offers nothing to press: it is read in the app.
            if kind != .quiet {
                notice.offers = Array((item.offers.filter(\.isMain) + item.offers.filter { !$0.isMain }).prefix(mostOffers))
                notice.offeredBy = notice.offers.compactMap(\.by).first
            }
            return notice
        }
        switch item.content {
        case .question(let asked) where asked.waits:
            let offered = asked.choices.first(where: \.isMain) ?? asked.choices.first
            return made(.decision, Markdown.plain(asked.text),
                        answer: offered.map { Notice.Answer(question: asked.id, choice: $0.id, title: $0.title) })
        case .secret(let asked) where asked.state == .waiting:
            // A secret is typed in the app's protected field, never under a notification.
            return made(.decision, Markdown.plain(asked.text))
        case .connection(let offered) where offered.state == .waiting:
            return made(.decision, Markdown.plain(offered.text))
        case .reminder(let words):
            return made(.reminder, Markdown.plain(words))
        case .brief(let brief):
            return made(.brief, [brief.title, Markdown.plain(brief.text)].filter { !$0.isEmpty }.joined(separator: "\n"))
        case .message(let message) where message.author == .person:
            return nil
        default:
            if item.isImportant { return made(.important, item.said) }
            if let why = item.keptQuiet { return made(.quiet, why) }
            return nil
        }
    }

    /// The name the system keeps a notice's buttons under. One per kind and offered answer, so
    /// the same answer always finds the same buttons.
    public static func category(of notice: Notice) -> String {
        if !notice.offers.isEmpty {
            return "assistant.\(notice.kind.rawValue).offers." + notice.offers.map { "\($0.id.value)=\($0.title)" }.joined(separator: "|")
        }
        guard let answer = notice.answer else { return "assistant.\(notice.kind.rawValue)" }
        return "assistant.\(notice.kind.rawValue).\(answer.title)"
    }
}

extension Item {
    /// The piece's own words on one line, without Markdown's characters.
    public var said: String {
        switch content {
        case .message(let message): Markdown.plain(message.text)
        case .report(let line): Markdown.plain(line)
        case .question(let asked): Markdown.plain(asked.text)
        case .secret(let asked): Markdown.plain(asked.text)
        case .connection(let offered): Markdown.plain(offered.text)
        case .event(let card): Markdown.plain(card.text)
        case .brief(let brief): Markdown.plain(brief.text)
        case .reminder(let words): Markdown.plain(words)
        }
    }

    /// Whether the piece waits for the person: an open question, a secret or a sign-in not yet given.
    public var waits: Bool {
        switch content {
        case .question(let asked): asked.waits
        case .secret(let asked): asked.state == .waiting
        case .connection(let offered): offered.state == .waiting
        default: false
        }
    }
}
