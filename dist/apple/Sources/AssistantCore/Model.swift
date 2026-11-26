import Foundation

// What the assistant's window shows, as plain values. Nothing here knows where the values come
// from: `AssistantSource` hands them over, and the views draw them.

/// Which conversation: the one main chat, or a sub-agent's own.
public enum Chat: Hashable, Sendable {
    case main
    case subAgent(SubAgent.ID)
}

/// Whose assistant this is.
public struct Person: Identifiable, Hashable, Sendable {
    public typealias ID = Identifier<Person>
    public var id: ID
    public var name: String
    /// The workspace the assistant lives in, as the sidebar's foot names it.
    public var workspace: String

    public init(id: ID, name: String, workspace: String) {
        self.id = id
        self.name = name
        self.workspace = workspace
    }

    /// The letters in their picture: the first of the first and of the last word.
    public var initials: String {
        let words = name.split(whereSeparator: \.isWhitespace)
        let letters = [words.first, words.count > 1 ? words.last : nil].compactMap { $0?.first }
        return String(letters).uppercased()
    }
}

/// Where this device stands with the person's Tilo.
public enum Session: Equatable, Sendable {
    /// Nobody is signed in on this device. The window shows the way in.
    case signedOut
    /// Nobody is signed in on this device any more: the workspace it was let into is no longer
    /// on its site, which was set up again since, or the workspace was deleted. The window
    /// shows the way in and says so, until the device is paired again.
    case reset
    /// A link was taken, and Tilo asks before anything else: whose devices this one would join
    /// and which server adds it. The window shows what Tilo says and goes on only when the
    /// person presses.
    case asked(PairingAsk)
    /// A link was taken, and a device where the person is signed in has yet to say yes.
    case pairing
    /// A device where the person is signed in asks whether it shows this code. The window shows
    /// it, so the person compares the two before they approve there.
    case confirming(code: String)
    case signedIn(Person)
}

/// What Tilo asks a person before a device is paired with a link, in Tilo's own sentences and
/// with its own words for the two ways on. A link is written by whoever made it, so the person
/// reads whose devices this one would join, and on which server, before anyone is asked.
public struct PairingAsk: Equatable, Sendable {
    public var heading: String
    /// Whose device this becomes, which server adds it, and what follows from that.
    public var said: [String]
    /// What going on is called, and what stopping is called.
    public var go: String
    public var stop: String
    /// The site this app pairs with, its address in full. The card says it under Tilo's own
    /// sentences, whoever handed the link. Empty where nothing knows it.
    public var site: String

    public init(heading: String, said: [String], go: String, stop: String, site: String = "") {
        self.heading = heading
        self.said = said
        self.go = go
        self.stop = stop
        self.site = site
    }
}

/// What stands between the person and their assistant's first answer, in the order it is
/// settled: the assistant itself, then a provider of their own, then a model of it.
public enum Setup: Equatable, Sendable {
    /// Not known yet, or nothing to say of it: the main chat shows nothing.
    case unknown
    /// The person's assistant is being looked for, as at every start, and what answers for it
    /// is being read. Nothing is said of it but that the app waits for Tilo: the window's one
    /// sentence from its opening until the main chat stands.
    case findingAssistant
    /// As `findingAssistant`, where their server has been making their own workspace for a
    /// while: the first time it is asked for, that can take minutes, and the main chat says so.
    /// Only the start of a device that keeps no person yet says it: a later start makes nothing.
    case makingWorkspace
    /// The person keeps no assistant, and one is being made for them: the first time. Only
    /// then does the main chat say that their assistant is being set up.
    case makingAssistant
    /// It could not be found or made. The sentence says why, in Tilo's own words where it has any.
    case noAssistant(String)
    /// No provider is linked. These can be, each with a key of the person's own.
    case needsProvider([ProviderChoice])
    /// A provider is linked and no model answers in the main chat yet. These are offered.
    case needsModel([ModelChoice])
    case ready
}

/// A model provider the person can link with a key of their own.
public struct ProviderChoice: Identifiable, Equatable, Sendable {
    public typealias ID = Identifier<ProviderChoice>
    public var id: ID
    public var name: String
    /// Whether the person's vault keeps a key for it already: it is linked, and a key typed
    /// for it takes that key's place.
    public var isLinked: Bool

