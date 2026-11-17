import AssistantCore
import Foundation
import ShellCore
import SwiftUI
import TiloLive

/// `AssistantSource` on Tilo itself: the app as the person's own device, through the engine.
///
/// It answers what Tilo answers a device, and says so where Tilo does not yet: a call that
/// nothing in Tilo can take throws `SourceProblem.refused` with a sentence that is true, and a
/// read that nothing answers gives what is known (nothing) and stays silent.
///
/// - The conversation, the question that waits, sub-agents and what was learned are records
///   Chat keeps: read through the page's own mount and heard by the page's word of a change.
/// - The person's models, their vault and their events are providers of their own on their
///   host: asked through the session the page holds, as the person on this device
///   (`TiloEngine.ask`). They are asked when the device gets in and after a call of this app
///   changed them, never on a clock.
/// - A secret goes from the protected field into the one call that carries it to the host's
///   vault. The conversation is told the item's reference and nothing else.
@MainActor
public final class TiloSource: AssistantSource {
    /// Which of Chat's conversations is the main chat.
    public enum MainChat: Sendable, Equatable {
        /// The person's own conversation with the assistant they keep. It needs Chat's agents.
        case assistant
        /// One conversation by its id: for a site whose Chat keeps no agents yet, and for a check.
        case conversation(id: String, name: String)
        /// The person's conversation with one agent, named by its id: for a site whose page
        /// does not say which agents its workspace keeps, and for a check.
        case agent(id: String, name: String)
    }

    public let engine: TiloEngine
    /// Where live things stand. The app puts `engineRoom` into its root view.
    public let stage: LiveStage

    private let mainChat: MainChat
    private let defaults: UserDefaults
    private let livesInOwn: Bool
    private static let routeKey = "TiloLiveRoute"
    private static let personKey = "TiloLivePerson"

    private var person: Person?
    /// The person's identity, as Chat names the author of what they write.
    private var me: String?
    /// The agent that is the person's assistant, by its id in Chat's record of agents.
    private var assistant: String?
    private var recipient: ChatRecipient?
    /// Whether the page names a member by its kind (`{"person": …}`), and so spells the id of
    /// a conversation between members with their kinds.
    private var namesKinds = true
    /// Whether the person's own workspace was asked for since this device got in.
    private var soughtOwn = false
    /// The person's own workspace, as their server named it when it was asked for.
    private var ownRoute: String?
    /// Whether the page stands in the person's own workspace, on their server's word.
    private var standsInOwn = false
    /// Whether the person's assistant is being found right now: two at once could make two.
    private var finding = false
    /// How often this page refused to read the workspace's agents since the device got in. A
    /// page that just got in publishes them a moment later, so they are asked for again at its
    /// next words; a page that never does is left alone after a few.
    private var agentsRefused = 0
    /// What the main chat says of the person's providers, as their server last listed them.
    private var providerSetup = Setup.unknown
    /// Nor one of what was learned.
    private var keepsNothingLearned = false
    /// What was learned, by where it is kept: the person's own record in Chat (agents on their
    /// devices), and their host (the agents it runs, so the assistant). They are two lists.
    private var learnedInChat: [Supervision.Learned] = []
    private var learnedOnHost: [Supervision.Learned] = []
    /// What marks a fact the host keeps, in the name the frame hands back to forget it.
    private static let onHost = "host:"

    /// One conversation as it is held: the id Chat keeps it under, and its pieces.
    private struct Feed {
        var conversation: String?
        var held: [Item] = []
        var told = false
        /// Whether an agent is at work on a message of it.
        var answering = false
        var listeners: [UUID: AsyncStream<ConversationChange>.Continuation] = [:]
    }

    private var feeds: [Chat: Feed] = [.main: Feed()]
    private var looking = false
    private var again = false
    private var surveying = false
    private var surveyAgain: Survey?

    private let sessions = Told<Session>(.signedOut)
    private let reaches = Told<Reach>(.unreachable)
    private let agents = Told<[SubAgent]>([])
    private let taskList = Told<[AssistantTask]>([])
    private let automationList = Told<[Automation]>([])
    private let supervisionNow = Told<Supervision>(Supervision())
    private let setups = Told<Setup>(.unknown)
    private var choices: [ModelChoice] = []
    private var listening: [Task<Void, Never>] = []

    /// - Parameters:
    ///   - store: the name of the store the device's key is kept in, the same at every launch.
    ///   - livesInOwn: the assistant lives in the person's own workspace on their host: a device
    ///     that was let in elsewhere goes there, where the page can say which workspace that is.
    ///   - defaults: where the app keeps the workspace it was paired into and the person's name.
    ///   - deviceName: what this device is called where its person lets it in and lists their
    ///     devices.
    public convenience init(settings: ShellSettings, store: UUID, mainChat: MainChat = .assistant, livesInOwn: Bool = true,
                            defaults: UserDefaults = .standard, deviceName: String = "") {
        self.init(engine: TiloEngine(settings: settings, store: .kept(store), deviceName: deviceName), mainChat: mainChat,
                  livesInOwn: livesInOwn, defaults: defaults)
    }

    /// On an engine that is there already: for a check that keeps nothing.
    init(engine: TiloEngine, mainChat: MainChat, livesInOwn: Bool, defaults: UserDefaults) {
        self.engine = engine
        stage = LiveStage(engine: engine)
        self.mainChat = mainChat
        self.livesInOwn = livesInOwn
        self.defaults = defaults
        if let kept = defaults.array(forKey: TiloSource.personKey) as? [String], kept.count == 3 {
            person = Person(id: .init(kept[0]), name: kept[1], workspace: kept[2])
            me = kept[0]
        }
        sessions.set(sessionNow)
        let engine = engine
        listening = [
            Task { [weak self] in
                for await state in engine.states() { await self?.stood(state) }
            },
            Task { [weak self] in
                for await _ in engine.changes() {
                    await self?.look()
                    await self?.survey(.records)
                }
            },
        ]
        // A device that was paired before comes back as itself.
        if let route = defaults.string(forKey: TiloSource.routeKey) { try? engine.open(route: route) }
    }

