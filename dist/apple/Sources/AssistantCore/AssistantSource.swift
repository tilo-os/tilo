import Foundation

/// The one interface the assistant's views read and call. The views never talk to Tilo: an
/// implementation of this does, against Tilo's own resources and actions. `MemorySource` is the
/// one for tests and pictures.
///
/// **Reading.** Every stream begins with what is known now and goes on with each change, for as
/// long as someone listens. A stream never fails: while Tilo cannot be reached it is silent, and
/// `reach()` says so. Each call hands out a stream of its own.
///
/// **Calling.** A call returns when Tilo has taken it, and throws a `SourceProblem` when it did
/// not. What the call changed arrives through the streams, never as the call's result, so every
/// device shows the same. A call made at the start, while it is not known yet whether Tilo
/// answers, waits until it is known: it is not said to have failed before anything has.
///
/// Nothing here is about one person's line of work: a task, an event or a sub-agent is whatever
/// the assistant was told to make of it.
public protocol AssistantSource: Sendable {
    // MARK: The way in

    /// The site's browser entry, where the person can open their workspace and pair this device.
    @MainActor var devicePairingSite: URL? { get }

    /// Where this device stands: signed out, pairing, or signed in as someone.
    func session() -> AsyncStream<Session>

    /// Whether the person's Tilo answers right now. From the start until a first try has ended
    /// it is `.unknown`, and nothing is said to have failed: a page on its way in is not away.
    func reach() -> AsyncStream<Reach>

    /// Which look the person's Tilo draws in right now, light or dark: the theme they have
    /// there, whether they chose it there or told their assistant to. The frame draws in the
    /// same, so that frame and Tilo never differ. Nil while nothing is known of it: the
    /// system's look stands in until then.
    func pageLook() -> AsyncStream<Look?>

    /// Why the last pairing ended without letting this device in, in Tilo's own sentence: its
    /// site refused the link, or its page could not go on. Nil where none did, and again once
    /// another link is taken or the device is in. The way in says it.
    func pairingEnded() -> AsyncStream<String?>

    /// The technical details Tilo's page offers under that sentence, to read or copy: what the
    /// page makes of the stop, the server, the step, the error and its build, never a token, a
    /// key or a certificate. Tilo's sentence may say "copy the technical details below": the
    /// way in shows these below it. Nil where the page shows none, and while no end is said.
    func pairingEndedDetails() -> AsyncStream<String?>

    /// When a device where the person is signed in approved this one, for a device that then
    /// stopped before it got in, whatever stopped it: its page, its site, the network, or the
    /// app's own end. It is kept across launches, and the app goes on as the device that was
    /// approved: nothing is paired a second time. Nil where no approved join waits to get in,
    /// and again once the device is in, another link is taken or it is signed out. The way in
    /// says it in the place of what a first pairing promises.
    func joinApproved() -> AsyncStream<Date?>

    /// Go on as the device that was approved (`joinApproved()`): its page is asked for again.
    /// `session()` says how it goes on, and `pairingEnded()` what stopped it where it stops.
    func goOn() async

    /// Pair this device with a link that "Pair a new device" made on a device where the person is
    /// signed in: scanned as a code, or pasted. `session()` says how it goes on.
    func pair(with link: PairingLink) async throws

    /// Answer what Tilo asked before pairing (`Session.asked`): go on, or stop. Only the
    /// person's own press calls this; nothing answers for them.
    func answerPairing(goOn: Bool) async

    /// Forget this device's place in the person's Tilo.
    func signOut() async

    /// Whether the person's Tilo answers and does not let this device in: in the try that ended
    /// last its site served its page, and the page's join stopped. False while the device is in,
    /// while it stands at a way in, and where its site did not answer. The window then offers
    /// the way in where it would say that Tilo cannot be reached. Nothing is given up by that:
    /// only the person's own press signs the device out.
    func joinStops() -> AsyncStream<Bool>

    // MARK: The first run

    /// What stands between the person and their assistant's first answer. A source finds the
    /// person's assistant by itself when the device gets in, and makes it where they keep none.
    func setup() -> AsyncStream<Setup>

    /// Find the person's assistant once more, and make it where they keep none: what the
    /// source does by itself, for the person to ask again after it did not go through.
    func makeAssistant() async throws

    /// Link a provider with the key the person typed into the protected field, and answer the
    /// models that can be chosen then. The key goes to the person's vault and nowhere else: not
    /// into the conversation, not to a model, not into a log.
    func link(_ provider: ProviderChoice.ID, key: Secret) async throws -> [ModelChoice]

    /// The providers a key can be typed for: the ones the person linked, each said to be so,
    /// and the ones they can link beside them. It is what the provider step offers a person
    /// who comes back to it for another key or another provider.
    func providers() async throws -> [ProviderChoice]