    public init(id: ID, name: String, isLinked: Bool = false) {
        self.id = id
        self.name = name
        self.isLinked = isLinked
    }
}

/// Whether the person's Tilo answers right now, as far as anything has shown it.
public enum Reach: Equatable, Sendable {
    /// Not known yet: nothing was tried, or the first try has not ended. Nothing is said to
    /// have failed, however long the try takes.
    case unknown
    case reachable
    /// A try has ended without getting in.
    case unreachable
}

// MARK: - The conversation

/// One piece of a conversation.
public struct Item: Identifiable, Equatable, Sendable {
    public typealias ID = Identifier<Item>
    public var id: ID
    public var at: Date
    public var content: Content
    /// The task this belongs to. Pieces that follow one another under the same task stand under
    /// one line that names it: the task's line with its replies.
    public var task: TaskLine?
    /// The assistant judged this important for the person's day: it carries the mark.
    public var isImportant: Bool
    /// Why this did not interrupt the person, in the assistant's own sentence. Nil when it did
    /// interrupt, or when there is nothing to say about it.
    public var keptQuiet: String?
    /// What a notification says of this piece, in the assistant's words, where that is shorter
    /// than the piece. Nil: the piece's own words.
    public var notice: String?
    /// What the person can do about this piece with one press where they are told of it, as
    /// their Tilo offers it there. Empty: the piece's own first answer, where it asks something.
    public var offers: [Offer]
    /// How often the person's Tilo told them of this again because it insists. A piece whose
    /// count went up is told once more.
    public var raised: Int
    /// The name a push gives this piece on this device, where the person's Tilo told them of it
    /// and the device may be told while the app is closed. The system keeps the piece's notice
    /// under it, so a push for the same piece takes that notice's place.
    public var toldAs: String?

    public init(id: ID, at: Date, _ content: Content, task: TaskLine? = nil, isImportant: Bool = false,
                keptQuiet: String? = nil, notice: String? = nil, offers: [Offer] = [], raised: Int = 0) {
        self.id = id
        self.at = at
        self.content = content
        self.task = task
        self.isImportant = isImportant
        self.keptQuiet = keptQuiet
        self.notice = notice
        self.offers = offers
        self.raised = raised
    }

    public enum Content: Equatable, Sendable {
        /// Words from the person, the assistant, or the main agent to a sub-agent.
        case message(Message)
        /// One line after the assistant acted alone: who reached out, and that it answered.
        case report(String)
        /// Something the assistant asks, with the answers as buttons.
        case question(Question)
        /// A secret it needs: a protected field that is no part of the conversation.
        case secret(SecretRequest)
        /// A service the person named by its address, which wants a sign-in.
        case connection(ConnectionOffer)
        /// Something the assistant set up to happen by itself, shown for review.
        case event(EventCard)
        /// What the person should know before something begins.
        case brief(Brief)
        /// A reminder the person asked for, when its time has come.
        case reminder(String)
    }
}

/// The line that names a task above the pieces that belong to it.
public struct TaskLine: Equatable, Sendable {
    public var task: AssistantTask.ID
    public var title: String
    public var state: State

    public init(task: AssistantTask.ID, title: String, state: State = .none) {
        self.task = task
        self.title = title
        self.state = state
    }

    /// What the line says after the task's name.
    public enum State: Equatable, Sendable {
        case none
        /// It came at its time. The words say how often, as the source says it: "every day".
        case scheduled(String)
        /// The assistant acted alone, on a lasting instruction.
        case actedAlone
        /// The task has this many replies, more than stand here.
        case replies(Int)
    }
}

/// What a message answers: one piece of its conversation, whole or a part of its words.
public struct Answered: Equatable, Sendable {
    /// The piece that is answered.
    public var piece: Item.ID
    /// Its words as the piece holds them, or the part of them that is answered. Empty where this
    /// device does not hold the piece.
    public var words: String
    /// Whether `words` are a part of the piece that was marked, and not the piece whole.
    public var isPart: Bool
    /// Where a marked part stands in the piece's own words, in UTF-8 bytes. Nil for a piece
    /// answered whole.
    public var span: Range<Int>?

    public init(_ piece: Item.ID, words: String, isPart: Bool = false, span: Range<Int>? = nil) {
        self.piece = piece
        self.words = words
        self.isPart = isPart
        self.span = span
    }

