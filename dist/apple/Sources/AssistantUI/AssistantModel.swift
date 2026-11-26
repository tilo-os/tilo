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

    /// The provider step as a person comes back to it after the first start.
    public struct KeyStep: Equatable, Sendable {
        /// The providers it offers: the linked ones and the ones that can be linked.
        public var providers: [ProviderChoice]
        /// The one its protected field is for when it appears.
        public var picked: ProviderChoice.ID
    }

    /// A way back to the provider step, as the assistant view's Model line offers it under the models.
    public enum KeyWay: Equatable, Sendable, Identifiable {
        /// Another key for a provider that is linked.
        case anotherKey(ProviderChoice)
        /// A provider that is not linked yet.
        case anotherProvider

        public var id: String {
            switch self {
            case .anotherKey(let provider): "key-\(provider.id.value)"
            case .anotherProvider: "provider"
            }
        }

        /// The line's words.
        public var words: String {
            switch self {
            case .anotherKey(let provider): AssistantWords.anotherKey.said(["name": provider.name])
            case .anotherProvider: AssistantWords.linkAnother.said
            }
        }
    }

    /// The way back to the provider step under a reply whose model gave no answer.
    public struct ReplyWay: Equatable, Sendable {
        /// The reply it stands under.
        public var item: Item.ID
        public var way: KeyWay
    }

    @ObservationIgnored public let source: any AssistantSource
    /// Nil until the source has said where this device stands.
    public private(set) var session: Session?
    /// Not known until the source has said it: nothing is said to have failed before a try has ended.
    public private(set) var reach = Reach.unknown
    /// Whether the person's Tilo answers and does not let this device in. The window then
    /// offers the way in where it would say that Tilo cannot be reached.
    public private(set) var joinStops = false
    /// When this device was approved, where it then stopped before it got in: the way in goes
    /// on from there, and nothing is paired a second time. Nil where no approved join waits.
    public private(set) var joinApproved: Date?
    /// Which look the person's Tilo draws in, light or dark: the frame draws in the same
    /// (`AssistantRoot`). Nil while nothing is known of it, and the system's look stands in.
    public private(set) var look: Look?
    public private(set) var subAgents: [SubAgent] = []
    public private(set) var tasks: [AssistantTask] = []
    private var proposalReview: ProposalReview?
    @ObservationIgnored private var proposalRead = UUID()
    @ObservationIgnored private var decidingProposal = false
    public var proposals: [ProposedTask]? { proposalReview?.pending }
    public var canLookNow: Bool { proposalReview?.canLookNow == true }
    public var canAlwaysAllow: Bool { proposalReview?.canAlwaysAllow == true }
    public var proposalPreferences: [ProposalPreference] { proposalReview?.preferences ?? [] }

    public private(set) var automations: [Automation] = []
    public private(set) var supervision = Supervision()
    public private(set) var conversations: [Chat: Conversation] = [:]
    public private(set) var models: [ModelChoice] = []
    /// The providers a key can be typed for, as the source listed them: linked, or to link.
    public private(set) var providers: [ProviderChoice] = []
    /// The provider step shown in the main chat again after the first start: the person asked
    /// for it from the assistant view, for another key or another provider. Nil: not shown.
    public private(set) var keyStep: KeyStep?
    /// The replies under which a way back was offered and a key was typed since, on this device:
    /// nothing is offered under them again.
    private var mended: Set<Item.ID> = []
    /// What stands between the person and their assistant's first answer.
    public private(set) var setup = Setup.unknown
    public private(set) var chatPermissionBusy = false
    /// From the app's opening until the main chat stands: its page, its session, the look for
    /// the assistant and the reading of what answers for it. The window says one sentence
    /// through all of it and names no step.
    public var isStarting: Bool {
        reach == .unknown || setup == .findingAssistant || setup == .makingWorkspace
    }
    /// What Tilo said before it asked whether to pair this device, kept while its code is compared.
    public private(set) var pairingSaid: [String] = []
    /// A pairing link handed to the app from outside before it was known whether this device
    /// stands at its way in: the app was started by the address, or its page is still on its
    /// way in. It is taken once the device stands at its way in, and dropped once it is in.
    @ObservationIgnored private var handed: String?
    /// A tapped push can reach the app before the signed-in conversation supplies its piece.
    @ObservationIgnored private var pendingNotice: String?
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
    /// The sentence a pairing ended with, while `problem` says it.
    @ObservationIgnored private var endedSaid: String?
    /// The technical details Tilo's page offers under the sentence a pairing ended with.
    private var endedDetails: String?
    /// Those details while the way in says that sentence: what its "copy the technical details
    /// below" points to. Nil: what is said is no pairing's end, or its page shows none.
    public var problemDetails: String? { problem != nil && problem == endedSaid ? endedDetails : nil }
    /// Whether the window asks for a new sub-agent's job.
    public var asksForJob = false
    /// The piece whose words are laid out for the person to mark a part of them: Select text in
    /// a piece's menu. Nil: none are.
    public var selecting: PieceWords?
    /// A live artifact open in the Mac Split Studio stage or iPhone full-screen sheet.
    public var stagedLive: LiveThing?
    #if os(macOS)
    /// Where Copy in a piece's menu puts the words: the system's pasteboard. A test hands it one
    /// of its own.
    @ObservationIgnored public var pasteboard = NSPasteboard.general
    #else
    /// Where Copy in a piece's menu puts the words: the system's pasteboard. Whoever checks Copy
    /// hands it one of its own.
    @ObservationIgnored public var pasteboard = UIPasteboard.general
    #endif
    /// A piece the conversation should bring into view.
    public var shownItem: Item.ID?
    /// Counts up each time the message field should take the caret.
    public private(set) var fieldCalls = 0
    /// The replies that stand open under a task's line, by the piece the line stands before.
    public private(set) var openReplies: [Item.ID: [Item]] = [:]
    /// The conversations in which a message the person sent from here was taken up by no turn
    /// for `takeUpPatience`. A quiet line says so under the message, until a turn begins.
    private var notTakenUp: Set<Chat> = []
    /// How long a message the person sent may stand with no turn taking it up before the
    /// conversation says so.
    @ObservationIgnored var takeUpPatience: Duration = .seconds(45)
    @ObservationIgnored private var takeUpWaits: [Chat: Task<Void, Never>] = [:]

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
        follow(source.reach()) { $0.take($1) }
        follow(source.pairingEnded()) { $0.take(ended: $1) }
        follow(source.pairingEndedDetails()) { $0.endedDetails = $1 }
        follow(source.subAgents()) { model, agents in
            model.subAgents = agents
            for agent in agents { model.listen(.subAgent(agent.id)) }
        }
        follow(source.tasks()) { $0.tasks = $1 }
        follow(source.automations()) { $0.automations = $1 }
        follow(source.supervision()) { $0.supervision = $1 }
        follow(source.sinceLeft()) { $0.sinceLeft = $1 }
        follow(source.joinStops()) { $0.joinStops = $1 }
        follow(source.joinApproved()) { $0.joinApproved = $1 }
        follow(source.pageLook()) { $0.look = $1 }
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
        reach = await first(source.reach()) ?? .unknown
        if let ended = await first(source.pairingEnded()) { take(ended: ended) }
        if let details = await first(source.pairingEndedDetails()) { endedDetails = details }
        subAgents = await first(source.subAgents()) ?? []
        tasks = await first(source.tasks()) ?? []
        automations = await first(source.automations()) ?? []
        supervision = await first(source.supervision()) ?? Supervision()
        sinceLeft = await first(source.sinceLeft()) ?? nil
        joinStops = await first(source.joinStops()) ?? false
        joinApproved = await first(source.joinApproved()) ?? nil
        look = await first(source.pageLook()) ?? nil
        for chat in [Chat.main] + subAgents.map({ Chat.subAgent($0.id) }) {
            guard let change = await first(source.conversation(chat)) else { continue }
            var conversation = Conversation()
            conversation.apply(change)
            conversations[chat] = conversation
        }
        resolveNotice()
    }

    /// Why a pairing ended without letting this device in: the way in says Tilo's own sentence
    /// under its buttons, until another link is taken or the device is in.
    private func take(ended why: String?) {
        if let why {
            problem = why
        } else if problem == endedSaid {
            // Only its own sentence is taken back: what a call said since stays.
            problem = nil
        }
        endedSaid = why
    }

    /// Where the device stands now. Whose devices it would join and on which server is kept
    /// from the moment Tilo asked until the device is in, or out again.
    private func take(_ stands: Session) {
        switch stands {
        case .asked(let ask): pairingSaid = Array(ask.said.prefix(2))
        case .signedOut, .signedIn: pairingSaid = []
        case .reset:
            // What the window showed of the workspace that is gone is put away with it. What
            // the person wrote and did not send stays in its field for when they are back.
            pairingSaid = []
            place = .chat(.main)
            keyStep = nil
            openReplies = [:]
        case .pairing, .confirming: break
        }
        let previous = person
        session = stands
        let changedWorkspace = if let from = previous?.workspaceRoute, let to = person?.workspaceRoute {
            from != to
        } else { false }
        if let previous, previous.id != person?.id || changedWorkspace {
            proposalReview = nil
            proposalRead = UUID()

            conversations.removeAll()
            shownItem = nil
            pendingNotice = nil
        }
        if person == nil { pendingNotice = nil } else { resolveNotice() }
        guard let words = handed else { return }
        switch stands {
        case .signedOut, .reset:
            handed = nil
            pair(with: words)
        case .asked, .pairing, .confirming:
            handed = nil
        case .signedIn:
            // Whether the device gets in is known only once a try has ended.
            if reach != .unknown { handed = nil }
        }
    }

    private func take(_ reach: Reach) {
        self.reach = reach
        // A try has ended and the device is its person's: nothing handed from outside is kept for later.
        if reach != .unknown, person != nil { handed = nil }
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
        let opened = resolveNotice(in: chat)
        // A turn that begins, or anything that comes after the person's message, ends the wait for one.
        switch change {
        case .answering(true): turnBegan(in: chat)
        case .added(let new) where !new.allSatisfy(AssistantModel.isOwnMessage): turnBegan(in: chat)
        default: break
        }
        guard known else { return }
        switch change {
        case .added(let new): for item in new where item.id != opened { told?(item, chat) }
        case .changed(let item):
            // The person already opened this push as the source published its piece.
            guard item.id != opened else { return }
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

    /// Whether a reply is being written in a conversation right now. Its row in the sidebar
    /// says so for exactly as long as the conversation itself does: both read this one word.
    public func isWriting(_ chat: Chat) -> Bool {
        conversation(chat).isAnswering
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

    /// Sub-agents that are currently running or waiting for input.
    public var activeSubAgents: [SubAgent] {
        subAgents.filter { $0.state == .working || $0.state == .needsYou }
    }

    /// Every live artifact shared in `chat`, newest first, deduplicated by id.
    public func liveThings(in chat: Chat) -> [LiveThing] {
        var seen: Set<LiveThing.ID> = []
        var result: [LiveThing] = []
        for item in conversation(chat).items.reversed() {
            if case .message(let message) = item.content {
                for thing in message.live.reversed() where seen.insert(thing.id).inserted {
                    result.append(thing)
                }
            }
        }
        return result
    }

    /// Read the current workspace on entry to Tasks and after its assistant has acted.
    public func refreshProposals() async throws {
        guard let person else { proposalReview = nil; return }
        let request = UUID()
        proposalRead = request
        do {
            let review = try await source.proposalReview(for: person)
            guard request == proposalRead else { return }
            proposalReview = review
        } catch {
            if request == proposalRead { proposalReview = nil }
            throw error
        }
    }

    public func review(_ proposal: ProposedTask, accept: Bool) async throws {
        guard let person, proposal.person == person.id, proposal.workspace == person.workspaceRoute,
              proposals?.contains(proposal) == true else { throw SourceProblem.signedOut }
        try await decide { try await source.review(proposal, accept: accept, for: person) }
    }

    public func alwaysAllow(_ proposal: ProposedTask) async throws {
        guard let person, canAlwaysAllow, !proposal.isEvent, proposals?.contains(proposal) == true else { throw SourceProblem.signedOut }
        try await decide { try await source.alwaysAllow(proposal, for: person) }
    }

    public func changePreference(_ preference: ProposalPreference, suggestAgain: Bool) async throws {
        guard let person, proposalPreferences.contains(preference), suggestAgain ? preference.quiet : preference.always else {
            throw SourceProblem.signedOut
        }
        try await decide { try await source.changePreference(preference, suggestAgain: suggestAgain, for: person) }
    }

    public func lookNow() async throws {
        guard let person, canLookNow else { throw SourceProblem.signedOut }
        try await decide { try await source.lookNow(for: person) }
    }

    private func decide(_ apply: () async throws -> Void) async throws {
        guard !decidingProposal else { throw SourceProblem.refused("A request is already in progress") }
        decidingProposal = true
        defer { decidingProposal = false }
        try await apply()
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
        pendingNotice = nil
        open(chat)
        shownItem = item
    }

    func openNotice(named name: String) {
        guard session == nil || person != nil else { return }
        pendingNotice = name
        resolveNotice()
    }

    @discardableResult
    private func resolveNotice(in changed: Chat? = nil) -> Item.ID? {
        guard let name = pendingNotice, person != nil else { return nil }
        for chat in changed.map({ [$0] }) ?? Array(conversations.keys) {
            if let item = conversations[chat]?.items.last(where: { $0.toldAs == name }) {
                show(item.id, in: chat)
                saw(item.id, in: chat)
                return item.id
            }
        }
        return nil
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

    /// Answer one piece of the open conversation: it stands above the field, which is called,
    /// and the message then sent says which piece it answers. `part` is the part of its words
    /// that is answered, where the person marked one.
    public func reply(to piece: Item.ID, saying words: String, part: String? = nil) {
        guard case .chat(let chat) = place else { return }
        var draft = drafts[chat] ?? Draft(text: "")
        draft.answers = Answered(piece, words: part ?? words, isPart: part != nil, span: part.map { Answered.span(of: $0, in: words) })
        drafts[chat] = draft
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
            if sent { self.awaitTurn(in: chat) }
        }
    }

    /// Whether a quiet line stands under the person's newest message in a conversation: they
    /// sent it from here, no turn has taken it up for `takeUpPatience`, and none has since.
    public func isNotTakenUp(_ chat: Chat) -> Bool {
        notTakenUp.contains(chat) && waitsForTurn(chat)
    }

    private nonisolated static func isOwnMessage(_ item: Item) -> Bool {
        if case .message(let message) = item.content { message.author == .person } else { false }
    }

    /// Whether the newest piece of a conversation is the person's own message, and nobody is
    /// writing an answer.
    private func waitsForTurn(_ chat: Chat) -> Bool {
        let held = conversation(chat)
        guard !held.isAnswering, let newest = held.items.last else { return false }
        return AssistantModel.isOwnMessage(newest)
    }

    /// The person's message went out. Where no turn takes it up within `takeUpPatience`, the
    /// conversation says so under it: they are not left with words that stand alone and no
    /// word of why nothing answers.
    private func awaitTurn(in chat: Chat) {
        turnBegan(in: chat)
        let patience = takeUpPatience
        takeUpWaits[chat] = Task { [weak self] in
            try? await Task.sleep(for: patience)
            guard !Task.isCancelled, let self else { return }
            self.takeUpWaits[chat] = nil
            if self.waitsForTurn(chat) { self.notTakenUp.insert(chat) }
        }
    }

    /// A turn began, or the person wrote again: nothing is said of the message before.
    private func turnBegan(in chat: Chat) {
        takeUpWaits[chat]?.cancel()
        takeUpWaits[chat] = nil
        notTakenUp.remove(chat)
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

    /// End the showing of a live thing the person's own assistant shared for them.
    public func stopSharing(_ thing: LiveThing.ID) {
        Task { await self.call { try await self.source.stopSharing(thing) } }
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

    /// The person signed in on the page `authenticate` answered, and their browser came back
    /// to this address. Says whether Tilo took it.
    @discardableResult
    public func signedIn(_ offer: ConnectionOffer.ID, at returned: URL) async -> Bool {
        await call { try await self.source.signedIn(offer, at: returned) }
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

    public func addTask(_ draft: Draft) async -> Bool {
        await call { try await self.source.addTask(draft.text, delivery: draft.delivery) }
    }

    public func setOn(_ on: Bool, for automation: Automation.ID) {
        Task { await self.call { try await self.source.setOn(on, for: automation) } }
    }

    public func loadModels() {
        Task {
            var found: [ModelChoice] = []
            var listed: [ProviderChoice] = []
            let read = await self.call {
                found = try await self.source.models()
                listed = try await self.source.providers()
            }
            guard read else { return }
            self.models = found
            self.providers = listed
        }
    }

    /// The ways back to the provider step: another key for each linked provider, and another
    /// provider where one is left to link. None while no provider is linked: then the main
    /// chat shows the step by itself.
    public var keyWays: [KeyWay] {
        let linked = providers.filter(\.isLinked)
        guard !linked.isEmpty else { return [] }
        return linked.map(KeyWay.anotherKey) + (providers.contains { !$0.isLinked } ? [.anotherProvider] : [])
    }

    /// Take a way back: the provider step stands in the main chat again, with the way's
    /// provider picked. Its protected field is the one place of the app a key is typed into.
    public func take(_ way: KeyWay) {
        let picked: ProviderChoice.ID? = switch way {
        case .anotherKey(let provider): providers.first { $0.id == provider.id }?.id
        case .anotherProvider: providers.first { !$0.isLinked }?.id
        }
        guard let picked else { return }
        keyStep = KeyStep(providers: providers, picked: picked)
        open(.main)
    }

    /// Put the provider step away again: the person types no other key now.
    public func putAwayKeyStep() {
        keyStep = nil
    }

    /// The newest piece of a conversation that is not the person's own, where it is a reply
    /// whose model gave no answer for a cause another key or another provider mends.
    private func failedByKey(in chat: Chat) -> (item: Item.ID, fault: Message.KeyFault)? {
        guard let newest = conversation(chat).items.last(where: { !AssistantModel.isOwnMessage($0) }),
              case .message(let message) = newest.content, message.author == .assistant, let fault = message.keyFault
        else { return nil }
        return (newest.id, fault)
    }

    /// What the main chat offers under its newest reply, where that reply's model gave no answer
    /// and their server says the cause is the key, the key's account or a missing provider: the
    /// way back to the provider step, one press. Nil while the step stands already, while the
    /// first start has a step of its own to show, and once a key was typed since. The cause is
    /// the typed one the reply carries; no word of the reply is read.
    public func replyWay(in chat: Chat) -> ReplyWay? {
        guard chat == .main, keyStep == nil, setup == .ready || setup == .unknown,
              let failed = failedByKey(in: chat), !mended.contains(failed.item)
        else { return nil }
        switch failed.fault {
        case .key(let id, let name):
            // Named as their server names it with the failure; what is linked now is read when the way is taken.
            let provider = providers.first { $0.id == id } ?? ProviderChoice(id: id, name: name, isLinked: true)
            return ReplyWay(item: failed.item, way: .anotherKey(provider))
        case .noProvider:
            return ReplyWay(item: failed.item, way: .anotherProvider)
        }
    }

    /// Take the way a failed reply offers. The person's providers are read first, as the
    /// assistant view reads them when it opens, since the step offers them; then the step
    /// stands in the main chat as from the Model line, with the reply's provider picked.
    public func take(_ offered: ReplyWay) {
        Task {
            var listed: [ProviderChoice] = []
            guard await self.call({ listed = try await self.source.providers() }) else { return }
            self.providers = listed
            var asked: ProviderChoice.ID?
            if case .anotherKey(let provider) = offered.way { asked = provider.id }
            // A provider their server lists no longer cannot be given a key: then the first that
            // can be linked, as from "Link another provider", or the first there is.
            guard let picked = listed.first(where: { $0.id == asked })?.id ?? listed.first(where: { !$0.isLinked })?.id ?? listed.first?.id
            else { return }
            self.keyStep = KeyStep(providers: listed, picked: picked)
            self.open(.main)
        }
    }

    /// Choose the model of the main chat: the one that answers there and the one that works there.
    public func choose(_ model: ModelChoice.ID) {
        Task {
            await self.call { try await self.source.choose(model, for: [.request, .task]) }
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

    /// The same action as the request button in the conversation.
    @discardableResult
    public func requestChatPermission() async -> Bool {
        await requestAccess(.chat)
    }

    @discardableResult
    public func requestAssistantWorkPermission() async -> Bool {
        await requestAccess(.work)
    }

    private func requestAccess(_ scope: AssistantAccess) async -> Bool {
        guard person != nil, place == .chat(.main), case .chatPermission(let permission) = setup, permission.canRequest,
              supervision.assistantAccess?.revocable != true, !chatPermissionBusy else { return false }
        chatPermissionBusy = true
        defer { chatPermissionBusy = false }
        return await call {
            switch scope {
            case .chat: try await source.requestChatPermission()
            case .work: try await source.requestAssistantWorkPermission()
            }
        }
    }

    public func checkChatPermission() async {
        guard person != nil, !chatPermissionBusy else { return }
        if place != .assistantView {
            guard place == .chat(.main), case .chatPermission(let permission) = setup, !permission.canRequest else { return }
        }
        chatPermissionBusy = true
        defer { chatPermissionBusy = false }
        await source.checkChatPermission()
    }

    /// The confirmation applies to the access the person reviewed, including its scope and expiry.
    @discardableResult
    public func revokeChatPermission(expected: Supervision.AssistantPermission) async -> Bool {
        guard person != nil, place == .assistantView, expected.revocable,
              supervision.assistantAccess == expected, !chatPermissionBusy else { return false }
        chatPermissionBusy = true
        defer { chatPermissionBusy = false }
        return await call { try await source.revokeChatPermission() }
    }

    /// Hand the key typed in the protected field to the person's vault, for this provider.
    /// Says whether it was taken; the models that can be chosen are held from then on.
    public func link(_ provider: ProviderChoice.ID, key: Secret) async -> Bool {
        var offered: [ModelChoice] = []
        guard await call({ offered = try await self.source.link(provider, key: key) }) else { return false }
        models = offered
        // What is linked now is read anew, and a step the person came back to is done with.
        if let listed = try? await source.providers() { providers = listed }
        // A reply that failed for this provider's key, or for want of a provider, has its mend.
        if let failed = failedByKey(in: .main) {
            if case .key(let asked, _) = failed.fault, asked != provider {} else { mended.insert(failed.item) }
        }
        keyStep = nil
        return true
    }

    /// A pairing link handed to the app from outside, in an address of the app's own
    /// (`HandedLink`). Only an app that stands at its way in takes one, and then as a pasted
    /// link. Whoever hands a link to an app that is in, or in the middle of a pairing, changes
    /// nothing and is told nothing. An app that does not know yet where it stands keeps the
    /// link until it does. Says whether the link was taken or kept.
    @discardableResult
    public func takeHanded(_ words: String) -> Bool {
        switch session {
        case .signedOut, .reset:
            pair(with: words)
            return true
        case nil:
            handed = words
            return true
        case .signedIn:
            // Its page may still be on its way in, and find the workspace gone.
            guard reach == .unknown else { return false }
            handed = words
            return true
        case .asked, .pairing, .confirming:
            return false
        }
    }

    /// Pair this device with the link in what was scanned or pasted.
    public func pair(with words: String, scanned: Bool = false) {
        guard let link = PairingLink(in: words) else {
            problem = (scanned ? AssistantWords.codeNoLink : AssistantWords.noLink).said
            return
        }
        Task { await self.call { try await self.source.pair(with: link) } }
    }

    /// Go on as the device that was approved: its person's press on Try again at the way in.
    /// Nothing is paired; what a call said before is said no more.
    public func goOn() {
        problem = nil
        Task { await self.source.goOn() }
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
    /// The full-size interactive live view for the Mac Split Studio stage and iPhone full-screen sheet.
    @Entry public var liveStageView: (@MainActor (LiveThing) -> AnyView)?
    /// Whether what stands in a protected field is put out of sight: while the app is not in
    /// front, when the system keeps a picture of its window.
    @Entry public var coversSecrets = false
}
