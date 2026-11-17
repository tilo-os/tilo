import AssistantCore
import Observation
import SwiftUI

/// What the window holds of the person's assistant, kept current from the source, and the calls
/// its views make. The views read this and nothing else; it reads `AssistantSource` and nothing else.
@MainActor
@Observable
public final class AssistantModel {
    /// What the middle of the window shows.
    public enum Place: Hashable, Sendable {
        case chat(Chat)
        /// The assistant view: supervision.
        case assistantView
    }

    public enum PanelTab: Hashable, Sendable, CaseIterable {
        case active, upNext, scheduled, resolved
    }

    @ObservationIgnored public let source: any AssistantSource
    /// Nil until the source has said where this device stands.
    public private(set) var session: Session?
    public private(set) var reach = Reach.reachable
    public private(set) var subAgents: [SubAgent] = []
    public private(set) var tasks: [AssistantTask] = []
    public private(set) var automations: [Automation] = []
    public private(set) var supervision = Supervision()
    public private(set) var conversations: [Chat: Conversation] = [:]
    public private(set) var models: [ModelChoice] = []
    /// What stands between the person and their assistant's first answer.
    public private(set) var setup = Setup.unknown
    /// What Tilo said before it asked whether to pair this device, kept while its code is compared.
    public private(set) var pairingSaid: [String] = []
    /// What the person was told while no device of theirs was looking. Nil: nothing to say of it.
    public private(set) var sinceLeft: SinceLeft?

    public var place = Place.chat(.main)
    public var showsSidebar = true
    /// The Tasks panel stands beside the main chat, and beside a sub-agent only when asked for.
    public var showsPanelInMain = true
    public var showsPanelElsewhere = false
    public var panelTab = PanelTab.active
    /// What the person has written and not sent, per conversation.
    public var drafts: [Chat: Draft] = [:]
    /// Why the last call did not go through, for the person to read. The next call that does clears it.
    public var problem: String?
    /// Whether the window asks for a new sub-agent's job.
    public var asksForJob = false
    /// A piece the conversation should bring into view.
    public var shownItem: Item.ID?
    /// Counts up each time the message field should take the caret.
    public private(set) var fieldCalls = 0
    /// The replies that stand open under a task's line, by the piece the line stands before.
    public private(set) var openReplies: [Item.ID: [Item]] = [:]

    /// Called for each piece that arrives after the conversation was first read.
    @ObservationIgnored public var told: ((Item, Chat) -> Void)?
    /// Called for each piece that changed, so what was told about it can be taken back.
    @ObservationIgnored public var settled: ((Item, Chat) -> Void)?
    /// Called for a piece that was told and now offers other things to press: what the system
    /// still shows of it is brought up to date, and nothing is told again.
    @ObservationIgnored public var offered: ((Item, Chat) -> Void)?
    @ObservationIgnored private var listeners: [Task<Void, Never>] = []
    @ObservationIgnored private var chats: [Chat: Task<Void, Never>] = [:]

    public init(source: any AssistantSource) {
        self.source = source
    }

    isolated deinit {
        for listener in listeners { listener.cancel() }
        for listener in chats.values { listener.cancel() }
    }

    // MARK: Reading

    /// Begin following the source. Calling it again changes nothing.
    public func start() {
        guard listeners.isEmpty else { return }
        follow(source.session()) { $0.take($1) }
        follow(source.setup()) { $0.setup = $1 }
        follow(source.reach()) { $0.reach = $1 }
        follow(source.subAgents()) { model, agents in
            model.subAgents = agents
            for agent in agents { model.listen(.subAgent(agent.id)) }
        }
        follow(source.tasks()) { $0.tasks = $1 }
        follow(source.automations()) { $0.automations = $1 }
        follow(source.supervision()) { $0.supervision = $1 }
        follow(source.sinceLeft()) { $0.sinceLeft = $1 }
        listen(.main)
    }

    /// Read what the source knows now, once, and return when it is held. For a picture or a
    /// test, which cannot wait for a listener's turn.
    public func prime() async {
        func first<Value: Sendable>(_ stream: AsyncStream<Value>) async -> Value? {
            for await value in stream { return value }
            return nil
        }
        if let stands = await first(source.session()) { take(stands) }
        setup = await first(source.setup()) ?? .unknown
        reach = await first(source.reach()) ?? .reachable
        subAgents = await first(source.subAgents()) ?? []
        tasks = await first(source.tasks()) ?? []
        automations = await first(source.automations()) ?? []
        supervision = await first(source.supervision()) ?? Supervision()
        sinceLeft = await first(source.sinceLeft()) ?? nil
        for chat in [Chat.main] + subAgents.map({ Chat.subAgent($0.id) }) {
            guard let change = await first(source.conversation(chat)) else { continue }
            var conversation = Conversation()
            conversation.apply(change)
            conversations[chat] = conversation
        }
    }

    /// Where the device stands now. Whose devices it would join and on which server is kept
    /// from the moment Tilo asked until the device is in, or out again.
    private func take(_ stands: Session) {
        switch stands {
        case .asked(let ask): pairingSaid = Array(ask.said.prefix(2))
        case .signedOut, .signedIn: pairingSaid = []
        case .pairing, .confirming: break
        }
        session = stands
    }