    /// Where a marked part stands in the words it was marked in, in UTF-8 bytes: the stretch that
    /// holds it. A part is marked in the plain words, so where Markdown's own signs stand between
    /// its words the stretch reaches from its first word to its last, and where even those are not
    /// found it is the words whole.
    public static func span(of part: String, in words: String) -> Range<Int> {
        func bytes(_ range: Range<String.Index>) -> Range<Int> {
            words.utf8.distance(from: words.startIndex, to: range.lowerBound)..<words.utf8.distance(from: words.startIndex, to: range.upperBound)
        }
        let whole = 0..<words.utf8.count
        if let exact = words.range(of: part) { return bytes(exact) }
        // The round sign a bubble draws for a list's dash is not in the words.
        let tokens = part.split(whereSeparator: \.isWhitespace).map(String.init).filter { $0 != "•" }
        var from = words.startIndex
        var first: String.Index?
        var last = words.startIndex
        for token in tokens {
            guard let found = words.range(of: token, range: from..<words.endIndex) else { return whole }
            if first == nil { first = found.lowerBound }
            last = found.upperBound
            from = found.upperBound
        }
        guard let first, first < last else { return whole }
        return bytes(first..<last)
    }

    /// The words as one line of plain text: what stands above the field and above a reply.
    public var line: String {
        Markdown.copied(words).split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }.joined(separator: " ")
    }
}

public struct Message: Equatable, Sendable {
    public var author: Author
    /// Markdown.
    public var text: String
    public var attachments: [Attachment]
    /// Live Tilo things that stand in this message. The views leave room for each and draw none.
    public var live: [LiveThing]
    /// Why the model gave no answer here, where the person's server says so with the reply and
    /// the cause is one the person mends themselves: the key, the key's account, or no provider.
    /// Nil for every other message, and for a failure whose cause their server does not say.
    public var keyFault: KeyFault?
    /// What the turn that writes this reply does, where the person's Tilo says so with the
    /// reply. Nil for a reply no turn works on, and for every other message.
    public var turn: Turn?
    /// The message of its conversation this one answers, where it answers one.
    public var answers: Answered?

    public init(_ author: Author, _ text: String, attachments: [Attachment] = [], live: [LiveThing] = [],
                keyFault: KeyFault? = nil, turn: Turn? = nil, answers: Answered? = nil) {
        self.author = author
        self.text = text
        self.attachments = attachments
        self.live = live
        self.keyFault = keyFault
        self.turn = turn
        self.answers = answers
    }

    public enum Author: Equatable, Sendable {
        case person
        /// Whoever this conversation is with: the assistant in the main chat, the sub-agent in its own.
        case assistant
        /// The main agent, writing to a sub-agent: the job it gave, or an instruction while it works.
        case mainAgent(MainAgentWord)
    }

    public enum MainAgentWord: Equatable, Sendable {
        case job
        case instruction
    }

    /// What a turn does while it writes a reply: what it does now, since when, and the steps
    /// it has taken. The words of a step are the system's own, never the app's.
    public struct Turn: Equatable, Sendable {
        public var now: Now
        /// When it took the message up.
        public var began: Date
        /// The steps it has taken so far, oldest first.
        public var steps: [Step]

        public init(now: Now, began: Date, steps: [Step] = []) {
            self.now = now
            self.began = began
            self.steps = steps
        }

        public enum Now: Equatable, Sendable {
            /// Its model thinks. The newest words it gave of its thinking; empty where it gave none.
            case thinking(String)
            /// It takes a step, in the step's own words as the person's Tilo says them; nil
            /// where their Tilo says only a tool's bare name.
            case step(String?)
            /// It writes the reply's words.
            case writing
        }

        /// One step of a turn, as one line says it.
        public struct Step: Identifiable, Equatable, Sendable {
            public var id: String
            /// The step in the system's own words: the action's label, and what it is about.
            public var words: String
            /// How long it took, where the device saw it begin and end.
            public var took: TimeInterval?
            public var outcome: Outcome

            public init(id: String, words: String, took: TimeInterval? = nil, outcome: Outcome = .done) {
                self.id = id
                self.words = words
                self.took = took
                self.outcome = outcome
            }

            public enum Outcome: Equatable, Sendable {
                /// It was begun and has not ended.
                case going
                case done
                /// It was refused or failed.
                case failed
            }
        }
    }