    // MARK: A conversation

    /// The pieces of a conversation, live. The first change is `.all` with the latest pieces.
    func conversation(_ chat: Chat) -> AsyncStream<ConversationChange>

    /// Older pieces than the oldest the caller holds, oldest first. Empty at the beginning.
    func earlier(in chat: Chat, before item: Item.ID) async throws -> [Item]

    /// The replies of a task that its line counts and the conversation does not show, oldest
    /// first: what stands under the line once the person opens "2 replies". Empty when there
    /// are none to show.
    func replies(of task: AssistantTask.ID, in chat: Chat) async throws -> [Item]

    /// Send what the person wrote, with what they attached.
    func send(_ draft: Draft, to chat: Chat) async throws
    /// Stop the answer that is being written in a conversation. Nothing happens where none is.
    func stopAnswering(in chat: Chat) async throws
    /// End the showing of a live thing that says it is the person's to end
    /// (`LiveThing.theirsToEnd`): what their own assistant shared for them. Tilo decides whether
    /// it is theirs to end, and its no is thrown.
    func stopSharing(_ thing: LiveThing.ID) async throws

    /// Give one of a question's answers. An event's card is answered the same way.
    func answer(_ question: Question.ID, with choice: Choice.ID) async throws

    /// Keep this secret for the request, in the person's vault. The secret goes nowhere else: not
    /// into the conversation, not to a model, not into a log.
    func keep(_ secret: Secret, for request: SecretRequest.ID) async throws

    /// Fetch what the request needs from the person's password manager instead.
    func fetchSecret(for request: SecretRequest.ID) async throws

    /// The person will not give this secret now.
    func decline(_ request: SecretRequest.ID) async throws

    /// Begin signing in to a service. Returns the page the person signs in on, when one has to be
    /// opened on this device; nil when the source does the rest.
    func authenticate(_ offer: ConnectionOffer.ID) async throws -> URL?

    /// The person will not sign in to this service now.
    func decline(_ offer: ConnectionOffer.ID) async throws

    /// The person signed in on the page `authenticate` answered, and their browser came back
    /// to this address. The source finishes the sign-in with it. The address opens the sign-in
    /// once: it goes to the person's Tilo and nowhere else, not into the conversation, not to
    /// a model, not into a log.
    func signedIn(_ offer: ConnectionOffer.ID, at returned: URL) async throws

    /// A file that came with a message, as a file on this device.
    func file(of attachment: Attachment.ID) async throws -> URL

    // MARK: Sub-agents

    /// The sub-agents, newest first.
    func subAgents() -> AsyncStream<[SubAgent]>

    /// Ask for a sub-agent for this job. The main agent picks its role and its model.
    func startSubAgent(job: String) async throws

    /// Stop a sub-agent that works.
    func stop(_ subAgent: SubAgent.ID) async throws

    // MARK: Tasks, scheduled things and events

    /// Every task of the assistant. The views sort them by where they stand.
    func tasks() -> AsyncStream<[AssistantTask]>

    /// Waiting work the assistant proposed in the certified person's workspace.
    func proposalReview(for person: Person) async throws -> ProposalReview?
    func review(_ proposal: ProposedTask, accept: Bool, for person: Person) async throws
    func alwaysAllow(_ proposal: ProposedTask, for person: Person) async throws
    func changePreference(_ preference: ProposalPreference, suggestAgain: Bool, for person: Person) async throws
    func lookNow(for person: Person) async throws


    /// Give the assistant a task.
    func addTask(_ title: String, delivery: UUID) async throws

    /// What comes at a time and what happens when something else does, in the order to show.
    func automations() -> AsyncStream<[Automation]>

    /// Pause one, or let it go on.
    func setOn(_ on: Bool, for automation: Automation.ID) async throws

    // MARK: Supervision

    func supervision() -> AsyncStream<Supervision>

    /// The models the person can choose from, of the providers they linked.
    func models() async throws -> [ModelChoice]

    /// Choose one model for several kinds of work together: one ask of the person's server, so
    /// that no moment stands between them in which the model answers for one kind and none for
    /// the other.
    func choose(_ model: ModelChoice.ID, for work: [WorkKind]) async throws

    /// Take back something it has learned.
    func forget(_ learned: Supervision.Learned.ID) async throws

    // MARK: Being told

    /// The person saw this piece on this device: they opened it from its notification, or it
    /// arrived in the conversation they were looking at. A source whose Tilo told the person of
    /// it says so there, so that they are told of it nowhere else.
    func seen(_ item: Item.ID, in chat: Chat) async

    /// What the person was told while no device of theirs was looking, once their Tilo has
    /// counted that stretch. Nil while they were not away, or while nothing is known of it.
    func sinceLeft() -> AsyncStream<SinceLeft?>