    private func follow<Value: Sendable>(_ stream: AsyncStream<Value>, _ take: @escaping (AssistantModel, Value) -> Void) {
        listeners.append(Task { [weak self] in
            for await value in stream {
                guard let self else { return }
                take(self, value)
            }
        })
    }

    private func listen(_ chat: Chat) {
        guard chats[chat] == nil else { return }
        let stream = source.conversation(chat)
        chats[chat] = Task { [weak self] in
            for await change in stream {
                guard let self else { return }
                self.take(change, in: chat)
            }
        }
    }

    private func take(_ change: ConversationChange, in chat: Chat) {
        var conversation = conversations[chat] ?? Conversation()
        let known = conversation.isLoaded
        let before = conversation.items
        conversation.apply(change)
        conversations[chat] = conversation
        guard known else { return }
        switch change {
        case .added(let new): for item in new { told?(item, chat) }
        case .changed(let item):
            // A piece that only now is a reminder, a brief or important was no news when its
            // words arrived: the person's Tilo said what it is a moment later. It is told now.
            // A piece that had something to tell before is settled: an answered question is
            // not told again for the mark it carries.
            // Why a piece did not notify is said the same way: listed once, when their Tilo says it.
            let held = before.first { $0.id == item.id }
            let was = held.flatMap { NoticePlan.notice(for: $0, in: chat)?.kind }
            let now = NoticePlan.notice(for: item, in: chat)
            if now != nil, was == nil || (was == .quiet && now?.interrupts == true) {
                told?(item, chat)
            } else if let held, now?.interrupts == true, item.raised > held.raised {
                // Their Tilo raised it again because it insists and nobody saw it: it is told once more.
                told?(item, chat)
            } else if let held, now?.interrupts == true, item.waits || held.offers.isEmpty, item.offers != held.offers {
                offered?(item, chat)
            } else {
                settled?(item, chat)
            }
        default: break
        }
    }

    /// The person saw a piece on this device: they opened it from its notification, or it
    /// arrived in the conversation they were looking at.
    public func saw(_ item: Item.ID, in chat: Chat) {
        let source = source
        Task { await source.seen(item, in: chat) }
    }

    /// The person pressed one of the things offered with a piece, where they were told of it.
    public func press(_ offer: Offer.ID, of item: Item.ID, in chat: Chat) {
        Task { await self.call { try await self.source.take(offer, of: item, in: chat) } }
    }

    public func conversation(_ chat: Chat) -> Conversation {
        conversations[chat] ?? Conversation()
    }

    public var person: Person? {
        if case .signedIn(let person) = session { person } else { nil }
    }

    /// The conversation in the middle of the window, when one is.
    public var chat: Chat? {
        if case .chat(let chat) = place { chat } else { nil }
    }

    public func subAgent(_ id: SubAgent.ID) -> SubAgent? {
        subAgents.first { $0.id == id }
    }

    public func tasks(_ state: AssistantTask.State) -> [AssistantTask] {
        tasks.filter { $0.state == state }
    }

    /// How many things wait for the person: what is ready for review and what needs their input.
    public var waiting: Int {
        tasks.filter { $0.state == .readyForReview || $0.state == .needsInput }.count
    }

    public var showsPanel: Bool {
        get { place == .chat(.main) ? showsPanelInMain : showsPanelElsewhere }
        set { if place == .chat(.main) { showsPanelInMain = newValue } else { showsPanelElsewhere = newValue } }
    }

    // MARK: Moving about

    public func open(_ chat: Chat) {
        listen(chat)
        place = .chat(chat)
    }

    /// Bring a piece of a conversation into view.
    public func show(_ item: Item.ID, in chat: Chat = .main) {
        open(chat)
        shownItem = item
    }

    public func showTasks(_ tab: PanelTab = .active) {
        panelTab = tab
        showsPanel = true
    }

    /// Open the main chat with words begun for the person, who finishes and sends them: how
    /// something the assistant set up is changed, by telling it.
    public func begin(_ words: String) {
        open(.main)
        var draft = drafts[.main] ?? Draft(text: "")
        if draft.text.isEmpty { draft.text = words }
        drafts[.main] = draft
        fieldCalls += 1
    }

    // MARK: Calling

    @discardableResult
    private func call(_ body: () async throws -> Void) async -> Bool {
        do {
            try await body()
            problem = nil
            return true
        } catch SourceProblem.refused(let why) {
            problem = why
        } catch {
            problem = AssistantWords.unreachable.said
        }
        return false
    }

    /// Send what is written in a conversation's field. Nothing is lost when it does not go through.
    public func send(in chat: Chat) {
        guard let draft = drafts[chat], !draft.isEmpty else { return }
        drafts[chat] = nil
        Task {
            let sent = await self.call { try await self.source.send(draft, to: chat) }
            if !sent, self.drafts[chat] == nil { self.drafts[chat] = draft }
            // A file the person picked is held open for the app until it is sent.
            if sent { for file in draft.files { file.url.stopAccessingSecurityScopedResource() } }
        }
    }

