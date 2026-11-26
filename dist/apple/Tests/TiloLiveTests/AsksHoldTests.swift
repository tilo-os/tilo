#if os(macOS)
import AssistantClosed
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
    private var cards: [SinceLeft?] = []
    private var supervisions: [Supervision] = []
    private var scheduled: [[Automation]] = []
    /// How often the page said that something changed.
    private var pageWords = 0
    private var listening: [Task<Void, Never>] = []

    /// `again` is a later start of the same device: the app was closed and is opened, with
    /// what it kept the time before and nothing of what it said then.
    private func start(_ mainChat: TiloSource.MainChat = .assistant, livesInOwn: Bool = false,
                       closedKeys: (any ClosedKeyStore)? = nil, again: Bool = false) async throws {
        if again {
            for task in listening { task.cancel() }
            sessions = []
            changes = []
            setups = []
        } else {
            suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        }
        let defaults = UserDefaults(suiteName: suite)!
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { _ in }
        source = TiloSource(engine: engine, mainChat: mainChat, livesInOwn: livesInOwn, defaults: defaults, closedKeys: closedKeys)
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
    /// her server `'chat read'`, `'chat ensure-personal-assistant'`, `'tilo.models list'` and so on, `'own'` (which workspace
    /// is her own); of the page `'messages'` and `'send'`. An ask the plan says nothing of is
    /// answered as usual.
    ///   `{late: ms}`    as usual, that much later
    ///   `'unanswered'`  carried out, and answered only that it was taken
    ///   `'lost'`        answered only that it was taken, and never carried out
    ///   `'empty'`       answered with nothing in it
    ///   `'failed'`      answered that her server failed at it: an ask it knows, this time
    ///   `'never'`       not answered at all
    ///   `'gave-up'`     the page itself says that no answer came within its own time
    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status"></span><div id="semantics"></div>
    <script>
      \#(testDeviceHandoff)
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
      window.registered = []; window.unregistered = 0;
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
        if (how === 'gave-up') throw Object.assign(new Error('no answer came in time'), {code: 'unanswered'});
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
        person: ADA, get workspace() { return window.admittedTo ?? (location.hash.includes('/w/own-7/') ? 'own-7' : 'ws-1'); },
        catalogue: async () => ({actions: []}), status: async invocation => ({invocation, state: 'succeeded'}),
        ownWorkspace: async () => {
          window.asks.push('own');
          const how = next('own');
          if (how === 'never') return never();
          if (how?.late) await new Promise(on => setTimeout(on, how.late));
          return how === 'empty' ? {} : {id: 'own-7', name: 'Mine', origin: 'https://tilo.example:8443'};
        },
        invoke: (target, argument) => {
          const action = target.address.action;
          // What is asked of the workspace's tracker is no ask of Chat: this server keeps no tracker.
          const kind = (target.provider.startsWith('tilo.') ? target.provider : target.provider.includes(':so.tilo.tasks:') ? 'tasks' : 'chat') + ' ' + action;
          window.asks.push(kind);
          return planned(kind, async () => {
            switch (kind) {
              case 'chat read':
                if (!window.agents) noSuch();
                return done({agents: window.agents});
              case 'chat ensure-personal-assistant':
                if (!window.agents.some(one => one.id === 'assistant')) {
                  window.agents.push(window.kept('assistant', {name: 'Assistant', created_at: 1794900009000 + window.agents.length}));
                }
                return done(null);
              case 'tilo.models list':
                return done(window.linked
                  ? {providers: [{id: 'anthropic', name: 'Anthropic', protocol: 'anthropic', endpoint: 'https://api.anthropic.com', configured: true,
                      models: [{id: 'm-1', label: 'Model one'}], defaultModel: 'm-1'}], defaultProvider: 'anthropic', assignments: []}
                  : {providers: window.preset ? [window.preset] : [], defaultProvider: null, assignments: window.chosen ?? []});
              // A provider she links: added by its name, then its key, then its models are asked for.
              case 'tilo.models add-preset':
                window.preset = {id: argument.preset, name: 'Anthropic', protocol: argument.preset, endpoint: 'https://api.anthropic.com',
                  configured: false, models: []};
                return done(window.preset);
              case 'tilo.models set-key': window.preset.configured = true; return done(window.preset);
              case 'tilo.models refresh-models':
                window.preset.models = [{id: 'm-1', label: 'Model one'}];
                return done({provider: window.preset});
              case 'tilo.models set-assignments':
                window.chosen = (window.chosen ?? []).concat(argument.kinds.map(kind => ({kind, provider: argument.provider, model: argument.model})));
                return done({providers: [window.preset], defaultProvider: null, assignments: window.chosen});
              // What her server keeps beside the conversation (`window.beside`): her logins, what
              // it learned, what is important to her, and what comes at a time.
              case 'tilo.vault list': return done({items: window.beside?.items ?? []});
              case 'tilo.learned list': return done({facts: window.beside?.facts ?? []});
              case 'tilo.telling read': return done({lines: window.beside?.lines ?? [], facts: []});
              case 'tilo.events list': return done({events: window.beside?.events ?? []});
              case 'tilo.telling list': {
                // An ask that waits is held until her server's log moves.
                const newer = () => window.told.filter(one => one.seq > (argument?.after ?? 0));
                if (!newer().length && argument?.waitMs) await new Promise(moved => { window.logMoved = moved; setTimeout(moved, argument.waitMs); });
                return done({told: newer(), newest: Math.max(0, ...window.told.map(one => one.seq))});
              }
              // The address a device hands her server to be reached at while its app is closed,
              // kept by what it is for, and how often one took its address back. A server that
              // takes no address does not know the ask (`pushless`).
              case 'tilo.telling register':
                if (window.pushless) noSuch();
                window.registered.push(argument.topic);
                return done({addresses: []});
              case 'tilo.telling unregister': window.unregistered += 1; return done({addresses: []});
              case 'tilo.telling acknowledge':
                window.acknowledged.push(argument.told + ' ' + argument.by);
                return done(null);
              // What she was told while she was away, where she was (`window.since`).
              case 'tilo.telling since-you-left': return done(window.since ?? {cameIn: [], didAlone: [], waits: []});
              default: return noSuch();
            }
          });
        },
      });
      window.leave = () => { root.dataset.entry = ''; root.dataset.arrival = 'load'; };
      window.enter = async () => {
        // Tilo's page hands the entry of its device to the app's own script, which stands in the
        // document to take it (Tilo's page calls it by its name).
        await giveDevice(device);
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

    /// The workspace's agents that are active, by ID.
    private func agentsKept() async throws -> String {
        try await page("return JSON.stringify(window.agents.filter(one => one.state.$variant === 'active').map(one => one.id))")
    }

    private static let assistant = TiloSource.MainChat.agent(id: "assistant", name: "Assistant")

    /// An address as Apple's service would give one. It is made up, as is everything here.
    private static let madeUpAddress = PushAddress(use: .notices, token: Data(repeating: 0xab, count: 32), service: .development,
                                                   app: "so.tilo.assistant")

    /// The way in is over once her server holds the ask that follows what it tells her: it is
    /// the last thing the way in begins.
    private func wayInIsOver() async throws {
        try await until("the way in is over") { ((try? await self.asked("tilo.telling list")) ?? 0) >= 2 }
    }

    /// Whether Tilo's page still stands in the engine's view.
    private func pageStands() async -> Bool {
        ((try? await source.engine.view.callAsyncJavaScript("return typeof window.enter", arguments: [:], in: nil, contentWorld: .page)) as? String)
            == "function"
    }

    /// Her reminder comes due: what her server tells the assistant then, the assistant's answer
    /// to that, and what her server tells her of it.
    private static let reminderComes = reminderMessages + "\n" + reminderTold

    private static let reminderMessages = """
        post({id: 'm_ask_1', author: {agent: 'e-9'}, text: 'Remind me to stretch. (for the assistant)',
          cause: {kind: 'reminder', label: 'Stretch', key: 'k-1', person: {person: ADA}}});
        post({id: 'm_reply_1', author: {agent: 'assistant'}, text: 'Time to stretch.', parent: 'm_ask_1'});
        """

    private static let reminderTold = """
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
            window.agents = []; window.plan = {'chat read': \(late), 'chat ensure-personal-assistant': \(late),
              'tilo.models list': \(late), 'tilo.telling list': \(late)}
            """)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(saidNoAssistant, "a sentence was said on the way")
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"["assistant"]"#)
        let made = try await asked("chat ensure-personal-assistant")
        XCTAssertEqual(made, 1)
        // What her server tells her is followed all the same.
        try await inPage(Self.reminderComes)
        try await isReminder()
    }

    // MARK: Finding and making her assistant

    func testAnAssistantMadeWhileThePageStartsOverIsMadeOnceAndNoSentenceIsSaid() async throws {
        try await start()
        // Her server makes it, and its answer comes after the page started over.
        try await enter("window.agents = []; window.plan = {'chat ensure-personal-assistant': [{late: 700}]}")
        try await until("her server is asked to make it") { ((try? await self.asked("chat ensure-personal-assistant")) ?? 0) == 1 }
        try await startOver()
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        try await Task.sleep(for: .milliseconds(600))
        XCTAssertEqual(setups.last, .ready)
        XCTAssertFalse(saidNoAssistant, "a sentence was said on the way")
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"["assistant"]"#)
        let made = try await asked("chat ensure-personal-assistant")
        XCTAssertEqual(made, 1)
    }

    func testAnAnswerWithNothingInItToWhichWorkspaceIsHerOwnIsAskedAgainAndTheAssistantIsMadeThere() async throws {
        try await start(livesInOwn: true)
        try await enter("window.agents = []; window.plan = {own: ['empty']}", own: true)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(saidNoAssistant, "a sentence was said on the way")
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"["assistant"]"#)
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
        // This ask has a bound of its own, longer than any other's: her server makes the workspace then.
        source.engine.ownPatience = .milliseconds(400)
        try await enter("window.agents = []; window.plan = {own: ['never']}", own: true)
        try await signedIn()
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"["assistant"]"#)
    }

    func testAnAnswerThatNeverComesOnTheWayInDoesNotKeepHerFromHearingWhatHerServerTellsHer() async throws {
        try await start(Self.assistant)
        source.engine.askPatience = .milliseconds(400)
        try await enter("window.plan = {'tilo.vault list': ['never']}")
        try await signedIn()
        try await inPage(Self.reminderComes)
        try await isReminder()
    }

    func testTheFirstListOfTheWorkspacesAgentsThatGoesUnansweredIsAskedAgainAndNoSentenceIsSaid() async throws {
        try await start()
        try await enter("window.agents = [kept('reed', {name: 'Reed'})]; window.plan = {'chat read': ['lost']}")
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(saidNoAssistant, "one unanswered ask was said to her")
        let made = try await asked("chat ensure-personal-assistant")
        XCTAssertEqual(made, 0)
    }

    func testAListOfTheWorkspacesAgentsWithNothingInItIsNotTakenForAServerThatOffersNone() async throws {
        try await start()
        try await enter("window.agents = [kept('reed', {name: 'Reed'})]; window.plan = {'chat read': ['empty']}")
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(saidNoAssistant, "one answer with nothing in it was said to her")
        let made = try await asked("chat ensure-personal-assistant")
        XCTAssertEqual(made, 0)
    }

    func testAnUnansweredSetupWriteIsRecoveredFromTheCommittedRoster() async throws {
        try await start()
        try await enter("window.agents = []; window.plan = {'chat ensure-personal-assistant': ['unanswered']}")
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(saidNoAssistant, "the committed write was treated as a failure")
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"["assistant"]"#)
        let made = try await asked("chat ensure-personal-assistant")
        XCTAssertEqual(made, 1)
    }

    /// As an ask that ended by the engine's own bound: her server never answered it.
    func testAListOfTheWorkspacesAgentsThatIsNeverAnsweredIsAskedAgainOnceItEndedAndNoSentenceIsSaid() async throws {
        try await start()
        source.engine.askPatience = .milliseconds(400)
        try await enter("window.agents = [kept('reed', {name: 'Reed'})]; window.plan = {'chat read': ['never']}")
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(saidNoAssistant, "one ask that was never answered was said to her")
        let made = try await asked("chat ensure-personal-assistant")
        XCTAssertEqual(made, 0)
    }

    func testWhereHerServerAnswersNoneOfFiveAsksForTheWorkspacesAgentsTheMainChatSaysSoAndHerPressFindsHerAssistant() async throws {
        try await start()
        source.firstPause = .milliseconds(20)
        try await enter("window.agents = [kept('reed', {name: 'Reed'})]; window.plan = {'chat read': ['lost', 'lost', 'lost', 'lost', 'lost']}")
        try await until("the main chat says that her server did not answer") {
            self.setups.last == .noAssistant("Tilo did not answer while your assistant was being set up")
        }
        let asks = try await asked("chat read")
        XCTAssertEqual(asks, 5)
        // Her own press asks again: her server answers, and her assistant is found.
        try await source.makeAssistant()
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        let made = try await asked("chat ensure-personal-assistant")
        XCTAssertEqual(made, 0)
    }

    func testAListOfHerProvidersThatGoesUnansweredOnTheWayInIsAskedAgainAndTheProviderStepIsOffered() async throws {
        try await start(Self.assistant)
        try await enter("window.linked = false; window.plan = {'tilo.models list': ['lost']}")
        try await signedIn()
        try await until("the provider step is offered") { if case .needsProvider = self.setups.last { true } else { false } }
    }

    func testAListOfHerProvidersWithNothingInItIsNotTakenForAPersonWhoLinkedNone() async throws {
        try await start(Self.assistant)
        try await enter("window.plan = {'tilo.models list': ['empty']}")
        try await signedIn()
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(setups.contains { if case .needsProvider = $0 { true } else { false } }, "she was offered the provider step though she linked one")
    }

    func testAnUnansweredAskForWhatHerServerTellsHerIsMadeAgainByItselfWithNoWordOfThePage() async throws {
        try await start(Self.assistant)
        listening.append(Task { for await _ in self.source.engine.changes() { self.pageWords += 1 } })
        try await enter(Self.reminderMessages)
        try await signedIn()
        // The page said its one word of a change as it got in, and her server holds the ask that follows its log.
        try await until("the page said its word and her server holds an ask") {
            let asks = (try? await self.asked("tilo.telling list")) ?? 0
            return self.pageWords >= 1 && asks >= 2
        }
        try await Task.sleep(for: .milliseconds(300))
        // Her server's log moves for something that is no piece of her conversation, and the ask
        // that follows is taken and lost.
        try await inPage("""
            window.plan = {'tilo.telling list': ['lost']};
            tell({seq: 1, id: 'aside:1', atMs: 1794900000000, kind: 'news', title: '', words: 'Something else.',
              mark: {important: false, why: {code: 'judged'}}, reaches: false})
            """)
        try await until("her server was asked again after the lost ask") { ((try? await self.asked("tilo.telling list")) ?? 0) >= 4 }
        XCTAssertEqual(pageWords, 1, "the page said a word of a change meanwhile, which begins the following anew by itself")
        // Her reminder comes due: her server tells her of it, and still the page says no word.
        try await inPage(Self.reminderTold.replacingOccurrences(of: "seq: 1", with: "seq: 2"))
        try await isReminder()
    }

    func testWhatCameWhileSheWasAwayIsAskedForAgainByItselfWhereTheAskWentUnanswered() async throws {
        try await start(Self.assistant)
        source.firstPause = .milliseconds(300)
        listening.append(Task { for await card in self.source.sinceLeft() { self.cards.append(card) } })
        try await enter("""
            window.since = {leftAtMs: 1794890000000, backAtMs: 1794900000000, didAlone: [], waits: [],
              cameIn: [{seq: 1, id: 'told-1', atMs: 1794895000000, kind: 'news', title: '', words: 'Maya asks for the October numbers.',
                mark: {important: true, why: {code: 'judged'}}, reaches: true}]};
            window.plan = {'tilo.telling since-you-left': ['lost']}
            """)
        try await signedIn()
        // Nothing moves her server's log and the page says no word: the ask is made again all the same.
        try await until("she is told what came while she was away") { (self.cards.last ?? nil)?.cameIn.count == 1 }
        let asks = try await asked("tilo.telling since-you-left")
        XCTAssertEqual(asks, 2, "her server was asked once more, and no more than that")
    }

    func testTheListsBesideTheConversationThatGoUnansweredOnTheWayInAreAskedAgainByThemselves() async throws {
        try await start(Self.assistant)
        source.firstPause = .milliseconds(200)
        listening.append(Task { for await now in self.source.supervision() { self.supervisions.append(now) } })
        listening.append(Task { for await now in self.source.automations() { self.scheduled.append(now) } })
        try await enter("""
            window.beside = {items: [{reference: 'vault.item:01', kind: 'login', belongsTo: ['https://mail.example'], addedAt: 1794900000}],
              facts: [{id: 'f1', text: 'Prefers short replies'}], lines: [{id: 'l1', words: 'Anything from the people I work for'}],
              events: [{id: 'e-1', name: 'Morning brief', sort: 'scheduled', when: 'every day at 07:30', on: true}]};
            window.plan = {'tilo.vault list': ['lost'], 'tilo.learned list': ['lost'], 'tilo.telling read': ['lost'], 'tilo.events list': ['lost']}
            """)
        try await signedIn()
        try await until("her logins, what it learned, what is important and what is scheduled are all held") {
            self.supervisions.last?.logins.count == 1 && self.supervisions.last?.learned.map(\.text) == ["Prefers short replies"]
                && self.supervisions.last?.importance == ["Anything from the people I work for"] && self.scheduled.last?.count == 1
        }
    }

    func testAnAnswerWithNothingInItToAListBesideTheConversationIsNotTakenForAnEmptyListAndIsAskedAgain() async throws {
        try await start(Self.assistant)
        source.firstPause = .milliseconds(200)
        listening.append(Task { for await now in self.source.supervision() { self.supervisions.append(now) } })
        listening.append(Task { for await now in self.source.automations() { self.scheduled.append(now) } })
        try await enter("""
            window.beside = {items: [{reference: 'vault.item:01', kind: 'login', belongsTo: ['https://mail.example'], addedAt: 1794900000}],
              events: [{id: 'e-1', name: 'Morning brief', sort: 'scheduled', when: 'every day at 07:30', on: true}]};
            window.plan = {'tilo.vault list': ['empty'], 'tilo.events list': ['empty']}
            """)
        try await signedIn()
        try await until("her logins and what is scheduled are held") {
            self.supervisions.last?.logins.count == 1 && self.scheduled.last?.count == 1
        }
    }

    func testWordThatSheSawAThingWhichHerServerDidNotTakeIsSaidAgainUntilItIsHeard() async throws {
        try await start(Self.assistant)
        try await enter("window.plan = {'tilo.telling acknowledge': ['lost']}")
        try await signedIn()
        try await inPage(Self.reminderComes)
        try await isReminder()
        await source.seen(held[0].id, in: .main)
        try await until("her server heard that she saw it") {
            (try? await self.page("return JSON.stringify(window.acknowledged)")) == #"["turn:ws-1:m_ask_1:0 opened"]"#
        }
    }

    /// As a server whose own write does not finish in time, and one that takes the word and loses it.
    func testWordThatSheSawAThingIsSaidAgainAfterAFailedAndAfterALostAnswerAndStandsOnceOnHerServer() async throws {
        try await start(Self.assistant)
        source.firstPause = .milliseconds(100)
        try await enter("window.plan = {'tilo.telling acknowledge': ['failed', 'lost']}")
        try await signedIn()
        try await inPage(Self.reminderComes)
        try await isReminder()
        await source.seen(held[0].id, in: .main)
        try await until("her server heard that she saw it") {
            (try? await self.page("return JSON.stringify(window.acknowledged)")) == #"["turn:ws-1:m_ask_1:0 opened"]"#
        }
        let asks = try await asked("tilo.telling acknowledge")
        XCTAssertEqual(asks, 3)
    }

    func testWordThatSheSawAThingIsNotSaidAgainOnceHerServersLogSaysSheSawItElsewhere() async throws {
        try await start(Self.assistant)
        source.firstPause = .milliseconds(600)
        try await enter("window.plan = {'tilo.telling acknowledge': ['lost']}")
        try await signedIn()
        try await inPage(Self.reminderComes)
        try await isReminder()
        await source.seen(held[0].id, in: .main)
        // Her other device said so meanwhile: her server's log moves, and says the thing is settled.
        try await inPage("tell({...window.told[0], seq: 2, acknowledged: {atMs: 1794900300000, device: 'phone', by: 'opened'}})")
        try await Task.sleep(for: .milliseconds(1500))
        let asks = try await asked("tilo.telling acknowledge")
        XCTAssertEqual(asks, 1, "her server was told again of what it knows already")
    }

    func testWordOfWhatShePressedUnderANoticeWhichHerServerDidNotTakeIsSaidAgainUntilItIsHeard() async throws {
        try await start(Self.assistant)
        source.firstPause = .milliseconds(200)
        try await enter("window.plan = {'tilo.telling acknowledge': ['lost']}")
        try await signedIn()
        // Her reminder comes due, and her server offers one thing to press with it.
        try await inPage(Self.reminderMessages + """

            tell({seq: 1, id: 'turn:ws-1:m_ask_1:0', atMs: 1794900000000, kind: 'reminder', title: 'Stretch', words: 'said',
              about: {workspace: 'ws-1', app: 'so.tilo.chat', resource: 'chat.messages/messages/m_ask_1'},
              mark: {important: false, why: {code: 'reminder'}}, reaches: true,
              actions: [{id: 'done', label: 'Done', main: true, target: {app: 'so.tilo.chat', resource: 'chat.workspace', action: 'send'},
                arguments: {text: 'Done.'}}]})
            """)
        try await until("the reminder offers what her server offers") { self.held.first?.offers.map(\.title) == ["Done"] }
        try await source.take("done", of: held[0].id, in: .main)
        try await until("her server heard what she pressed") {
            (try? await self.page("return JSON.stringify(window.acknowledged)")) == #"["turn:ws-1:m_ask_1:0 done"]"#
        }
        let sent = try await page("return JSON.stringify(window.sent)")
        XCTAssertEqual(sent, #"["Done."]"#, "what she pressed was done once")
    }

    func testAHandOverOfTheDevicesAddressThatGoesUnansweredOrFailsIsMadeAgainByItselfUntilHerServerHasIt() async throws {
        try await start(Self.assistant, closedKeys: MemoryClosedKeys())
        source.firstPause = .milliseconds(150)
        try await enter("window.plan = {'tilo.telling register': ['lost', 'failed']}")
        try await signedIn()
        try await wayInIsOver()
        await source.register(Self.madeUpAddress)
        try await until("her server has the address") {
            (try? await self.page("return JSON.stringify(window.registered)")) == #"["so.tilo.assistant"]"#
        }
        let asks = try await asked("tilo.telling register")
        XCTAssertEqual(asks, 3)
    }

    func testAServerThatTakesNoAddressIsAskedForItOnceAndNotAgainAndAgain() async throws {
        try await start(Self.assistant, closedKeys: MemoryClosedKeys())
        source.firstPause = .milliseconds(100)
        try await enter("window.pushless = true")
        try await signedIn()
        try await wayInIsOver()
        await source.register(Self.madeUpAddress)
        try await Task.sleep(for: .milliseconds(800))
        let asks = try await asked("tilo.telling register")
        XCTAssertEqual(asks, 1)
    }

    func testSigningOutDoesNotWaitForHerServerToForgetTheAddressAndAsksItAllTheSame() async throws {
        let keys = MemoryClosedKeys()
        try await start(Self.assistant, closedKeys: keys)
        source.engine.askPatience = .seconds(4)
        try await enter("window.plan = {'tilo.telling unregister': ['never']}")
        try await signedIn()
        try await wayInIsOver()
        await source.register(Self.madeUpAddress)
        XCTAssertNotNil(keys.read())
        let began = ContinuousClock.now
        await source.signOut()
        XCTAssertLessThan(ContinuousClock.now - began, .seconds(1), "signing out waited for an answer that never comes")
        try await until("the app is signed out") { self.sessions.last == .signedOut }
        XCTAssertNil(keys.read(), "the key outlived its person's sign-out")
        // Her server was asked to forget the address, by the page as it stood.
        let asks = try await asked("tilo.telling unregister")
        XCTAssertEqual(asks, 1)
    }

    func testAtASignOutThePageStaysUntilHerServerAnsweredAndIsPutAwayThen() async throws {
        try await start(Self.assistant, closedKeys: MemoryClosedKeys())
        source.engine.askPatience = .seconds(8)
        try await enter("window.plan = {'tilo.telling unregister': [{late: 400}]}")
        try await signedIn()
        try await wayInIsOver()
        await source.register(Self.madeUpAddress)
        await source.signOut()
        // The ask is out, and the page that carries it still stands.
        let asks = try await asked("tilo.telling unregister")
        XCTAssertEqual(asks, 1)
        // Her server answers: the page is put away, long before the engine's patience with an ask is over.
        try await until("the page is put away", within: .seconds(4)) { await self.pageStands() == false }
    }

    func testASignInThatBeginsWhileASignOutsLastAskIsStillOutIsNotPutAwayForIt() async throws {
        try await start(Self.assistant, closedKeys: MemoryClosedKeys())
        source.engine.askPatience = .milliseconds(500)
        try await enter("window.plan = {'tilo.telling unregister': ['never']}")
        try await signedIn()
        try await wayInIsOver()
        await source.register(Self.madeUpAddress)
        await source.signOut()
        // She signs in again at once. This test fetches nothing, so the page in the view stays as it is.
        try source.engine.open(route: "/w/ws-1")
        try await Task.sleep(for: .milliseconds(1100))
        // The patience with the sign-out's last ask is long over: what stands in the view now was not put away for it.
        let stands = await pageStands()
        XCTAssertTrue(stands, "the page of the new sign-in was put away for the last ask of the sign-out before it")
        if case .arriving = source.engine.state {} else { XCTFail("the engine is not on its way in") }
    }

    /// As on a busy site at a first start: her server makes her own workspace the first time it
    /// is asked which one that is, and the page follows the making for longer than any other ask.
    func testWhichWorkspaceIsHerOwnMayTakeLongerThanAnyOtherAskAndIsAskedOnce() async throws {
        try await start(livesInOwn: true)
        source.engine.askPatience = .milliseconds(300)
        try await enter("window.agents = []; window.plan = {own: [{late: 1200}]}", own: true)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(saidNoAssistant, "a sentence was said on the way")
        let asks = try await asked("own")
        XCTAssertEqual(asks, 1, "the ask was ended before her server answered, and made a second time")
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"["assistant"]"#)
    }

    /// As Tilo's page after thirty seconds without an answer of her server: it says so itself.
    func testThePagesOwnWordThatNoAnswerCameIsNoRefusalOfHerServersAndTheAskIsMadeAgain() async throws {
        try await start()
        try await enter("window.agents = [kept('reed', {name: 'Reed'})]; window.plan = {'chat read': ['gave-up']}")
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(saidNoAssistant, "the page's word that no answer came was said to her as her server's refusal")
        let made = try await asked("chat ensure-personal-assistant")
        XCTAssertEqual(made, 0)
    }

    /// Linking a provider is four asks and choosing a model two: each has the whole bound to itself.
    func testLinkingAProviderAndChoosingAModelGoThroughWhereTheirAsksTogetherTakeLongerThanOneAsksBound() async throws {
        try await start(Self.assistant)
        source.engine.askPatience = .seconds(1)
        try await enter("window.linked = false")
        try await signedIn()
        try await until("the provider step is offered") { if case .needsProvider = self.setups.last { true } else { false } }
        let slow = "[{late: 600}]"
        try await inPage("""
            window.plan = {'tilo.models list': \(slow), 'tilo.models add-preset': \(slow), 'tilo.models set-key': \(slow),
              'tilo.models refresh-models': \(slow), 'tilo.models set-assignments': [{late: 600}, {late: 600}]}
            """)
        // A made-up key, typed as into the protected field.
        let offered = try await source.link("anthropic", key: Secret("made-up-and-typed-into-the-protected-field"))
        XCTAssertEqual(offered.map(\.name), ["Model one"])
        try await source.choose("anthropic/m-1", for: .request)
        try await source.choose("anthropic/m-1", for: .task)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
    }

    // MARK: After the sentence

    private static let notAnswered = Setup.noAssistant("Tilo did not answer while your assistant was being set up")

    /// As a server that is too busy for a while: five asks in a row go unanswered, the next is answered.
    func testAfterFiveUnansweredAsksTheFindingOfHerAssistantIsBegunAgainByItselfAndNeedsNoPress() async throws {
        try await start()
        source.firstPause = .milliseconds(20)
        source.findingPause = .milliseconds(20)
        try await enter("""
            window.agents = [kept('assistant', {name: 'Reed'})]; \(Self.twoMessages);
            window.plan = {'chat read': ['lost', 'lost', 'lost', 'lost', 'lost']}
            """)
        try await until("the main chat says that her server did not answer") { self.setups.contains(Self.notAnswered) }
        // Nobody presses anything: her server answers the next ask, her assistant is found and
        // her conversation with it is read.
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        try await until("her conversation with her assistant is read") { self.held.count == 2 }
        let made = try await asked("chat ensure-personal-assistant")
        XCTAssertEqual(made, 0)
    }

    /// As a server that stays too busy for longer: the sentence is true for as long as no answer
    /// comes, so it stands, and it goes when her server answers.
    func testWhileTheFindingIsBegunAgainByItselfTheSentenceStandsAndGoesOnlyWhenHerServerAnswers() async throws {
        try await start()
        source.firstPause = .milliseconds(20)
        source.findingPause = .milliseconds(20)
        let lost = Array(repeating: "'lost'", count: 12).joined(separator: ", ")
        try await enter("window.agents = [kept('reed', {name: 'Reed'})]; window.plan = {'chat read': [\(lost)]}")
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        let from = try XCTUnwrap(setups.firstIndex(of: Self.notAnswered), "the sentence was never said")
        XCTAssertFalse(setups[from...].contains(.findingAssistant),
                       "the sentence gave way to the line of the setting up before her server answered")
        let asks = try await asked("chat read")
        XCTAssertEqual(asks, 13, "twelve asks that went unanswered and the one her server answered")
        let made = try await asked("chat ensure-personal-assistant")
        XCTAssertEqual(made, 0)
    }

    /// Her server's own refusal is its word, and stands until she presses: only what went
    /// unanswered is asked again by itself.
    func testWhereHerServerItselfRefusedTheFindingNothingIsAskedAgainByItself() async throws {
        try await start()
        source.firstPause = .milliseconds(20)
        source.findingPause = .milliseconds(20)
        try await enter("window.agents = [kept('reed', {name: 'Reed'})]; window.plan = {'chat read': ['failed']}")
        let refused = Setup.noAssistant("The write did not finish in time")
        try await until("the main chat says her server's own words") { self.setups.last == refused }
        try await Task.sleep(for: .milliseconds(500))
        XCTAssertEqual(setups.last, refused)
        let asks = try await asked("chat read")
        XCTAssertEqual(asks, 1)
    }

    // MARK: While her server makes her own workspace

    /// A later start of a device that has been in before, on a busy server: it takes its time
    /// to say which workspace is hers, as it may at any start. Nothing is made for the first
    /// time, so nothing is said of a making: what every start says stands until she is in.
    func testAtALaterStartAServerThatTakesItsTimeIsNotSaidToBeMakingHerWorkspace() async throws {
        // The first start: her server says which workspace is hers, and the device keeps its person.
        try await start(livesInOwn: true)
        try await enter("window.agents = [kept('reed', {name: 'Reed'})]", own: true)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        // The app is closed and opened again, and her server is four times as long at the same
        // ask as the app waits before it speaks of a making.
        try await start(livesInOwn: true, again: true)
        source.makingAfter = .milliseconds(300)
        try await enter("window.agents = [kept('reed', {name: 'Reed'})]; window.plan = {own: [{late: 1200}]}", own: true)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(setups.contains(.makingWorkspace), "a start that makes nothing said that her workspace is being made: \(setups)")
        let out = try await asked("own")
        XCTAssertEqual(out, 1)
    }

    /// The first time is not over while her server is still at it: where the app is closed in
    /// that time and opened again, the device keeps no person yet, and the main chat says what
    /// is waited for as it did before.
    func testAStartWhileHerServerIsStillAtTheFirstMakingSaysWhatItWaitsFor() async throws {
        try await start(livesInOwn: true)
        source.makingAfter = .milliseconds(300)
        try await enter("window.agents = []; window.plan = {own: ['never']}", own: true)
        try await until("the main chat says that her server is making her workspace") { self.setups.last == .makingWorkspace }
        try await start(livesInOwn: true, again: true)
        source.makingAfter = .milliseconds(300)
        try await enter("window.agents = []; window.plan = {own: ['never']}", own: true)
        try await until("the main chat says it at the next start too") { self.setups.last == .makingWorkspace }
        XCTAssertEqual(setups.filter { $0 != .unknown }, [.findingAssistant, .makingWorkspace])
    }

    /// As a first start on a busy server: it makes her own workspace the first time it is asked
    /// which one that is, and takes its time. She is in for all of that time, and reads what is
    /// waited for.
    func testWhileHerServerMakesHerOwnWorkspaceSheIsSignedInAndTheMainChatSaysWhatItWaitsFor() async throws {
        try await start(livesInOwn: true)
        source.makingAfter = .milliseconds(300)
        try await enter("window.agents = []; window.plan = {own: ['never']}", own: true)
        try await signedIn()
        try await until("the main chat says that her server is making her workspace") { self.setups.last == .makingWorkspace }
        XCTAssertEqual(setups.filter { $0 != .unknown }, [.findingAssistant, .makingWorkspace],
                       "the line of the setting up stands first, and the sentence comes under it after a while")
        let out = try await asked("own")
        XCTAssertEqual(out, 1)
        // Her server is done and says which workspace is hers: the way in goes on from there.
        try await inPage("window.owed.shift()({id: 'own-7', name: 'Mine', origin: 'https://tilo.example:8443'})")
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(saidNoAssistant, "a sentence was said on the way")
        XCTAssertEqual(setups.filter { $0 == .makingWorkspace }.count, 1, "the sentence came back after her server had answered")
        let asks = try await asked("own")
        XCTAssertEqual(asks, 1, "her server was asked a second time for what it was already making")
        let kept = try await agentsKept()
        XCTAssertEqual(kept, #"["assistant"]"#)
        try await until("her name is known") {
            if case .signedIn(let person) = self.sessions.last { person.name == "Ada Lovelace" } else { false }
        }
    }

    /// A server that says at once which workspace is hers was not making it: nothing is said of that.
    func testWhereHerServerSaysAtOnceWhichWorkspaceIsHerOwnNothingIsSaidOfItsMaking() async throws {
        try await start(livesInOwn: true)
        try await enter("window.agents = []", own: true)
        try await until("nothing more stands in the way") { self.setups.last == .ready }
        XCTAssertFalse(setups.contains(.makingWorkspace), "the sentence was said though her server answered at once")
        let asks = try await asked("own")
        XCTAssertEqual(asks, 1)
    }
}
#endif
