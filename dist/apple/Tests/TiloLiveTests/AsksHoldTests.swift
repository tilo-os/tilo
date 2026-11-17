#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// The asks the app makes of its person's server, answered late, with nothing in them, a second
/// time, after the page started over, or not at all: against a page that speaks as Tilo's does
/// and a server that answers each kind of ask as the test plans it. No window is opened,
/// nothing is fetched, nothing kept.
@MainActor
final class AsksHoldTests: XCTestCase {
    private static let site = "https://tilo.test"
    private var source: TiloSource!
    private var suite = ""
    private var sessions: [Session] = []
    private var changes: [ConversationChange] = []
    private var setups: [Setup] = []
    private var listening: [Task<Void, Never>] = []

    private func start(_ mainChat: TiloSource.MainChat = .assistant, livesInOwn: Bool = false) async throws {
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { _ in }
        source = TiloSource(engine: engine, mainChat: mainChat, livesInOwn: livesInOwn, defaults: defaults)
        let source = source!
        listening = [
            Task { for await session in source.session() { self.sessions.append(session) } },
            Task { for await change in source.conversation(.main) { self.changes.append(change) } },
            Task { for await setup in source.setup() { self.setups.append(setup) } },
        ]
        engine.view.loadHTMLString(Self.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page is there") {
            (try? await engine.view.callAsyncJavaScript("return typeof window.enter", arguments: [:], in: nil, contentWorld: .page)) as? String == "function"
        }
    }

    override func tearDown() async throws {
        for task in listening { task.cancel() }
        UserDefaults.standard.removePersistentDomain(forName: suite)
        source = nil
    }