    /// The person pressed one of the things offered with a piece (`Item.offers`) where they
    /// were told of it. The source does what the offer names, as the person on this device.
    func take(_ offer: Offer.ID, of item: Item.ID, in chat: Chat) async throws

    // MARK: Being told while the app does not run

    /// An address Apple's service gave this device. The person's host sends to it when the app
    /// does not run: a notification, the start of a live activity, or news for one that runs.
    func register(_ address: PushAddress) async

    /// The person's Tilo is to send to this device no more: it forgets the address it was handed.
    func unregister() async
}

extension AssistantSource {
    /// One submission from a caller without an editable task draft.
    public func addTask(_ title: String) async throws { try await addTask(title, delivery: UUID()) }

    @MainActor public var devicePairingSite: URL? { nil }

    public func proposalReview(for person: Person) async throws -> ProposalReview? { nil }
    public func review(_ proposal: ProposedTask, accept: Bool, for person: Person) async throws {
        throw SourceProblem.refused("This workspace does not offer proposals")
    }
    public func alwaysAllow(_ proposal: ProposedTask, for person: Person) async throws {
        throw SourceProblem.refused("This workspace does not offer advance approval")
    }
    public func changePreference(_ preference: ProposalPreference, suggestAgain: Bool, for person: Person) async throws {
        throw SourceProblem.refused("This workspace does not offer proposal preferences")
    }
    public func lookNow(for person: Person) async throws {
        throw SourceProblem.refused("This workspace does not offer an immediate look")
    }


    public func register(_ address: PushAddress) async {}
    public func unregister() async {}

    /// A source that keeps no replies apart from the conversation has none to show.
    public func replies(of task: AssistantTask.ID, in chat: Chat) async throws -> [Item] { [] }
    public func stopAnswering(in chat: Chat) async throws {}
    /// A source that shows nothing live has nothing to end.
    public func stopSharing(_ thing: LiveThing.ID) async throws {}
    public func seen(_ item: Item.ID, in chat: Chat) async {}
    public func sinceLeft() -> AsyncStream<SinceLeft?> { AsyncStream { $0.yield(nil) } }
    /// A source that answers no page for a sign-in has none to finish.
    public func signedIn(_ offer: ConnectionOffer.ID, at returned: URL) async throws {
        throw SourceProblem.refused("No sign-in was begun here")
    }
    public func take(_ offer: Offer.ID, of item: Item.ID, in chat: Chat) async throws {
        throw SourceProblem.refused("Nothing is offered with this here")
    }

    // A source with no first run of its own asks nothing before it pairs, has nothing in the
    // way of an answer and links no provider.
    public func answerPairing(goOn: Bool) async {}
    /// A source whose pairings end with no sentence of Tilo's has none to say.
    public func pairingEnded() -> AsyncStream<String?> { AsyncStream { $0.yield(nil) } }
    public func pairingEndedDetails() -> AsyncStream<String?> { AsyncStream { $0.yield(nil) } }
    /// A source that keeps no approved join has none to go on with.
    public func joinApproved() -> AsyncStream<Date?> { AsyncStream { $0.yield(nil) } }
    public func goOn() async {}
    public func setup() -> AsyncStream<Setup> { AsyncStream { $0.yield(.ready) } }
    public func makeAssistant() async throws {}
    public func link(_ provider: ProviderChoice.ID, key: Secret) async throws -> [ModelChoice] {
        throw SourceProblem.refused("No provider can be linked here")
    }
    public func providers() async throws -> [ProviderChoice] { [] }
    /// A source that never tries to get in is never stopped at it.
    public func joinStops() -> AsyncStream<Bool> { AsyncStream { $0.yield(false) } }
    /// A source that draws nothing of Tilo's has no look of its own to follow.
    public func pageLook() -> AsyncStream<Look?> { AsyncStream { $0.yield(nil) } }
}

/// Light or dark: which of the two a theme draws in.
public enum Look: Equatable, Sendable {
    case light
    case dark
}

/// How a conversation changed.
public enum ConversationChange: Equatable, Sendable {
    /// These are the latest pieces, oldest first: forget what was held before.
    case all([Item])
    /// These came after everything held.
    case added([Item])
    /// This piece is different now: an answer was given, a reply grew, a card was settled.
    case changed(Item)
    case removed(Item.ID)
    /// Whether whoever the conversation is with is writing an answer right now.
    case answering(Bool)
}

/// What the person wrote and attached, before it is sent.
public struct Draft: Equatable, Sendable {
    /// Markdown, as typed.
    public var text: String { didSet { if text != oldValue { delivery = UUID() } } }
    public var files: [File] { didSet { if files != oldValue { delivery = UUID() } } }
    /// The message of the conversation these words answer, where the person chose one. It is
    /// kept here and nowhere else until the words are sent.
    public var answers: Answered? { didSet { if answers != oldValue { delivery = UUID() } } }
    /// A retry of the same draft names its original Chat message and action.
    public private(set) var delivery = UUID()

