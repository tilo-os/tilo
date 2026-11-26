import Foundation

/// An `AssistantSource` that keeps everything in memory, for tests and pictures. It starts
/// empty and signed out, and nothing in it answers by itself: what the assistant would say is
/// put in from outside (`post`, `change`), as a test or a picture needs it.
public final class MemorySource: AssistantSource, @unchecked Sendable {
    /// What a source holds when it starts.
    public struct Content: Sendable {
        public var session: Session = .signedOut
        public var main: [Item] = []
        public var subAgents: [SubAgent] = []
        public var subAgentChats: [SubAgent.ID: [Item]] = [:]
        /// The replies a task's line counts and the conversation does not show.
        public var replies: [AssistantTask.ID: [Item]] = [:]
        public var tasks: [AssistantTask] = []
        public var automations: [Automation] = []
        public var supervision = Supervision()
        public var models: [ModelChoice] = []
        /// Who a pairing link signs in. Nil: Tilo cannot be reached.
        public var pairsAs: Person?
        /// The code a pairing shows for the person to compare, when it shows one.
        public var pairingCode: String?
        /// What is asked before a pairing, when something is: it goes on once the person answered.
        public var pairingAsk: PairingAsk?
        /// Why the last pairing ended without letting this device in, where one did.
        public var pairingEnded: String?
        /// The technical details Tilo's page offers under that sentence, where it shows them.
        public var pairingEndedDetails: String?
        /// When this device was approved, where it then stopped before it got in.
        public var joinApproved: Date?
        /// What stands between the person and a first answer.
        public var setup: Setup = .ready
        /// The models a provider offers once it is linked.
        public var offered: [ModelChoice] = []
        /// The providers a person can come back to the provider step for: linked, or to link.
        public var providers: [ProviderChoice] = []
        /// What the person was told while they were away.
        public var sinceLeft: SinceLeft?
        /// Whether the person's Tilo answers and does not let this device in.
        public var joinStops = false
        /// Which look the person's Tilo draws in, where it has said one.
        public var look: Look?

        public init() {}
    }

    private let lock = NSLock()
    private let now: @Sendable () -> Date
    /// How long after it is asked a conversation's pieces arrive, as a site's do a moment after
    /// the window stands. Nothing: at once.
    private let arrives: Duration?
    private let sessionNow: Broadcast<Session>
    private let reachNow = Broadcast<Reach>(.reachable)
    private let endedNow: Broadcast<String?>
    private let endedDetailsNow: Broadcast<String?>
    private let approvedNow: Broadcast<Date?>
    private let agents: Broadcast<[SubAgent]>
    private let taskList: Broadcast<[AssistantTask]>
    private let automationList: Broadcast<[Automation]>
    private let supervisionNow: Broadcast<Supervision>
    private let modelChoices: [ModelChoice]
    private let pairsAs: Person?
    private let taskReplies: [AssistantTask.ID: [Item]]
    private let pairingCode: String?
    private let pairingAsk: PairingAsk?
    private let setupNow: Broadcast<Setup>
    private let sinceNow: Broadcast<SinceLeft?>
    private let joinStopsNow: Broadcast<Bool>
    private let lookNow: Broadcast<Look?>
    private var presses: [Press] = []
    private let offered: [ModelChoice]
    private let providerChoices: [ProviderChoice]
    private var linked: [ModelChoice] = []
    private var keys: [ProviderChoice.ID: Secret] = [:]
    private var feeds: [Chat: Feed]
    private var vault: [SecretRequest.ID: Secret] = [:]
    private var files: [Attachment.ID: URL] = [:]
    private var made = 0

