#if os(macOS)
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// The engine against a page that speaks to it the way Tilo's does. No window is opened,
/// nothing is fetched and nothing is kept: the page is given as text under the site's address.
@MainActor
final class EngineTests: XCTestCase {
    private static let site = "https://tilo.test"
    private var engine: TiloEngine!
    private var stood: [TiloEngine.State] = []
    private var changes = 0
    private var listening: [Task<Void, Never>] = []

    override func setUp() async throws {
        engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        stood = []
        changes = 0
        let engine = engine!
        listening = [
            Task { for await state in engine.states() { self.stood.append(state) } },
            Task { for await _ in engine.changes() { self.changes += 1 } },
        ]
        engine.view.loadHTMLString(Self.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page says where it stands") { self.stood.contains(.arriving("load")) }
    }

    override func tearDown() async throws {
        for task in listening { task.cancel() }
    }

    /// As Tilo's page: `entry-page.mjs` writes its step on the root element and its words into
    /// the status line, names its mount `tiloMount` once it is in, and the mount describes
    /// what it shows in an element of its own. A refusal reads `code: reason`.
    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status">Loading Tilo…</span>
    <div id="semantics"></div>
    <script>
      const root = document.documentElement;
      window.frames = 0;
      requestAnimationFrame(() => { window.frames += 1; });
      window.pairing = code => { root.dataset.arrival = 'join'; root.dataset.pairingCode = code; };
      window.refuse = words => { document.getElementById('status').textContent = words; root.dataset.arrival = 'stopped'; root.dataset.entry = 'refused'; };
      window.fail = words => { document.getElementById('status').textContent = words; root.dataset.arrival = 'stopped'; root.dataset.entry = 'failed'; };
      // As the page that has no graphics adapter: it says that it stopped, empties its status
      // line, and says why in a card it adds a moment later.
      window.cannotDraw = words => {
        root.dataset.arrival = 'stopped'; root.dataset.entry = 'unsupported';
        document.getElementById('status').textContent = '';
        setTimeout(() => {
          const card = document.createElement('form'); card.className = 'tilo-entry-card'; card.dataset.card = 'renderer';
          const message = document.createElement('p'); message.textContent = words;
          const again = document.createElement('button'); again.textContent = 'Try again';
          card.append(message, again); document.body.append(card);
        }, 150);
      };
      window.enter = () => {
        const semantics = document.getElementById('semantics');
        const heard = [];
        root.dataset.arrival = 'done';
        root.dataset.entry = 'open';
        globalThis.tiloMount = {
          semantics, heard,
          actions: resource => [{address: {resource: resource ?? 'notes.book', action: 'read'}, resource_kind: 'notes.book', kind: 'View', params: []}],
          invokeAction: async (resource, action, args, options) => {
            heard.push({resource, action, args, confirm: options.confirm});
            if (action === 'refuse') throw new Error('forbidden: Editors may not do this');
            // Chat offers its send a moment after a device got in.
            if (action === 'send') {
              if (!window.sendPublished) throw new Error('unknown_action: no action addressed by (chat.workspace, send)');
              window.messagesTaken = (window.messagesTaken ?? 0) + 1;
              return {state: 'completed', output: {value: {accepted: true, message: 'm-1', conversation: args.to[0].id}}};
            }
            if (action === 'read') return {state: 'completed', output: {value: {words: semantics.textContent}}};
            return {state: 'completed', output: {value: {took: args, confirmed: options.confirm}}};
          },
        };
      };
      window.write = words => { document.getElementById('semantics').textContent = words; };
      // A page that mounts beside itself: each mount in the element it is given.
      window.opened = [];
      window.offerBeside = () => {
        globalThis.tiloOpen = async (element, what) => {
          if (what.app === 'so.tilo.refuses') throw new Error('not_permitted: You may not open this');
          window.opened.push({...what, shown: element.style.display});
          const record = what.record ?? 'uuid:made-now';
          return {
            semantics: document.createElement('div'), editor: {focus: () => { window.focused = record; }},
            state: () => ({phase: 'presented', record}),
            actions: () => [{address: {resource: 'word-processor.document', action: 'read-text'}, resource_kind: 'word-processor.document'}],
            invokeAction: async (resource, action) => ({state: 'completed', output: {value: {from: record, action}}}),
            cleanup: async () => { window.tookDown = record; },
          };
        };
      };
      // A page that knows a native app may hold it: once it is in, it hands the host standing in
      // its document the entry of its device (web/native-host.mjs). What its host answers
      // here is deliberately careless: it says a key where none belongs and repeats a secret.
      window.asked = [];
      window.handOver = () => globalThis.tiloNativeHost?.device(Object.freeze({
        person: 'ad'.repeat(32), workspace: 'ws-1',
        catalogue: async () => ({actions: []}),
        status: async invocation => ({invocation, state: 'succeeded'}),
        ownWorkspace: async () => ({id: 'own-7', name: 'Mine', origin: 'https://host.example:8443'}),
        invoke: async (target, argument) => {
          window.asked.push({target, argument});
          const done = value => ({invocation: 'i-1', state: 'succeeded', output: {value, resources: []}});
          switch (target.provider + ' ' + target.address.action) {
            case 'tilo.models list':
              return done({providers: [{id: 'anthropic', name: 'Anthropic', protocol: 'anthropic', endpoint: 'https://api.anthropic.com',
                configured: true, models: [{id: 'm-1', label: 'Model one'}], defaultModel: 'm-1',
                apiKey: 'sk-said-by-mistake', token: 'tok-said-by-mistake', secret: 'said-by-mistake'}],
                defaultProvider: 'anthropic', assignments: [{kind: 'subtask', provider: 'anthropic', model: 'm-2'}]});
            case 'tilo.vault add':
              return done({reference: 'vault.item:0a', kind: argument.kind, belongsTo: argument.belongsTo, addedAt: 1794900000,
                echoed: 'you sent ' + argument.secret + ' twice ' + argument.secret, secret: argument.secret, nested: [{note: argument.secret}]});
            case 'tilo.models set-key':
              throw Object.assign(new Error('The provider did not accept ' + argument.key), {code: 'key-refused'});
            case 'tilo.events pause':
              return {invocation: 'i-2', state: 'failed', error: {code: 'forbidden', message: 'This event is not yours'}};
            default:
              throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'});
          }
        },
      }));
      // A page that says itself when something it shows changed.
      window.told = [];
      window.offerChanges = () => { globalThis.tiloChanges = {listen: listener => { window.told.push(listener); return () => { window.told = []; }; }}; };
    </script></html>
    """#

    func testThePageSaysWhereItStandsAndTheEngineIsInOnceItsMountIsNamed() async throws {
        try await run("location.hash = '#/@host.example/w/ws-1/so.tilo.chat'")
        try await run("pairing('135790')")
        try await until("the code to compare is told") { self.stood.contains(.pairing(code: "135790")) }
        XCTAssertFalse(stood.contains { if case .open = $0 { true } else { false } })
        try await run("enter()")
        try await until("the engine is in") { self.engine.state == .open(route: "/@host.example/w/ws-1/so.tilo.chat") }
        // The code stays on the page after the approval: it is no longer what the page waits for.
        XCTAssertEqual(stood.last, .open(route: "/@host.example/w/ws-1/so.tilo.chat"))
    }

    func testARefusalIsSaidInTheSitesOwnWords() async throws {
        try await run("refuse('Lounge is invite-only for now.')")
        try await until("the refusal is told") { self.engine.state == .refused("Lounge is invite-only for now.") }
    }

    func testAPageThatStoppedIsLoadedOnceMoreAndItsOwnSentenceIsSaidWhenThatLoadSaysNothing() async throws {
        var asked = 0
        engine.loads = { _ in asked += 1 }
        engine.stopPatience = .milliseconds(300)
        try engine.open(route: "/w/ws-1")
        XCTAssertNil(engine.lastStop)
        try await run("fail('Couldn\u{2019}t open Tilo. It stopped while joining.')")
        // Loaded once more, a moment later: a page before it may still have held its storage.
        try await until("the page is loaded again") { asked == 2 }
        XCTAssertEqual(engine.lastStop, "Couldn\u{2019}t open Tilo. It stopped while joining.")
        XCTAssertEqual(engine.state, .arriving("load"))
        // That load says nothing. The page's own sentence is said, and nothing is loaded a third time.
        try await until("the stop is said") { self.engine.state == .failed("Couldn\u{2019}t open Tilo. It stopped while joining.") }
        XCTAssertEqual(asked, 2)
    }

    func testAPageThatGetsOnAfterItWasLoadedAgainIsNotCalledStopped() async throws {
        var asked = 0
        engine.loads = { _ in asked += 1 }
        engine.stopPatience = .milliseconds(400)
        try engine.open(route: "/w/ws-1")
        try await run("fail('This device is already open in another tab')")
        try await until("the page is loaded again") { asked == 2 }
        // The second load gets in.
        try await run("document.getElementById('status').textContent = ''; location.hash = '#/w/ws-1/so.tilo.chat'; enter()")
        try await until("the engine is in") { self.engine.state == .open(route: "/w/ws-1/so.tilo.chat") }
        try await Task.sleep(for: .milliseconds(700))
        XCTAssertEqual(engine.state, .open(route: "/w/ws-1/so.tilo.chat"))
    }

    func testAPageThatCannotDrawIsSaidInItsOwnWordsAndIsNotLoadedAgain() async throws {
        var asked = 0
        engine.loads = { _ in asked += 1 }
        try engine.open(route: "/w/ws-1")
        let words = "Tilo draws with WebGPU. This browser has it, but offered this page no graphics adapter."
        try await run("cannotDraw('\(words)')")
        try await until("the page's own sentence is said") { self.engine.state == .failed(words) }
        // Longer than the moment after which a page that stopped is loaded once more.
        try await Task.sleep(for: .seconds(2.2))
        XCTAssertEqual(asked, 1)
        XCTAssertNil(engine.lastStop)
        XCTAssertEqual(engine.state, .failed(words))
    }

    func testNothingIsAskedOfAPageThatIsNotIn() async throws {
        do {
            _ = try await engine.read("notes.book")
            XCTFail("a page that is not in answered a read")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .notOpen)
        }
    }

    func testReadsAndActionsGoThroughThePagesOwnEntryAndAChangeIsSaidWithoutAClock() async throws {
        try await run("location.hash = '#/w/ws-1/so.tilo.chat'; enter()")
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        try await until("the first word that there is something to read") { self.changes >= 1 }
        let before = changes
        try await run("write('Ship the report')")
        try await until("the change is said") { self.changes > before }
        let read = try await engine.read("notes.book")
        XCTAssertEqual(read, ["words": "Ship the report"])
        let listed = try await engine.actions(under: "notes.book/notes/n1")
        XCTAssertEqual(listed.first?["address"]?["resource"]?.string, "notes.book/notes/n1")
        let answer = try await engine.invoke("rename", on: "notes.book/notes/n1", ["title": "Report", "pinned": true, "order": 2],
                                             confirm: true)
        XCTAssertEqual(answer["output"]?["value"], ["took": ["title": "Report", "pinned": true, "order": 2], "confirmed": true])
        do {
            try await engine.invoke("refuse", on: "notes.book")
            XCTFail("a refused action was taken for done")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .refused(code: "forbidden", reason: "Editors may not do this"))
        }
        let heard = try await engine.view.callAsyncJavaScript("return JSON.stringify(tiloMount.heard.map(call => call.action))",
                                                               arguments: [:], in: nil, contentWorld: .page) as? String
        XCTAssertEqual(heard, #"["read","rename","refuse"]"#)
    }

    func testAPageNobodySeesStillGetsItsFrames() async throws {
        // The view is in no window: without the engine's clock the page would never be asked
        // for a frame, and Tilo's page is in only once it drew one.
        try await until("the page's first frame") {
            (try? await self.engine.view.callAsyncJavaScript("return window.frames", arguments: [:], in: nil, contentWorld: .page)) as? Int == 1
        }
        let seen = try await engine.view.callAsyncJavaScript("return document.visibilityState + ' ' + document.hidden",
                                                              arguments: [:], in: nil, contentWorld: .page) as? String
        XCTAssertEqual(seen, "visible false")
    }

    func testFramesGoBackToTheClockWhenNobodyLooksAnyMore() async throws {
        try await until("the page's first frame") {
            (try? await self.engine.view.callAsyncJavaScript("return window.frames", arguments: [:], in: nil, contentWorld: .page)) as? Int == 1
        }
        // Someone looks: the screen is asked. This view is in no window, so no frame comes.
        engine.setSeen(true)
        try await run("await new Promise(done => setTimeout(done, 30)); requestAnimationFrame(() => { window.frames += 1; });")
        try await Task.sleep(for: .milliseconds(300))
        let waiting = try await engine.view.callAsyncJavaScript("return window.frames", arguments: [:], in: nil, contentWorld: .page) as? Int
        XCTAssertEqual(waiting, 1, "a frame came from a screen that shows nothing of this view")
        // Nobody looks any more: the frame that was asked of the screen comes from the clock.
        engine.setSeen(false)
        try await until("the frame the screen never gave") {
            (try? await self.engine.view.callAsyncJavaScript("return window.frames", arguments: [:], in: nil, contentWorld: .page)) as? Int == 2
        }
    }

    func testALiveThingIsOneMoreMountOfTheSamePageAndItsActionsAreItsOwn() async throws {
        try await run("location.hash = '#/w/ws-1/so.tilo.chat'; offerBeside(); enter()")
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        let made = try await engine.show(live: "draft", .new(app: "so.tilo.word-processor"))
        XCTAssertEqual(made, "uuid:made-now")
        let known = try await engine.show(live: "report", .record(app: "so.tilo.word-processor", record: "uuid:1f0e"))
        XCTAssertEqual(known, "uuid:1f0e")
        // Shown again it is the same mount, brought to the front: nothing is mounted twice.
        _ = try await engine.show(live: "draft", .new(app: "so.tilo.word-processor"))
        let opened = try await engine.view.callAsyncJavaScript(
            "return JSON.stringify(opened.map(what => [what.app, what.record ?? null, what.newDocument ?? false, what.instance]))",
            arguments: [:], in: nil, contentWorld: .page) as? String
        XCTAssertEqual(opened, #"[["so.tilo.word-processor",null,true,100],["so.tilo.word-processor","uuid:1f0e",false,101]]"#)
        let front = try await engine.view.callAsyncJavaScript(
            "return [...document.querySelectorAll('[data-tilo-live]')].map(place => place.dataset.tiloLive + ':' + place.style.display).join(' ')",
            arguments: [:], in: nil, contentWorld: .page) as? String
        XCTAssertEqual(front, "draft:block report:none")
        // Its actions are its own, and Chat's stay Chat's.
        let answer = try await engine.invoke("read-text", on: "word-processor.document", in: "report")
        XCTAssertEqual(answer["output"]?["value"], ["from": "uuid:1f0e", "action": "read-text"])
        let listed = try await engine.actions(in: "report")
        XCTAssertEqual(listed.first?["resource_kind"]?.string, "word-processor.document")
        let chats = try await engine.actions()
        XCTAssertEqual(chats.first?["resource_kind"]?.string, "notes.book")
        // A refusal is Tilo's own, and leaves nothing standing.
        do {
            try await engine.show(live: "secret", .record(app: "so.tilo.refuses", record: "uuid:2"))
            XCTFail("a refused mount was taken for shown")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .refused(code: "not_permitted", reason: "You may not open this"))
        }
        engine.rest()
        await engine.close(live: "report")
        let left = try await engine.view.callAsyncJavaScript(
            "return [...document.querySelectorAll('[data-tilo-live]')].map(place => place.dataset.tiloLive + ':' + place.style.display).join(' ') + ' took down ' + window.tookDown",
            arguments: [:], in: nil, contentWorld: .page) as? String
        XCTAssertEqual(left, "draft:none took down uuid:1f0e")
    }

    func testAPageThatSaysItsOwnChangesIsListenedTo() async throws {
        try await run("location.hash = '#/w/ws-1/so.tilo.chat'; offerChanges(); enter()")
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        try await until("the first word") { self.changes >= 1 }
        let before = changes
        try await run("told[0]()")
        try await until("the page's own word is passed on") { self.changes > before }
    }

    func testAnotherSitesAddressIsNeverOpened() throws {
        XCTAssertThrowsError(try engine.pair(with: URL(string: "https://elsewhere.example/#/w/ws-1?pair=\(String(repeating: "a", count: 96))")!))
        XCTAssertThrowsError(try engine.open(route: "/somewhere/else"))
    }

    // MARK: The person's own providers

    private func enterAsADevice() async throws {
        try await run("location.hash = '#/w/ws-1/so.tilo.chat'; handOver(); enter()")
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
    }

    /// What the page's host was asked, as text.
    private func asked() async throws -> String {
        try await engine.view.callAsyncJavaScript("return JSON.stringify(window.asked)", arguments: [:], in: nil,
                                                  contentWorld: .page) as? String ?? ""
    }

    func testThePersonsOwnProvidersAnswerTheAppsCodeThroughThePagesSession() async throws {
        try await enterAsADevice()
        let linked = try await engine.modelProviders()
        XCTAssertEqual(linked.providers.map(\.id), ["anthropic"])
        XCTAssertEqual(linked.providers.first?.hasKey, true)
        XCTAssertEqual(linked.providers.first?.models, [.init(id: "m-1", label: "Model one")])
        XCTAssertEqual(linked.defaultProvider, "anthropic")
        // A kind with a model of its own runs on it, any other on the default.
        XCTAssertEqual(linked.model(for: "subtask")?.model, "m-2")
        XCTAssertEqual(linked.model(for: "request")?.model, "m-1")
        let heard = try await asked()
        XCTAssertEqual(heard, #"[{"target":{"provider":"tilo.models","address":{"resource":"models","action":"list"}},"argument":null}]"#)
        // Whose device it is and the person's own workspace come the same way.
        let whose = try await engine.whose()
        XCTAssertEqual(whose?.person, String(repeating: "ad", count: 32))
        XCTAssertEqual(whose?.workspace, "ws-1")
        let own = try await engine.ownWorkspace()
        XCTAssertEqual(own.route, "/@host.example:8443/w/own-7")
        XCTAssertEqual(own.name, "Mine")
        // A refusal is the host's own, whether it throws it or answers it.
        do {
            try await engine.ask(.events, "pause", of: "events", ["event": "e-1"])
            XCTFail("a refused action was taken for done")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .refused(code: "forbidden", reason: "This event is not yours"))
        }
        do {
            try await engine.ask(.telling, "list", of: "told")
            XCTFail("an action nobody offers was taken for done")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .refused(code: "unknown_action", reason: "No such action is offered"))
        }
    }

    func testWithoutTheEnginesPassNoScriptReachesThePersonsProviders() async throws {
        try await enterAsADevice()
        // As any other script in the document would ask: with no pass, a guessed one, one of
        // the right length, and the pass cut short.
        let wrong = String(engine.pass.reversed())
        XCTAssertNotEqual(wrong, engine.pass)
        let answers = try await engine.view.callAsyncJavaScript(#"""
            const tries = [undefined, '', 'open sesame', wrong, cut, 0, {}, [pass]];
            const said = [];
            for (const given of tries) {
              said.push(JSON.parse(await __tiloLive.person(given, 'tilo.models', 'models', 'list', null, null)).error?.code ?? 'answered');
              said.push(JSON.parse(await __tiloLive.own(given)).error?.code ?? 'answered');
              said.push(JSON.parse(__tiloLive.whose(given)).error?.code ?? 'answered');
            }
            return JSON.stringify({said: [...new Set(said)], asked: window.asked.length});
            """#, arguments: ["wrong": wrong, "cut": String(engine.pass.dropLast()), "pass": engine.pass], in: nil, contentWorld: .page) as? String
        XCTAssertEqual(answers, #"{"said":["unknown_action"],"asked":0}"#)
    }

    func testThePassStandsNowhereAPagesScriptCanRead() async throws {
        try await enterAsADevice()
        _ = try await engine.modelProviders()
        let found = try await engine.view.callAsyncJavaScript(#"""
            const found = [];
            const print = value => {
              try { return typeof value === 'function' ? Function.prototype.toString.call(value) : typeof value === 'string' ? value : ''; }
              catch { return ''; }
            };
            const look = (owner, label) => {
              for (const name of [...Object.getOwnPropertyNames(owner), ...Object.getOwnPropertySymbols(owner)]) {
                const described = Object.getOwnPropertyDescriptor(owner, name);
                for (const part of [described?.get, described?.set, 'value' in (described ?? {}) ? described.value : undefined]) {
                  if (print(part).includes(pass)) found.push(label + '.' + String(name));
                }
              }
            };
            look(globalThis, 'globalThis');
            look(globalThis.__tiloLive, '__tiloLive');
            look(globalThis.tiloNativeHost, 'tiloNativeHost');
            look(document, 'document');
            if (document.documentElement.outerHTML.includes(pass)) found.push('the document');
            for (const store of [localStorage, sessionStorage]) {
              for (let at = 0; at < store.length; at++) if ((store.getItem(store.key(at)) ?? '').includes(pass)) found.push('storage');
            }
            return JSON.stringify(found);
            """#, arguments: ["pass": engine.pass], in: nil, contentWorld: .page) as? String
        XCTAssertEqual(found, "[]")
        XCTAssertEqual(engine.pass.count, 64)
        // Each engine makes its own.
        let other = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        XCTAssertNotEqual(other.pass, engine.pass)
    }

    func testAProviderOffTheEnginesListIsRefusedBeforeThePageIsAsked() async throws {
        try await enterAsADevice()
        XCTAssertEqual(PersonProvider.allCases.map(\.rawValue),
                       ["tilo.models", "tilo.vault", "tilo.events", "tilo.telling", "tilo.initiative", "tilo.asking", "tilo.learned"])
        // With the engine's own pass: what the device could ask its host, and the engine does not.
        let refused = try await engine.view.callAsyncJavaScript(#"""
            const said = [];
            for (const provider of ['tilo.environment', 'tilo.mcp-server', 'tilo.device-pairing', 'tilo.accounts', 'tilo.hosting',
                                    'workspace.permissions:ws-1', 'record:so.tilo.chat:x', 'tilo.models ', 'TILO.MODELS', '']) {
              said.push(JSON.parse(await __tiloLive.person(pass, provider, 'models', 'list', null, null)).error?.code ?? 'answered');
            }
            return JSON.stringify({said: [...new Set(said)], asked: window.asked.length});
            """#, arguments: ["pass": engine.pass], in: nil, contentWorld: .page) as? String
        XCTAssertEqual(refused, #"{"said":["unknown_action"],"asked":0}"#)
    }

    func testWhatWasTypedIntoAProtectedFieldReachesTheHostAndComesBackInNoAnswer() async throws {
        try await enterAsADevice()
        let typed = "s3cret \"typed\" \\ into the field"
        let board = NSPasteboard.general.changeCount
        let item = try await engine.keepInVault(Protected(typed), kind: "key", belongsTo: ["https://api.example"], username: "ada")
        XCTAssertEqual(item.reference, "vault.item:0a")
        XCTAssertEqual(item.kind, "key")
        XCTAssertEqual(item.belongsTo, ["https://api.example"])
        // It reached the host's own call, in the field the action names, exactly as typed.
        let reached = try await engine.view.callAsyncJavaScript(
            "const last = window.asked.at(-1); return JSON.stringify([last.target.provider, last.target.address.action, last.argument.secret === typed, last.argument.username, last.argument.kind])",
            arguments: ["typed": typed], in: nil, contentWorld: .page) as? String
        XCTAssertEqual(reached, #"["tilo.vault","add",true,"ada","key"]"#)
        // The host repeated it three times over. Nothing of it crosses to the app.
        let raw = try await engine.ask(.vault, "add", of: "vault", ["kind": "key", "belongsTo": ["https://api.example"]],
                                       protected: (field: "secret", value: Protected(typed)))
        XCTAssertEqual(raw["echoed"], "you sent  twice ")
        XCTAssertNil(raw["secret"])
        XCTAssertEqual(raw["nested"], [["note": ""]])
        XCTAssertFalse(raw.text.contains("s3cret"))
        // A refusal that repeats a key says the rest and not the key.
        do {
            try await engine.keepModelKey(Protected("sk-typed-key"), for: "anthropic")
            XCTFail("a refused key was taken for kept")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .refused(code: "key-refused", reason: "The provider did not accept "))
        }
        // Nothing went to the pasteboard, and the value says nothing of itself.
        XCTAssertEqual(NSPasteboard.general.changeCount, board)
        let protected = Protected(typed)
        XCTAssertEqual("\(protected)", "Protected")
        XCTAssertEqual(String(reflecting: protected), "Protected")
        XCTAssertTrue(Mirror(reflecting: protected).children.isEmpty)
        var dumped = ""
        dump(protected, to: &dumped)
        XCTAssertFalse(dumped.contains("s3cret"))
        XCTAssertFalse(Protected(typed).isEmpty)
    }

    func testAnAnswerThatStillHoldsWhatWasTypedIsHandedToNobody() async throws {
        try await enterAsADevice()
        // Should the script ever fail to take a protected value out, the engine stops the answer.
        do {
            _ = try await engine.ask("return JSON.stringify({ok: {said: hidden}})", ["hidden": "typed \"once\""], hiding: "typed \"once\"")
            XCTFail("an answer that holds a protected value crossed to the app")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .noAnswer)
        }
        let plain = try await engine.ask("return JSON.stringify({ok: {said: 'nothing of it'}})", [:], hiding: "typed \"once\"")
        XCTAssertEqual(plain, ["said": "nothing of it"])
    }

    func testNoTextUnderASecretsNameCrossesToTheApp() async throws {
        try await enterAsADevice()
        // The host said a key, a token and a secret where none belongs.
        let raw = try await engine.ask(.models, "list", of: "models")
        let provider = raw["providers"]?[0]
        XCTAssertEqual(provider?["id"], "anthropic")
        XCTAssertNil(provider?["apiKey"])
        XCTAssertNil(provider?["token"])
        XCTAssertNil(provider?["secret"])
        XCTAssertFalse(raw.text.contains("said-by-mistake"))
        // What is no secret keeps its name: an assignment's kind, a provider's id.
        XCTAssertEqual(raw["assignments"]?[0]?["kind"], "subtask")
    }

    func testOnlyTheFirstEntryOfADocumentIsTakenAndTheHostCannotBeReplaced() async throws {
        try await enterAsADevice()
        try await run(#"""
            globalThis.tiloNativeHost.device({invoke: async () => { window.stolen = true; return {state: 'succeeded', output: {value: {providers: []}}}; }});
            try { globalThis.tiloNativeHost = {device() { window.stolen = true; }}; } catch { /* It cannot be written. */ }
            try { delete globalThis.tiloNativeHost; } catch { /* Nor taken away. */ }
            try { globalThis.tiloNativeHost.device = () => { window.stolen = true; }; } catch { /* Nor changed. */ }
            """#)
        let linked = try await engine.modelProviders()
        XCTAssertEqual(linked.providers.map(\.id), ["anthropic"])
        let stolen = try await engine.view.callAsyncJavaScript("return String(window.stolen) + ' ' + typeof globalThis.tiloNativeHost.device",
                                                               arguments: [:], in: nil, contentWorld: .page) as? String
        XCTAssertEqual(stolen, "undefined function")
    }

    func testAPageThatHandsANativeAppNothingSaysSo() async throws {
        // Tilo's page as it was before it knew of a native host, and any page in a browser.
        try await run("location.hash = '#/w/ws-1/so.tilo.chat'; enter()")
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        do {
            _ = try await engine.modelProviders()
            XCTFail("a page that hands over nothing answered for the person's providers")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .refused(code: "unavailable", reason: "This page hands a native app no way to its person's own providers"))
        }
        let whose = try await engine.whose()
        XCTAssertNil(whose)
        // Chat's own actions are the mount's, as before.
        let read = try await engine.read("notes.book")
        XCTAssertEqual(read, ["words": ""])
    }

    func testAFrameInsideThePageIsGivenNoContent() async throws {
        try await enterAsADevice()
        // What a live thing would put into the page from elsewhere, or from the site itself.
        try await run(#"""
            for (const address of ['https://tilo.test/app/inside.html', 'https://elsewhere.example/', 'data:text/html,<script>parent.postMessage(1, "*")<\/script>']) {
              const frame = document.createElement('iframe');
              frame.src = address;
              document.body.append(frame);
            }
            """#)
        try await until("the frames were refused") { self.engine.refusedFrames == 3 }
        // The page itself goes on as before.
        let linked = try await engine.modelProviders()
        XCTAssertEqual(linked.defaultProvider, "anthropic")
    }

    func testNothingOfThePersonsIsAskedOfAPageThatIsNotIn() async throws {
        do {
            _ = try await engine.keepInVault(Protected("typed"), kind: "key", belongsTo: ["https://api.example"])
            XCTFail("a page that is not in was handed a secret")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .notOpen)
        }
    }

    func testASendWaitsForChatToOfferItAtThePagesNextWord() async throws {
        try await run("location.hash = '#/w/ws-1/so.tilo.chat'; enter()")
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        let engine = engine!
        let sending = Task { try await engine.chatSend("Hello", to: ChatRecipient(.conversation, id: "c_chat", name: "chat")) }
        // Chat refuses the send it does not offer yet, and the engine asks again at the page's
        // word (the first of which says the page is in).
        try await until("Chat refused the send it does not offer yet") {
            ((try? await engine.view.callAsyncJavaScript("return tiloMount.heard.filter(call => call.action === 'send').length",
                                                         arguments: [:], in: nil, contentWorld: .page)) as? Int ?? 0) >= 1
        }
        try await run("window.sendPublished = true; write('Chat is ready')")
        let message = try await sending.value
        XCTAssertEqual(message, "m-1")
        let sent = try await engine.view.callAsyncJavaScript(
            "return JSON.stringify([...new Set(tiloMount.heard.filter(call => call.action === 'send').map(call => [call.resource, call.args.text, call.args.to[0].id].join(' ')))])",
            arguments: [:], in: nil, contentWorld: .page) as? String
        // Every try was the same send, and the one Chat took was the first it could take.
        XCTAssertEqual(sent, #"["chat.workspace Hello c_chat"]"#)
        let taken = try await engine.view.callAsyncJavaScript("return window.messagesTaken ?? 0", arguments: [:], in: nil, contentWorld: .page) as? Int
        XCTAssertEqual(taken, 1)
    }

    func testAnActionThatIsNeverOfferedIsRefusedInTilosWordsWhenThePatienceIsOver() async throws {
        try await run("location.hash = '#/w/ws-1/so.tilo.chat'; enter()")
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        do {
            _ = try await engine.whenPublished(within: .milliseconds(400)) {
                try await self.engine.invoke("send", on: "chat.workspace", ["to": [["id": "c_chat"]], "text": "Hello"])
            }
            XCTFail("an action nobody offers was taken for done")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .refused(code: "unknown_action", reason: "no action addressed by (chat.workspace, send)"))
        }
        // Another refusal is said at once, not waited on.
        let before = ContinuousClock.now
        do {
            _ = try await engine.whenPublished(within: .seconds(30)) { try await self.engine.invoke("refuse", on: "notes.book") }
            XCTFail("a refused action was taken for done")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .refused(code: "forbidden", reason: "Editors may not do this"))
        }
        XCTAssertLessThan(ContinuousClock.now - before, .seconds(5))
    }

    func testAfterAPairingTheWorkspaceIsLoadedAgainWithoutTheLinksSecret() async throws {
        var asked: [URL] = []
        engine.loads = { asked.append($0) }
        let secret = String(repeating: "cd", count: 48)
        try engine.pair(with: URL(string: "\(Self.site)/#/@host.example/w/own-1?pair=\(secret)")!)
        // The page that paired says it is in, under the address of the workspace its link
        // names. What it holds is whatever its site let the new device into.
        try await run("location.hash = '#/@host.example/w/own-1/so.tilo.chat'; pairing('246810'); enter()")
        try await until("the workspace is loaded again") { asked.count == 2 }
        XCTAssertEqual(asked.last?.absoluteString, "\(Self.site)/app/#/@host.example/w/own-1/so.tilo.chat")
        XCTAssertFalse(asked.last?.absoluteString.contains(secret) ?? true)
        XCTAssertFalse(stood.contains { if case .open = $0 { true } else { false } }, "the engine was in on the page that paired")
        // The page of that second load says it is in: now the engine is.
        try await run("document.documentElement.dataset.arrival = 'done again'")
        try await until("the engine is in") { self.engine.state == .open(route: "/@host.example/w/own-1/so.tilo.chat") }
        XCTAssertEqual(asked.count, 2)
    }

    func testADeviceLetInElsewhereIsOpenedInTheWorkspaceItsLinkNames() async throws {
        var asked: [URL] = []
        engine.loads = { asked.append($0) }
        let secret = String(repeating: "ab", count: 48)
        try engine.pair(with: URL(string: "\(Self.site)/#/@host.example/w/own-1?pair=\(secret)")!)
        XCTAssertEqual(asked.map(\.absoluteString), ["\(Self.site)/app/#/@host.example/w/own-1/so.tilo.chat?pair=\(secret)"])
        // The site lets a new device in at its home workspace, whatever its link names.
        try await run("location.hash = '#/@host.example/w/home-9/so.tilo.chat'; enter()")
        try await until("the workspace the link names is asked for") { asked.count == 2 }
        XCTAssertEqual(asked.last?.absoluteString, "\(Self.site)/app/#/@host.example/w/own-1/so.tilo.chat")
        XCTAssertFalse(stood.contains { if case .open = $0 { true } else { false } }, "the engine was in at a workspace nobody asked for")
        // There it is in.
        try await run("location.hash = '#/@host.example/w/own-1/so.tilo.chat'")
        try await until("the engine is in") { self.engine.state == .open(route: "/@host.example/w/own-1/so.tilo.chat") }
        XCTAssertEqual(asked.count, 2)
    }

    func testASiteThatKeepsOpeningAnotherWorkspaceIsSaidAndNotTriedForEver() async throws {
        var asked = 0
        engine.loads = { _ in asked += 1 }
        try engine.open(route: "/w/own-1")
        try await run("location.hash = '#/w/home-0/so.tilo.chat'; enter()")
        for n in 1...4 {
            try await Task.sleep(for: .milliseconds(120))
            try await run("location.hash = '#/w/home-\(n)/so.tilo.chat'")
        }
        try await until("it is said") { if case .failed = self.engine.state { true } else { false } }
        // The address itself, and three more tries.
        XCTAssertEqual(asked, 4)
        XCTAssertFalse(stood.contains { if case .open = $0 { true } else { false } })
    }

    private func run(_ script: String) async throws {
        _ = try await engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page)
    }

    private func until(_ what: String, _ holds: @MainActor () async -> Bool) async throws {
        for _ in 0..<200 {
            if await holds() { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        XCTFail("waited ten seconds for \(what)")
        throw CancellationError()
    }
}

final class EngineAddressTests: XCTestCase {
    private let settings = try! ShellSettings(site: "https://tilo.sh")
    private let secret = String(repeating: "ab", count: 48)

    func testAPairingLinkIsOpenedOnThePageThatShowsChatAlone() {
        let made = URL(string: "https://tilo.sh/#/@host.example:8443/w/1f0e/so.tilo.notes?pair=\(secret)")!
        XCTAssertEqual(EngineAddress.pairing(made, of: settings)?.absoluteString,
                       "https://tilo.sh/app/#/@host.example:8443/w/1f0e/so.tilo.chat?pair=\(secret)")
        let own = URL(string: "https://tilo.sh/#/w/1f0e?pair=\(secret)")!
        XCTAssertEqual(EngineAddress.pairing(own, of: settings)?.absoluteString, "https://tilo.sh/app/#/w/1f0e/so.tilo.chat?pair=\(secret)")
    }

    func testALinkThatPairsNothingOrLeadsElsewhereIsNoPairingLink() {
        for text in ["https://tilo.sh/#/w/1f0e", "https://tilo.sh/#/w/1f0e?pair=", "https://tilo.sh/#/w/1f0e?invite=abc",
                     "https://other.example/#/w/1f0e?pair=\(secret)", "https://tilo.sh/i/abcdefghijklmnop#\(secret)",
                     "https://tilo.sh/docs/#/w/1f0e?pair=\(secret)", "https://tilo.sh/#/settings?pair=\(secret)"] {
            XCTAssertNil(EngineAddress.pairing(URL(string: text)!, of: settings), text)
        }
    }

    func testAPageStandsElsewhereOnlyInAnotherWorkspace() {
        XCTAssertFalse(EngineAddress.isElsewhere("/@h.example/w/ws-1/so.tilo.chat", than: "/@h.example/w/ws-1"))
        // The same workspace under another name of its host.
        XCTAssertFalse(EngineAddress.isElsewhere("/w/ws-1/so.tilo.chat", than: "/@127.0.0.1:24100/w/ws-1"))
        XCTAssertTrue(EngineAddress.isElsewhere("/@h.example/w/ws-2/so.tilo.chat", than: "/@h.example/w/ws-1"))
        XCTAssertTrue(EngineAddress.isElsewhere("/me/so.tilo.chat", than: "/w/ws-1"))
        XCTAssertTrue(EngineAddress.isElsewhere("/settings", than: "/w/ws-1"))
        // The person's own is wherever the site opens it, and nothing asked for is nowhere else.
        XCTAssertFalse(EngineAddress.isElsewhere("/@h.example/w/ws-2", than: "/me"))
        XCTAssertFalse(EngineAddress.isElsewhere("/@h.example/w/ws-2", than: nil))
        XCTAssertEqual(EngineAddress.workspace(ofPage: URL(string: "https://tilo.sh/app/#/@h.example/w/ws-1/so.tilo.chat?pair=\(secret)")!),
                       "/@h.example/w/ws-1")
        XCTAssertNil(EngineAddress.workspace(ofPage: URL(string: "https://tilo.sh/app/")!))
    }

    func testAnAppsPageNamesItsWorkspaceAndItsRecord() {
        XCTAssertEqual(EngineAddress.page(of: "so.tilo.chat", in: "/me/so.tilo.notes", of: settings)?.absoluteString,
                       "https://tilo.sh/app/#/me/so.tilo.chat")
        XCTAssertEqual(EngineAddress.page(of: "so.tilo.word-processor", record: "uuid:1f0e", in: "/@h.example/w/ws/so.tilo.chat", of: settings)?.absoluteString,
                       "https://tilo.sh/app/#/@h.example/w/ws/so.tilo.word-processor/uuid:1f0e")
        XCTAssertNil(EngineAddress.page(of: "not an app", in: "/me", of: settings))
        XCTAssertNil(EngineAddress.page(of: "so.tilo.chat", in: "/w/", of: settings))
        XCTAssertNil(EngineAddress.page(of: "so.tilo.chat", in: "w/ws", of: settings))
    }
}

final class JSONTests: XCTestCase {
    func testAValueKeepsItsKindThroughText() {
        let value: JSON = ["on": true, "count": 1, "share": 0.5, "name": "Ada", "none": nil, "list": [false, 0]]
        XCTAssertEqual(JSON(text: value.text), value)
        XCTAssertEqual(JSON(text: "{\"on\":true,\"count\":1}"), ["on": true, "count": 1])
        // One and true are different values, though Foundation holds both as numbers.
        XCTAssertNotEqual(JSON(text: "[true]"), JSON(text: "[1]"))
        XCTAssertNil(JSON(text: "not json"))
        XCTAssertEqual(JSON(any: value.any), value)
    }
}
#endif