    public static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.text == rhs.text && lhs.files == rhs.files && lhs.answers == rhs.answers
    }


    public init(text: String, files: [File] = [], answers: Answered? = nil) {
        self.text = text
        self.files = files
        self.answers = answers
    }

    public var isEmpty: Bool {
        text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && files.isEmpty
    }

    /// A file on this device that goes with the message.
    public struct File: Identifiable, Equatable, Sendable {
        public var id: URL { url }
        public var url: URL
        public var name: String

        public init(url: URL, name: String? = nil) {
            self.url = url
            self.name = name ?? url.lastPathComponent
        }
    }
}

/// A link that pairs a device, taken out of whatever was scanned or pasted around it.
public struct PairingLink: Equatable, Sendable {
    public let url: URL

    /// The first web address in the words. Nil when they hold none.
    public init?(in words: String) {
        let edges = CharacterSet(charactersIn: "<>\"'()[]{}.,;").union(.whitespacesAndNewlines)
        for word in words.split(whereSeparator: \.isWhitespace) {
            let trimmed = word.trimmingCharacters(in: edges)
            guard let found = URLComponents(string: trimmed), let scheme = found.scheme?.lowercased(),
                  scheme == "https" || scheme == "http", let host = found.host, !host.isEmpty,
                  found.user == nil, let url = found.url
            else { continue }
            self.url = url
            return
        }
        return nil
    }
}

/// A pairing link handed to the app from outside, in an address of the app's own:
/// `tilo-assistant://pair?link=<the pairing link>`. Tilo's page can offer one beside the link
/// and the code of "Pair a new device", so that on the computer the app is on nobody
/// copies and pastes. What it carries is a link as any other: the app takes it only at its
/// way in, and treats it as a pasted one.
public enum HandedLink {
    public static let scheme = "tilo-assistant"

    /// What an address hands over: the words its `link` holds. Nil for any other address.
    public static func words(in address: URL) -> String? {
        // The address is exactly the app's name for it, then what it carries: nothing before the question mark is read as more.
        let written = address.absoluteString
        guard written.lowercased().hasPrefix("\(scheme)://pair?"), let parts = URLComponents(string: written),
              let link = parts.queryItems?.first(where: { $0.name == "link" })?.value, !link.isEmpty
        else { return nil }
        return link
    }

    /// The address that hands `link` over.
    public static func address(for link: URL) -> URL? {
        // Every letter of the link that could end the address, or a part of it, is written out.
        guard let written = link.absoluteString.addingPercentEncoding(withAllowedCharacters: .alphanumerics) else { return nil }
        return URL(string: "\(scheme)://pair?link=\(written)")
    }
}

/// Something the person typed into the protected field. It says nothing of itself: printing it,
/// describing it or looking into it shows no letter of what is inside.
public struct Secret: Sendable, CustomStringConvertible, CustomDebugStringConvertible, CustomReflectable {
    private let letters: [UInt8]

    public init(_ typed: String) {
        letters = Array(typed.utf8)
    }

    public var isEmpty: Bool { letters.isEmpty }

    /// What was typed, for the one place that keeps it: the vault.
    public func handOver() -> String {
        String(decoding: letters, as: UTF8.self)
    }

    public var description: String { "Secret" }
    public var debugDescription: String { "Secret" }
    public var customMirror: Mirror { Mirror(self, children: [], displayStyle: .struct) }
}

/// Where Apple's service reaches this device for the app.
public struct PushAddress: Equatable, Sendable {
    public var use: Use
    public var token: Data
    /// Which of Apple's two services gave the address.
    public var service: Service
    /// The app the address reaches: its bundle identifier.
    public var app: String

    public init(use: Use, token: Data, service: Service = .production, app: String = "") {
        self.use = use
        self.token = token
        self.service = service
        self.app = app
    }

    public enum Service: String, Equatable, Sendable {
        case development, production
    }

    public enum Use: Equatable, Sendable {
        /// Notifications.
        case notices
        /// Starting a live activity while the app does not run.
        case activityStart
        /// News for one live activity that runs, named as `LiveActivityPlan` names it.
        case activity(String)
    }
}

extension AssistantSource {
    /// The same choice with its kinds of work written out one after the other.
    public func choose(_ model: ModelChoice.ID, for work: WorkKind...) async throws {
        try await choose(model, for: work)
    }
}

/// Why a call did not go through.
public enum SourceProblem: Error, Equatable, Sendable {
    /// Tilo did not answer.
    case unreachable
    /// Nobody is signed in on this device.
    case signedOut
    /// Tilo said no. The sentence is its own, for the person to read.
    case refused(String)
}
