import AssistantClosed
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
    /// The one place that follows what the parts of the window show (`ChangeFollower`): a part
    /// says what it shows and how it reads it again, and reads again at the page's word.
    public private(set) lazy var follows = ChangeFollower(engine: engine)

    private let mainChat: MainChat
    private let defaults: UserDefaults
    private let livesInOwn: Bool
    /// What this device calls itself where it says that its person saw something.
    private let deviceName: String
    /// What the person's server told them, by the message of Chat each thing names.
    private var toldByMessage: [String: ToldThing] = [:]
    /// The things told that still wait for the person, by the piece each is shown as.
    private var toldPieces: [Chat: [Item.ID: String]] = [:]
    /// The things told that offer something to press, by the piece each is shown as.
    private var toldOffered: [Chat: [Item.ID: ToldThing]] = [:]
    /// The piece of the main chat each thing told is shown as, by the thing's id.
    private var toldShown: [String: Item.ID] = [:]
    /// What the person was told while they were away, as their server counted it when this device got in.
    private var toldSince: ToldSince?
    private let since = Told<SinceLeft?>(nil)
    /// The place in the server's log that was read up to.
    private var toldUpTo = 0
    private var followsTold = false
    /// The device is in and what its person is told is to be followed.
    private var toldWanted = false
    /// The server tells nothing this way, or does not hold an ask until its log moves.
    private var toldRefused = false
    /// Whether this app read her server's log since it started. What the log held before that
    /// is no news.
    private var toldRead = false
    /// Whether the log was read on this way in, before the conversation was said.
    private var toldFresh = false
    /// How long a read with nothing in it is doubted where pieces are held.
    var emptyDoubt: Duration = .seconds(3)
    /// How long after a conversation was said empty its first pieces are still what the device
    /// kept, come a moment late, and no news.
    var settling: Duration = .seconds(20)
    /// How long an ask of her server that failed waits before it is made again, the first time.
    /// The wait doubles each time, up to a minute.
    var firstPause: Duration = .seconds(1)
    /// How often one ask of her server on the way to her assistant is made in all, where it
    /// goes unanswered or is answered with nothing in it, before the main chat says so.
    var setupAsks = 5
    /// How long the ask for which workspace is her own is out before the main chat says that
    /// her server is making that workspace.
    var makingAfter: Duration = .seconds(10)
    /// How long after the main chat said that her server left the finding of her assistant
    /// unanswered it is begun again by itself, the first time. The wait doubles each time, up
    /// to a minute.
    var findingPause: Duration = .seconds(15)
    /// Her server's asks beside the conversation, made again where one of them went unanswered.
    private let serverAgain = Again()
    /// The ask for what came while she was away, made again where it went unanswered.
    private let sinceAgain = Again()
    /// The finding of her assistant, begun again where her server left it unanswered and the
    /// main chat said so.
    private let findingAgain = Again()
    /// What she saw on this device that her server has not heard yet: each thing it told her
    /// of, by its id, with how she saw it. It is said again until her server has heard it.
    private var unheard: [String: String] = [:]
    private let hearing = Again()
    /// Whether word that she saw something is on its way right now: one round goes at a time.
    private var saysSeen = false
    private static let routeKey = "TiloLiveRoute"
    private static let personKey = "TiloLivePerson"
    /// Kept from the moment the app was told that the workspace it was let into is gone until
    /// the device is paired again or its person signs out: the way in says why it stands there.
    private static let resetKey = "TiloLiveReset"
    private var wasReset = false

    private var person: Person?
    /// The ask for the person's name that goes on beside the way in.
    private var naming: Task<Void, Never>?
    /// The person's identity, as Chat names the author of what they write.
    private var me: String?
    /// The agent that is the person's assistant, by its id among the agents Chat keeps.
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
    /// The person's connections as their server last listed them, and what was asked of it for
    /// a card in a conversation (Connections.swift).
    var connected = Connected()
    /// What marks a fact the host keeps, in the name the frame hands back to forget it.
    private static let onHost = "host:"

    /// One conversation as it is held: the id Chat keeps it under, and its pieces.
    private struct Feed {
        var conversation: String?
        var held: [Item] = []
        var told = false
        /// Since when a read with nothing in it is doubted, while pieces are held.
        var emptySince: ContinuousClock.Instant?
        /// Since when it stands said with nothing in it.
        var saidEmptyAt: ContinuousClock.Instant?
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
    private let reaches = Told<Reach>(.unknown)
    /// Whether the page just got in and who the main chat is with is still being found.
    private var finds = false
    /// The calls that wait for the device's way in to end.
    private var waitingIn: [CheckedContinuation<Void, Never>] = []
    private let agents = Told<[SubAgent]>([])
    private let taskList = Told<[AssistantTask]>([])
    private let automationList = Told<[Automation]>([])
    private let supervisionNow = Told<Supervision>(Supervision())
    /// The assistant's day as the assistant view says it, and the clock it is read by.
    let assistantDay = AssistantDay()
    private let setups = Told<Setup>(.unknown)
    private var choices: [ModelChoice] = []
    private var listening: [Task<Void, Never>] = []
    /// Where the device's key for being told while the app is closed is kept. Nil in a build
    /// without the push right: such a build hands its person's server no address and keeps no key.
    private let closedKeys: (any ClosedKeyStore)?
    private var closedKey: ClosedKey?
    /// The address Apple's service gave this device at this launch, and whether the person's
    /// server has it. An address that comes before the device is in waits until it is.
    private var address: PushAddress?
    private var addressHanded = false
    /// The hand-over of the address, made again where it went unanswered.
    private let handing = Again()

    /// - Parameters:
    ///   - store: the name of the store the device's key is kept in, the same at every launch.
    ///   - livesInOwn: the assistant lives in the person's own workspace on their host: a device
    ///     that was let in elsewhere goes there, where the page can say which workspace that is.
    ///   - defaults: where the app keeps the workspace it was paired into and the person's name.
    ///   - deviceName: what this device is called where its person lets it in and lists their
    ///     devices.
    ///   - closedKeys: where the device's key for being told while the app is closed is kept,
    ///     in a build that has the push right; nil in any other, which is then told nothing
    ///     while it is closed and asks nothing for it.
    public convenience init(settings: ShellSettings, store: UUID, mainChat: MainChat = .assistant, livesInOwn: Bool = true,
                            defaults: UserDefaults = .standard, deviceName: String = "", closedKeys: (any ClosedKeyStore)? = nil) {
        self.init(engine: TiloEngine(settings: settings, store: .kept(store), deviceName: deviceName), mainChat: mainChat,
                  livesInOwn: livesInOwn, defaults: defaults, deviceName: deviceName, closedKeys: closedKeys)
    }

    /// On an engine that is there already: for a check that keeps nothing.
    init(engine: TiloEngine, mainChat: MainChat, livesInOwn: Bool, defaults: UserDefaults, deviceName: String = "",
         closedKeys: (any ClosedKeyStore)? = nil) {
        self.engine = engine
        self.closedKeys = closedKeys
        self.deviceName = deviceName
        stage = LiveStage(engine: engine)
        self.mainChat = mainChat
        self.livesInOwn = livesInOwn
        self.defaults = defaults
        if let kept = defaults.array(forKey: TiloSource.personKey) as? [String], kept.count == 3 {
            person = Person(id: .init(kept[0]), name: kept[1], workspace: kept[2])
            me = kept[0]
            // The key this device handed its person's server at an earlier launch.
            closedKey = closedKeys?.read()
        }
        wasReset = defaults.bool(forKey: TiloSource.resetKey)
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
                    // Where the following of what the person is told ended, it begins again.
                    if self?.toldWanted == true { self?.followTold() }
                }
            },
        ]
        // The Tasks panel reads again when the page says that something Tasks keeps in the
        // workspace changed. Its other moments stay: they are what it reads at where a page
        // tells nothing this way.
        follows.follow(Followed(app: "so.tilo.tasks")) { [weak self] in await self?.readTasks() }
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
        // A sign-in begins. Whatever key this device still keeps is not the next person's: a
        // new one is made when their server is handed this device's address.
        forgetClosedKey()
        do {
            soughtOwn = false
            try engine.pair(with: link.url)
        } catch {
            // Of a link it refuses the app says where the link is from, by name and port, and
            // nothing else: what the link carries to pair with is shown, kept and written nowhere.
            let site = engine.settings.origin
            guard let from = EngineAddress.pairingElsewhere(link.url, of: engine.settings) else {
                throw SourceProblem.refused(AssistantWords.pairsNothing.said(["site": site]))
            }
            throw SourceProblem.refused(AssistantWords.codeFromElsewhere.said(["from": from, "site": site]))
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
        // Their server pushes to this device no more, and the key that opened its pushes goes:
        // whoever signs in here next is handed nothing of this person's. Their server is asked
        // to forget the address as the page is put away, below: signing out waits for no answer.
        let forgets = person != nil && closedKeys != nil
        mark(reset: false)
        forget()
        if forgets { engine.leaveForgettingAddress() } else { engine.leave() }
        tell()
    }

    /// The site says that the workspace this device was let into is no longer there. What the
    /// app held of it is put away as at a sign-out, and nothing is asked of a server that
    /// holds none of it. The device stands at its way in, which says why until it is paired
    /// again. Its key stays in its store, as at a sign-out.
    private func lost() {
        // Only a device that was somebody's here is told at its way in that its workspace is gone.
        mark(reset: wasReset || person != nil || defaults.string(forKey: TiloSource.routeKey) != nil)
        forget()
        engine.leave()
        tell()
    }

    /// Whether the way in says that the device stood in a workspace that is gone, kept across launches.
    private func mark(reset: Bool) {
        wasReset = reset
        if reset { defaults.set(true, forKey: TiloSource.resetKey) } else { defaults.removeObject(forKey: TiloSource.resetKey) }
    }

    /// Everything the app keeps and holds of its person and their workspace is forgotten.
    private func forget() {
        address = nil
        handing.settle()
        forgetClosedKey()
        defaults.removeObject(forKey: TiloSource.routeKey)
        defaults.removeObject(forKey: TiloSource.personKey)
        naming?.cancel()
        naming = nil
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
        connected = Connected()
        toldByMessage = [:]
        toldPieces = [:]
        toldOffered = [:]
        toldShown = [:]
        toldSince = nil
        since.set(nil)
        toldUpTo = 0
        toldRead = false
        toldFresh = false
        toldWanted = false
        toldRefused = false
        serverAgain.settle()
        sinceAgain.settle()
        findingAgain.settle()
        unheard = [:]
        hearing.settle()
        for chat in Array(feeds.keys) {
            feeds[chat]?.conversation = nil
            say([], in: chat, doubting: false)
        }
        agents.set([])
        taskList.set([])
        automationList.set([])
        supervisionNow.set(Supervision())
        choices = []
    }

    private var sessionNow: Session {
        switch engine.state {
        case .open: person.map(Session.signedIn) ?? .pairing
        // The page shows a code at its join step: the window shows it too, to compare.
        // The page asks before it pairs: the window shows its sentences and its two ways on.
        case .asking(let ask): .asked(PairingAsk(heading: ask.heading, said: ask.said, go: ask.go, stop: ask.stop))
        case .pairing(let code): code.isEmpty ? .pairing : .confirming(code: code)
        case .arriving: person.map(Session.signedIn) ?? .pairing
        case .away, .refused, .failed, .gone: person.map(Session.signedIn) ?? (wasReset ? .reset : .signedOut)
        }
    }

    private func tell() {
        sessions.set(sessionNow)
        sayReach()
    }

    /// Whether the person's Tilo answers, as far as a try has shown. It does once the page is
    /// in, and it does not once a try has ended without getting in. A try that is in work says
    /// nothing new, however long it takes: what was said last holds, and at the start that is
    /// nothing, so a page on its way in is not said to be away. No count of seconds ends a
    /// try: its page's own word does, or the network's failure.
    private func sayReach() {
        switch engine.state {
        case .open: reaches.set(.reachable)
        case .refused, .failed: reaches.set(.unreachable)
        // Nothing is tried, so nothing is known. Where the workspace is gone nothing is tried
        // either: the way in says so, and no try at it failed.
        case .away, .gone: reaches.set(.unknown)
        // A try is in work, or the page waits for its person: nothing new is known.
        case .arriving, .asking, .pairing: break
        }
        endWaits()
    }

    /// Every call that waits for the way in looks again at whether it still has to.
    private func endWaits() {
        let waiting = waitingIn
        waitingIn = []
        for call in waiting { call.resume() }
    }

    /// A call made at the start, while it is not known yet whether Tilo answers, waits for the
    /// try that is in work to end: it is not answered that Tilo cannot be reached before a
    /// try has failed.
    private func tried() async {
        while reaches.now == .unknown, startedOver {
            await withCheckedContinuation { waitingIn.append($0) }
        }
    }

    /// Who a message in the main chat goes to is found a moment after the page is in. A
    /// message written before that waits for it: it is not answered that its person has no
    /// assistant while nobody has been asked. The wait ends when the main chat's other side is
    /// found or said not to be, or when a try fails.
    private func found() async {
        while recipient == nil, reaches.now != .unreachable, finds || startedOver {
            await withCheckedContinuation { waitingIn.append($0) }
        }
    }

    private func stood(_ state: TiloEngine.State) async {
        // The workspace this device was let into is gone: the app goes back to its way in by itself.
        if case .gone = state {
            lost()
            return
        }
        // Whether Tilo answers is said the moment a try ends, before anything is asked of the page.
        sayReach()
        if case .open(let route) = state {
            // The device is in again: its way in has nothing left to say of a workspace that was gone.
            if wasReset { mark(reset: false) }
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
        finds = true
        defer {
            finds = false
            endWaits()
        }
        agentsRefused = 0
        if let whose = try? await engine.whose() {
            root = whose.person
            // The assistant lives in the person's own workspace. A device is let in where its
            // link leads, so it goes to the person's own once, where the page says which it is.
            if livesInOwn {
                // An ask that went unanswered is made again the next time the page is in.
                if ownRoute == nil { ownRoute = await ownAsked(of: whose.person) }
                if !soughtOwn, let ownRoute {
                    soughtOwn = true
                    if EngineAddress.isElsewhere(route, than: ownRoute) {
                        try? engine.open(route: ownRoute)
                        return
                    }
                }
            }
        }
        standsInOwn = ownRoute.map { !EngineAddress.isElsewhere(route, than: $0) } ?? false
        let asked = try? await engine.view.callAsyncJavaScript(TiloSource.whoAndWhere, arguments: [:], in: nil, contentWorld: .page)
        let read = (asked as? String).flatMap(JSON.init(text:))
        if let root = root ?? read?["person"]?.string {
            me = root
            // Chat's own row for the person says their name, and how this page names a member. A
            // device that just got in publishes it a moment later, and Chat keeps none for a
            // person it was never told a name of: the way in does not wait for it. It is asked
            // for once here, and beside everything else for ten seconds more.
            let workspace = read?["workspace"]?.string ?? ""
            let row = try? await ownRow(of: root)
            take(row: row, of: root, in: workspace)
            if row == nil { nameLater(root, in: workspace) }
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
        // Who the main chat is with is found, or is said not to be: a message that waited goes on.
        finds = false
        endWaits()
        // What her server told her so far is read before her conversation is said: what was a
        // reminder yesterday stands as one from the first, and is not told again.
        toldRefused = false
        await readToldOnce()
        for chat in Array(feeds.keys) { feeds[chat]?.told = false }
        await look()
        await survey(.everything)
        toldWanted = true
        followTold()
        await handAddress()
    }

    /// Ask her server which workspace is her own. It makes that workspace the first time it is
    /// asked, and a busy server takes minutes over it. The page has said whose device this is,
    /// so she is signed in from here on, and the main chat says what is waited for: that her
    /// assistant is being set up, and once the ask has been out for `makingAfter`, that her
    /// server is making her workspace. Her name and the rest come as the way in goes on.
    private func ownAsked(of root: String) async -> String? {
        if person == nil {
            person = Person(id: .init(root), name: "", workspace: "")
            tell()
        }
        guard mainChat == .assistant, assistant == nil else { return (try? await engine.ownWorkspace())?.route }
        setups.set(.findingAssistant)
        let patience = makingAfter
        let slow = Task { [weak self] in
            try? await Task.sleep(for: patience)
            guard !Task.isCancelled, let self, self.setups.now == .findingAssistant else { return }
            self.setups.set(.makingWorkspace)
        }
        let own = (try? await engine.ownWorkspace())?.route
        slow.cancel()
        if setups.now == .makingWorkspace { setups.set(.findingAssistant) }
        return own
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

    /// What Chat's own row says of the person, or nothing where there is none yet: their name,
    /// and whether this page names a member by its kind.
    private func take(row: JSON?, of root: String, in workspace: String) {
        namesKinds = row?["id"]?.string == nil
        let known = Person(id: .init(root), name: row?["name"]?.string ?? person?.name ?? "", workspace: workspace)
        person = known
        defaults.set([root, known.name, known.workspace], forKey: TiloSource.personKey)
    }

    /// Ask for Chat's own row of the person beside everything else, for ten seconds. When it
    /// comes the person has their name; and where it shows that this page names a member by a
    /// bare id, their conversation with the assistant is spelled anew and read again.
    private func nameLater(_ root: String, in workspace: String) {
        naming?.cancel()
        naming = Task { [weak self] in
            guard let self,
                  let row = try? await self.engine.whenPublished(within: .seconds(10), { try await self.ownRow(of: root) }),
                  !Task.isCancelled, self.me == root
            else { return }
            let named = self.namesKinds
            self.take(row: row, of: root, in: workspace)
            self.tell()
            guard named != self.namesKinds else { return }
            switch self.mainChat {
            case .agent(let id, _): self.feeds[.main]?.conversation = self.direct(root, id)
            case .assistant: if let assistant = self.assistant { self.feeds[.main]?.conversation = self.direct(root, assistant) }
            case .conversation: break
            }
            self.feeds[.main]?.told = false
            await self.look()
        }
    }

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
    /// which every agent can be read whole. An answer that names an agent it does not let this
    /// device read throws `unavailable`: then nothing says that the person keeps no assistant,
    /// and none is made on it. An answer that holds no list at all is nobody's word: it throws
    /// `NothingInIt`, and whoever asked asks again.
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
                throw NothingInIt()
            }
            guard let next = listed["nextCursor"], !next.isNull else { return read }
            cursor = next
        }
        throw unread
    }

    /// The workspace's agents as its server says them. A page that is in hands its device's
    /// entry over a moment later, and until then nothing can be asked of its server: that
    /// moment is waited for, three seconds at most.
    private func agentsWhenOffered() async throws -> [JSON] {
        let end = ContinuousClock.now.advanced(by: .seconds(3))
        while true {
            do {
                return try await engine.whenPublished(within: .seconds(6)) { try await self.agentsOnServer() }
            } catch let problem as TiloEngine.Problem {
                guard case .refused(code: "unavailable", reason: _) = problem, ContinuousClock.now < end,
                      case .open = engine.state, (try? await engine.whose()) == nil
                else { throw problem }
                _ = await engine.nextChange(within: .milliseconds(500))
            }
        }
    }

    /// The person's assistant among a workspace's agents: the active one they keep that no
    /// outside client connects as and that is nobody's sub-agent, the earliest made.
    private static func assistant(among all: [JSON], of person: String) -> JSON? {
        assistants(among: all, of: person).first
    }

    /// Every agent that could be the person's assistant, the earliest made first. Two made in
    /// the same moment stand in the order of their ids, so every device reads the same first.
    private static func assistants(among all: [JSON], of person: String) -> [JSON] {
        all.filter {
            ChatMember($0["keeper"])?.id == person && ($0["mcp_client"]?.isNull ?? true) && ($0["sub"]?.isNull ?? true)
                && variant($0["state"]) == "active" && $0["id"]?.string != nil
        }
        .sorted {
            let made = ($0["created_at"]?.number ?? 0, $1["created_at"]?.number ?? 0)
            return made.0 == made.1 ? ($0["id"]?.string ?? "") < ($1["id"]?.string ?? "") : made.0 < made.1
        }
    }

    /// Chat's own name for the mark that the workspace's server answers for an agent, and for
    /// the action that sets it.
    static let answeredByServer = "on_host"
    private static let answersFor = "set-agent-on-host"

    /// Have the workspace's server answer for an agent. Where the ask goes unanswered it is
    /// made again: asked more than once, it changes nothing a second time.
    private func haveServerAnswer(for agent: String) async throws {
        let arguments: [String: JSON] = ["agent": .string(agent), TiloSource.answeredByServer: true]
        _ = try await askedAgain { try await self.engine.askAgents(TiloSource.answersFor, arguments) }
    }

    /// An answer of her server with nothing in it of what was asked: not her server's word
    /// that there is none, and not the word of a server that offers no such thing.
    private struct NothingInIt: Error {}

    /// Make an ask of her server on the way to her assistant. Where it goes unanswered, or is
    /// answered with nothing in it, it is made again by itself after a pause that doubles each
    /// time, for as long as the page is in: one late or lost answer says nothing to her. Only
    /// where `setupAsks` asks in a row came to nothing is the last one's failure thrown, for
    /// the main chat to say.
    private func askedAgain<Value: Sendable>(_ ask: @MainActor () async throws -> Value) async throws -> Value {
        var pause = firstPause
        var left = setupAsks
        while true {
            do {
                return try await ask()
            } catch TiloEngine.Problem.noAnswer where left > 1 {
                // Asked again below.
            } catch is NothingInIt where left > 1 {
                // Asked again below.
            } catch is NothingInIt {
                // Still nothing in it: a server that does not say the workspace's agents to a device.
                throw TiloEngine.Problem.refused(code: "unavailable", reason: "The server did not say the workspace's agents whole")
            }
            left -= 1
            try? await Task.sleep(for: pause)
            pause = min(pause * 2, .seconds(60))
            // A page that is on its way in again is asked anew once it is in.
            guard case .open = engine.state else { throw TiloEngine.Problem.notOpen }
        }
    }

    /// Whether an agent beside the assistant is one another device of the person's made as its
    /// assistant in the same moment: it carries the name the app gives one, and the server
    /// answers for it nowhere yet. Only such a one is taken back; an agent the person made and
    /// named, or one that already answers, is theirs to keep.
    private static func isTwin(_ agent: JSON) -> Bool {
        agent["name"]?.string == assistantName && agent[answeredByServer]?.bool != true
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
    ///
    /// Where her server left it unanswered the main chat says so, and it is begun again by
    /// itself (`findLater`), `quietly`: the sentence that stands is true until her server
    /// answers, so it stays in place of the line that says her assistant is being set up.
    private func findAssistant(quietly: Bool = false) async {
        feeds[.main, default: Feed()].conversation = nil
        recipient = nil
        assistant = nil
        guard let me, !finding else { return }
        finding = true
        defer { finding = false }
        if !quietly { setups.set(.findingAssistant) }
        do {
            var all = try await askedAgain { try await self.agentsWhenOffered() }
            // The agent this device just had the server answer for.
            var finished: String?
            if TiloSource.assistant(among: all, of: me) == nil {
                // An assistant lives in its person's own workspace: none is made anywhere else.
                // Where the server did not say which workspace that is, it is asked once more.
                if livesInOwn, !standsInOwn, ownRoute == nil, case .open(let route) = engine.state,
                   let own = try? await engine.whenPublished(within: .seconds(4), { try await self.engine.ownWorkspace() }) {
                    ownRoute = own.route
                    if EngineAddress.isElsewhere(route, than: own.route) {
                        soughtOwn = true
                        try? engine.open(route: own.route)
                        return
                    }
                    standsInOwn = true
                }
                guard !livesInOwn || standsInOwn else {
                    if startedOver { return }
                    setups.set(.noAssistant(TiloSource.notInOwn))
                    // Her server left unanswered which workspace is her own: it is asked again.
                    if ownRoute == nil { findLater() }
                    return
                }
                do {
                    try await engine.askAgents("create-agent", ["name": .string(TiloSource.assistantName)])
                } catch TiloEngine.Problem.noAnswer {
                    // A busy server takes the ask and is not done with it yet: what it keeps is read below.
                }
                // A server lists a new agent a moment after it made it, and an ask of a busy one
                // goes unanswered: the list is read again until it holds the assistant.
                var made: String?
                for _ in 0..<8 where made == nil {
                    if let now = try? await agentsOnServer(), let found = TiloSource.assistant(among: now, of: me)?["id"]?.string {
                        all = now
                        made = found
                    } else {
                        _ = await engine.nextChange(within: .milliseconds(700))
                    }
                }
                guard let made else { throw TiloEngine.Problem.noAnswer }
                try await haveServerAnswer(for: made)
                finished = made
                // Another device of the person's found none in the same moment and made one too.
                // The earliest made is the assistant on both, and what was made after it is
                // taken back, so the person keeps one and no agent nobody writes to.
                for twin in TiloSource.assistants(among: all, of: me).dropFirst() where TiloSource.isTwin(twin) {
                    if let id = twin["id"]?.string { _ = try? await engine.askAgents("retire-agent", ["agent": .string(id)]) }
                }
            }
            if let found = TiloSource.assistant(among: all, of: me) {
                // One the app made that no server answers for yet: the device that made it lost an
                // ask on its way. It is finished here, so it answers with every device off.
                if let id = found["id"]?.string, id != finished, TiloSource.isTwin(found), !livesInOwn || standsInOwn {
                    try? await haveServerAnswer(for: id)
                }
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
            findLater()
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

    /// Her server left the finding of her assistant unanswered, and the main chat says so. It
    /// is begun again by itself after `findingPause`, which doubles each time up to a minute, for as
    /// long as the page is in and she has none: she presses nothing. A page that is on its way
    /// in again finds her assistant when it is in. Her own press does the same at once.
    private func findLater() {
        findingAgain.later(first: findingPause) { [weak self] in
            guard let self, case .open = self.engine.state, self.mainChat == .assistant, self.assistant == nil else { return }
            await self.findAssistant(quietly: true)
            guard self.assistant != nil else { return }
            // Found: her conversation with it and what stands beside it are read, as after her press.
            for chat in Array(self.feeds.keys) { self.feeds[chat]?.told = false }
            await self.look()
            await self.survey(.everything)
        }
    }

    /// This agent is the person's assistant: the main chat is their conversation with it.
    private func take(_ found: JSON, of me: String) {
        guard let id = found["id"]?.string else { return }
        findingAgain.settle()
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

    /// Where a sub-agent works, in words a person reads, as the assistant view tells the two
    /// places apart: a server keeps a coding part for a sub-agent exactly where a coding
    /// program does the work in the workspace's machine, with what that program is called.
    /// Every other sub-agent is worked by the person's server itself.
    static func place(of sub: JSON) -> String {
        guard let coding = sub["coding"], !coding.isNull else { return AssistantWords.worksOnServer.said }
        let program = coding["harness_name"]?.string ?? ""
        return program.isEmpty ? AssistantWords.worksOnMachine.said : AssistantWords.worksOnMachineWith.said(["program": program])
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
                now[chat] = pieces(of: read, in: chat)
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
        // An answer that ended may have made a task or moved one on: the list is read again.
        if !on { Task { [weak self] in await self?.readTasks() } }
        // An answer that ended may have set an event up, changed one or removed one.
        if !on, chat == .main { Task { await self.readEvents() } }
    }

    private func held(_ id: String) -> (Chat, Item)? {
        for (chat, feed) in feeds {
            if let known = feed.held.first(where: { $0.id.value == id }) { return (chat, known) }
        }
        return nil
    }

    private func say(_ now: [Item], in chat: Chat, doubting: Bool = true) {
        guard var feed = feeds[chat] else { return }
        // An event of the person's stands as its card under the answer it was set up in.
        let now = chat == .main ? cards.carded(now) : now
        // One read with nothing in it does not empty a conversation that holds pieces: a page
        // that just got in reads so before it has published Chat's messages. It is read again,
        // and believed where it still says so after a while. What the app empties itself, at a
        // sign-out, is no read and is not doubted.
        if doubting, now.isEmpty, !feed.held.isEmpty {
            guard let since = feed.emptySince else {
                feed.emptySince = .now
                feeds[chat] = feed
                lookAgain(after: emptyDoubt)
                return
            }
            if ContinuousClock.now - since < emptyDoubt { return }
        }
        feed.emptySince = nil
        let before = feed.held
        feed.held = now
        defer { feeds[chat] = feed }
        // The pieces that first fill a conversation just said empty are what the device kept,
        // come a moment late: they are said whole, and none of them is news.
        let late = feed.saidEmptyAt.map { !now.isEmpty && ContinuousClock.now - $0 < settling } ?? false
        guard feed.told, !late else {
            feed.told = true
            feed.saidEmptyAt = now.isEmpty ? .now : nil
            for listener in feed.listeners.values { listener.yield(.all(now)) }
            return
        }
        feed.saidEmptyAt = now.isEmpty ? feed.saidEmptyAt ?? .now : nil
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

    // MARK: What the person's server told them

    /// Follow what the person's server tells them (`tilo.telling`): its log is read whole
    /// once, then asked for what is newer, and the server holds each ask until its log moves.
    /// So the app hears of a reminder that comes due while it is open, with no clock of its
    /// own. Every ask counts as a device of the person looking: their server tells them
    /// nowhere else while this app does.
    ///
    /// An ask that is not answered is made again by itself, after a pause that grows, for as
    /// long as the page is in. The following ends where the page is on its way in again, and
    /// begins anew when it is in.
    private func followTold() {
        guard !followsTold, !toldRefused, case .open = engine.state else { return }
        followsTold = true
        let engine = engine
        let device = deviceName
        Task { [weak self] in
            var waits = false
            var quick = 0
            var pause = self?.firstPause ?? .seconds(1)
            // The log was read on this way in, before the conversation was said: what follows
            // is what came while she was away, and then the ask her server holds.
            if let self, self.toldFresh {
                self.toldFresh = false
                await self.readSince()
                waits = true
            }
            while let place = self?.toldUpTo {
                let asked = ContinuousClock.now
                do {
                    let read = try await engine.told(after: place, waiting: waits ? .seconds(10) : nil, device: device)
                    guard let self else { return }
                    self.toldUpTo = max(self.toldUpTo, read.newest)
                    // Where the read on the way in went unanswered, this is the app's first read
                    // of her server's log: the pieces it marks are said whole, none of them told.
                    let before = !self.toldRead
                    self.toldRead = true
                    if read.told.isEmpty {
                        // A server that does not hold the ask until its log moves is not asked
                        // without end. One quick answer with nothing in it is no such server: its
                        // log also moves for what this device is not told, a look it wrote down.
                        if waits, ContinuousClock.now - asked < .seconds(1) {
                            quick += 1
                            if quick >= 3 {
                                self.toldRefused = true
                                break
                            }
                        } else {
                            quick = 0
                        }
                    } else {
                        quick = 0
                        for thing in read.told {
                            if let message = thing.message { self.toldByMessage[message] = thing }
                        }
                        if before { for chat in Array(self.feeds.keys) { self.feeds[chat]?.told = false } }
                        await self.look()
                        // What she was told may be of a task that moved on: the list is read again.
                        if waits { await self.readTasks() }
                    }
                    // What happened while the person was away is counted at their first look after.
                    if !waits { await self.readSince() }
                    waits = true
                    pause = self.firstPause
                } catch TiloEngine.Problem.refused(let code, _) where TiloSource.offersNone(code) {
                    // A server that tells nothing this way is not asked again before the device gets in anew.
                    self?.toldRefused = true
                    break
                } catch TiloEngine.Problem.refused {
                    // Her server knows the ask and failed at it this time. One failed answer does
                    // not end the following: the ask is made again by itself, after a pause that
                    // grows, for as long as the page is in.
                    try? await Task.sleep(for: pause)
                    pause = min(pause * 2, .seconds(60))
                    guard let self, case .open = self.engine.state else { break }
                } catch TiloEngine.Problem.notOpen {
                    // The page is on its way in again: the following begins anew when it is in.
                    break
                } catch {
                    // Not answered. The ask is made again by itself, after a pause that grows: no
                    // word of the page is waited for, and a busy server is not asked without end.
                    try? await Task.sleep(for: pause)
                    pause = min(pause * 2, .seconds(60))
                    guard let self, case .open = self.engine.state else { break }
                }
            }
            self?.followsTold = false
        }
    }

    /// Read what her server told her so far, once since the app started, before her conversation
    /// is said. A reminder that came due yesterday is then a reminder in the first thing the
    /// conversation says, not a piece that turns into one a moment later: a piece that turns
    /// into a reminder is told, and that told every old one again at every start.
    private func readToldOnce() async {
        guard !toldRead, case .open = engine.state else { return }
        do {
            let read = try await engine.told(after: toldUpTo, device: deviceName)
            toldRead = true
            toldFresh = true
            toldUpTo = max(toldUpTo, read.newest)
            for thing in read.told {
                if let message = thing.message { toldByMessage[message] = thing }
            }
        } catch TiloEngine.Problem.refused(let code, _) where TiloSource.offersNone(code) {
            // A server that tells nothing this way is not asked again before the device gets in anew.
            toldRefused = true
        } catch {
            // Not answered, or her server failed at it this time: the following reads the log,
            // and says whole what it then marks.
        }
    }

    /// Whether a refusal says that her server offers no such thing: it does not know the ask,
    /// or this page hands the app no way to it. Any other failed answer is her server failing
    /// at an ask it knows, this time: a write of its own that did not finish, a busy moment.
    private static func offersNone(_ code: String) -> Bool {
        code == "unknown_action" || code == "unavailable"
    }

    /// Read the conversations once more after a while, where no word of a change may come.
    private func lookAgain(after pause: Duration) {
        Task { [weak self] in
            try? await Task.sleep(for: pause + .milliseconds(50))
            await self?.look()
        }
    }

    /// Ask what the person was told while they were away. Their server counts that stretch at
    /// the first look after it, which this device's first read of its log was, and keeps it
    /// until they are next away: where the ask goes unanswered, or their server fails at it
    /// this time, it is made again by itself after a pause that grows.
    private func readSince() async {
        do {
            toldSince = try await engine.toldSinceLeft()
            sinceAgain.settle()
        } catch TiloEngine.Problem.refused(let code, _) where TiloSource.offersNone(code) {
            // A server that counts no such stretch is not asked again.
            toldSince = nil
            sinceAgain.settle()
        } catch TiloEngine.Problem.notOpen {
            // The page is on its way in again: asked anew when it is in.
            sinceAgain.settle()
        } catch {
            sinceAgain.later(first: firstPause) { [weak self] in await self?.readSince() }
        }
        saySince()
    }

    private func saySince() {
        let said = toldSince.map { TiloSource.since($0, shown: toldShown) }
        since.set(said?.isEmpty == false ? said : nil)
    }

    public nonisolated func sinceLeft() -> AsyncStream<SinceLeft?> { since.stream() }

    /// The pieces of one conversation's messages, oldest first.
    ///
    /// A message that a cause wrote is no piece of its own: an event of the person's that
    /// came, a moment they asked to be told at. It is what their server told the assistant
    /// then, and what the assistant answered to it stands under a line with the cause's name.
    /// What the server told the person of that answer says what it is: a reminder, a brief, or
    /// news, each with the mark the person's own rule gave it. Nothing here decides that.
    private func pieces(of read: [ChatMessage], in chat: Chat) -> [Item] {
        var causes: [String: String] = [:]
        var lastAnswer: [String: String] = [:]
        for message in read {
            if message.author.id != me, let cause = message.fields["cause"], !cause.isNull {
                causes[message.name] = cause["label"]?.string ?? ""
            } else if let parent = message.fields["parent"]?.string {
                lastAnswer[parent] = message.name
            }
        }
        var waiting: [Item.ID: String] = [:]
        var offered: [Item.ID: ToldThing] = [:]
        var shown: [String: Item.ID] = [:]
        let made = read.compactMap { message -> Item? in
            guard causes[message.name] == nil else { return nil }
            var item = piece(of: message, in: chat, isFirst: message.id == read.first?.id)
            let cause = message.fields["parent"]?.string.flatMap { causes[$0] == nil ? nil : $0 }
            if let cause, let label = causes[cause], !label.isEmpty { item.task = TaskLine(task: .init(cause), title: label) }
            // A thing told names the message itself, or the cause's message it is the answer to.
            let answers = cause.flatMap { lastAnswer[$0] == message.name ? toldByMessage[$0] : nil }
            guard let told = toldByMessage[message.name] ?? answers else { return item }
            if answers != nil, case .message(let said) = item.content, said.author == .assistant {
                switch told.kind {
                case .reminder:
                    item.content = .reminder(said.text)
                case .brief:
                    // A brief carries its own line.
                    item.content = .brief(Brief(title: told.title, text: said.text))
                    item.task = nil
                case .news, .question:
                    break
                }
            }
            item.isImportant = told.isImportant
            // Why it did not notify, what is offered under its notice, how often it was raised again.
            TiloSource.mark(&item, as: told)
            item.toldAs = closedKey?.name(of: told.id)
            shown[told.id] = item.id
            if !item.offers.isEmpty { offered[item.id] = told }
            if told.reaches, !told.isSettled { waiting[item.id] = told.id }
            return item
        }
        toldPieces[chat] = waiting
        toldOffered[chat] = offered
        if chat == .main {
            toldShown = shown
            saySince()
        }
        return made
    }

    // MARK: Told while the app is closed

    /// Apple's service gave this device an address: the person's server is handed it, with the
    /// device's own key, at every launch and whenever the system gives a new one. Only an
    /// address for notifications is handed over, and only in a build that has the push right.
    public func register(_ address: PushAddress) async {
        guard closedKeys != nil, address.use == .notices else { return }
        self.address = address
        addressHanded = false
        await handAddress()
    }

    /// The person's server forgets this device's address, where it was handed one.
    public func unregister() async {
        address = nil
        addressHanded = false
        handing.settle()
        guard closedKeys != nil else { return }
        try? await engine.unregisterAddress()
    }

    /// Hand the person's server the address with the device's key, once the device is in. The
    /// key is the one this device keeps since its person signed in, or a new one where it
    /// keeps none; a key that could not be kept is handed to nobody, because nothing could
    /// open what is sealed for it later.
    private func handAddress() async {
        guard let closedKeys, let address, !addressHanded, person != nil, case .open = engine.state else { return }
        var key = closedKey ?? closedKeys.read()
        if key == nil {
            let made = ClosedKey.made()
            guard (try? closedKeys.keep(made)) != nil else { return }
            key = made
        }
        guard let key else { return }
        closedKey = key
        let handed = key.handedOver
        do {
            try await engine.registerAddress(token: address.token, service: address.service.rawValue, app: address.app,
                                             p256dh: handed.p256dh, auth: handed.auth)
            addressHanded = true
            handing.settle()
            // The pieces already told now carry the names a push gives them.
            await look()
        } catch TiloEngine.Problem.refused(let code, _) where TiloSource.offersNone(code) {
            // A server that takes no address is asked again when the device is in anew.
            handing.settle()
        } catch TiloEngine.Problem.notOpen {
            // The page is on its way in again: the address is handed over when it is in.
            handing.settle()
        } catch {
            // Not answered, or their server failed at it this time. Until it has the address it
            // pushes nothing to this app while it is closed: the hand-over is made again by
            // itself, after a pause that grows.
            handing.later(first: firstPause) { [weak self] in await self?.handAddress() }
        }
    }

    private func forgetClosedKey() {
        closedKeys?.forget()
        closedKey = nil
        addressHanded = false
    }

    /// The person saw this piece on this device. Where their server told them of it, the
    /// server hears so, once: it then tells them of it nowhere else.
    public func seen(_ item: Item.ID, in chat: Chat) async {
        guard let told = toldPieces[chat]?[item] else { return }
        toldPieces[chat]?[item] = nil
        await saySeen(told, by: "opened")
    }

    /// Tell her server that she saw a thing it told her of, or acted on it: `how` says which.
    /// Word her server did not take is not lost: it is said again by itself, after a pause that
    /// grows, until her server has heard it. Until then her server would go on raising a thing
    /// that insists, and tell her of it on her other devices, though she saw it here.
    private func saySeen(_ told: String, by how: String) async {
        // What she pressed says more than that she looked.
        if unheard[told] == nil || how != "opened" { unheard[told] = how }
        await sayUnheard()
    }

    private func sayUnheard() async {
        guard !saysSeen else { return }
        saysSeen = true
        defer { saysSeen = false }
        var owed = false
        // Word that came while another was on its way is said in the next round.
        while !owed, !unheard.isEmpty {
            for (told, how) in unheard.sorted(by: { $0.key < $1.key }) {
                // What her server's log calls settled meanwhile needs no word any more: she saw
                // it on another device, or what it told was taken back.
                if toldByMessage.values.contains(where: { $0.id == told && $0.isSettled }) {
                    unheard[told] = nil
                    continue
                }
                do {
                    try await engine.acknowledge(told: told, by: how, device: deviceName)
                    unheard[told] = nil
                } catch TiloEngine.Problem.refused(let code, _) where TiloSource.offersNone(code) {
                    // A server that takes no such word is not told again.
                    unheard[told] = nil
                } catch {
                    // Not answered, or her server failed at it this time.
                    owed = true
                }
            }
        }
        guard owed else { return hearing.settle() }
        hearing.later(first: firstPause) { [weak self] in await self?.sayUnheard() }
    }

    /// The person pressed one of the things their server offers with a piece, where they were
    /// told of it. The action is invoked as the person on this device, with the arguments
    /// their server gave and no others, and their server hears which one it was.
    public func take(_ offer: Offer.ID, of item: Item.ID, in chat: Chat) async throws {
        guard let told = toldOffered[chat]?[item], let wanted = told.offers.first(where: { $0.id == offer.value }),
              TiloSource.mayPress(wanted)
        else { throw SourceProblem.refused("This is no longer offered") }
        if wanted.action == "answer-question", let message = wanted.arguments["message"]?.string,
           let words = wanted.arguments["answer"]?.string {
            // A question's answer goes the way the app's own answer to it goes.
            try await answer(message, words, detail: nil)
        } else {
            try await taken { try await self.engine.invoke(wanted.action, on: wanted.resource, wanted.arguments.object ?? [:]) }
        }
        toldPieces[chat]?[item] = nil
        await saySeen(told.id, by: wanted.id)
    }

    private func piece(of message: ChatMessage, in chat: Chat, isFirst: Bool) -> Item {
        let id = Item.ID(message.id)
        cards.see(message, as: id)
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
            // A service their assistant proposes to connect is its card (Connections.swift).
            if let asked = TiloSource.connectionAsk(of: message) {
                let (offer, askAgain) = connected.card(asked, of: message.id, text: message.text, site: TiloSource.placeName(asked.address))
                // Their server added it on an answer given elsewhere: its list is asked once more.
                if askAgain { Task { [weak self] in await self?.survey(.server) } }
                return Item(id: id, at: message.at, .connection(offer))
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
        // The view a message carries stands in it live, named by its message: its text stays in Tilo.
        if live.isEmpty, let view = fields["view"], !view.isNull {
            let apps = (view["reads"]?.array ?? []).compactMap(\.string).map { $0.replacingOccurrences(of: "_", with: " ").capitalized }
            live.append(AssistantCore.LiveThing(id: .init(message.id), title: apps.isEmpty ? message.text : apps.joined(separator: ", "),
                                                reference: "view:\(message.name):\(message.conversation)", sizesItself: true))
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
        return Item(id: id, at: message.at,
                    .message(Message(author, message.text, attachments: TiloFiles.attachments(of: message), live: live,
                                     keyFault: author == .assistant ? TiloSource.keyFault(of: fields["work"]) : nil)))
    }

    /// Why a reply's model gave no answer, where another key or another provider mends it: read
    /// from the typed cause their server keeps with the reply's work (`failure_cause`: the kind
    /// as the provider configuration names it, and the provider by the id and the name their
    /// server gives it). Never from the reply's words, and nil where their server says no cause.
    static func keyFault(of work: JSON?) -> Message.KeyFault? {
        guard let cause = work?["failure_cause"], !cause.isNull, let kind = variant(cause["kind"]) else { return nil }
        switch kind {
        case "no_provider":
            return .noProvider
        case "authentication", "permission_denied", "credits_exhausted":
            // A key's fault is some provider's: without one named there is nothing to type a key for.
            guard let id = cause["provider"]?["id"]?.string, !id.isEmpty else { return nil }
            let name = cause["provider"]?["name"]?.string ?? ""
            return .key(.init(id), name: name.isEmpty ? id : name)
        default:
            return nil
        }
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
        // A message written while the device is still on its way in waits for the way in to end.
        await tried()
        if chat == .main { await found() }
        let to: ChatRecipient
        switch chat {
        case .main:
            // Where no try got in, nobody was asked who the assistant is: Tilo did not answer.
            guard let recipient else {
                // A device that is nobody's any more, its workspace gone or its person signed out, sends nothing.
                if person == nil { throw SourceProblem.signedOut }
                throw reaches.now == .unreachable ? SourceProblem.unreachable : noAssistant
            }
            to = recipient
        case .subAgent(let agent):
            guard let id = feeds[chat]?.conversation else { throw SourceProblem.refused("This sub-agent has no conversation yet") }
            to = ChatRecipient(.conversation, id: id, name: agent.value)
        }
        try await taken {
            // The picked files go into the person's workspace first, a piece at a time. One
            // message then carries the words and names each file. A file that did not go in
            // whole sends nothing.
            let carried = try await TiloFiles.carried(draft.files, through: self.engine)
            _ = try await self.engine.chatSend(draft.text, to: to, carrying: carried)
        }
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
        // An event's card is answered on the event itself, at the person's server.
        if let event = EventCards.event(of: question) { return try await answer(event: event, with: choice) }
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

    /// What a message asks about a service its person may connect, where it asks that: the
    /// card their server keeps beside the question's words.
    private static func connectionAsk(of message: ChatMessage) -> ConnectionAsk? {
        guard let question = message.fields["question"], !question.isNull, variant(question["kind"]) == "action" else { return nil }
        return ConnectionAsk(detail: detail(of: question), answered: variant(question["answered"]?["answer"]),
                             outcome: variant(question["outcome"]?["outcome"]), why: question["outcome"]?["detail"]?.string ?? "",
                             withdrawn: !(question["withdrawn"]?.isNull ?? true))
    }

    private func connectionAsk(_ offer: ConnectionOffer.ID) async throws -> ConnectionAsk {
        guard let asked = try await engine.chatMessage(offer.value).flatMap(TiloSource.connectionAsk(of:)) else {
            throw TiloEngine.Problem.refused(code: "gone", reason: "This question is no longer there")
        }
        return asked
    }

    /// The person's own yes to a service their assistant proposes: Chat's answer to the
    /// question, on which their server adds what it looked at when it asked. Where the service
    /// wants a sign-in, the page it happens on is answered: the service's own, for the
    /// system's sign-in sheet. Nothing of a sign-in is typed into this app, and nothing of it
    /// reaches the conversation or the assistant.
    public func authenticate(_ offer: ConnectionOffer.ID) async throws -> URL? {
        var page: URL?
        var failure: (any Error)?
        // The press reads their server's list itself: no card asks for it meanwhile.
        connected.press(offer.value, begins: true)
        do {
            try await taken { page = try await self.connect(offer) }
        } catch {
            failure = error
        }
        // However it ended, their server's list says how the service stands now.
        await survey(.server)
        connected.press(offer.value, begins: false)
        if let failure { throw failure }
        return page
    }

    /// Their yes, then their server's word that it added the service, then the page of its
    /// sign-in where it wants one. Nil where nothing is to be signed in to.
    private func connect(_ offer: ConnectionOffer.ID) async throws -> URL? {
        var asked = try await connectionAsk(offer)
        if asked.waits { try await answer(offer.value, "allow", detail: nil) }
        // Their server carries their yes out and says so on the question, which is read again
        // until it does: for as long as `connected.patience`, and no longer.
        let patient = ContinuousClock.now + connected.patience
        while !asked.added {
            if asked.failed {
                throw TiloEngine.Problem.refused(code: "failed", reason: asked.why.isEmpty ? "Tilo could not connect this service" : asked.why)
            }
            if asked.withdrawn || (asked.answered != nil && asked.answered != "allow") {
                throw TiloEngine.Problem.refused(code: "settled", reason: "This question no longer waits for an answer")
            }
            guard ContinuousClock.now < patient else {
                throw TiloEngine.Problem.refused(code: "not-yet", reason: "Tilo has not connected this service yet. Try again in a moment.")
            }
            try await Task.sleep(for: .milliseconds(250))
            asked = try await connectionAsk(offer)
        }
        guard let kept = try await engine.connections().first(where: { $0.address == asked.address }) else {
            throw TiloEngine.Problem.refused(code: "gone", reason: "This service is no longer among your connections")
        }
        guard kept.needsSignIn else { return nil }
        let page = try await engine.beginSignIn(to: kept.id, returnTo: SignInReturn.address)
        connected.signingIn[offer] = kept.id
        return page
    }

    /// The person signed in at the service's own page, and the system's sign-in sheet came
    /// back with this address. It opens the sign-in once: it goes into the one call that
    /// carries it to their server, as what a protected field holds, and is kept nowhere here.
    public func signedIn(_ offer: ConnectionOffer.ID, at returned: URL) async throws {
        guard let connection = connected.signingIn.removeValue(forKey: offer) else {
            throw SourceProblem.refused("No sign-in was begun from this card")
        }
        var failure: (any Error)?
        do {
            try await taken { try await self.engine.finishSignIn(to: connection, returned: Protected(returned.absoluteString)) }
        } catch {
            failure = error
        }
        await survey(.server)
        if let failure { throw failure }
    }

    /// The person will not connect this service now: Chat's answer to the question, on which
    /// their server adds nothing. Where they had said yes and then left the sign-in, the
    /// service their server added on that yes is taken out again, as their own act. One that
    /// was ever signed in to is never removed from a card.
    public func decline(_ offer: ConnectionOffer.ID) async throws {
        var read: ConnectionAsk?
        try await taken { read = try await self.connectionAsk(offer) }
        guard let asked = read else { return }
        if asked.waits { return try await answer(offer.value, "deny", detail: nil) }
        guard asked.added else { throw SourceProblem.refused("This question no longer waits for an answer") }
        connected.signingIn[offer] = nil
        var failure: (any Error)?
        do {
            try await taken {
                guard let kept = try await self.engine.connections().first(where: { $0.address == asked.address }),
                      kept.state == "needs-sign-in"
                else { return }
                try await self.engine.removeConnection(kept.id)
                // The list that is read next no longer holds it: the card says so then.
                self.connected.readAgain(for: offer.value)
            }
        } catch {
            failure = error
        }
        await survey(.server)
        if let failure { throw failure }
    }

    /// How a file's link is fetched to this device: the system's own download. A check hands
    /// in one of its own making.
    var download: TiloFiles.Download = TiloFiles.download

    public func file(of attachment: Attachment.ID) async throws -> URL {
        var fetched: URL?
        try await taken { fetched = try await TiloFiles.fetch(attachment, through: self.engine, by: self.download) }
        guard let fetched else { throw SourceProblem.unreachable }
        return fetched
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

    /// Whether the tasks are being read right now, and whether they are wanted once more after.
    private var readsTasks = false
    private var tasksWanted = false
    /// The read of the tasks, made again where it went unanswered.
    private let tasksAgain = Again()

    /// Read the assistant's tasks again: the issues of the workspace's tracker that are given
    /// to it, as the workspace's server lists them to the person on this device.
    ///
    /// Nothing tells a native app that Tasks changed, and her server is not asked on a clock.
    /// The list is read when the device gets in, when this app changed something there, when
    /// her server told her something and when an answer in a conversation ended: after each of
    /// these a task may have been made or moved on. A read that went unanswered, or that her
    /// server failed at, is made again by itself after a pause that grows, and what was listed
    /// stands meanwhile. A server that keeps no tracker, or offers no such read, lists nothing.
    private func readTasks() async {
        guard case .open = engine.state, let assistant else { return }
        guard !readsTasks else {
            tasksWanted = true
            return
        }
        readsTasks = true
        defer { readsTasks = false }
        repeat {
            tasksWanted = false
            do {
                let rows = try await engine.issues(givenTo: assistant)
                // Whoever signed out meanwhile is listed nothing of the person before.
                guard self.assistant == assistant else { return }
                taskList.set(TiloTasks.listed(rows))
                tasksAgain.settle()
            } catch TiloEngine.Problem.refused(let code, _) where TiloSource.offersNone(code) {
                tasksAgain.settle()
            } catch TiloEngine.Problem.notOpen {
                // The page is on its way in again: the tasks are read anew when it is in.
                tasksAgain.settle()
            } catch {
                tasksAgain.later(first: firstPause) { [weak self] in await self?.readTasks() }
            }
        } while tasksWanted
    }

    /// Give the assistant a task: an issue of the workspace's tracker, filed by the person and
    /// given to their assistant through Tasks' own action. Their server starts the work.
    public func addTask(_ title: String) async throws {
        let wanted = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !wanted.isEmpty else { return }
        guard let assistant else { throw noAssistant }
        do {
            try await engine.giveIssue(wanted, to: assistant)
        } catch TiloEngine.Problem.refused(let code, let reason) {
            // A server that keeps no tracker says so in words a person reads.
            if TiloSource.offersNone(code) { throw SourceProblem.refused("Tilo keeps no tasks for your assistant in this workspace yet") }
            throw SourceProblem.refused(reason.isEmpty ? code : reason)
        }
        await readTasks()
    }

    public nonisolated func automations() -> AsyncStream<[Automation]> { automationList.stream() }

    /// The person's events as their server last listed them, and where each stands as a card.
    private var cards = EventCards()

    /// Take the person's events as their server listed them: the Tasks panel's list, and the
    /// cards of the main chat.
    private func take(events listed: [JSON]) {
        cards.take(listed)
        automationList.set(cards.automations)
        sayCards()
    }

    /// Ask the person's server for their events alone: when an answer in the main chat ended,
    /// which may have set one up, changed one or removed one. Never on a clock.
    private func readEvents() async {
        guard case .open = engine.state,
              let listed = try? await engine.ask(.events, "list", of: "events")["events"]?.array
        else { return }
        take(events: listed)
    }

    /// Say the main chat again where its cards are different now: whole, so that a card
    /// stands under its answer and not at the foot.
    private func sayCards() {
        guard var feed = feeds[.main], feed.told else { return }
        let now = cards.carded(feed.held)
        guard now != feed.held else { return }
        feed.held = now
        feeds[.main] = feed
        for listener in feed.listeners.values { listener.yield(.all(now)) }
    }

    /// Carry out what the person pressed on an event's card, as the person, on the event at
    /// their server. The card changes by what the server answered, never by the press alone.
    private func answer(event: Automation.ID, with choice: Choice.ID) async throws {
        // Whatever was carried out before something failed is said too.
        defer {
            automationList.set(cards.automations)
            sayCards()
        }
        let named: [String: JSON] = ["event": .string(event.value)]
        switch choice.value {
        case EventCards.show:
            // Read fresh: what is shown is what the event keeps now.
            var listed: [JSON]?
            try await taken { listed = try await self.engine.ask(.events, "list", of: "events")["events"]?.array }
            if let listed { cards.take(listed) }
            guard cards.show(event) else { throw SourceProblem.refused("This event is gone") }
        case EventCards.ask:
            // Every permission to act without asking is taken back: its work asks each time.
            guard let permits = cards.permits(of: event) else { throw SourceProblem.refused("This event is gone") }
            try await taken {
                for rule in permits {
                    let view = try await self.engine.ask(.events, "withdraw-permission", of: "events",
                                                         named.merging(["rule": rule]) { _, new in new })
                    self.cards.take(event: view)
                }
            }
            cards.asked(event)
        case EventCards.undo:
            // Removed for good at the server, which answers the events that remain.
            var remain: JSON = .null
            try await taken { remain = try await self.engine.ask(.events, "remove", of: "events", named) }
            cards.undid(event)
            if let listed = remain["events"]?.array { cards.take(listed) }
        default:
            throw SourceProblem.refused("This is not one of the answers")
        }
    }

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
        return .needsProvider(linkable(beside: linked))
    }

    /// The providers a key can be typed for. A provider the person added already stands first,
    /// under the name their server gives it and said to be linked where it has its key; the
    /// rest of the catalogue follows.
    private static func linkable(beside linked: ModelProviders) -> [ProviderChoice] {
        var linkable = linked.providers.map { ProviderChoice(id: .init($0.id), name: $0.name, isLinked: $0.hasKey) }
        for (id, name) in catalogue where !linkable.contains(where: { $0.id.value == id || $0.name == name }) {
            linkable.append(ProviderChoice(id: .init(id), name: name))
        }
        return linkable
    }

    /// What the provider step offers a person who comes back to it for another key or another provider.
    public func providers() async throws -> [ProviderChoice] {
        var read: [ProviderChoice] = []
        try await taken { read = TiloSource.linkable(beside: try await self.engine.modelProviders()) }
        return read
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
        // Whether one of her server's asks went unanswered, where they were made.
        var unanswered: Bool?
        defer { if let unanswered { serverAsked(unanswered: unanswered) } }
        while let now = next {
            surveyAgain = nil
            var seen = supervisionNow.now
            if now != .server { await surveyRecords(&seen) }
            if now != .records { unanswered = await surveyServer(&seen) }
            seen.learned = learnedOnHost + learnedInChat
            seen.learnedCount = max(seen.learned.count, seen.learnedCount)
            // The one workspace the assistant works in. Nothing is said of any other of the person's.
            seen.workspaces = worksIn
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
                    runsOn: TiloSource.place(of: sub),
                    started: Date(timeIntervalSince1970: (agent["created_at"]?.number ?? 0) / 1000),
                    startedBy: sub["started_by"]?.string == assistant ? .mainAgent : .person,
                    finished: state == .done ? since : nil))
                if let channel = sub["conversation"]?.string, feeds[.subAgent(.init(id))]?.conversation != channel {
                    feeds[.subAgent(.init(id)), default: Feed()].conversation = channel
                    again = true
                }
            }
            agents.set(listed.sorted { $0.started > $1.started })
            // What the assistant itself used today, as its server counted it, and the most it may spend.
            if let record = all.first(where: { $0["id"]?.string == assistant }) {
                seen.usage = assistantDay.usage(of: record, before: seen.usage)
                seen.isOn = TiloSource.variant(record["state"]) == "active"
                seen.places = AssistantPlaces.places(among: all, assistant: assistant, keptBy: me)
                // A day turns with nothing written anywhere: what was counted is read again then.
                assistantDay.atNextDay { [weak self] in await self?.survey(.records) }
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

    /// Her server's asks beside the conversation are made when the device gets in and when the
    /// app changed something there. Where one of them went unanswered, all are made again by
    /// themselves after a pause that doubles each time, up to a minute: one late or lost answer
    /// leaves no step of the main chat unoffered, and no list empty, until the next way in.
    private func serverAsked(unanswered: Bool) {
        guard unanswered else { return serverAgain.settle() }
        serverAgain.later(first: firstPause) { [weak self] in await self?.survey(.server) }
    }

    /// One ask of her server beside the conversation. A server that offers no such thing is
    /// passed over. An ask that went unanswered, that her server failed at this time, or whose
    /// answer has nothing in it is counted, to be made again: what was shown stays meanwhile.
    private func answered<Value: Sendable>(_ unanswered: inout Bool, _ ask: @MainActor () async throws -> Value) async -> Value? {
        do {
            let value = try await ask()
            if let said = value as? JSON, said.isNull {
                unanswered = true
                return nil
            }
            return value
        } catch TiloEngine.Problem.refused(let code, _) where TiloSource.offersNone(code) {
            return nil
        } catch TiloEngine.Problem.notOpen {
            // The page is on its way in again: all of it is asked anew when it is in.
            return nil
        } catch {
            unanswered = true
            return nil
        }
    }

    /// Says whether one of the asks went unanswered.
    private func surveyServer(_ seen: inout Supervision) async -> Bool {
        var unanswered = false
        // The person's models: which one the main chat answers on.
        if let linked = await answered(&unanswered, { try await self.engine.modelProviders() }) {
            choices = TiloSource.choices(of: linked)
            seen.model = linked.model(for: WorkKind.request.rawValue).map { chosen in
                chosen.provider.models.first(where: { $0.id == chosen.model })?.label ?? chosen.model
            }
            // What the main chat says of it: none linked, none chosen, or nothing in the way.
            providerSetup = TiloSource.setup(of: linked, choices: choices)
            saySetup()
        }
        // What it can sign in to: the vault's items, by where each belongs. Never a secret.
        if let items = await answered(&unanswered, { try await self.engine.vaultItems() }) {
            seen.logins = items.map {
                Supervision.Login(id: .init($0.reference), name: TiloSource.placeName($0.belongsTo.first ?? $0.kind), typed: $0.added,
                                  lastUsed: $0.lastUsed)
            }
        }
        // What the agents the host runs have learned about the person, newest first.
        if let facts = await answered(&unanswered, { try await self.engine.ask(.learned, "list", of: "facts") })?["facts"]?.array {
            learnedOnHost = facts.compactMap { fact in
                guard let id = fact["id"]?.string, let text = fact["text"]?.string else { return nil }
                return Supervision.Learned(id: .init(TiloSource.onHost + id), text: text)
            }
        }
        // What is important, in the person's own words, and the situations those words depend on.
        if let rule = await answered(&unanswered, { try await self.engine.ask(.telling, "read", of: "importance") }) {
            seen.importance = (rule["lines"]?.array ?? []).compactMap { $0["words"]?.string }
            seen.situations = (rule["facts"]?.array ?? []).compactMap { fact in
                guard let name = fact["name"]?.string else { return nil }
                return Supervision.Situation(name: name, value: fact["value"]?.string ?? "")
            }
        }
        // What comes at a time and what happens when something else does.
        if let events = await answered(&unanswered, { try await self.engine.ask(.events, "list", of: "events") })?["events"]?.array {
            take(events: events)
        }
        // The services they connected by their address: the assistant view's line of each, and
        // how each one's card in a conversation stands (Connections.swift).
        let awaited = connected.awaited
        if let listed = await answered(&unanswered, { try await self.engine.connections() }) {
            seen.connections = Connected.lines(of: listed, site: TiloSource.placeName)
            if connected.heard(listed, for: awaited) { await look() }
        }
        // The assistant's tasks: the issues of the workspace's tracker that are given to it.
        await readTasks()
        return unanswered
    }

    private var noAssistant: SourceProblem { .refused("You have no assistant in this workspace yet") }

    /// The workspace the person's assistant works in, as the assistant view names it: the one
    /// this page stands in once the assistant was found there, by its own name, or as the
    /// person's own where their server said so and the page gives it no name.
    private var worksIn: [String] {
        guard assistant != nil else { return [] }
        if let name = person?.workspace, !name.isEmpty { return [name] }
        return standsInOwn ? [AssistantWords.ownWorkspace.said] : []
    }

    // MARK: Plumbing

    /// Run a call and say a failure the way the views read one.
    private func taken(_ call: @MainActor () async throws -> Void) async throws {
        await tried()
        do {
            try await call()
        } catch TiloEngine.Problem.refused(let code, let reason) {
            throw SourceProblem.refused(reason.isEmpty ? code : reason)
        } catch is TiloEngine.Problem {
            throw person == nil ? SourceProblem.signedOut : SourceProblem.unreachable
        }
    }
}

/// Something that is done again by itself where it did not go through: after a pause that
/// doubles each time, up to a minute, so that a busy server is not asked without end.
@MainActor
final class Again {
    private var pause: Duration?
    private var waiting: Task<Void, Never>?

    nonisolated init() {}

    /// It went through, or is wanted no more: nothing is done again, and the next pause is the first.
    func settle() {
        waiting?.cancel()
        waiting = nil
        pause = nil
    }

    /// It did not go through: `again` is done after the next pause, unless `settle` comes first.
    func later(first: Duration, _ again: @escaping @MainActor () async -> Void) {
        waiting?.cancel()
        let wait = pause ?? first
        pause = min(wait * 2, .seconds(60))
        waiting = Task {
            try? await Task.sleep(for: wait)
            guard !Task.isCancelled else { return }
            // What is done again is not cut short where it settles this, or asks for itself again.
            await Task { await again() }.value
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
        return AnyView(TiloLiveView(live, on: stage, size: size, says: TiloSource.said))
    }

    /// What a live thing's place says where its thing is not shown, in the app's own sentences.
    static func said(_ trouble: LiveStage.Trouble) -> String {
        switch trouble {
        case .notOffered: AssistantWords.viewNotOffered.said
        case .notShown: AssistantWords.liveNotShown.said
        }
    }
}