    /// As Tilo's page that shows Chat alone, with its device's entry handed to the app: its mount
    /// reads one conversation's messages and takes Chat's send; behind the entry her server keeps
    /// the workspace's agents, her providers and what it told her.
    ///
    /// `window.plan` says how the next asks of one kind are answered, taken from the front: of
    /// her server `'chat read'`, `'chat create-agent'`, `'chat answer-for'` (the ask that has
    /// the server answer for an agent), `'tilo.models list'` and so on, `'own'` (which workspace
    /// is her own); of the page `'messages'` and `'send'`. An ask the plan says nothing of is
    /// answered as usual.
    ///   `{late: ms}`    as usual, that much later
    ///   `'unanswered'`  carried out, and answered only that it was taken
    ///   `'lost'`        answered only that it was taken, and never carried out
    ///   `'empty'`       answered with nothing in it
    ///   `'failed'`      answered that her server failed at it: an ask it knows, this time
    ///   `'never'`       not answered at all
    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status"></span><div id="semantics"></div>
    <script>
      const root = document.documentElement;
      const ADA = 'ad'.repeat(32);
      const DIRECT = 'direct:[{"person":"' + ADA + '"},{"agent":"assistant"}]';
      const hex = text => [...text].map(letter => letter.charCodeAt(0).toString(16).padStart(2, '0')).join('');
      window.touch = () => { document.getElementById('semantics').textContent = String(Math.random()); };
      const said = value => ({state: 'completed', output: {value}});
      const done = value => ({invocation: 'i', state: 'succeeded', output: {value, resources: []}});
      const noSuch = () => { throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'}); };
      window.messages = []; window.sent = []; window.asks = []; window.told = []; window.acknowledged = [];
      window.agents = null; window.linked = true; window.published = true; window.plan = {};
      const next = kind => (window.plan[kind] ?? []).shift();
      // An ask that is never answered: its answer stays owed. (The system itself ends an ask
      // whose answer nothing could give any more; her server's is owed for as long as the page lives.)
      window.owed = [];
      const never = () => new Promise(answer => { window.owed.push(answer); });
      const planned = async (kind, usual) => {
        const how = next(kind);
        if (how === 'never') return never();
        if (how === 'lost') return {invocation: 'i', state: 'accepted'};
        if (how === 'empty') return done(null);
        if (how === 'failed') return {invocation: 'i', state: 'failed', error: {code: 'write_timeout', message: 'The write did not finish in time'}};
        if (how?.late) await new Promise(on => setTimeout(on, how.late));
        const answer = await usual();
        return how === 'unanswered' ? {invocation: 'i', state: 'accepted'} : answer;
      };
      window.post = message => {
        window.messages.push({ts: 1794900000 + window.messages.length, conversation: DIRECT, question: null, share: null, attachments: [], ...message});
        window.touch();
      };
      window.tell = thing => { window.told = window.told.filter(one => one.id !== thing.id).concat([thing]); window.logMoved?.(); };
      // An agent as her server says one, whole.
      window.kept = (id, more = {}) => ({id, name: id, keeper: {person: ADA}, mcp_client: null, sub: null,
        state: {$variant: 'active'}, created_at: 1794900000000, ...more});
      const device = Object.freeze({
        person: ADA, workspace: 'ws-1',
        catalogue: async () => ({actions: []}), status: async invocation => ({invocation, state: 'succeeded'}),
        ownWorkspace: async () => {
          window.asks.push('own');
          const how = next('own');
          if (how === 'never') return never();
          return how === 'empty' ? {} : {id: 'own-7', name: 'Mine', origin: 'https://tilo.example:8443'};
        },
        invoke: (target, argument) => {
          const action = target.address.action.startsWith('set-agent-') ? 'answer-for' : target.address.action;
          const kind = (target.provider.startsWith('tilo.') ? target.provider : 'chat') + ' ' + action;
          window.asks.push(kind);
          return planned(kind, async () => {
            switch (kind) {
              case 'chat read':
                if (!window.agents) noSuch();
                return done({agents: window.agents});
              case 'chat create-agent':
                window.agents.push(window.kept(argument.name.toLowerCase(), {name: argument.name, created_at: 1794900009000 + window.agents.length}));
                return done(null);
              case 'chat answer-for': {
                // Which agent, and the mark the app sets on it, kept as it was said.
                const {agent, ...marks} = argument;
                Object.assign(window.agents.find(one => one.id === agent), marks, {answeredFor: Object.values(marks).includes(true)});
                return done(null);
              }
              case 'chat retire-agent':
                window.agents.find(one => one.id === argument.agent).state = {$variant: 'retired'};
                return done(null);
              case 'tilo.models list':
                return done(window.linked
                  ? {providers: [{id: 'anthropic', name: 'Anthropic', protocol: 'anthropic', endpoint: 'https://api.anthropic.com', configured: true,
                      models: [{id: 'm-1', label: 'Model one'}], defaultModel: 'm-1'}], defaultProvider: 'anthropic', assignments: []}
                  : {providers: [], defaultProvider: null, assignments: []});
              case 'tilo.vault list': return done({items: []});
              case 'tilo.learned list': return done({facts: []});
              case 'tilo.telling read': return done({lines: [], facts: []});
              case 'tilo.events list': return done({events: []});
              case 'tilo.telling list': {
                // An ask that waits is held until her server's log moves.
                const newer = () => window.told.filter(one => one.seq > (argument?.after ?? 0));
                if (!newer().length && argument?.waitMs) await new Promise(moved => { window.logMoved = moved; setTimeout(moved, argument.waitMs); });
                return done({told: newer(), newest: Math.max(0, ...window.told.map(one => one.seq))});
              }
              case 'tilo.telling acknowledge':
                window.acknowledged.push(argument.told + ' ' + argument.by);
                return done(null);
              case 'tilo.telling since-you-left': return done({cameIn: [], didAlone: [], waits: []});
              default: return noSuch();
            }
          });
        },
      });
      window.leave = () => { root.dataset.entry = ''; root.dataset.arrival = 'load'; };
      window.enter = () => {
        // Tilo's page hands the entry of its device to the app's own script, which stands in the
        // document to take it (Tilo's page calls it by its name).
        const taker = Object.getOwnPropertyNames(globalThis).map(name => { try { return globalThis[name]; } catch { return null; } })
          .find(one => one && typeof one === 'object' && typeof one.device === 'function' && typeof one.name === 'string');
        taker.device(device);
        root.dataset.arrival = 'done';
        root.dataset.entry = 'open';
        globalThis.tiloMount = {
          semantics: document.getElementById('semantics'),
          state: () => ({workspaceName: 'Ada\'s workspace'}),
          // A page that just got in publishes Chat's messages a moment later (`published`).
          actions: () => [
            ...(window.published ? [{address: {resource: 'chain:messages', action: 'read'}, resource_kind: 'chat.messages',
              params: [{name: 'cursor'}, {name: 'pageSize'}, {name: 'fields'}, {name: 'where'}]}] : []),
            // Chat keeps no row for a person it was never told a name of (`rowless`).
            ...(window.rowless ? [] : [{address: {resource: 'chat.workspace/people/' + hex(ADA), action: 'read'}, resource_kind: 'chat.workspace.people'}])],
          invokeAction: async (resource, action, args) => {
            if (resource === 'chain:messages' && action === 'read') {
              const how = next('messages');
              if (how === 'never') return never();
              // The read names the one conversation it wants, under Chat's own name for that field.
              const [named, wanted] = Object.entries(args.where ?? {})[0] ?? [];
              const listed = how === 'empty' ? [] : [...window.messages].reverse().filter(m => m.conversation === wanted).slice(0, args.pageSize ?? 50);
              return said({kind: 'chat.messages', messages: listed.map(m => ({item: 'chat.messages/messages/' + m.id, name: m.text,
                ...Object.fromEntries((args.fields ?? []).map(field => [field, field === named ? m.conversation : m[field] ?? null]))}))});
            }
            if (resource === 'chat.workspace/people/' + hex(ADA) && action === 'read' && !window.rowless) return said({id: {person: ADA}, name: 'Ada Lovelace'});
            if (resource === 'chat.workspace' && action === 'send') {
              if (next('send') === 'never') return never();
              const id = 's' + window.messages.length;
              window.sent.push(args.text);
              window.post({id, author: {person: ADA}, text: args.text});
              return said({accepted: true, message: id, conversation: DIRECT});
            }
            throw new Error(`unknown_action: no action addressed by (${resource}, ${action})`);
          },
        };
      };
    </script></html>
    """#

    // MARK: What the tests are written with

    private func inPage(_ script: String) async throws {
        _ = try await source.engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page)
    }

    private func page(_ script: String, _ arguments: [String: Any] = [:]) async throws -> String {
        try await source.engine.view.callAsyncJavaScript(script, arguments: arguments, in: nil, contentWorld: .page) as? String ?? ""
    }

    private func until(_ what: String, within patience: Duration = .seconds(12), _ holds: @MainActor () async -> Bool) async throws {
        let end = ContinuousClock.now.advanced(by: patience)
        while ContinuousClock.now < end {
            if await holds() { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        XCTFail("waited \(patience) for \(what)")
        throw CancellationError()
    }

    /// The page gets in, in the workspace its link names or in her own.
    private func enter(_ before: String = "", own: Bool = false) async throws {
        let place = own ? "#/@tilo.example:8443/w/own-7/so.tilo.chat" : "#/w/ws-1/so.tilo.chat"
        try await inPage("\(before); location.hash = '\(place)'; enter()")
    }

    private func signedIn() async throws {
        try await until("the app is signed in") { if case .signedIn = self.sessions.last { true } else { false } }
    }

    /// The page starts over, as Tilo's does once after it let a device in, and is in again.
    private func startOver(_ before: String = "") async throws {
        try await inPage("leave()")
        try await until("the page is on its way in again") { if case .arriving = self.source.engine.state { true } else { false } }
        try await inPage("\(before); enter()")
        try await until("the page is in again") { if case .open = self.source.engine.state { true } else { false } }
    }

    private var held: [Item] {
        var conversation = Conversation()
        for change in changes { conversation.apply(change) }
        return conversation.items
    }

    private var saidNoAssistant: Bool {
        setups.contains { if case .noAssistant = $0 { true } else { false } }
    }

    /// How often her server, or the page, was asked one kind of thing.
    private func asked(_ kind: String) async throws -> Int {
        Int(try await page("return String(window.asks.filter(one => one === kind).length)", ["kind": kind])) ?? -1
    }

    /// The workspace's agents that are active, each with whether her server was told to answer for it.
    private func agentsKept() async throws -> String {
        try await page("return JSON.stringify(window.agents.filter(one => one.state.$variant === 'active').map(one => [one.id, one.answeredFor ?? false]))")
    }

    private static let assistant = TiloSource.MainChat.agent(id: "assistant", name: "Assistant")

    /// Her reminder comes due: what her server tells the assistant then, the assistant's answer
    /// to that, and what her server tells her of it.
    private static let reminderComes = """
        post({id: 'm_ask_1', author: {agent: 'e-9'}, text: 'Remind me to stretch. (for the assistant)',
          cause: {kind: 'reminder', label: 'Stretch', key: 'k-1', person: {person: ADA}}});
        post({id: 'm_reply_1', author: {agent: 'assistant'}, text: 'Time to stretch.', parent: 'm_ask_1'});
        tell({seq: 1, id: 'turn:ws-1:m_ask_1:0', atMs: 1794900000000, kind: 'reminder', title: 'Stretch', words: 'said',
          about: {workspace: 'ws-1', app: 'so.tilo.chat', resource: 'chat.messages/messages/m_ask_1'},
          mark: {important: false, why: {code: 'reminder'}}, reaches: true})
        """

    private static let twoMessages = """
        post({id: 'm1', author: {agent: 'assistant'}, text: 'Good morning.'});
        post({id: 'm2', author: {agent: 'assistant'}, text: 'Nine mails sorted.'})
        """

    private func isReminder() async throws {
        try await until("the answer is her reminder") { self.held.last?.content == .reminder("Time to stretch.") }
    }

    // MARK: Late

    func testAnswersThatComeMoreThanASecondLateOnTheWayInLeaveNoSentenceAndMakeNothingTwice() async throws {
        try await start()
        let late = "[{late: 1200}]"
        try await enter("""
            window.agents = []; window.plan = {'chat read': \(late), 'chat create-agent': \(late), 'chat answer-for': \(late),
              'tilo.models list': \(late), 'tilo.telling list': \(late)}
            """)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(saidNoAssistant, "a sentence was said on the way")
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"[["assistant",true]]"#)
        let made = try await asked("chat create-agent")
        XCTAssertEqual(made, 1)
        // What her server tells her is followed all the same.
        try await inPage(Self.reminderComes)
        try await isReminder()
    }

    // MARK: Finding and making her assistant

    func testAnAssistantMadeWhileThePageStartsOverIsMadeOnceAndNoSentenceIsSaid() async throws {
        try await start()
        // Her server makes it, and its answer comes after the page started over.
        try await enter("window.agents = []; window.plan = {'chat create-agent': [{late: 700}]}")
        try await until("her server is asked to make it") { ((try? await self.asked("chat create-agent")) ?? 0) == 1 }
        try await startOver()
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        try await Task.sleep(for: .milliseconds(600))
        XCTAssertEqual(setups.last, .ready)
        XCTAssertFalse(saidNoAssistant, "a sentence was said on the way")
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"[["assistant",true]]"#)
        let made = try await asked("chat create-agent")
        XCTAssertEqual(made, 1)
    }

    func testAnAnswerWithNothingInItToWhichWorkspaceIsHerOwnIsAskedAgainAndTheAssistantIsMadeThere() async throws {
        try await start(livesInOwn: true)
        try await enter("window.agents = []; window.plan = {own: ['empty']}", own: true)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(saidNoAssistant, "a sentence was said on the way")
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"[["assistant",true]]"#)
    }

    // MARK: The provider and the model

    // MARK: What her server tells her

    func testAfterThePageStartedOverWhatHerServerTellsHerIsFollowedAgain() async throws {
        try await start(Self.assistant)
        try await enter()
        try await signedIn()
        try await until("her server holds an ask") { ((try? await self.asked("tilo.telling list")) ?? 0) >= 2 }
        try await startOver()
        try await inPage(Self.reminderComes)
        try await isReminder()
        XCTAssertEqual(held.count, 1)
    }

    func testAThingHerServerListsASecondTimeUnchangedChangesNothingASecondTime() async throws {
        try await start(Self.assistant)
        try await enter()
        try await signedIn()
        try await inPage(Self.reminderComes)
        try await isReminder()
        let changed = { self.changes.filter { if case .changed = $0 { true } else { false } }.count }
        let once = changed()
        // The same thing again, under a new place in her server's log.
        try await inPage("tell({...window.told[0], seq: 2})")
        try await until("her server was asked for what is newer") { ((try? await self.asked("tilo.telling list")) ?? 0) >= 4 }
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(changed(), once)
        XCTAssertEqual(held.count, 1)
    }

    // MARK: The conversation

    func testAReadOfTheConversationThatComesBackEmptyOnceEmptiesNothingAndSaysNothingAgain() async throws {
        try await start(Self.assistant)
        try await enter(Self.twoMessages)
        try await signedIn()
        try await until("both stand in the main chat") { self.held.count == 2 }
        try await inPage("window.plan = {messages: ['empty']}; touch()")
        try await until("the read was answered with nothing") { (try? await self.page("return String(window.plan.messages.length)")) == "0" }
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertFalse(changes.contains { if case .removed = $0 { true } else { false } }, "one empty read emptied her conversation")
        try await inPage("touch()")
        try await Task.sleep(for: .milliseconds(400))
        XCTAssertEqual(held.count, 2)
        XCTAssertFalse(changes.contains { if case .added = $0 { true } else { false } }, "what she held was said again as new")
    }

    func testAPageThatStartsOverAndPublishesChatsMessagesAMomentLaterNeitherEmptiesTheConversationNorSaysItAgainAsNew() async throws {
        try await start(Self.assistant)
        try await enter(Self.twoMessages)
        try await signedIn()
        try await until("both stand in the main chat") { self.held.count == 2 }
        try await startOver("window.published = false; setTimeout(() => { window.published = true; touch(); }, 600)")
        try await Task.sleep(for: .milliseconds(1200))
        try await until("both stand in the main chat") { self.held.count == 2 }
        XCTAssertFalse(changes.dropFirst().contains(.all([])), "her conversation was said to be empty")
        XCTAssertFalse(changes.contains { if case .added = $0 { true } else { false } }, "what she held was said again as new")
    }

    func testASendWhosePageStartsOverUnderItIsSaidNotToHaveGoneAndNothingIsSentTwice() async throws {
        try await start(Self.assistant)
        try await enter()
        try await signedIn()
        try await until("the conversation is told") { !self.changes.isEmpty }
        try await inPage("window.plan = {send: ['never']}")
        let source = source!
        let sending = Task { @MainActor () -> SourceProblem? in
            do {
                try await source.send(Draft(text: "Hello"), to: .main)
                return nil
            } catch {
                return error as? SourceProblem
            }
        }
        try await until("Chat was asked to send") { (try? await self.page("return String(window.plan.send.length)")) == "0" }
        try await startOver()
        let problem = await sending.value
        XCTAssertEqual(problem, .unreachable)
        // She sends it again herself: it stands once.
        try await source.send(Draft(text: "Hello"), to: .main)
        let sent = try await page("return JSON.stringify(window.sent)")
        XCTAssertEqual(sent, #"["Hello"]"#)
    }

    /// As on every later start of the app: what her server told her of before is there already.
    func testAtAStartWhatHerServerToldHerBeforeIsAReminderFromTheFirstAndNoPieceTurnsIntoOne() async throws {
        try await start(Self.assistant)
        try await enter(Self.reminderComes)
        try await signedIn()
        try await isReminder()
        try await until("her server holds an ask") { ((try? await self.asked("tilo.telling list")) ?? 0) >= 2 }
        try await Task.sleep(for: .milliseconds(300))
        // A piece that turns into a reminder is told: yesterday's would be told at every start.
        XCTAssertFalse(changes.contains { if case .changed = $0 { true } else { false } }, "a piece she was told of before changed after her conversation was said")
        XCTAssertFalse(changes.contains { if case .added = $0 { true } else { false } }, "what she held was said again as new")
        XCTAssertEqual(held.count, 1)
    }

    func testAConversationThatStaysEmptyIsSaidEmptyAfterAWhile() async throws {
        try await start(Self.assistant)
        source.emptyDoubt = .milliseconds(300)
        try await enter(Self.twoMessages)
        try await signedIn()
        try await until("both stand in the main chat") { self.held.count == 2 }
        try await inPage("window.messages = []; touch()")
        try await until("the main chat is empty") { self.held.isEmpty }
    }

    /// As a server whose own writes do not finish in time: it knows the ask and fails at it.
    func testAFailedAnswerToWhatHerServerTellsHerDoesNotEndTheFollowingAndTheAskIsMadeAgain() async throws {
        try await start(Self.assistant)
        source.firstPause = .milliseconds(200)
        // The read on the way in fails, and so does the first of the following.
        try await enter(Self.twoMessages + "; window.plan = {'tilo.telling list': ['failed', 'failed']}")
        try await signedIn()
        try await until("her server was asked a third time") { ((try? await self.asked("tilo.telling list")) ?? 0) >= 3 }
        // Her reminder comes due: it is told.
        try await inPage(Self.reminderComes)
        try await isReminder()
        // It came as news, which is what is told: a new piece, or a piece that turned into the reminder.
        let asNews = changes.contains {
            switch $0 {
            case .added(let new): new.contains { $0.content == .reminder("Time to stretch.") }
            case .changed(let item): item.content == .reminder("Time to stretch.")
            default: false
            }
        }
        XCTAssertTrue(asNews, "the reminder came in no change that tells it")
    }

    /// As on the test site, whose people have no name: Chat keeps no row for them.
    func testAPersonChatKeepsNoRowForIsInAtOnceAndHerConversationIsSaidWithoutWaitingForHerName() async throws {
        try await start(Self.assistant)
        try await enter("window.rowless = true; " + Self.twoMessages)
        try await until("the app is signed in", within: .seconds(4)) { if case .signedIn = self.sessions.last { true } else { false } }
        try await until("both stand in the main chat", within: .seconds(4)) { self.held.count == 2 }
        guard case .signedIn(let person) = sessions.last else { return XCTFail("the app is not signed in") }
        XCTAssertEqual(person.name, "")
    }

    func testAPersonsNameThatChatPublishesAMomentLaterIsTakenWhenItComes() async throws {
        try await start(Self.assistant)
        try await enter("window.rowless = true; setTimeout(() => { window.rowless = false; touch(); }, 700); " + Self.twoMessages)
        try await until("both stand in the main chat", within: .seconds(4)) { self.held.count == 2 }
        try await until("she is signed in under her name") {
            if case .signedIn(let person) = self.sessions.last { person.name == "Ada Lovelace" } else { false }
        }
        XCTAssertEqual(held.count, 2)
    }

    func testWhereHerServerNeverSaysWhichWorkspaceIsHerOwnTheDeviceStillGetsInAndTheAskIsMadeAgain() async throws {
        try await start(livesInOwn: true)
        source.engine.askPatience = .milliseconds(400)
        try await enter("window.agents = []; window.plan = {own: ['never']}", own: true)
        try await signedIn()
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"[["assistant",true]]"#)
    }

    func testAnAnswerThatNeverComesOnTheWayInDoesNotKeepHerFromHearingWhatHerServerTellsHer() async throws {
        try await start(Self.assistant)
        source.engine.askPatience = .milliseconds(400)
        try await enter("window.plan = {'tilo.vault list': ['never']}")
        try await signedIn()
        try await inPage(Self.reminderComes)
        try await isReminder()
    }
}
#endif