    /// Open a task's replies under its line, or put them away again.
    public func toggleReplies(of task: AssistantTask.ID, under line: Item.ID, in chat: Chat) {
        if openReplies[line] != nil {
            openReplies[line] = nil
            return
        }
        Task {
            var found: [Item] = []
            guard await self.call({ found = try await self.source.replies(of: task, in: chat) }) else { return }
            self.openReplies[line] = found
        }
    }

    /// Stop the answer that is being written in a conversation.
    public func stopAnswering(in chat: Chat) {
        Task { await self.call { try await self.source.stopAnswering(in: chat) } }
    }

    public func answer(_ question: Question.ID, with choice: Choice.ID) {
        Task { await self.call { try await self.source.answer(question, with: choice) } }
    }

    /// Hand what was typed in the protected field to the vault. Says whether it was taken.
    public func keep(_ secret: Secret, for request: SecretRequest.ID) async -> Bool {
        await call { try await self.source.keep(secret, for: request) }
    }

    public func fetchSecret(for request: SecretRequest.ID) {
        Task { await self.call { try await self.source.fetchSecret(for: request) } }
    }

    /// Begin signing in to a service. Returns the page to open on this device, when there is one.
    public func authenticate(_ offer: ConnectionOffer.ID) async -> URL? {
        var page: URL?
        await call { page = try await self.source.authenticate(offer) }
        return page
    }

    public func decline(_ offer: ConnectionOffer.ID) {
        Task { await self.call { try await self.source.decline(offer) } }
    }

    public func file(of attachment: Attachment.ID) async -> URL? {
        var file: URL?
        await call { file = try await self.source.file(of: attachment) }
        return file
    }

    public func startSubAgent(job: String) {
        Task { await self.call { try await self.source.startSubAgent(job: job) } }
    }

    public func stop(_ subAgent: SubAgent.ID) {
        Task { await self.call { try await self.source.stop(subAgent) } }
    }

    public func addTask(_ title: String) {
        Task { await self.call { try await self.source.addTask(title) } }
    }

    public func setOn(_ on: Bool, for automation: Automation.ID) {
        Task { await self.call { try await self.source.setOn(on, for: automation) } }
    }

    public func setAssistantOn(_ on: Bool) {
        Task { await self.call { try await self.source.setAssistantOn(on) } }
    }

    public func loadModels() {
        Task {
            var found: [ModelChoice] = []
            if await self.call({ found = try await self.source.models() }) { self.models = found }
        }
    }

    /// Choose the model of the main chat: the one that answers there and the one that works there.
    public func choose(_ model: ModelChoice.ID) {
        Task {
            await self.call {
                try await self.source.choose(model, for: .request)
                try await self.source.choose(model, for: .task)
            }
        }
    }

    public func forget(_ learned: Supervision.Learned.ID) {
        Task { await self.call { try await self.source.forget(learned) } }
    }

    public func earlier(in chat: Chat) {
        guard let first = conversation(chat).items.first else { return }
        Task {
            var older: [Item] = []
            guard await self.call({ older = try await self.source.earlier(in: chat, before: first.id) }) else { return }
            self.conversations[chat]?.prepend(older)
        }
    }

    /// The person's answer to what Tilo asked before pairing: go on, or stop. A button's press
    /// calls this and nothing else does.
    public func answerPairing(goOn: Bool) {
        Task { await self.source.answerPairing(goOn: goOn) }
    }

    /// Find the person's assistant once more, and make it where they keep none.
    public func makeAssistant() {
        Task { await self.call { try await self.source.makeAssistant() } }
    }

    /// Hand the key typed in the protected field to the person's vault, for this provider.
    /// Says whether it was taken; the models that can be chosen are held from then on.
    public func link(_ provider: ProviderChoice.ID, key: Secret) async -> Bool {
        var offered: [ModelChoice] = []
        guard await call({ offered = try await self.source.link(provider, key: key) }) else { return false }
        models = offered
        return true
    }

    /// Pair this device with the link in what was scanned or pasted.
    public func pair(with words: String, scanned: Bool = false) {
        guard let link = PairingLink(in: words) else {
            problem = (scanned ? AssistantWords.codeNoLink : AssistantWords.noLink).said
            return
        }
        Task { await self.call { try await self.source.pair(with: link) } }
    }

    public func signOut() {
        Task {
            await self.source.signOut()
            self.place = .chat(.main)
        }
    }
}

extension EnvironmentValues {
    /// How the window words its lines: the person's language, their calendar, and what time it is.
    @Entry public var wording = Wording()
    /// The view that draws a live Tilo thing. Whoever implements `AssistantSource` supplies it;
    /// the frame draws an empty place where there is none.
    @Entry public var liveThingView: (@MainActor (LiveThing) -> AnyView)?
    /// Whether what stands in a protected field is put out of sight: while the app is not in
    /// front, when the system keeps a picture of its window.
    @Entry public var coversSecrets = false
}