    /// A cause of a reply without an answer that another key or another provider mends.
    public enum KeyFault: Equatable, Sendable {
        /// The provider refused the key, or the key's account: the provider by the id and the
        /// name the person's server gives it.
        case key(ProviderChoice.ID, name: String)
        /// No provider of the person's was there to ask.
        case noProvider
    }
}

extension Message {
    /// Whether nothing of it is drawn: no words, nothing attached, no live thing, and no way back
    /// to a key under it. A turn leaves such a reply when what its person wrote next takes the
    /// turn along: the answer stands under their newest words.
    public var showsNothing: Bool {
        text.allSatisfy(\.isWhitespace) && attachments.isEmpty && live.isEmpty && keyFault == nil
    }
}

extension Item {
    /// Whether a conversation draws nothing for it: a message that shows nothing.
    public var showsNothing: Bool {
        if case .message(let message) = content { message.showsNothing } else { false }
    }
}

/// A file that came with a message.
public struct Attachment: Identifiable, Equatable, Sendable {
    public typealias ID = Identifier<Attachment>
    public var id: ID
    public var name: String
    /// The system's name for the kind of file, such as `public.png`.
    public var type: String
    public var bytes: Int?

    public init(id: ID, name: String, type: String, bytes: Int? = nil) {
        self.id = id
        self.name = name
        self.type = type
        self.bytes = bytes
    }
}

/// A live Tilo thing: an app on a record, or a written view on live records. What it is and how
/// it is drawn is the source's own business. The frame only leaves room for it and hands this
/// value to the view the source supplies.
public struct LiveThing: Identifiable, Hashable, Sendable {
    public typealias ID = Identifier<LiveThing>
    public var id: ID
    /// What it is called, for people and for VoiceOver.
    public var title: String
    /// Whatever the source needs to find it again. Nothing but the source's own view reads it.
    public var reference: String
    /// Whether the source's view says how tall the thing is, as a view on live data does. One that
    /// does not keeps the shape its source's view gives it.
    public var sizesItself: Bool
    /// Whether it is the person's to end here: a thing their own assistant shared for them. The
    /// source says so from what Tilo keeps of the share. Tilo decides when they ask.
    public var theirsToEnd: Bool

    public init(id: ID, title: String, reference: String, sizesItself: Bool = false, theirsToEnd: Bool = false) {
        self.id = id
        self.title = title
        self.reference = reference
        self.sizesItself = sizesItself
        self.theirsToEnd = theirsToEnd
    }
}

/// An answer the person can give with one press.
public struct Choice: Identifiable, Equatable, Sendable {
    public typealias ID = Identifier<Choice>
    public var id: ID
    /// What happens, in the assistant's words: "Send it".
    public var title: String
    /// The answer the assistant expects first.
    public var isMain: Bool

    public init(id: ID, title: String, isMain: Bool = false) {
        self.id = id
        self.title = title
        self.isMain = isMain
    }
}

/// Something the person can do about a piece with one press where they are told of it: a
/// button under its notification. Their Tilo says what it does; the frame shows it and hands
/// the press back.
public struct Offer: Identifiable, Equatable, Sendable {
    public typealias ID = Identifier<Offer>
    public var id: ID
    /// What happens, in the words of whoever offers it: "Send it".
    public var title: String
    /// The one expected first.
    public var isMain: Bool
    /// The agent that offered it, by name. Nil for what Tilo offers itself.
    public var by: String?

    public init(id: ID, title: String, isMain: Bool = false, by: String? = nil) {
        self.id = id
        self.title = title
        self.isMain = isMain
        self.by = by
    }
}

/// What the person was told while no device of theirs was looking, as their Tilo counts it at
/// their first look after: one message at the top of the new stretch of the main chat.
public struct SinceLeft: Equatable, Sendable {
    /// When the person left.
    public var left: Date
    /// What still waits for them: a question not answered, a thing that insists and was not seen.
    public var waits: [Line]
    /// What the assistant did without asking, because something of theirs came.
    public var didAlone: [Line]
    /// Everything else they were told meanwhile.
    public var cameIn: [Line]

    public init(left: Date, waits: [Line] = [], didAlone: [Line] = [], cameIn: [Line] = []) {
        self.left = left
        self.waits = waits
        self.didAlone = didAlone
        self.cameIn = cameIn
    }