    deinit {
        for task in listening { task.cancel() }
    }

    // MARK: The way in

    public nonisolated func session() -> AsyncStream<Session> { sessions.stream() }
    public nonisolated func reach() -> AsyncStream<Reach> { reaches.stream() }

    public func pair(with link: PairingLink) async throws {
        do {
            soughtOwn = false
            try engine.pair(with: link.url)
        } catch {
            throw SourceProblem.refused("This link does not lead into \(engine.settings.origin)")
        }
    }

    /// The person's answer to what the page asked before pairing: the press of the page's own
    /// button, and nothing more.
    public func answerPairing(goOn: Bool) async {
        await engine.answerPairing(goOn: goOn)
    }

    /// Forgets what the app keeps of the person and puts the page away. The device's key stays
    /// in its store until the app removes the store: the page has no way yet to forget its
    /// device, and a store in use cannot be removed.
    public func signOut() async {
        defaults.removeObject(forKey: TiloSource.routeKey)
        defaults.removeObject(forKey: TiloSource.personKey)
        person = nil
        me = nil
        assistant = nil
        recipient = nil
        soughtOwn = false
        ownRoute = nil
        standsInOwn = false
        agentsRefused = 0
        providerSetup = .unknown
        setups.set(.unknown)
        keepsNothingLearned = false
        learnedInChat = []
        learnedOnHost = []
        for chat in Array(feeds.keys) {
            feeds[chat]?.conversation = nil
            say([], in: chat)
        }
        agents.set([])
        taskList.set([])
        automationList.set([])
        supervisionNow.set(Supervision())
        choices = []
        engine.leave()
        tell()
    }

    private var sessionNow: Session {
        switch engine.state {
        case .open: person.map(Session.signedIn) ?? .pairing
        // The page shows a code at its join step: the window shows it too, to compare.
        // The page asks before it pairs: the window shows its sentences and its two ways on.
        case .asking(let ask): .asked(PairingAsk(heading: ask.heading, said: ask.said, go: ask.go, stop: ask.stop))
        case .pairing(let code): code.isEmpty ? .pairing : .confirming(code: code)
        case .arriving: person.map(Session.signedIn) ?? .pairing
        case .away, .refused, .failed: person.map(Session.signedIn) ?? .signedOut
        }
    }

    private func tell() {
        sessions.set(sessionNow)
        if case .open = engine.state { reaches.set(.reachable) } else { reaches.set(.unreachable) }
    }

    private func stood(_ state: TiloEngine.State) async {
        if case .open(let route) = state {
            // The workspace, without the app the page shows of it.
            if let workspace = EngineAddress.workspace(of: route) { defaults.set(workspace, forKey: TiloSource.routeKey) }
            await arrived(at: route)
        }
        tell()
    }

    /// The page is in: who the person is, whether this is where the assistant lives, and which
    /// conversation is the main chat.
    private func arrived(at route: String) async {
        var root: String?
        agentsRefused = 0
        if let whose = try? await engine.whose() {
            root = whose.person
            // The assistant lives in the person's own workspace. A device is let in where its
            // link leads, so it goes to the person's own once, where the page says which it is.
            if livesInOwn, !soughtOwn {
                soughtOwn = true
                ownRoute = (try? await engine.ownWorkspace())?.route
                if let ownRoute, EngineAddress.isElsewhere(route, than: ownRoute) {
                    try? engine.open(route: ownRoute)
                    return
                }
            }
        }
        standsInOwn = ownRoute.map { !EngineAddress.isElsewhere(route, than: $0) } ?? false
        let asked = try? await engine.view.callAsyncJavaScript(TiloSource.whoAndWhere, arguments: [:], in: nil, contentWorld: .page)
        let read = (asked as? String).flatMap(JSON.init(text:))
        if let root = root ?? read?["person"]?.string {
            me = root
            // Chat's own row for the person, which a device that just got in publishes a moment later.
            let row = try? await engine.whenPublished(within: .seconds(10)) { try await self.ownRow(of: root) }
            namesKinds = row?["id"]?.string == nil
            let known = Person(id: .init(root), name: row?["name"]?.string ?? person?.name ?? "",
                               workspace: read?["workspace"]?.string ?? "")
            person = known
            defaults.set([root, known.name, known.workspace], forKey: TiloSource.personKey)
        }
        tell()
        switch mainChat {
        case .conversation(let id, let name):
            feeds[.main, default: Feed()].conversation = id
            recipient = ChatRecipient(.conversation, id: id, name: name)
        case .agent(let id, let name):
            assistant = id
            feeds[.main, default: Feed()].conversation = me.map { direct($0, id) }
            recipient = ChatRecipient(.agent, id: id, name: name)
        case .assistant:
            await findAssistant()
        }
        for chat in Array(feeds.keys) { feeds[chat]?.told = false }
        await look()
        await survey(.everything)
    }