    public init(_ content: Content = Content(), now: @escaping @Sendable () -> Date = { Date() }, arrives: Duration? = nil) {
        self.now = now
        self.arrives = arrives
        sessionNow = Broadcast(content.session)
        endedNow = Broadcast(content.pairingEnded)
        endedDetailsNow = Broadcast(content.pairingEndedDetails)
        approvedNow = Broadcast(content.joinApproved)
        agents = Broadcast(content.subAgents)
        taskList = Broadcast(content.tasks)
        automationList = Broadcast(content.automations)
        supervisionNow = Broadcast(content.supervision)
        modelChoices = content.models
        pairsAs = content.pairsAs
        taskReplies = content.replies
        pairingCode = content.pairingCode
        pairingAsk = content.pairingAsk
        setupNow = Broadcast(content.setup)
        sinceNow = Broadcast(content.sinceLeft)
        joinStopsNow = Broadcast(content.joinStops)
        lookNow = Broadcast(content.look)
        offered = content.offered
        providerChoices = content.providers
        var feeds: [Chat: Feed] = [.main: Feed(content.main)]
        for agent in content.subAgents {
            feeds[.subAgent(agent.id)] = Feed(content.subAgentChats[agent.id] ?? [])
        }
        self.feeds = feeds
    }

    // MARK: The far side, for tests and pictures

    /// As if whoever the conversation is with had written this.
    public func post(_ item: Item, in chat: Chat = .main) {
        feed(chat)?.add([item])
    }

    /// As if a piece had changed on the far side.
    public func change(_ item: Item, in chat: Chat = .main) {
        feed(chat)?.change(where: { $0.id == item.id }) { $0 = item }
    }

    public func setAnswering(_ on: Bool, in chat: Chat = .main) {
        feed(chat)?.setAnswering(on)
    }

    public func set(_ reach: Reach) {
        reachNow.set(reach)
    }

    /// As if the person's Tilo had said where this device stands.
    public func set(_ session: Session) {
        sessionNow.set(session)
    }

    /// As if a pairing had ended without letting this device in, with Tilo's sentence for why
    /// and, where its page shows them, the technical details under it.
    public func endPairing(saying why: String, details: String? = nil) {
        endedDetailsNow.set(details)
        endedNow.set(why)
        sessionNow.set(.signedOut)
    }

    /// As if this device had been approved then and had stopped before it got in, or as if
    /// no approved join waited any more.
    public func set(joinApproved: Date?) {
        approvedNow.set(joinApproved)
    }

    public func set(_ agent: SubAgent) {
        agents.change { list in
            if let index = list.firstIndex(where: { $0.id == agent.id }) { list[index] = agent }
        }
    }

    /// What the vault holds for a request. The only way a kept secret is ever seen again.
    public func kept(for request: SecretRequest.ID) -> Secret? {
        lock.withLock { vault[request] }
    }

    /// The key the vault holds for a provider. The only way a linked key is ever seen again.
    public func key(for provider: ProviderChoice.ID) -> Secret? {
        lock.withLock { keys[provider] }
    }

    public func set(_ setup: Setup) {
        setupNow.set(setup)
    }

    /// As if the person's Tilo answered and did not let this device in, or let it in again.
    public func set(joinStops: Bool) {
        joinStopsNow.set(joinStops)
    }

    /// As if the person's Tilo said that it draws in this look now, or said none any more.
    public func set(look: Look?) {
        lookNow.set(look)
    }

    /// Everything a conversation holds now.
    public func items(in chat: Chat = .main) -> [Item] {
        feed(chat)?.all ?? []
    }

    // MARK: The way in

    public func session() -> AsyncStream<Session> { sessionNow.stream() }
    public func reach() -> AsyncStream<Reach> { reachNow.stream() }
    public func pairingEnded() -> AsyncStream<String?> { endedNow.stream() }
    public func pairingEndedDetails() -> AsyncStream<String?> { endedDetailsNow.stream() }
    public func joinApproved() -> AsyncStream<Date?> { approvedNow.stream() }

    /// The device that was approved is on its way in again: what stopped it is said no more.
    public func goOn() async {
        guard approvedNow.current != nil else { return }
        endedNow.set(nil)
        endedDetailsNow.set(nil)
        sessionNow.set(.pairing)
    }