    /// Nothing happened meanwhile: the conversation says nothing of the absence.
    public var isEmpty: Bool { waits.isEmpty && didAlone.isEmpty && cameIn.isEmpty }

    public struct Line: Identifiable, Equatable, Sendable {
        public var id: String
        /// Plain words, as the person was told them.
        public var words: String
        /// The piece of the main chat it is about, where that piece is held.
        public var item: Item.ID?
        public var isImportant: Bool

        public init(id: String, words: String, item: Item.ID? = nil, isImportant: Bool = false) {
            self.id = id
            self.words = words
            self.item = item
            self.isImportant = isImportant
        }
    }
}

public struct Question: Identifiable, Equatable, Sendable {
    public typealias ID = Identifier<Question>
    public var id: ID
    /// Markdown.
    public var text: String
    public var choices: [Choice]
    /// The choice the person made. Nil while the question waits.
    public var answer: Choice.ID?

    public init(id: ID, text: String, choices: [Choice], answer: Choice.ID? = nil) {
        self.id = id
        self.text = text
        self.choices = choices
        self.answer = answer
    }

    public var waits: Bool { answer == nil }
}

/// A secret the assistant needs. It uses the secret and never reads it: what the person types
/// goes to `AssistantSource.keep(_:for:)` and nowhere else.
public struct SecretRequest: Identifiable, Equatable, Sendable {
    public typealias ID = Identifier<SecretRequest>
    public var id: ID
    /// Markdown.
    public var text: String
    /// What the secret is for: the name of a site or a service.
    public var name: String
    /// The person's password manager by name, when one is connected.
    public var manager: String?
    public var state: State

    public init(id: ID, text: String, name: String, manager: String? = nil, state: State = .waiting) {
        self.id = id
        self.text = text
        self.name = name
        self.manager = manager
        self.state = state
    }

    public enum State: Equatable, Sendable {
        case waiting
        /// The vault has it now.
        case kept
        /// The person did not give it.
        case declined
        /// It was typed and kept, and where it belongs did not take it: what their server said of
        /// that, in its words.
        case refused(String)
    }
}

public struct ConnectionOffer: Identifiable, Equatable, Sendable {
    public typealias ID = Identifier<ConnectionOffer>
    public var id: ID
    /// Markdown.
    public var text: String
    /// The service's name.
    public var name: String
    /// How many commands it offers, when it says so before anyone signed in.
    public var commands: Int?
    /// Whether the service wants the person to sign in before it is used.
    public var needsSignIn: Bool
    /// The site that sign-in happens at, by name, when the service names it.
    public var signInAt: String?
    public var state: State

    public init(id: ID, text: String, name: String, commands: Int? = nil, needsSignIn: Bool = true, signInAt: String? = nil,
                state: State = .waiting) {
        self.id = id
        self.text = text
        self.name = name
        self.commands = commands
        self.needsSignIn = needsSignIn
        self.signInAt = signInAt
        self.state = state
    }

    public enum State: Equatable, Sendable {
        case waiting
        case connected
        case declined
    }
}

/// Something the assistant set up, shown in the conversation so the person can look at it and
/// change it by answering.
public struct EventCard: Equatable, Sendable {
    /// Markdown.
    public var text: String
    public var automation: Automation
    /// The answers, as a question the person may leave unanswered.
    public var question: Question.ID
    public var choices: [Choice]
    public var answer: Choice.ID?

    /// What the event keeps for its answers, once the person asked to see it: Markdown.
    public var shown: String?
    /// Where the card stands once something the person pressed was carried out.
    public var settled: Settled?

    public init(text: String, automation: Automation, question: Question.ID, choices: [Choice],
                answer: Choice.ID? = nil) {
        self.text = text
        self.automation = automation
        self.question = question
        self.choices = choices
        self.answer = answer
    }

    public enum Settled: Equatable, Sendable {
        /// Its person took back that it may act alone: it asks them each time.
        case asksEachTime
        /// Its person undid it: it is removed.
        case undone
    }
}

public struct Brief: Equatable, Sendable {
    /// The line above it, in the assistant's words: "Call in 15 minutes".
    public var title: String
    /// When what it prepares for begins, when that is known.
    public var begins: Date?
    /// Markdown.
    public var text: String

    public init(title: String, begins: Date? = nil, text: String) {
        self.title = title
        self.begins = begins
        self.text = text
    }
}