    /// What the page keeps of its own membership, for a page that hands a native app nothing:
    /// whose device it is. The page's own tests read the same entry.
    private static let whoAndWhere = #"""
    let person = null;
    try {
      const kept = JSON.parse(localStorage.getItem('tilo-host-memberships') ?? '{}');
      const all = Array.isArray(kept) ? kept : Object.values(kept);
      const here = all.find(entry => entry?.workspace && location.hash.includes(entry.workspace)) ?? all.find(entry => entry?.userPublicKey);
      person = here?.userPublicKey ?? null;
    } catch { /* nothing kept */ }
    return JSON.stringify({person, workspace: globalThis.tiloMount?.state?.()?.workspaceName ?? ''});
    """#

    /// Chat's row for the person: under their identity where the page addresses it so, else
    /// the one of the rows it publishes whose id is theirs.
    private func ownRow(of root: String) async throws -> JSON {
        if let row = try? await engine.read("chat.workspace/people/\(root)") { return row }
        let rows = Set(try await engine.actions().compactMap { action -> String? in
            guard action["resource_kind"]?.string == "chat.workspace.people", action["address"]?["action"]?.string == "read"
            else { return nil }
            return action["address"]?["resource"]?.string
        })
        for resource in rows.sorted().prefix(64) {
            if let row = try? await engine.read(resource), ChatMember(row["id"])?.id == root { return row }
        }
        throw TiloEngine.Problem.refused(code: "unknown_action", reason: "Chat has not published this person yet")
    }

    /// The workspace's agents as this page holds them, each read whole. Nil while the page
    /// publishes none of it.
    private func agentsInPage() async -> [JSON]? {
        guard agentsRefused < 8 else { return nil }
        guard let listed = try? await engine.read("chat.agent") else {
            agentsRefused += 1
            return nil
        }
        agentsRefused = 0
        var read: [JSON] = []
        for row in listed["agents"]?.array ?? [] {
            if let item = row["item"]?.string, let agent = try? await engine.read(item) { read.append(agent) }
        }
        return read
    }

    /// The workspace's agents as its server says them to the person on this device, each whole.
    ///
    /// Its answer is taken as the server's word only where it is one: a list of the agents in
    /// which every agent can be read whole. An answer that holds no such list, or names an
    /// agent it does not let this device read, throws `unavailable`: then nothing says that
    /// the person keeps no assistant, and none is made on it.
    private func agentsOnServer() async throws -> [JSON] {
        let unread = TiloEngine.Problem.refused(code: "unavailable", reason: "The server did not say the workspace's agents whole")
        var read: [JSON] = []
        var cursor: JSON?
        // A long list comes in pages; a workspace keeps a handful of agents.
        for _ in 0..<20 {
            let listed = try await engine.askAgents("read", cursor.map { ["cursor": $0] } ?? [:])
            if let named = listed["agents"]?.object {
                // Agents by their ids.
                for (id, agent) in named.sorted(by: { $0.key < $1.key }) {
                    guard var whole = agent.object else { throw unread }
                    if whole["id"] == nil { whole["id"] = .string(id) }
                    read.append(.object(whole))
                }
            } else if let rows = listed["agents"]?.array {
                for row in rows {
                    // A row that says who keeps the agent is the agent whole; any other names its item.
                    if !(row["keeper"]?.isNull ?? true), row["id"]?.string != nil {
                        read.append(row)
                    } else if let id = row["item"]?.string?.split(separator: "/").last.map(String.init) ?? row["id"]?.string,
                              let agent = try? await engine.askAgents("read", agent: id), agent["id"]?.string != nil {
                        read.append(agent)
                    } else {
                        throw unread
                    }
                }
            } else {
                throw unread
            }
            guard let next = listed["nextCursor"], !next.isNull else { return read }
            cursor = next
        }
        throw unread
    }

    /// The person's assistant among a workspace's agents: the active one they keep that no
    /// outside client connects as and that is nobody's sub-agent, the earliest made.
    private static func assistant(among all: [JSON], of person: String) -> JSON? {
        all.filter {
            ChatMember($0["keeper"])?.id == person && ($0["mcp_client"]?.isNull ?? true) && ($0["sub"]?.isNull ?? true)
                && variant($0["state"]) == "active" && $0["id"]?.string != nil
        }
        .min { ($0["created_at"]?.number ?? 0) < ($1["created_at"]?.number ?? 0) }
    }

    /// What a new assistant is called until its person renames it.
    private static let assistantName = "Assistant"
    private static let agentsNotOffered = "This Tilo does not let an app find or make your assistant yet"
    private static let notAnswered = "Tilo did not answer while your assistant was being set up"
    private static let notInOwn = "Your assistant lives in your own workspace, and this Tilo did not say which one that is"

    /// Find the person's assistant, and make it where they keep none. Its conversation is the
    /// direct one of the two.
    ///
    /// The workspace's server is asked first, as the person on this device: it knows which
    /// agents the workspace keeps also where this page does not hold them yet. So only its word
    /// that the person keeps none makes one, and only in the person's own workspace: through
    /// Chat's own `create-agent`, set to be run by the server, because an assistant answers
    /// while every device of its person is off.
    /// Where the page hands an app no way to its server, or the server offers none of this,
    /// the assistant is found in what the page itself holds, and nothing is made.
    private func findAssistant() async {
        feeds[.main, default: Feed()].conversation = nil
        recipient = nil
        assistant = nil
        guard let me, !finding else { return }
        finding = true
        defer { finding = false }
        setups.set(.findingAssistant)
        do {
            var all = try await engine.whenPublished(within: .seconds(6)) { try await self.agentsOnServer() }
            if TiloSource.assistant(among: all, of: me) == nil {
                // An assistant lives in its person's own workspace: none is made anywhere else.
                guard !livesInOwn || standsInOwn else {
                    setups.set(.noAssistant(TiloSource.notInOwn))
                    return
                }
                try await engine.askAgents("create-agent", ["name": .string(TiloSource.assistantName)])
                all = try await agentsOnServer()
                guard let made = TiloSource.assistant(among: all, of: me)?["id"]?.string else { throw TiloEngine.Problem.noAnswer }
                try await engine.askAgents("set-agent-on-host", ["agent": .string(made), "on_host": true])
            }
            if let found = TiloSource.assistant(among: all, of: me) {
                take(found, of: me)
                return
            }
        } catch TiloEngine.Problem.refused(let code, let reason) where code != "unavailable" && code != "unknown_action" {
            // The server's own refusal, in its words: this person's role may not make an agent.
            setups.set(.noAssistant(reason.isEmpty ? code : reason))
            return
        } catch TiloEngine.Problem.refused {
            // An older page or an older server: what the page holds is read below.
        } catch {
            // A page that just let a device in starts over once more, and what it was asked
            // then is never answered. Nothing is wrong: it is asked again once it is in.
            if startedOver { return }
            setups.set(.noAssistant(TiloSource.notAnswered))
            return
        }
        if let all = await agentsInPage(), let found = TiloSource.assistant(among: all, of: me) {
            take(found, of: me)
            return
        }
        if startedOver { return }
        setups.set(.noAssistant(TiloSource.agentsNotOffered))
    }

    /// Whether the page is on its way in again. What it was asked before it started over is
    /// asked anew when it is in, so nothing is said of an ask that ended with it.
    private var startedOver: Bool {
        if case .arriving = engine.state { true } else { false }
    }

    /// This agent is the person's assistant: the main chat is their conversation with it.
    private func take(_ found: JSON, of me: String) {
        guard let id = found["id"]?.string else { return }
        assistant = id
        feeds[.main, default: Feed()].conversation = direct(me, id)
        recipient = ChatRecipient(.agent, id: id, name: found["name"]?.string ?? id)
        saySetup()
    }

    /// Say what stands in the way now: the assistant first, then a provider of the person's own.
    private func saySetup() {
        if mainChat == .assistant, assistant == nil { return }
        setups.set(providerSetup)
    }

    public nonisolated func setup() -> AsyncStream<Setup> { setups.stream() }

    /// Find the person's assistant once more, and make it where they keep none: what the app
    /// does by itself when the device gets in, for the person to ask again. Why it did not go
    /// through is said by `setup()`, where the main chat shows it.
    public func makeAssistant() async throws {
        guard case .open = engine.state else { throw person == nil ? SourceProblem.signedOut : SourceProblem.unreachable }
        guard mainChat == .assistant else { return }
        await findAssistant()
        for chat in Array(feeds.keys) { feeds[chat]?.told = false }
        await look()
        await survey(.everything)
    }

    /// The id Chat keeps the conversation of the person and one agent under, as this page spells it.
    private func direct(_ person: String, _ agent: String) -> String {
        ChatConversation.direct([ChatMember(namesKinds ? .person : nil, id: person), ChatMember(namesKinds ? .agent : nil, id: agent)])
    }

    /// An enum's case, however the read spells it: the bare word, or `{"$variant": word}`.
    private static func variant(_ value: JSON?) -> String? {
        value?.string ?? value?["$variant"]?.string
    }

    // MARK: A conversation

    public nonisolated func conversation(_ chat: Chat) -> AsyncStream<ConversationChange> {
        let (stream, continuation) = AsyncStream.makeStream(of: ConversationChange.self, bufferingPolicy: .bufferingNewest(64))
        let id = UUID()
        Task { @MainActor [weak self] in
            guard let self else { continuation.finish(); return }
            self.feeds[chat, default: Feed()].listeners[id] = continuation
            if let feed = self.feeds[chat], feed.told {
                continuation.yield(.all(feed.held))
                if feed.answering { continuation.yield(.answering(true)) }
            }
        }
        continuation.onTermination = { [weak self] _ in
            Task { @MainActor in self?.feeds[chat]?.listeners[id] = nil }
        }
        return stream
    }

    public func earlier(in chat: Chat, before item: Item.ID) async throws -> [Item] {
        // The newest sixty are held: Chat's read has no way yet to read one conversation further back.
        []
    }

    /// Read Chat's newest messages again and say what is different in each conversation that
    /// is held. Started by the engine's word that something changed, never by a clock.
    private func look() async {
        guard case .open = engine.state else { return }
        guard !looking else { again = true; return }
        looking = true
        defer { looking = false }
        repeat {
            again = false
            let wanted = feeds.compactMapValues(\.conversation)
            guard !wanted.isEmpty else { return }
            var now: [Chat: [Item]] = [:]
            var atWork: [Chat: Bool] = [:]
            // A page whose read names a conversation answers each one whole, in one read.
            var whole = true
            for (chat, id) in wanted {
                guard let read = try? await engine.chatConversation(id) else { whole = false; break }
                now[chat] = read.map { piece(of: $0, in: chat, isFirst: $0.id == read.first?.id) }
                atWork[chat] = read.contains(where: \.isWorkedOn)
            }
            if !whole {
                now = [:]
                atWork = [:]
                guard let items = try? await engine.chatLatest(60) else { return }
                for id in items {
                    // An older piece is read once; the newest may still be growing.
                    let settled = !items.suffix(8).contains(id)
                    if settled, let (chat, known) = held(id) {
                        now[chat, default: []].append(known)
                    } else if let message = try? await engine.chatMessage(id),
                              let chat = wanted.first(where: { $0.value == message.conversation })?.key {
                        now[chat, default: []].append(piece(of: message, in: chat, isFirst: now[chat]?.isEmpty ?? true))
                        if message.isWorkedOn { atWork[chat] = true }
                    }
                }
            }
            for chat in wanted.keys {
                say(now[chat] ?? [], in: chat)
                answers(atWork[chat] ?? false, in: chat)
            }
            // An answer that is being written grows: its next words are expected.
            if atWork.values.contains(true) { engine.expectChanges(for: .seconds(4)) }
        } while again
    }

    /// Say whether an agent is at work in a conversation, when that changed.
    private func answers(_ on: Bool, in chat: Chat) {
        guard var feed = feeds[chat], feed.answering != on else { return }
        feed.answering = on
        feeds[chat] = feed
        for listener in feed.listeners.values { listener.yield(.answering(on)) }
    }

    private func held(_ id: String) -> (Chat, Item)? {
        for (chat, feed) in feeds {
            if let known = feed.held.first(where: { $0.id.value == id }) { return (chat, known) }
        }
        return nil
    }

    private func say(_ now: [Item], in chat: Chat) {
        guard var feed = feeds[chat] else { return }
        let before = feed.held
        feed.held = now
        defer { feeds[chat] = feed }
        guard feed.told else {
            feed.told = true
            for listener in feed.listeners.values { listener.yield(.all(now)) }
            return
        }
        let known = Set(before.map(\.id))
        let present = Set(now.map(\.id))
        for gone in before where !present.contains(gone.id) {
            for listener in feed.listeners.values { listener.yield(.removed(gone.id)) }
        }
        for item in now where known.contains(item.id) && before.first(where: { $0.id == item.id }) != item {
            for listener in feed.listeners.values { listener.yield(.changed(item)) }
        }
        let added = now.filter { !known.contains($0.id) }
        if !added.isEmpty { for listener in feed.listeners.values { listener.yield(.added(added)) } }
    }

    private func piece(of message: ChatMessage, in chat: Chat, isFirst: Bool) -> Item {
        let id = Item.ID(message.id)
        let fields = message.fields
        if let question = fields["question"], !question.isNull {
            let answered = TiloSource.variant(question["answered"]?["answer"])
            let withdrawn = !(question["withdrawn"]?.isNull ?? true)
            if TiloSource.variant(question["kind"]) == "secret" {
                let detail = TiloSource.detail(of: question)
                let state: SecretRequest.State = answered == "filled" ? .kept : answered == "declined" || withdrawn ? .declined : .waiting
                return Item(id: id, at: message.at, .secret(SecretRequest(
                    id: .init(message.id), text: message.text, name: TiloSource.placeName(detail?["belongsTo"]?[0]?.string ?? ""),
                    state: state)))
            }
            let choices = (question["answers"]?.array ?? []).enumerated().compactMap { index, answer -> Choice? in
                guard let name = answer["id"]?.string else { return nil }
                let label = answer["label"]?.string ?? ""
                return Choice(id: .init(name), title: label.isEmpty ? name : label, isMain: index == 0)
            }
            return Item(id: id, at: message.at, .question(Question(
                id: .init(message.id), text: message.text, choices: choices, answer: answered.map { .init($0) })))
        }
        var live: [AssistantCore.LiveThing] = []
        if let share = fields["share"], !share.isNull, share["ended"]?.bool != true,
           let shared = (fields["attachments"]?.array ?? []).compactMap({ $0["at"] ?? $0["resource"]?["at"] }).first,
           let app = shared["app"]?.string {
            let record = shared["record"]?.string ?? "@tilo:package-primary"
            let title = shared["reference"]?["label"]?.string ?? message.text
            live.append(AssistantCore.LiveThing(id: .init(message.id), title: title, reference: "record:\(app):\(record)"))
        }
        let author: Message.Author
        if message.author.id == me, message.author.kind != .agent {
            author = .person
        } else if case .subAgent(let agent) = chat, message.author.id != agent.value {
            // In a sub-agent's own conversation, whoever is neither the person nor the sub-agent
            // is the agent that started it: its first words are the job.
            author = .mainAgent(isFirst ? .job : .instruction)
        } else {
            author = .assistant
        }
        return Item(id: id, at: message.at, .message(Message(author, message.text, live: live)))
    }

    /// What a question's kind keeps with it: JSON, as text or already read.
    private static func detail(of question: JSON) -> JSON? {
        guard let detail = question["detail"] else { return nil }
        return detail.string.flatMap(JSON.init(text:)) ?? (detail.object == nil ? nil : detail)
    }

    /// A place as a person reads it: the site's or the server's name, without scheme or port.
    private static func placeName(_ place: String) -> String {
        URLComponents(string: place)?.host ?? place
    }

    public func send(_ draft: Draft, to chat: Chat) async throws {
        guard draft.files.isEmpty else { throw SourceProblem.refused("A file cannot be sent from this app yet") }
        let to: ChatRecipient
        switch chat {
        case .main:
            guard let recipient else { throw noAssistant }
            to = recipient
        case .subAgent(let agent):
            guard let id = feeds[chat]?.conversation else { throw SourceProblem.refused("This sub-agent has no conversation yet") }
            to = ChatRecipient(.conversation, id: id, name: agent.value)
        }
        try await taken { _ = try await self.engine.chatSend(draft.text, to: to) }
    }

    /// Stop the agent that is at work in a conversation, as the person: Chat's own
    /// `stop-agent` on the newest message an agent still works on. Nothing is asked where
    /// none is at work.
    public func stopAnswering(in chat: Chat) async throws {
        guard let id = feeds[chat]?.conversation else { throw noAssistant }
        try await taken {
            var read = try await self.engine.chatConversation(id)
            if read == nil {
                var found: [ChatMessage] = []
                for item in try await self.engine.chatLatest(60) {
                    if let message = try await self.engine.chatMessage(item), message.conversation == id { found.append(message) }
                }
                read = found
            }
            guard let atWork = read?.last(where: \.isWorkedOn) else { return }
            try await self.engine.chatStop(atWork)
        }
    }

    public func answer(_ question: Question.ID, with choice: Choice.ID) async throws {
        try await answer(question.value, choice.value, detail: nil)
    }

    private func answer(_ message: String, _ words: String, detail: JSON?) async throws {
        try await taken {
            guard let messages = try await self.engine.chatMessagesResource() else { throw TiloEngine.Problem.notOpen }
            // Chat names a message by the last part of its item.
            let named = message.split(separator: "/").last.map(String.init) ?? message
            var arguments: [String: JSON] = ["message": .string(named), "answer": .string(words)]
            if let detail { arguments["detail"] = detail }
            try await self.engine.invoke("answer-question", on: messages, arguments)
        }
    }

    /// The secret goes into the person's vault on their host, and the question is answered with
    /// the item's reference. Nothing else is told anyone: not the conversation, not a log.
    public func keep(_ secret: Secret, for request: SecretRequest.ID) async throws {
        guard !secret.isEmpty else { throw SourceProblem.refused("Nothing was typed") }
        try await taken {
            guard let message = try await self.engine.chatMessage(request.value),
                  let question = message.fields["question"], !question.isNull
            else { throw TiloEngine.Problem.refused(code: "gone", reason: "This question is no longer there") }
            let detail = TiloSource.detail(of: question)
            let belongsTo = (detail?["belongsTo"]?.array ?? []).compactMap(\.string)
            guard !belongsTo.isEmpty else {
                throw TiloEngine.Problem.refused(code: "no-place", reason: "The question does not say where this secret belongs")
            }
            let item = try await self.engine.keepInVault(Protected(secret.handOver()),
                                                         kind: TiloSource.variant(detail?["kind"]) ?? "value",
                                                         belongsTo: belongsTo, username: detail?["username"]?.string ?? "")
            try await self.answer(request.value, "filled", detail: ["reference": .string(item.reference)])
        }
        await survey(.server)
    }

    public func fetchSecret(for request: SecretRequest.ID) async throws {
        throw SourceProblem.refused("No password manager is connected to Tilo yet")
    }

    public func decline(_ request: SecretRequest.ID) async throws {
        try await answer(request.value, "declined", detail: nil)
    }

    public func authenticate(_ offer: ConnectionOffer.ID) async throws -> URL? {
        throw SourceProblem.refused("Tilo keeps no connections to sign in to yet")
    }

    public func decline(_ offer: ConnectionOffer.ID) async throws {
        throw SourceProblem.refused("Tilo keeps no connections to sign in to yet")
    }

    public func file(of attachment: Attachment.ID) async throws -> URL {
        throw SourceProblem.refused("A file cannot be fetched into this app yet")
    }

    // MARK: Sub-agents

    public nonisolated func subAgents() -> AsyncStream<[SubAgent]> { agents.stream() }

    /// The person starts one with just the job: the main agent is asked in the main chat, picks
    /// its role and its model and starts it. Where Chat has no such ask yet, the job goes to
    /// the main agent as the person's own words.
    public func startSubAgent(job: String) async throws {
        guard let channel = feeds[.main]?.conversation else { throw noAssistant }
        do {
            try await taken {
                guard let messages = try await self.engine.chatMessagesResource() else { throw TiloEngine.Problem.notOpen }
                try await self.engine.invoke("ask-agent", on: messages, [
                    "text": .string(job), "channel": .string(channel),
                    "cause": ["kind": "sub-agent", "label": "Start a sub-agent", "key": .string(UUID().uuidString)],
                ])
            }
        } catch SourceProblem.refused {
            try await send(Draft(text: "Start a sub-agent for this job: \(job)"), to: .main)
        }
    }

    public func stop(_ subAgent: SubAgent.ID) async throws {
        try await taken { try await self.engine.invoke("stop-sub-agent", on: "chat.agent", ["agent": .string(subAgent.value)]) }
    }

    // MARK: Tasks, scheduled things and events

    public nonisolated func tasks() -> AsyncStream<[AssistantTask]> { taskList.stream() }

    public func addTask(_ title: String) async throws {
        throw SourceProblem.refused("Tasks cannot be reached from this app yet")
    }

    public nonisolated func automations() -> AsyncStream<[Automation]> { automationList.stream() }

    /// Set up something that comes at a time or when something else happens, as the person:
    /// what it is called, what starts it and what their assistant is told to do then. The
    /// assistant sets these up in its answers; this is the same ask, for the app's own code.
    public func setUpEvent(name: String, when: String, instruction: String) async throws {
        try await taken { try await self.engine.setUpEvent(name: name, when: when, instruction: instruction) }
        await survey(.server)
    }

    public func setOn(_ on: Bool, for automation: Automation.ID) async throws {
        try await taken { try await self.engine.ask(.events, on ? "resume" : "pause", of: "events", ["event": .string(automation.value)]) }
        await survey(.server)
    }

    // MARK: Supervision

    public nonisolated func supervision() -> AsyncStream<Supervision> { supervisionNow.stream() }

    public func setAssistantOn(_ on: Bool) async throws {
        throw SourceProblem.refused("Tilo cannot pause an assistant yet")
    }

    public func models() async throws -> [ModelChoice] {
        var read: [ModelChoice] = []
        try await taken { read = TiloSource.choices(of: try await self.engine.modelProviders()) }
        choices = read
        return read
    }

    /// Link a provider the person's server knows by name (`anthropic`, `gemini`, `openai`,
    /// `openrouter`) with the key the person typed into the protected field, and answer the
    /// models it then offers. The key goes from the field into the one ask that carries it to
    /// their server, which keeps it; nothing here keeps or says it. With `model`, work is done on
    /// that model of it unless its kind names another.
    @discardableResult
    public func link(provider preset: String, key: Secret, model: String? = nil) async throws -> [ModelChoice] {
        guard !key.isEmpty else { throw SourceProblem.refused("Nothing was typed") }
        try await taken {
            let linked = try await self.engine.modelProviders()
            // A provider that is linked already keeps its place: only its key is typed anew.
            let known = linked.providers.first { $0.id == preset || $0.name.lowercased() == preset.lowercased() }
            let added = known == nil ? try await self.engine.linkModelProvider(preset: preset) : known
            guard let provider = added else { throw TiloEngine.Problem.noAnswer }
            try await self.engine.keepModelKey(Protected(key.handOver()), for: provider.id)
            let listed = try await self.engine.refreshModels(of: provider.id)
            switch listed.problem {
            case nil, "no-list": break
            case "key-refused": throw TiloEngine.Problem.refused(code: "key-refused", reason: "\(provider.name) did not accept this key")
            case "unreachable": throw TiloEngine.Problem.refused(code: "unreachable", reason: "\(provider.name) did not answer")
            case let other?: throw TiloEngine.Problem.refused(code: other, reason: "\(provider.name)'s models could not be read")
            }
            if let model { try await self.engine.chooseDefaultModel(model, of: provider.id) }
        }
        await survey(.server)
        return choices
    }

    /// The provider step of the main chat: the same link, for the provider the person picked.
    public func link(_ provider: ProviderChoice.ID, key: Secret) async throws -> [ModelChoice] {
        try await link(provider: provider.value, key: key)
    }

    /// The providers a person's server links by name, as its `add-preset` knows them. Their
    /// server offers no list of them to ask for yet, so their four names stand here.
    private static let catalogue = [("anthropic", "Anthropic"), ("gemini", "Google Gemini"), ("openai", "OpenAI"),
                                    ("openrouter", "OpenRouter")]

    /// What the person's providers leave to do before the main chat is answered.
    private static func setup(of linked: ModelProviders, choices: [ModelChoice]) -> Setup {
        if let answers = linked.model(for: WorkKind.request.rawValue), answers.provider.hasKey { return .ready }
        if !choices.isEmpty { return .needsModel(choices) }
        // A provider the person added already stands first, under the name their server gives it.
        var linkable = linked.providers.map { ProviderChoice(id: .init($0.id), name: $0.name) }
        for (id, name) in catalogue where !linkable.contains(where: { $0.id.value == id || $0.name == name }) {
            linkable.append(ProviderChoice(id: .init(id), name: name))
        }
        return .needsProvider(linkable)
    }

    public func choose(_ model: ModelChoice.ID, for work: WorkKind) async throws {
        let parts = model.value.split(separator: "/", maxSplits: 1).map(String.init)
        guard parts.count == 2 else { throw SourceProblem.refused("This is no model of a provider you linked") }
        try await taken { try await self.engine.chooseModel((id: parts[1], provider: parts[0]), for: [work.rawValue]) }
        await survey(.server)
    }

    public func forget(_ learned: Supervision.Learned.ID) async throws {
        if learned.value.hasPrefix(TiloSource.onHost) {
            let fact = String(learned.value.dropFirst(TiloSource.onHost.count))
            try await taken { try await self.engine.ask(.learned, "forget", of: "facts", ["fact": .string(fact)]) }
            await survey(.server)
        } else {
            try await taken { try await self.engine.invoke("forget", on: "person:chat.learned", ["fact": .string(learned.value)]) }
        }
    }

    private static func choices(of linked: ModelProviders) -> [ModelChoice] {
        linked.providers.filter(\.hasKey).flatMap { provider in
            provider.models.map { ModelChoice(id: .init("\(provider.id)/\($0.id)"), name: $0.label, provider: provider.name) }
        }
    }

    // MARK: What is read beside the conversation

    private enum Survey {
        /// What Chat keeps, which the page holds: read at the page's word of a change.
        case records
        /// What the person's server keeps: asked when this app changed it.
        case server
        case everything
    }

    /// Read what stands beside the conversation and say what is different.
    private func survey(_ what: Survey) async {
        guard case .open = engine.state else { return }
        guard !surveying else {
            surveyAgain = surveyAgain == nil || surveyAgain == what ? what : .everything
            return
        }
        surveying = true
        defer { surveying = false }
        var next: Survey? = what
        while let now = next {
            surveyAgain = nil
            var seen = supervisionNow.now
            if now != .server { await surveyRecords(&seen) }
            if now != .records { await surveyServer(&seen) }
            seen.learned = learnedOnHost + learnedInChat
            seen.learnedCount = max(seen.learned.count, seen.learnedCount)
            supervisionNow.set(seen)
            next = surveyAgain
        }
    }

    private func surveyRecords(_ seen: inout Supervision) async {
        // Sub-agents: the agents of the person's that an agent started for a job.
        if let me, let all = await agentsInPage() {
            // An assistant this page did not hold yet when the device got in.
            if mainChat == .assistant, assistant == nil, case .noAssistant = setups.now,
               let found = TiloSource.assistant(among: all, of: me) {
                take(found, of: me)
                again = true
            }
            var listed: [SubAgent] = []
            for agent in all {
                guard let sub = agent["sub"], !sub.isNull, ChatMember(agent["keeper"])?.id == me, TiloSource.variant(agent["state"]) == "active",
                      let id = agent["id"]?.string
                else { continue }
                let stands = TiloSource.variant(sub["stands"]) ?? "working"
                let state: SubAgent.State = switch stands {
                case "needs_person": .needsYou
                case "done": .done
                case "failed": .failed
                case "stopped": .stopped
                default: .working
                }
                let since = sub["since"]?.number.map { Date(timeIntervalSince1970: $0 / 1000) }
                listed.append(SubAgent(
                    id: .init(id), title: agent["name"]?.string ?? id, role: sub["role"]?.string ?? "", state: state,
                    runsOn: TiloSource.variant(sub["runs"]) ?? "",
                    started: Date(timeIntervalSince1970: (agent["created_at"]?.number ?? 0) / 1000),
                    startedBy: sub["started_by"]?.string == assistant ? .mainAgent : .person,
                    finished: state == .done ? since : nil))
                if let channel = sub["conversation"]?.string, feeds[.subAgent(.init(id))]?.conversation != channel {
                    feeds[.subAgent(.init(id)), default: Feed()].conversation = channel
                    again = true
                }
            }
            agents.set(listed.sorted { $0.started > $1.started })
            // What the assistant itself spent today, and the most it may.
            if let record = all.first(where: { $0["id"]?.string == assistant }) {
                seen.usage.spentTodayMicros = Int(record["spent"]?["cost_micros"]?.number ?? 0)
                seen.usage.dailyLimitMicros = record["spend_limit_micros"]?.number.map { Int($0) }
                seen.isOn = TiloSource.variant(record["state"]) == "active"
            }
            if again { await look() }
        }
        // What it has learned: the person's own record in Chat.
        guard !keepsNothingLearned else { return }
        guard let learned = try? await engine.read("person:chat.learned") else {
            keepsNothingLearned = true
            return
        }
        if let facts = learned["facts"]?.array {
            var read: [Supervision.Learned] = []
            for row in facts.suffix(40) {
                if let item = row["item"]?.string, let fact = try? await engine.read(item), let text = fact["text"]?.string {
                    read.append(.init(id: .init(item.split(separator: "/").last.map(String.init) ?? item), text: text))
                } else if let text = row["text"]?.string, let id = row["id"]?.string {
                    read.append(.init(id: .init(id), text: text))
                }
            }
            learnedInChat = read.reversed()
            seen.learnedCount = learnedOnHost.count + facts.count
        }
    }

    private func surveyServer(_ seen: inout Supervision) async {
        // The person's models: which one the main chat answers on.
        if let linked = try? await engine.modelProviders() {
            choices = TiloSource.choices(of: linked)
            seen.model = linked.model(for: WorkKind.request.rawValue).map { chosen in
                chosen.provider.models.first(where: { $0.id == chosen.model })?.label ?? chosen.model
            }
            // What the main chat says of it: none linked, none chosen, or nothing in the way.
            providerSetup = TiloSource.setup(of: linked, choices: choices)
            saySetup()
        }
        // What it can sign in to: the vault's items, by where each belongs. Never a secret.
        if let items = try? await engine.vaultItems() {
            seen.logins = items.map {
                Supervision.Login(id: .init($0.reference), name: TiloSource.placeName($0.belongsTo.first ?? $0.kind), typed: $0.added,
                                  lastUsed: $0.lastUsed)
            }
        }
        // What the agents the host runs have learned about the person, newest first.
        if let facts = try? await engine.ask(.learned, "list", of: "facts")["facts"]?.array {
            learnedOnHost = facts.compactMap { fact in
                guard let id = fact["id"]?.string, let text = fact["text"]?.string else { return nil }
                return Supervision.Learned(id: .init(TiloSource.onHost + id), text: text)
            }
        }
        // What is important, in the person's own words, and the situations those words depend on.
        if let rule = try? await engine.ask(.telling, "read", of: "importance") {
            seen.importance = (rule["lines"]?.array ?? []).compactMap { $0["words"]?.string }
            seen.situations = (rule["facts"]?.array ?? []).compactMap { fact in
                guard let name = fact["name"]?.string else { return nil }
                return Supervision.Situation(name: name, value: fact["value"]?.string ?? "")
            }
        }
        // What comes at a time and what happens when something else does.
        if let events = try? await engine.ask(.events, "list", of: "events")["events"]?.array {
            automationList.set(events.compactMap { event -> Automation? in
                guard let id = event["id"]?.string else { return nil }
                let scheduled = TiloSource.variant(event["sort"]) != "event"
                let when = event["when"]?.string ?? ""
                return Automation(
                    id: .init(id), sort: scheduled ? .scheduled : .event, title: event["name"]?.string ?? id,
                    summary: scheduled ? when : event["instruction"]?.string ?? when,
                    next: event["nextAtMs"]?.number.map { Date(timeIntervalSince1970: $0 / 1000) },
                    once: when.lowercased().hasPrefix("once"), filter: event["onlyIf"]?.string,
                    threshold: event["every"]?.number.map { Int($0) }, note: nil,
                    actsAlone: !(event["permits"]?.array ?? []).isEmpty, isOn: event["on"]?.bool ?? true)
            })
        }
    }

    private var noAssistant: SourceProblem { .refused("You have no assistant in this workspace yet") }

    // MARK: Plumbing

    /// Run a call and say a failure the way the views read one.
    private func taken(_ call: @MainActor () async throws -> Void) async throws {
        do {
            try await call()
        } catch TiloEngine.Problem.refused(let code, let reason) {
            throw SourceProblem.refused(reason.isEmpty ? code : reason)
        } catch is TiloEngine.Problem {
            throw person == nil ? SourceProblem.signedOut : SourceProblem.unreachable
        }
    }
}

/// A value that is told to everyone who listens: what it is now, then each change.
@MainActor
final class Told<Value: Sendable & Equatable>: Sendable {
    private(set) var now: Value
    private var listeners: [UUID: AsyncStream<Value>.Continuation] = [:]

    nonisolated init(_ value: Value) {
        now = value
    }

    func set(_ value: Value) {
        guard value != now else { return }
        now = value
        for listener in listeners.values { listener.yield(value) }
    }

    nonisolated func stream() -> AsyncStream<Value> {
        let (stream, continuation) = AsyncStream.makeStream(of: Value.self, bufferingPolicy: .bufferingNewest(16))
        let id = UUID()
        Task { @MainActor in
            self.listeners[id] = continuation
            continuation.yield(self.now)
        }
        continuation.onTermination = { [weak self] _ in
            Task { @MainActor in self?.listeners[id] = nil }
        }
        return stream
    }
}

extension TiloSource {
    /// What the app puts into its root view, unseen: where the engine's web view rests.
    public var engineRoom: AnyView { AnyView(EngineRoom(stage: stage)) }

    /// The view of a live thing that stands in a message.
    public func liveView(_ thing: AssistantCore.LiveThing, size: TiloLiveView.Size = .compact) -> AnyView {
        guard let live = TiloLive.LiveThing(id: thing.id.value, title: thing.title, reference: thing.reference) else {
            return AnyView(EmptyView())
        }
        return AnyView(TiloLiveView(live, on: stage, size: size))
    }
}