    public func pair(with link: PairingLink) async throws {
        await known()
        // Another link is taken: what the last pairing ended with is said no more.
        endedNow.set(nil)
        endedDetailsNow.set(nil)
        approvedNow.set(nil)
        guard pairsAs != nil, reachNow.current == .reachable else { throw SourceProblem.unreachable }
        // Where something is asked first, the pairing goes on once the person answered.
        if let pairingAsk {
            sessionNow.set(.asked(pairingAsk))
            return
        }
        letIn()
    }

    public func answerPairing(goOn: Bool) async {
        guard case .asked = sessionNow.current else { return }
        if goOn { letIn() } else { sessionNow.set(.signedOut) }
    }

    private func letIn() {
        guard let person = pairsAs else { return }
        sessionNow.set(.pairing)
        if let pairingCode { sessionNow.set(.confirming(code: pairingCode)) }
        approvedNow.set(nil)
        sessionNow.set(.signedIn(person))
    }

    // MARK: The first run

    public func setup() -> AsyncStream<Setup> { setupNow.stream() }
    public func joinStops() -> AsyncStream<Bool> { joinStopsNow.stream() }
    public func pageLook() -> AsyncStream<Look?> { lookNow.stream() }

    // MARK: Being told

    /// One press of something offered with a piece.
    public struct Press: Equatable, Sendable {
        public var offer: Offer.ID
        public var item: Item.ID
        public var chat: Chat

        public init(offer: Offer.ID, item: Item.ID, chat: Chat) {
            self.offer = offer
            self.item = item
            self.chat = chat
        }
    }

    /// Every offer the person pressed, in order: for a test.
    public var pressed: [Press] { lock.withLock { presses } }

    public func sinceLeft() -> AsyncStream<SinceLeft?> { sinceNow.stream() }

    /// As if the person's Tilo had counted what happened while they were away.
    public func setSinceLeft(_ since: SinceLeft?) {
        sinceNow.set(since)
    }

    public func take(_ offer: Offer.ID, of item: Item.ID, in chat: Chat) async throws {
        guard feed(chat)?.all.first(where: { $0.id == item })?.offers.contains(where: { $0.id == offer }) == true else {
            throw SourceProblem.refused("This is no longer offered.")
        }
        lock.withLock { presses.append(Press(offer: offer, item: item, chat: chat)) }
    }

    public func makeAssistant() async throws {
        try signedIn()
        if case .noAssistant = setupNow.current { setupNow.set(.ready) }
    }

    public func link(_ provider: ProviderChoice.ID, key: Secret) async throws -> [ModelChoice] {
        try signedIn()
        guard !key.isEmpty else { throw SourceProblem.refused("Nothing was typed.") }
        var first: [ProviderChoice] = []
        if case .needsProvider(let choices) = setupNow.current { first = choices }
        guard (first + providerChoices).contains(where: { $0.id == provider }) else {
            throw SourceProblem.refused("This provider cannot be linked.")
        }
        let all = lock.withLock {
            keys[provider] = key
            linked = offered
            return modelChoices + linked
        }
        // A person who comes back to the step keeps the model they chose: only the first
        // link goes on to the choice of one.
        if !first.isEmpty { setupNow.set(offered.isEmpty ? .ready : .needsModel(offered)) }
        return all
    }

    public func providers() async throws -> [ProviderChoice] {
        try signedIn()
        return lock.withLock {
            providerChoices.map { choice in
                var choice = choice
                if keys[choice.id] != nil { choice.isLinked = true }
                return choice
            }
        }
    }

    public func signOut() async {
        joinStopsNow.set(false)
        sessionNow.set(.signedOut)
    }

    // MARK: A conversation

    public func conversation(_ chat: Chat) -> AsyncStream<ConversationChange> {
        guard let feed = feed(chat) else { return AsyncStream { $0.yield(.all([])) } }
        guard let arrives else { return feed.stream() }
        // The pieces come as a site's do: a moment after they were asked for.
        return AsyncStream { continuation in
            let coming = Task {
                try? await Task.sleep(for: arrives)
                guard !Task.isCancelled else { return }
                for await change in feed.stream() { continuation.yield(change) }
                continuation.finish()
            }
            continuation.onTermination = { _ in coming.cancel() }
        }
    }