// MARK: - Sub-agents

/// An agent the main agent started for one job. It is no task: the Tasks panel never lists it.
public struct SubAgent: Identifiable, Equatable, Sendable {
    public typealias ID = Identifier<SubAgent>
    public var id: ID
    /// The job in a few words: "Usage drop: investigate".
    public var title: String
    /// What the main agent made it: "Coder". Empty until it has chosen.
    public var role: String
    public var state: State
    /// Where it runs: "Your Tilo server".
    public var runsOn: String
    public var started: Date
    public var startedBy: Starter
    /// When it finished, for one that is done.
    public var finished: Date?
    /// A live view of what it works on, when the source has one.
    public var live: LiveThing?

    public init(id: ID, title: String, role: String = "", state: State = .working, runsOn: String = "",
                started: Date, startedBy: Starter = .mainAgent, finished: Date? = nil, live: LiveThing? = nil) {
        self.id = id
        self.title = title
        self.role = role
        self.state = state
        self.runsOn = runsOn
        self.started = started
        self.startedBy = startedBy
        self.finished = finished
        self.live = live
    }

    public enum State: Equatable, Sendable {
        case working
        case needsYou
        case done
        case failed
        case stopped
    }

    public enum Starter: Equatable, Sendable {
        case mainAgent
        case person
    }
}

// MARK: - Tasks, scheduled things and events

/// A piece of work, however many sub-agents it takes.
public struct AssistantTask: Identifiable, Equatable, Sendable {
    public typealias ID = Identifier<AssistantTask>
    public var id: ID
    public var title: String
    public var state: State
    public var isImportant: Bool
    /// What it waits for, or one line about it: "Two versions exist. Which one?"
    public var note: String?
    /// Where it stands in the main chat, when it does.
    public var item: Item.ID?

    public init(id: ID, title: String, state: State, isImportant: Bool = false, note: String? = nil,
                item: Item.ID? = nil) {
        self.id = id
        self.title = title
        self.state = state
        self.isImportant = isImportant
        self.note = note
        self.item = item
    }

    public enum State: Equatable, Sendable, CaseIterable {
        case readyForReview
        case needsInput
        case inProgress
        case upNext
        case resolved
    }
}

/// Something the assistant does by itself: at a time, or when something else happens. Each was
/// set up by the assistant from what the person said, and is changed by telling it.
public struct Automation: Identifiable, Equatable, Sendable {
    public typealias ID = Identifier<Automation>
    public var id: ID
    public var sort: Sort
    /// "Morning rundown", "When a mail comes in".
    public var title: String
    /// One sentence under the title. For a scheduled thing when it comes ("Every day at 7:30."),
    /// for an event what the assistant does then.
    public var summary: String
    /// The next time it comes, for a scheduled thing.
    public var next: Date?
    /// Whether it comes once and is gone after.
    public var once: Bool
    /// What must hold for an event to count: "The sender is new".
    public var filter: String?
    /// How many times something must happen before the event counts.
    public var threshold: Int?
    /// What the assistant keeps with it: "your style, from 14 replies".
    public var note: String?
    /// Whether it acts without asking first.
    public var actsAlone: Bool
    /// Off is paused.
    public var isOn: Bool

    public init(id: ID, sort: Sort, title: String, summary: String, next: Date? = nil, once: Bool = false,
                filter: String? = nil, threshold: Int? = nil, note: String? = nil, actsAlone: Bool = false,
                isOn: Bool = true) {
        self.id = id
        self.sort = sort
        self.title = title
        self.summary = summary
        self.next = next
        self.once = once
        self.filter = filter
        self.threshold = threshold
        self.note = note
        self.actsAlone = actsAlone
        self.isOn = isOn
    }

    public enum Sort: Equatable, Sendable {
        /// Something at a time.
        case scheduled
        /// Something that happens when something else does.
        case event
    }
}

// MARK: - Supervision

/// What the assistant view shows: for looking, with a few switches.
public struct Supervision: Equatable, Sendable {
    public var isOn: Bool
    /// The model of the main chat. Nil while the person has linked no provider.
    public var model: String?
    public var usage: Usage
    public var connections: [Connection]
    public var logins: [Login]
    /// What the person's password manager holds for it: fetched when needed, never kept here.
    public var manager: ManagerLogins?
    /// What it has learned, newest first. `learnedCount` may be more than are listed.
    public var learned: [Learned]
    public var learnedCount: Int
    public var places: [Place]
    public var workspaces: [String]

