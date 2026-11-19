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

    /// A message sent from this device whose delivery failed: kept visible in its conversation
    /// with individual retry and dismiss actions.
    public struct FailedSend: Equatable, Identifiable, Sendable {
        public var id: Item.ID
        public var submissionID: UUID
        public var chat: Chat
        public var reason: String
    }

    /// Which role a model choice applies to when picked independently in the assistant view.
    public enum ModelRole: Hashable, Sendable {
        case mainThread
        case subAgentTask

        public var workKinds: [WorkKind] {
            switch self {
            case .mainThread: [.request]
            case .subAgentTask: [.task, .subtask]
            }
        }
    }

    /// Failed optimistic sends by their optimistic item id.
    public private(set) var failedSends: [Item.ID: FailedSend] = [:]

    /// The model picked independently for sub-agents and background tasks, when set.
    public private(set) var subAgentModel: String?

    /// Returns the failed send state for an optimistic item, when its delivery failed.
    public func failedSend(for item: Item.ID) -> FailedSend? {
        failedSends[item]
    }

    /// Cancels active message-history stepping in the composer field before a window cancel
    /// shortcut closes a side panel or stage.
    @ObservationIgnored var cancelFieldStepping: (@MainActor () -> Bool)?

    @ObservationIgnored public let source: any AssistantSource
    /// What the window showed the last time it was open: read where the app opens, so that its
    /// first frame holds it, and written as the window changes (`SnapshotCache`).
    @ObservationIgnored private let snapshots: SnapshotCache
    /// Whether what stands was read from the snapshot of an earlier opening rather than from the
    /// source, and nothing of the source has come to take its place yet.
    @ObservationIgnored private var standsFromSnapshot = false
    /// Whose the snapshot's pieces are: where the source says this device is somebody else's,
    /// what was kept of another person is put away rather than shown.
    @ObservationIgnored private var snapshotPerson: Person.ID?
    /// What the person sent and the window shows before the source's own copy of it comes, by
    /// the piece each stands as (`Sent`).
    @ObservationIgnored private var sent: [Chat: [Sent]] = [:]
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
    public internal(set) var subAgents: [SubAgent] = []
    public internal(set) var tasks: [AssistantTask] = []
    public internal(set) var automations: [Automation] = []
    public private(set) var supervision = Supervision()
    public internal(set) var conversations: [Chat: Conversation] = [:]
    public internal(set) var models: [ModelChoice] = []
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
    /// Whether the window rests on what was written down of an earlier opening while a try at
    /// this device's Tilo is still in work: a person who had an assistant saw it a moment ago, and
    /// an opening that found nothing where it looked is not yet no assistant at all. Every step of
    /// the first start says so until a try ends. A try that ended with the assistant gone is read
    /// at once, and only where the window showed an assistant of its own a moment ago.
    private var waitsForFirstTry: Bool {
        guard let person, reach == .unknown else { return false }
        return switch setup {
        case .unknown, .findingAssistant, .makingAssistant, .makingWorkspace: true
        case .noAssistant: standsFromSnapshot && snapshotPerson == person.id
        case .needsProvider, .needsModel, .ready: false
        }
    }
    /// From the app's opening until the main chat stands: its page, its session, the look for
    /// the assistant and the reading of what answers for it. The window says one sentence
    /// through all of it and names no step.
    public var isStarting: Bool {
        reach == .unknown || setup == .findingAssistant || setup == .makingWorkspace || waitsForFirstTry
    }
    /// What Tilo said before it asked whether to pair this device, kept while its code is compared.
    public private(set) var pairingSaid: [String] = []
    /// A pairing link handed to the app from outside before it was known whether this device
    /// stands at its way in: the app was started by the address, or its page is still on its
    /// way in. It is taken once the device stands at its way in, and dropped once it is in.
    @ObservationIgnored private var handed: String?
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
    /// The live views opened in the Mac Split Studio stage or the iPhone's Now strip, newest last.
    /// They stay while the stage is put away, so that a view put away opens again by one press on
    /// its tab, as it stands now and with no word to the assistant and no second read of it.
    public var openLiveTabs: [LiveThing] = []
    /// The live view the Mac Split Studio stage, or the iPhone's full-screen sheet, shows now.
    /// Nil: the stage is put away, and its tabs stand ready to open a view without asking again.
    public var stagedLive: LiveThing? {
        didSet {
            if let old = oldValue, old.id != stagedLive?.id {
                unstage(old.id)
            }
            if let thing = stagedLive {
                if !openLiveTabs.contains(where: { $0.id == thing.id }) {
                    openLiveTabs.append(thing)
                }
                if place != .chat(thing.chat) {
                    open(thing.chat)
                }
            }
        }
    }

    /// Put the stage away, keeping what was opened in it: the bar's tabs stand, and a press on one
    /// opens its view again in the place it stood, as it stands now.
    public func putAwayStage() {
        stagedLive = nil
    }

    /// The tabs the stage shows, all in the conversation it is beside: the ones opened elsewhere
    /// are that conversation's, and are not drawn here.
    public func openLiveTabs(in chat: Chat) -> [LiveThing] {
        openLiveTabs.filter { $0.chat == chat }
    }

    /// Show a live view in the stage, or in the full-screen sheet on an iPhone: the one press a
    /// tab, a card's head and a strip's tab are. Showing the one that already stands changes
    /// nothing, and showing it puts the stage in the conversation it belongs to.
    public func show(_ thing: LiveThing) {
        guard stagedLive?.id != thing.id else { return }
        stagedLive = thing
    }

    /// Open the next of a conversation's live views, the one after the stage's and around to the
    /// first: the key for looking through what a conversation shared, with the stage put away too.
    /// Nothing where the conversation shared no view.
    public func showNextLive(in chat: Chat) {
        let list = liveThings(in: chat)
        guard !list.isEmpty else { return }
        guard let stage = stagedLive, let now = list.firstIndex(where: { $0.id == stage.id }) else {
            stagedLive = list[0]
            return
        }
        stagedLive = list[(now + 1) % list.count]
    }

    /// A live view read anew: every tab that holds it holds what it says now, so that a view put
    /// away that comes again says what it is called now, not what it was called when it was opened.
    public func freshen(_ thing: LiveThing) {
        if let index = openLiveTabs.firstIndex(where: { $0.id == thing.id }) { openLiveTabs[index] = thing }
        if stagedLive?.id == thing.id { stagedLive = thing }
    }

    /// A live view nothing shows any more, told to the source: a stage that was put away, or came
    /// to show another thing. The page keeps it alive in this app's one process, so a view nobody sees is
    /// a thing still running. The tab it was stands all the same: a thing put away is opened again
    /// from its tab, and its page mounts anew.
    public func unstage(_ thing: LiveThing.ID) {
        guard stagedLive?.id != thing, let stops = stopsLive else { return }
        Task { await stops(thing) }
    }

    /// Close a single tab in the Split Studio tab bar; activates an adjacent tab if one remains.
    public func closeLiveTab(_ id: LiveThing.ID) {
        guard let index = openLiveTabs.firstIndex(where: { $0.id == id }) else { return }
        openLiveTabs.remove(at: index)
        if stagedLive?.id == id {
            if openLiveTabs.isEmpty {
                stagedLive = nil
            } else {
                stagedLive = openLiveTabs[min(index, openLiveTabs.count - 1)]
            }
        }
    }
    #if os(macOS)
    /// Where Copy in a piece's menu puts the words: the system's pasteboard. A test hands it one
    /// of its own.
    @ObservationIgnored public var pasteboard = NSPasteboard.general
    #else
    /// Where Copy in a piece's menu puts the words: the system's pasteboard. Whoever checks Copy
    /// hands it one of its own.
    @ObservationIgnored public var pasteboard = UIPasteboard.general
    #endif
    /// Tells the source that a live view is not to be shown any more, where the app's views live in
    /// Tilo's page (`Source`). Nil where they do not: a picture's or a test's.
    @ObservationIgnored public var stopsLive: (@MainActor (LiveThing.ID) async -> Void)?
    /// A piece the conversation should bring into view.
    public var shownItem: Item.ID?
    /// Counts up each time the message field should take the caret.
    public private(set) var fieldCalls = 0
    /// The replies that stand open under a task's line, by the piece the line stands before.
    public private(set) var openReplies: [Item.ID: [Item]] = [:]
    /// The task's line whose replies are being read, the conversation they stand in, and the
    /// replies themselves once they came. The main chat shows them in place; a sub-agent's
    /// window, or the assistant view, opens them where a piece is shown whole.
    public var readingReplies: Replies?

    /// A task's replies, being read under its line.
    public struct Replies: Equatable, Identifiable, Sendable {
        public var line: Item.ID
        public var task: AssistantTask.ID
        public var chat: Chat
        /// Nil while they are being read: the line says so until they come.
        public var items: [Item]?

        public var id: Item.ID { line }
    }
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

    /// The window reads what it showed the last time as it opens, so that its first frame holds
    /// every piece, sub-agent, task and automation rather than waiting for the source's reading.
    public init(source: any AssistantSource) {
        self.source = source
        snapshots = SnapshotCache()
        take(snapshots)
    }

    /// A window whose snapshot is kept in a place of the check's own: nothing is written to this
    /// Mac's caches, and a check may open a window twice and see what the first one kept.
    public init(source: any AssistantSource, snapshots: SnapshotCache) {
        self.source = source
        self.snapshots = snapshots
        take(snapshots)
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
        follow(source.setup()) { model, setup in
            model.setup = setup
            model.keepSnapshot()
        }
        follow(source.reach()) { model, reach in
            model.take(reach)
            model.keepSnapshot()
        }
        follow(source.pairingEnded()) { $0.take(ended: $1) }
        follow(source.pairingEndedDetails()) { $0.endedDetails = $1 }
        follow(source.subAgents()) { model, agents in
            model.subAgents = agents
            for agent in agents { model.listen(.subAgent(agent.id)) }
            model.keepSnapshot()
        }
        follow(source.tasks()) { model, tasks in
            model.tasks = tasks
            model.keepSnapshot()
        }
        follow(source.automations()) { model, automations in
            model.automations = automations
            model.keepSnapshot()
        }
        follow(source.supervision()) { $0.supervision = $1 }
        follow(source.sinceLeft()) { $0.sinceLeft = $1 }
        follow(source.joinStops()) { $0.joinStops = $1 }
        follow(source.joinApproved()) { $0.joinApproved = $1 }
        follow(source.pageLook()) { model, look in
            model.look = look
            model.keepSnapshot()
        }
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
            var conversation = conversations[chat] ?? Conversation()
            conversation.apply(change)
            conversations[chat] = conversation
        }
        keepSnapshot()
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
        case .signedOut: pairingSaid = []
        case .signedIn(let person):
            pairingSaid = []
            // Another person's assistant is not shown the pieces of the one before.
            if let kept = snapshotPerson, kept != person.id { putAwaySnapshot() }
        case .reset:
            // What the window showed of the workspace that is gone is put away with it. What
            // the person wrote and did not send stays in its field for when they are back.
            pairingSaid = []
            stagedLive = nil
            openLiveTabs = []
            place = .chat(.main)
            keyStep = nil
            openReplies = [:]
            readingReplies = nil
        case .pairing, .confirming: break
        }
        session = stands
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
        // A try has ended: what stands from the snapshot is what the source read, and what the
        // snapshot says stands in its place no longer.
        if reach != .unknown, person != nil, setup != .unknown { standsFromSnapshot = false }
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
        // What the source says of the conversation takes the place of what was kept, and takes
        // over the pieces the person sent that it now holds.
        let sentBefore = sent[chat] ?? []
        var sending = sentBefore
        var matchedServerIDs: Set<Item.ID> = []
        conversation.apply(change)
        switch change {
        case .all:
            standsFromSnapshot = false
            let result = Sent.reconcile(sending, against: conversation.items)
            sending = result.remaining
            matchedServerIDs = result.matchedServerIDs
            if !sending.isEmpty { conversation.apply(.added(sending.map(\.item))) }
        case .added:
            if !sending.isEmpty {
                let result = Sent.reconcile(sending, against: conversation.items)
                sending = result.remaining
                matchedServerIDs = result.matchedServerIDs
                for localID in result.matchedLocalIDs {
                    conversation.apply(.removed(localID))
                }
            }
        default:
            break
        }
        sent[chat] = sending.isEmpty ? nil : sending
        conversations[chat] = conversation
        // What was kept of the main chat waits until a reply that is being written is whole: a
        // reply growing letter by letter is no reason for a file to be written.
        if chat == .main {
            switch change {
            case .all(let all) where all.contains(where: { $0.message?.turn != nil }): break
            case .changed(let piece) where piece.message?.turn != nil: break
            case .all, .added, .changed, .removed: keepSnapshot()
            default: break
            }
        }
        // A turn that begins, or anything that comes after the person's message, ends the wait for one.
        switch change {
        case .answering(true): turnBegan(in: chat)
        case .added(let new) where !new.allSatisfy(AssistantModel.isOwnMessage): turnBegan(in: chat)
        case .changed(let piece) where !AssistantModel.isOwnMessage(piece): turnBegan(in: chat)
        default: break
        }
        guard known else { return }
        // What a live view says of itself is read anew with its piece: its tab, and the stage it
        // stands in, call it what it says now.
        for thing in change.live { freshen(thing) }
        switch change {
        // What the person sent stands here before the source's own copy of it comes: the copy is
        // the news, and the copy that takes the place of a message sent from this device is not.
        case .added(let new):
            for item in new where !item.id.value.hasPrefix(Sent.prefix) && !matchedServerIDs.contains(item.id) {
                told?(item, chat)
            }
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

    /// Sub-agents that are currently working or waiting for input.
    public var activeSubAgents: [SubAgent] {
        subAgents.filter { $0.state == .working || $0.state == .needsYou }
    }

    /// Active sub-agents ordered with those needing the person's input first, then those working.
    public var prioritizedActiveSubAgents: [SubAgent] {
        let needsYou = activeSubAgents.filter { $0.state == .needsYou }
        let working = activeSubAgents.filter { $0.state == .working }
        return needsYou + working
    }

    /// How many active sub-agents currently need the person's input.
    public var subAgentsNeedingYou: Int {
        subAgents.filter { $0.state == .needsYou }.count
    }

    /// The next active sub-agent to jump to when cycling through parallel sub-agents.
    public func nextActiveSubAgent(after current: SubAgent.ID? = nil) -> SubAgent? {
        let list = prioritizedActiveSubAgents
        guard !list.isEmpty else { return nil }
        guard let current, let index = list.firstIndex(where: { $0.id == current }) else {
            return list.first
        }
        guard list.count > 1 else { return nil }
        return list[(index + 1) % list.count]
    }

    /// Live mid-flight steering guidance shown above the composer when viewing an active sub-agent.
    public func steeringGuidance(for chat: Chat) -> String? {
        guard case .subAgent(let id) = chat, let agent = subAgent(id) else { return nil }
        switch agent.state {
        case .working:
            return "Steer this sub-agent while it works…"
        case .needsYou:
            return "Needs your input — reply below to guide this sub-agent…"
        case .done, .failed, .stopped:
            return nil
        }
    }

    /// Whether sending a note in `chat` reaches an active sub-agent mid-flight without stopping it.
    public func canSteerMidFlight(in chat: Chat) -> Bool {
        guard case .subAgent(let id) = chat, let agent = subAgent(id) else { return false }
        return agent.state == .working || agent.state == .needsYou
    }

    /// Give keyboard focus to the message composer.
    public func focusComposer() {
        fieldCalls += 1
    }

    /// Send a mid-flight steering note to a sub-agent without stopping or cancelling its work.
    public func steer(subAgent id: SubAgent.ID, with note: String) {
        let trimmed = note.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        let chat = Chat.subAgent(id)
        var draft = drafts[chat] ?? Draft(text: "")
        draft.text = note
        drafts[chat] = draft
        send(in: chat)
    }

    /// Build the Code Factory active sub-agent strip plan for the phone `Now` strip.
    public func subAgentStripPlan(in chat: Chat) -> SubAgentStripPlan {
        let current: SubAgent.ID? = if case .subAgent(let id) = chat { id } else { nil }
        let ordered = prioritizedActiveSubAgents
        let needsCount = ordered.filter { $0.state == .needsYou }.count
        let workingCount = ordered.filter { $0.state == .working }.count
        let groupPill: SubAgentStripPlan.GroupPill? = if ordered.count >= 2,
            let target = nextActiveSubAgent(after: current) ?? ordered.first
        {
            SubAgentStripPlan.GroupPill(
                count: ordered.count,
                needsYouCount: needsCount,
                workingCount: workingCount,
                target: target.id
            )
        } else {
            nil
        }
        let entries = ordered.map { agent in
            SubAgentStripPlan.Entry(
                agent: agent,
                badge: SubAgentStatusBadge(state: agent.state),
                isWriting: isWriting(.subAgent(agent.id)),
                isSelected: current == agent.id
            )
        }
        return SubAgentStripPlan(groupPill: groupPill, entries: entries)
    }

    /// Every live artifact shared in `chat`, newest first, deduplicated by id.
    public func liveThings(in chat: Chat) -> [LiveThing] {
        var seen: Set<LiveThing.ID> = []
        var result: [LiveThing] = []
        for item in conversation(chat).items.reversed() {
            if case .message(let message) = item.content {
                for thing in message.live.reversed() where thing.chat == chat && seen.insert(thing.id).inserted {
                    result.append(thing)
                }
            }
        }
        return result
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

    /// Answer one piece of the open conversation: it stands above the field, which is called,
    /// and the message then sent says which piece it answers. `part` is the part of its words
    /// that is answered, where the person marked one. `chat` is the conversation the piece
    /// stands in, where the person answered it from somewhere else: opening a piece from the
    /// Tasks panel answers it in the main chat, whatever the middle of the window shows.
    public func reply(to piece: Item.ID, saying words: String, part: String? = nil, in chat: Chat? = nil) {
        if let chat { open(chat) }
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

    /// Send what is written in a conversation's field. Each optimistic item carries a stable
    /// client submission id so duplicate short replies and attachment-only messages never collide.
    /// If delivery fails, the optimistic item stays visible in the thread with individual retry
    /// and dismiss actions rather than clobbering the composer draft.
    public func send(in chat: Chat) {
        guard let draft = drafts[chat], !draft.isEmpty else { return }
        drafts[chat] = nil
        turnBegan(in: chat)
        let baseline = serverItemIDs(in: chat)
        let sending = Sent(draft, in: chat, baselineIDs: baseline)
        sent[chat, default: []].append(sending)
        conversations[chat, default: Conversation()].apply(.added([sending.item]))
        keepSnapshot()
        dispatchSend(submissionID: sending.submissionID, in: chat)
    }

    /// Retry a failed optimistic message using its same stable client submission id.
    public func retrySend(_ item: Item.ID, in chat: Chat) {
        guard var list = sent[chat], let index = list.firstIndex(where: { $0.item.id == item && $0.failedReason != nil }) else {
            return
        }
        list[index].failedReason = nil
        list[index].said = Date()
        list[index].baselineIDs = serverItemIDs(in: chat)
        let submissionID = list[index].submissionID
        sent[chat] = list
        failedSends[item] = nil
        dispatchSend(submissionID: submissionID, in: chat)
    }

    /// Dismiss a failed optimistic message from the thread and release any attached files.
    public func dismissFailedSend(_ item: Item.ID, in chat: Chat) {
        failedSends[item] = nil
        if var list = sent[chat], let index = list.firstIndex(where: { $0.item.id == item }) {
            let removed = list.remove(at: index)
            for file in removed.draft.files { file.url.stopAccessingSecurityScopedResource() }
            sent[chat] = list.isEmpty ? nil : list
        }
        conversations[chat]?.apply(.removed(item))
        keepSnapshot()
    }

    private func serverItemIDs(in chat: Chat) -> Set<Item.ID> {
        Set(conversation(chat).items.map(\.id).filter { !$0.value.hasPrefix(Sent.prefix) })
    }

    private func dispatchSend(submissionID: UUID, in chat: Chat) {
        guard let sending = sent[chat]?.first(where: { $0.submissionID == submissionID }) else { return }
        let draft = sending.draft
        let itemID = sending.item.id
        Task {
            let went = await self.call { try await self.source.send(draft, to: chat) }
            if !went {
                let reason = self.problem ?? AssistantWords.unreachable.said
                if var list = self.sent[chat], let index = list.firstIndex(where: { $0.submissionID == submissionID }) {
                    list[index].failedReason = reason
                    self.sent[chat] = list
                    self.failedSends[itemID] = FailedSend(id: itemID, submissionID: submissionID, chat: chat, reason: reason)
                }
                self.keepSnapshot()
                return
            }
            // Release security-scoped file access once the message has been accepted.
            for file in draft.files { file.url.stopAccessingSecurityScopedResource() }
            if self.waitsForTurn(chat) { self.awaitTurn(in: chat) }
            // Reconcile once the source has had time to echo the message.
            try? await Task.sleep(for: Sent.patience)
            self.reconcilePending(in: chat)
        }
    }

    /// Reconcile any optimistic submissions in `chat` whose server copy is already present.
    private func reconcilePending(in chat: Chat) {
        guard let held = sent[chat], !held.isEmpty, var conversation = conversations[chat] else { return }
        let result = Sent.reconcile(held, against: conversation.items)
        guard !result.matchedLocalIDs.isEmpty else { return }
        for localID in result.matchedLocalIDs {
            conversation.apply(.removed(localID))
        }
        sent[chat] = result.remaining.isEmpty ? nil : result.remaining
        conversations[chat] = conversation
        keepSnapshot()
    }

    /// What the person sent, on this device, as it stands in a conversation until the source's own
    /// copy of it comes. Identified by a stable client submission id and reconciled one-to-one
    /// against non-baseline server messages so identical short replies and attachment-only
    /// messages never collide.
    private struct Sent {
        /// How long a source is given to hand the message over before pending items are checked.
        static let patience = Duration.seconds(4)

        /// Stable client submission id preserved across retries.
        let submissionID: UUID
        /// Original draft preserved so failed items can be retried individually.
        let draft: Draft
        /// The words as the source was given them.
        let text: String
        /// Attached file names in order so attachment-only messages never collide.
        let fileNames: [String]
        /// The piece each part of them answers, where the person answered one.
        let answers: Answered?
        /// The piece the window shows until the source's copy comes: a message by this device,
        /// with an id no source writes.
        let item: Item
        /// When this submission was dispatched, updated on retry.
        var said: Date
        /// Server item ids already present before this submission was dispatched, so an earlier
        /// identical message in history can never consume this submission.
        var baselineIDs: Set<Item.ID>
        /// Failure reason when delivery failed; failed items remain visible until retried or dismissed.
        var failedReason: String?

        init(_ draft: Draft, in chat: Chat, baselineIDs: Set<Item.ID>) {
            let said = Date()
            let submissionID = UUID()
            self.submissionID = submissionID
            self.draft = draft
            self.text = draft.text
            self.fileNames = draft.files.map(\.name)
            self.answers = draft.answers
            self.said = said
            self.baselineIDs = baselineIDs
            let optimisticFiles = draft.files.enumerated().map { index, file in
                Attachment(id: .init("\(Sent.prefix)\(submissionID.uuidString)-file-\(index)"), name: file.name, type: "")
            }
            let message = Message(.person, draft.text, attachments: optimisticFiles, answers: draft.answers)
            item = Item(id: Item.ID("\(Sent.prefix)\(submissionID.uuidString)"), at: said, .message(message))
        }

        /// The mark of a piece only this device holds: no source writes a piece named like one.
        static let prefix = "local-"

        /// Whether a server piece can reconcile this submission.
        func matches(_ piece: Item) -> Bool {
            guard !piece.id.value.hasPrefix(Sent.prefix) else { return false }
            if piece.id == item.id || piece.id.value == submissionID.uuidString { return true }
            guard !baselineIDs.contains(piece.id) else { return false }
            guard case .message(let message) = piece.content, message.author == .person else { return false }
            return message.text == text
                && message.answers?.piece == answers?.piece
                && message.attachments.map(\.name) == fileNames
        }

        struct Reconciliation {
            var remaining: [Sent]
            var matchedLocalIDs: Set<Item.ID>
            var matchedServerIDs: Set<Item.ID>
        }

        /// One-to-one FIFO reconciliation of pending optimistic submissions against server pieces.
        /// Failed submissions are never dropped automatically.
        static func reconcile(_ waiting: [Sent], against pieces: [Item]) -> Reconciliation {
            let serverPieces = pieces.filter { !$0.id.value.hasPrefix(Sent.prefix) }
            var consumed: Set<Item.ID> = []
            var remaining: [Sent] = []
            var matchedLocalIDs: Set<Item.ID> = []
            for sending in waiting {
                if sending.failedReason != nil {
                    remaining.append(sending)
                    continue
                }
                if let match = serverPieces.first(where: { !consumed.contains($0.id) && sending.matches($0) }) {
                    consumed.insert(match.id)
                    matchedLocalIDs.insert(sending.item.id)
                } else {
                    remaining.append(sending)
                }
            }
            return Reconciliation(remaining: remaining, matchedLocalIDs: matchedLocalIDs, matchedServerIDs: consumed)
        }
    }

    // MARK: What the window showed the last time it was open

    /// Read what the window showed the last time, before the source has said anything: the main
    /// chat's pieces, the sub-agents, the tasks, the automations, the look, and the models. The
    /// very first frame of the app holds all of them, and the source's reading takes their places
    /// as it comes. Nothing is read where the last window was somebody else's than this device's.
    private func take(_ snapshots: SnapshotCache) {
        guard let kept = snapshots.read(), let person = kept.person else { return }
        snapshotPerson = person
        standsFromSnapshot = true
        // The person stands where they stood: their session is read again by the source, and what
        // was kept only says whose the pieces are.
        if case .signedOut = session { return }
        session = .signedIn(Person(id: person, name: "", workspace: ""))
        var conversation = Conversation()
        conversation.apply(.all(kept.pieces + (sent[.main] ?? []).map(\.item)))
        conversations[.main] = conversation
        subAgents = kept.keptSubAgents
        tasks = kept.tasks
        automations = kept.keptAutomations
        look = kept.keptLook
        models = kept.keptModels
    }

    /// Put away what was kept of somebody else's window: their pieces, their sub-agents, their
    /// tasks, and their automations.
    private func putAwaySnapshot() {
        guard standsFromSnapshot || snapshotPerson != nil else { return }
        standsFromSnapshot = false
        snapshotPerson = nil
        conversations[.main] = Conversation()
        subAgents = []
        tasks = []
        automations = []
        releasePendingFiles()
        sent = [:]
        failedSends = [:]
        snapshots.forget()
    }

    private func releasePendingFiles() {
        for list in sent.values {
            for sending in list {
                for file in sending.draft.files { file.url.stopAccessingSecurityScopedResource() }
            }
        }
    }

    /// Write what the window shows now, a moment from now (`SnapshotCache`), so that the next
    /// opening shows it at once. Nothing is written of a window that is nobody's: a device signed
    /// out keeps nothing of what it showed.
    private func keepSnapshot() {
        // A window whose source this device has not got in is nobody's to keep: an app signed out
        // leaves nothing of what it showed.
        guard let person, case .signedIn = session else { return }
        snapshotPerson = person.id
        let pieces = conversation(.main).items.filter { !$0.id.value.hasPrefix(Sent.prefix) }
        let agents = subAgents
        let work = tasks
        let meetings = automations
        let look = self.look
        let models = self.models
        snapshots.keep {
            SnapshotCache.Snapshot(pieces: pieces, subAgents: agents, works: work, automations: meetings,
                                   look: look, models: models, person: person.id)
        }
    }

    // MARK: One press out of what stopped

    /// Whether their Tilo answers and does not let this device in. The window then offers the way
    /// in, and no other way out, where it says what stopped.
    public var needsWayIn: Bool { joinStops && reach == .unreachable }

    /// Whether the main chat stands, and what last stopped can be mended by one press in it: the
    /// provider step, the choice of a model, or Try again where their assistant could not be made.
    /// Nil everywhere else, so that a person is never offered a press that leads nowhere.
    public var onePressSetup: Setup? {
        guard person != nil, reach == .reachable else { return nil }
        switch setup {
        case .noAssistant, .needsProvider, .needsModel: return setup
        case .unknown, .ready: return keyStep != nil ? setup : nil
        case .findingAssistant, .makingWorkspace, .makingAssistant: return nil
        }
    }

    /// A try at the person's Tilo that ended without getting in, asked for again at once: their
    /// page is asked for as the device the store holds, and none of what was typed waits for it.
    public func tryAgain(whereItStopped itStopped: Bool = false) {
        guard reach == .unreachable else { return }
        Task {
            if itStopped {
                await self.source.goOn()
            } else {
                await self.source.startOver()
            }
        }
    }

    /// Go to what one press mends, and make the press: the main chat, where the step stands or
    /// Try again does. What stands nowhere yet is read first and loaded, so that the press is the
    /// one that shows the step, not a step already gone.
    public func mend() {
        guard let setup = onePressSetup else { return }
        open(.main)
        switch setup {
        case .noAssistant:
            makeAssistant()
        case .needsProvider, .needsModel:
            // A step that stands already is the one pressed at; otherwise the models are read,
            // and the main chat's step turns up with them.
            if keyStep == nil { loadModels() }
        case .unknown, .ready, .findingAssistant, .makingWorkspace, .makingAssistant:
            break
        }
    }

    /// A called-out problem put away, once the person read it: a failure stands under the
    /// conversation until it is read, and never for ever.
    public func dismissProblem() {
        problem = nil
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
        guard !held.isAnswering, let newest = held.items.last, failedSends[newest.id] == nil else { return false }
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

    /// Whether a task's line has its replies open under it: in the main chat they stand there, and
    /// anywhere else the conversation they were read from stands over the piece.
    public func repliesStandOpen(_ line: Item.ID) -> Bool {
        openReplies[line] != nil || readingReplies?.line == line
    }

    /// Open a task's replies under its line, or put them away again. The main chat draws them
    /// under the line itself; elsewhere the conversation says them over the piece, where the
    /// line does not stand. A press that could not read them says so, and nothing is left open.
    public func toggleReplies(of task: AssistantTask.ID, under line: Item.ID, in chat: Chat) {
        if openReplies[line] != nil {
            openReplies[line] = nil
            if readingReplies?.line == line { readingReplies = nil }
            return
        }
        if readingReplies?.line == line {
            readingReplies = nil
            return
        }
        if chat != .main { readingReplies = Replies(line: line, task: task, chat: chat, items: nil) }
        Task {
            var found: [Item] = []
            guard await self.call({ found = try await self.source.replies(of: task, in: chat) }) else {
                if self.readingReplies?.line == line { self.readingReplies = nil }
                return
            }
            self.openReplies[line] = found
            if self.readingReplies?.line == line { self.readingReplies?.items = found }
        }
    }

    /// Put away the replies being read over a piece.
    public func stopReadingReplies() {
        readingReplies = nil
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
            var listed: [ProviderChoice] = []
            let read = await self.call {
                found = try await self.source.models()
                listed = try await self.source.providers()
            }
            guard read else { return }
            self.models = found
            self.providers = listed
            self.keepSnapshot()
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
        choose(model, for: [.request, .task])
    }

    /// Choose a model independently for the main thread or for sub-agents and background tasks.
    public func choose(_ model: ModelChoice.ID, for role: ModelRole) {
        let chosenName = models.first { $0.id == model }?.name
        Task {
            let ok = await self.call { try await self.source.choose(model, for: role.workKinds) }
            if ok, role == .subAgentTask {
                self.subAgentModel = chosenName
            }
        }
    }

    /// Choose a model for specific kinds of work.
    public func choose(_ model: ModelChoice.ID, for work: [WorkKind]) {
        Task {
            await self.call { try await self.source.choose(model, for: work) }
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
        keepSnapshot()
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

    /// Sign this device out, and forget what it showed: another person, or this one pairing it
    /// afresh, is never shown the pieces of the window that was. What stands is taken down at once,
    /// so that no frame of the way in holds it.
    public func signOut() {
        snapshots.forget()
        standsFromSnapshot = false
        snapshotPerson = nil
        conversations = [:]
        releasePendingFiles()
        sent = [:]
        failedSends = [:]
        subAgentModel = nil
        subAgents = []
        tasks = []
        automations = []
        openReplies = [:]
        readingReplies = nil
        stagedLive = nil
        openLiveTabs = []
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

extension LiveThing {
    /// What its tab, its card's head and the Now strip call it: its page writes its name as a
    /// page writes words, Markdown and all, and nothing that names a view shows the signs of it.
    /// Its name as its page gave it stays for its source to find it by.
    public var called: String { Markdown.plainTitle(title) }
}

/// Compact classification of where a sub-agent works, shown in its header bar.
public enum SubAgentLocation: Equatable, Sendable {
    case server
    case macFolder
    case codingSandbox

    public var label: String {
        switch self {
        case .server: "Server"
        case .macFolder: "Mac folder"
        case .codingSandbox: "Coding sandbox"
        }
    }

    /// Distill a sub-agent's `runsOn` description into Server, Mac folder, or Coding sandbox.
    public static func classify(_ runsOn: String) -> SubAgentLocation {
        let lower = runsOn.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if lower.isEmpty { return .server }
        let words = lower.split { !$0.isLetter && !$0.isNumber }
        if words.contains("mac") || words.contains("macbook") || words.contains("macos")
            || words.contains("folder") || words.contains("local")
            || lower.contains("/users/") || lower.hasPrefix("~/")
        {
            return .macFolder
        }
        if lower.contains("sandbox") || lower.contains("coding") || lower.contains("claude code")
            || lower.contains("codex") || lower.contains("workspace's machine")
            || lower.contains("workspace’s machine") || lower.contains("container") || words.contains("vm")
        {
            return .codingSandbox
        }
        return .server
    }
}

/// Live status badge copy and action availability for a sub-agent's state.
public struct SubAgentStatusBadge: Equatable, Sendable {
    public let state: SubAgent.State
    public let label: String
    public let isActive: Bool
    public let canStop: Bool
    public let needsAttention: Bool

    public init(state: SubAgent.State) {
        self.state = state
        switch state {
        case .working:
            label = "Working"
            isActive = true
            canStop = true
            needsAttention = false
        case .needsYou:
            label = "Needs you"
            isActive = true
            canStop = true
            needsAttention = true
        case .done:
            label = "Done"
            isActive = false
            canStop = false
            needsAttention = false
        case .failed:
            label = "Failed"
            isActive = false
            canStop = false
            needsAttention = false
        case .stopped:
            label = "Stopped"
            isActive = false
            canStop = false
            needsAttention = false
        }
    }
}

/// Cross-platform layout plan for active sub-agent pills inside the phone `Now` strip.
public struct SubAgentStripPlan: Equatable, Sendable {
    public struct GroupPill: Equatable, Sendable {
        public let count: Int
        public let needsYouCount: Int
        public let workingCount: Int
        public let target: SubAgent.ID

        public var label: String {
            AssistantWords.subAgentsCount.said(["count": String(count)])
        }

        public var detailLabel: String? {
            guard needsYouCount > 0 else { return nil }
            return "\(needsYouCount) \(AssistantWords.needsYou.said)"
        }

        public var accessibilityLabel: String {
            if needsYouCount > 0 {
                return "\(label), \(needsYouCount) \(AssistantWords.needsYou.said)"
            }
            return label
        }
    }

    public struct Entry: Identifiable, Equatable, Sendable {
        public let agent: SubAgent
        public let badge: SubAgentStatusBadge
        public let isWriting: Bool
        public let isSelected: Bool

        public var id: SubAgent.ID { agent.id }

        public var accessibilityLabel: String {
            "\(agent.title), \(badge.label), \(AssistantWords.showSubAgent.said)"
        }
    }

    public let groupPill: GroupPill?
    public let entries: [Entry]

    public var isEmpty: Bool { entries.isEmpty }
}