    public func earlier(in chat: Chat, before item: Item.ID) async throws -> [Item] {
        // Memory holds a conversation whole: nothing is older than what a listener got.
        try signedIn()
        return []
    }

    public func replies(of task: AssistantTask.ID, in chat: Chat) async throws -> [Item] {
        try signedIn()
        return taskReplies[task] ?? []
    }

    public func send(_ draft: Draft, to chat: Chat) async throws {
        await known()
        try signedIn()
        guard !draft.isEmpty else { return }
        guard let feed = feed(chat) else { throw SourceProblem.refused("This conversation is gone.") }
        let attachments = draft.files.map { file in
            let id = Attachment.ID(nextName("file"))
            lock.withLock { files[id] = file.url }
            return Attachment(id: id, name: file.name, type: "public.data")
        }
        feed.add([Item(id: Item.ID(nextName("item")), at: now(),
                       .message(Message(.person, draft.text, attachments: attachments, answers: draft.answers)))])
    }

    public func stopAnswering(in chat: Chat) async throws {
        feed(chat)?.setAnswering(false)
    }

    public func stopSharing(_ thing: LiveThing.ID) async throws {
        try signedIn()
        for feed in allFeeds() {
            let ended = feed.change(where: { item in
                guard case .message(let message) = item.content else { return false }
                return message.live.contains { $0.id == thing && $0.theirsToEnd }
            }) { item in
                guard case .message(var message) = item.content else { return }
                message.live.removeAll { $0.id == thing }
                item.content = .message(message)
            }
            if ended { return }
        }
        throw SourceProblem.refused("This is not yours to stop sharing.")
    }

    public func answer(_ question: Question.ID, with choice: Choice.ID) async throws {
        try signedIn()
        var problem: SourceProblem? = .refused("This question is gone.")
        for feed in allFeeds() {
            let found = feed.change(where: { $0.content.question(named: question) != nil }) { item in
                switch item.content {
                case .question(var asked):
                    guard asked.waits else { problem = .refused("This was answered already."); return }
                    guard asked.choices.contains(where: { $0.id == choice }) else {
                        problem = .refused("This is not one of the answers."); return
                    }
                    asked.answer = choice
                    item.content = .question(asked)
                    problem = nil
                case .event(var card):
                    guard card.choices.contains(where: { $0.id == choice }) else {
                        problem = .refused("This is not one of the answers."); return
                    }
                    card.answer = choice
                    item.content = .event(card)
                    problem = nil
                default:
                    break
                }
            }
            if found { break }
        }
        if let problem { throw problem }
    }

    public func keep(_ secret: Secret, for request: SecretRequest.ID) async throws {
        try signedIn()
        guard !secret.isEmpty else { throw SourceProblem.refused("Nothing was typed.") }
        try settle(request, as: .kept)
        lock.withLock { vault[request] = secret }
    }

    public func fetchSecret(for request: SecretRequest.ID) async throws {
        try signedIn()
        try settle(request, as: .kept, needsManager: true)
    }

    public func decline(_ request: SecretRequest.ID) async throws {
        try signedIn()
        try settle(request, as: .declined)
    }

    public func authenticate(_ offer: ConnectionOffer.ID) async throws -> URL? {
        try signedIn()
        try settle(offer, as: .connected)
        return nil
    }

    public func decline(_ offer: ConnectionOffer.ID) async throws {
        try signedIn()
        try settle(offer, as: .declined)
    }

    public func file(of attachment: Attachment.ID) async throws -> URL {
        try signedIn()
        guard let url = lock.withLock({ files[attachment] }) else {
            throw SourceProblem.refused("This file is gone.")
        }
        return url
    }

    // MARK: Sub-agents

    public func subAgents() -> AsyncStream<[SubAgent]> { agents.stream() }