    public init(isOn: Bool = true, model: String? = nil, usage: Usage = Usage(), connections: [Connection] = [],
                logins: [Login] = [], manager: ManagerLogins? = nil, learned: [Learned] = [], learnedCount: Int? = nil,
                places: [Place] = [], workspaces: [String] = []) {
        self.isOn = isOn
        self.model = model
        self.usage = usage
        self.connections = connections
        self.logins = logins
        self.manager = manager
        self.learned = learned
        self.learnedCount = learnedCount ?? learned.count
        self.places = places
        self.workspaces = workspaces
    }

    public struct Usage: Equatable, Sendable {
        /// What the assistant wrote today, as the person's server counted it. Nil where their
        /// server keeps no such count: the view says so then, and shows no number.
        public var messagesToday: Int?
        /// The tokens of the day's model calls, as the person's server counted them. Nil where
        /// their server keeps no such count.
        public var tokensToday: Int?
        /// Spent today, in millionths of the currency.
        public var spentTodayMicros: Int
        /// The most it may spend in a day. Nil for no limit.
        public var dailyLimitMicros: Int?
        /// The currency's sign as the person reads it: "$".
        public var currency: String

        public init(messagesToday: Int? = nil, tokensToday: Int? = nil, spentTodayMicros: Int = 0,
                    dailyLimitMicros: Int? = nil, currency: String = "$") {
            self.messagesToday = messagesToday
            self.tokensToday = tokensToday
            self.spentTodayMicros = spentTodayMicros
            self.dailyLimitMicros = dailyLimitMicros
            self.currency = currency
        }
    }

    public struct Connection: Identifiable, Equatable, Sendable {
        public typealias ID = Identifier<Connection>
        public var id: ID
        public var name: String
        public var added: Date
        public var commands: Int
        /// Whether nobody is signed in to it: its person never did, or the service no longer
        /// takes their sign-in. It offers nothing until they are.
        public var needsSignIn: Bool

        public init(id: ID, name: String, added: Date, commands: Int, needsSignIn: Bool = false) {
            self.id = id
            self.name = name
            self.added = added
            self.commands = commands
            self.needsSignIn = needsSignIn
        }
    }

    /// Something it can sign in to. It uses the login and never sees it.
    public struct Login: Identifiable, Equatable, Sendable {
        public typealias ID = Identifier<Login>
        public var id: ID
        public var name: String
        /// When the person typed it.
        public var typed: Date
        public var lastUsed: Date?

        public init(id: ID, name: String, typed: Date, lastUsed: Date? = nil) {
            self.id = id
            self.name = name
            self.typed = typed
            self.lastUsed = lastUsed
        }
    }

    public struct ManagerLogins: Equatable, Sendable {
        /// The manager's name.
        public var manager: String
        /// One of them by name, so the line says something.
        public var first: String
        public var count: Int

        public init(manager: String, first: String, count: Int) {
            self.manager = manager
            self.first = first
            self.count = count
        }
    }

    public struct Learned: Identifiable, Equatable, Sendable {
        public typealias ID = Identifier<Learned>
        public var id: ID
        public var text: String

        public init(id: ID, text: String) {
            self.id = id
            self.text = text
        }
    }

    /// Where it works: "The cloud machine", "always"; "This Mac", "in ~/Documents".
    public struct Place: Equatable, Sendable {
        public var name: String
        public var detail: String

        public init(name: String, detail: String) {
            self.name = name
            self.detail = detail
        }
    }
}

/// A model the person may choose, from their own providers.
public struct ModelChoice: Identifiable, Equatable, Sendable {
    public typealias ID = Identifier<ModelChoice>
    public var id: ID
    public var name: String
    public var provider: String

    public init(id: ID, name: String, provider: String) {
        self.id = id
        self.name = name
        self.provider = provider
    }
}

/// The kinds of work a model is chosen for. They are Tilo's own kinds; there is no other list.
public enum WorkKind: String, Sendable, CaseIterable {
    /// The main chat answering.
    case request
    /// The main chat working.
    case task
    /// A sub-agent.
    case subtask
    /// A filter that judges.
    case classifier
}