    public func startSubAgent(job: String) async throws {
        try signedIn()
        let job = job.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !job.isEmpty else { return }
        let agent = SubAgent(id: SubAgent.ID(nextName("sub-agent")), title: job, started: now(), startedBy: .person)
        let first = Item(id: Item.ID(nextName("item")), at: now(), .message(Message(.mainAgent(.job), job)))
        lock.withLock { feeds[.subAgent(agent.id)] = Feed([first]) }
        agents.change { $0.insert(agent, at: 0) }
    }

    public func stop(_ subAgent: SubAgent.ID) async throws {
        try signedIn()
        let time = now()
        let stopped = agents.change { list -> Bool in
            guard let index = list.firstIndex(where: { $0.id == subAgent }), list[index].state == .working
                    || list.contains(where: { $0.id == subAgent && $0.state == .needsYou })
            else { return false }
            list[index].state = .stopped
            list[index].finished = time
            return true
        }
        guard stopped else { throw SourceProblem.refused("This sub-agent does not work any more.") }
    }

    // MARK: Tasks, scheduled things and events

    public func tasks() -> AsyncStream<[AssistantTask]> { taskList.stream() }

    public func addTask(_ title: String, delivery: UUID) async throws {
        try signedIn()
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else { return }
        let task = AssistantTask(id: .init(delivery.uuidString), title: title, state: .upNext)
        taskList.change { if !$0.contains(where: { $0.id == task.id }) { $0.append(task) } }
    }

    public func automations() -> AsyncStream<[Automation]> { automationList.stream() }

    public func setOn(_ on: Bool, for automation: Automation.ID) async throws {
        try signedIn()
        let found = automationList.change { list -> Bool in
            guard let index = list.firstIndex(where: { $0.id == automation }) else { return false }
            list[index].isOn = on
            return true
        }
        guard found else { throw SourceProblem.refused("This is gone.") }
    }

    // MARK: Supervision

    public func supervision() -> AsyncStream<Supervision> { supervisionNow.stream() }

    public func models() async throws -> [ModelChoice] {
        try signedIn()
        return lock.withLock { modelChoices + linked }
    }

    public func choose(_ model: ModelChoice.ID, for work: [WorkKind]) async throws {
        try signedIn()
        guard let chosen = lock.withLock({ modelChoices + linked }).first(where: { $0.id == model }) else {
            throw SourceProblem.refused("This model is not one of yours.")
        }
        // The view names the main chat's model: the one that answers.
        guard work.contains(.request) else { return }
        supervisionNow.change { $0.model = chosen.name }
        if case .needsModel = setupNow.current { setupNow.set(.ready) }
    }

    public func forget(_ learned: Supervision.Learned.ID) async throws {
        try signedIn()
        supervisionNow.change { facts in
            guard let index = facts.learned.firstIndex(where: { $0.id == learned }) else { return }
            facts.learned.remove(at: index)
            facts.learnedCount = max(0, facts.learnedCount - 1)
        }
    }

    // MARK: Inside

    /// A call made before it is known whether Tilo answers waits until it is known, as a
    /// source on Tilo does at its start.
    private func known() async {
        guard reachNow.current == .unknown else { return }
        for await reach in reachNow.stream() where reach != .unknown { return }
    }

    private func signedIn() throws {
        guard reachNow.current != .unreachable else { throw SourceProblem.unreachable }
        guard case .signedIn = sessionNow.current else { throw SourceProblem.signedOut }
    }

    private func feed(_ chat: Chat) -> Feed? {
        lock.withLock { feeds[chat] }
    }

    private func allFeeds() -> [Feed] {
        lock.withLock { Array(feeds.values) }
    }

    private func nextName(_ kind: String) -> String {
        lock.withLock {
            made += 1
            return "\(kind)-made-\(made)"
        }
    }

    private func settle(_ request: SecretRequest.ID, as state: SecretRequest.State,
                        needsManager: Bool = false) throws {
        var problem: SourceProblem? = .refused("This request is gone.")
        for feed in allFeeds() {
            let found = feed.change(where: { $0.content.secret?.id == request }) { item in
                guard case .secret(var asked) = item.content else { return }
                guard asked.state == .waiting else { problem = .refused("This was settled already."); return }
                guard !needsManager || asked.manager != nil else {
                    problem = .refused("No password manager is connected."); return
                }
                asked.state = state
                item.content = .secret(asked)
                problem = nil
            }
            if found { break }
        }
        if let problem { throw problem }
    }

    private func settle(_ offer: ConnectionOffer.ID, as state: ConnectionOffer.State) throws {
        var problem: SourceProblem? = .refused("This offer is gone.")
        for feed in allFeeds() {
            let found = feed.change(where: { $0.content.connection?.id == offer }) { item in
                guard case .connection(var offered) = item.content else { return }
                guard offered.state == .waiting else { problem = .refused("This was settled already."); return }
                offered.state = state
                item.content = .connection(offered)
                problem = nil
            }
            if found { break }
        }
        if let problem { throw problem }
    }
}

extension Item.Content {
    /// The question a piece asks under this name: its own, or its event card's.
    func question(named id: Question.ID) -> Question.ID? {
        switch self {
        case .question(let asked) where asked.id == id: id
        case .event(let card) where card.question == id: id
        default: nil
        }
    }

    public var secret: SecretRequest? {
        if case .secret(let asked) = self { asked } else { nil }
    }

    public var connection: ConnectionOffer? {
        if case .connection(let offered) = self { offered } else { nil }
    }
}

/// A value many listeners follow: each gets what it is now, then every change.
final class Broadcast<Value: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Value
    private var listeners: [UUID: AsyncStream<Value>.Continuation] = [:]

    init(_ value: Value) {
        self.value = value
    }

    var current: Value { lock.withLock { value } }

    func stream() -> AsyncStream<Value> {
        AsyncStream { continuation in
            let id = UUID()
            lock.withLock {
                continuation.yield(value)
                listeners[id] = continuation
            }
            continuation.onTermination = { [weak self] _ in
                self?.forget(id)
            }
        }
    }

    func set(_ new: Value) {
        change { $0 = new }
    }

    @discardableResult
    func change<Result>(_ body: (inout Value) -> Result) -> Result {
        lock.withLock {
            let result = body(&value)
            for listener in listeners.values { listener.yield(value) }
            return result
        }
    }

    private func forget(_ id: UUID) {
        lock.withLock { _ = listeners.removeValue(forKey: id) }
    }
}

/// One conversation's pieces, and whoever listens to them.
final class Feed: @unchecked Sendable {
    private let lock = NSLock()
    private var items: [Item]
    private var answering = false
    private var listeners: [UUID: AsyncStream<ConversationChange>.Continuation] = [:]

    init(_ items: [Item]) {
        self.items = items
    }

    var all: [Item] { lock.withLock { items } }

    func stream() -> AsyncStream<ConversationChange> {
        AsyncStream { continuation in
            let id = UUID()
            lock.withLock {
                continuation.yield(.all(items))
                if answering { continuation.yield(.answering(true)) }
                listeners[id] = continuation
            }
            continuation.onTermination = { [weak self] _ in
                self?.forget(id)
            }
        }
    }

    func add(_ new: [Item]) {
        lock.withLock {
            items.append(contentsOf: new)
            tell(.added(new))
        }
    }

    /// Change the first piece that matches. Says whether one did. The piece is told on only
    /// when the change made it different.
    @discardableResult
    func change(where matches: (Item) -> Bool, _ body: (inout Item) -> Void) -> Bool {
        lock.withLock {
            guard let index = items.firstIndex(where: matches) else { return false }
            let before = items[index]
            body(&items[index])
            if items[index] != before { tell(.changed(items[index])) }
            return true
        }
    }

    func setAnswering(_ on: Bool) {
        lock.withLock {
            guard answering != on else { return }
            answering = on
            tell(.answering(on))
        }
    }

    private func tell(_ change: ConversationChange) {
        for listener in listeners.values { listener.yield(change) }
    }

    private func forget(_ id: UUID) {
        lock.withLock { _ = listeners.removeValue(forKey: id) }
    }
}
