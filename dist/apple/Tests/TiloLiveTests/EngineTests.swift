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
    /// How often a listener that takes its time over each word read.
    private var readings = 0
    private var listening: [Task<Void, Never>] = []
    /// Every look the engine said of its page, in the order it said them.
    private var looks: [TiloEngine.Look?] = []

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

    func testHeadlessClientsCanForgetANamedStoreBeforeOpeningAnyPage() throws {
        let products = Bundle(for: Self.self).bundleURL.deletingLastPathComponent()
        for (name, options) in [("AssistantMac", ["--drive"]), ("TiloLiveProbe", ["--site", Self.site])] {
            let process = Process()
            process.executableURL = products.appendingPathComponent(name)
            process.arguments = options + ["--store", UUID().uuidString, "--forget"]
            let output = Pipe()
            process.standardOutput = output
            try process.run()
            process.waitUntilExit()
            XCTAssertEqual(process.terminationReason, .exit, name)
            XCTAssertEqual(process.terminationStatus, 0, name)
            let data = output.fileHandleForReading.readDataToEndOfFile()
            let reply = try JSONSerialization.jsonObject(with: data) as? [String: Bool]
            XCTAssertEqual(reply?["forgot"], true, name)
        }
    }

    /// As Tilo's page: `entry-page.mjs` writes its step on the root element and its words into
    /// the status line, names its mount `tiloMount` once it is in, and the mount describes
    /// what it shows in an element of its own. A refusal reads `code: reason`.
    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status">Loading Tilo…</span>
    <div id="semantics"></div>
    <script>
      \#(testDeviceHandoff)
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
          // Chat offers, on the item of a message that carries a view, the view `view`.
          const chat = what.app === 'so.tilo.chat';
          return {
            semantics: document.createElement('div'), editor: {focus: () => { window.focused = record; }},
            state: () => ({phase: 'presented', record}),
            actions: () => chat
              ? [{address: {resource: 'chat.workspace/messages/m6', action: 'reveal-message'}, resource_kind: 'chat.workspace.messages', view: {id: 'message', open_action: 'reveal-message', primary: true}},
                 {address: {resource: 'chat.workspace/messages/m7', action: 'reveal-message'}, resource_kind: 'chat.workspace.messages', view: {id: 'message', open_action: 'reveal-message', primary: true}},
                 {address: {resource: 'chat.workspace/messages/m7', action: 'show-view'}, resource_kind: 'chat.workspace.messages', view: {id: 'view', open_action: 'show-view'}}]
              : [{address: {resource: 'word-processor.document', action: 'read-text'}, resource_kind: 'word-processor.document'}],
            invokeAction: async (resource, action, args) => {
              if (chat) {
                (window.shownAlone ??= []).push({resource, action, args});
                // Its mount then says the size the view asks for, and says it again unchanged.
                setTimeout(() => { what.onStatus?.({content: {width: 320, height: 148.2}}); what.onStatus?.({content: {width: 320, height: 148.4}}); }, 0);
              }
              return {state: 'completed', output: {value: {from: record, action}}};
            },
            cleanup: async () => { window.tookDown = record; },
          };
        };
      };
      // A page that knows a native app may hold it: once it is in, it hands the host in
      // its document the entry of its device (web/native-host.mjs). What its host answers
      // here is deliberately careless: it says a key where none belongs and repeats a secret.
      window.asked = [];
      window.handOver = () => giveDevice(Object.freeze({
        person: 'ad'.repeat(32), workspace: 'ws-1',
        catalogue: async query => window.grantCatalogue?.(query) ?? {actions: []},
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
            case 'agents grant-status':
            case 'agents request-chat-grant':
            case 'agents request-assistant-work-grant':
            case 'agents request-app-grant':
            case 'agents revoke':
              return window.grantReceipt ?? done(window.grantAnswer);
            default:
              throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'});
          }
        },
      }));
      // As the page that asks before it pairs (entry-page.mjs, pairingAgree): a card of its own
      // that says whose device this becomes, which server adds it and what follows, with its
      // two buttons. What a press did is kept, so a test sees who pressed.
      window.askWhose = (whose, server) => {
        const card = document.createElement('div'); card.className = 'tilo-entry-card tilo-pair'; card.dataset.card = 'pair-whose';
        const heading = document.createElement('h2'); heading.textContent = 'Add this device?';
        const line = (text, name) => { const said = document.createElement('p'); said.textContent = text; if (name) said.className = name; return said; };
        const actions = document.createElement('div'); actions.className = 'tilo-entry-actions';
        const go = document.createElement('button'); go.type = 'button'; go.className = 'tilo-entry-primary'; go.textContent = 'Add this device';
        const stop = document.createElement('button'); stop.type = 'button'; stop.textContent = 'Stop';
        go.addEventListener('click', () => { card.remove(); window.agreed = true; });
        stop.addEventListener('click', () => { card.remove(); window.agreed = false; });
        actions.append(go, stop);
        card.append(heading, line(whose, 'tilo-pair-whose'), line(server, 'tilo-pair-whose'), line('From then on this device acts as them.'), actions);
        document.body.append(card);
      };
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

    /// The page says on its root element which look it draws in, the theme its person has in
    /// Tilo, and says it again whenever that changes. The engine hands each on, so that a frame
    /// around the page draws in the same. A word that names neither look is not taken, and
    /// nothing is known of a page that was left.
    func testThePageSaysWhichLookItDrawsInAndTheEngineSaysEachChange() async throws {
        looks = []
        let engine = engine!
        let hears = Task { for await look in engine.looks() { self.looks.append(look) } }
        defer { hears.cancel() }
        try await until("nothing is known of the page's look before it says one") { self.looks == [nil] }
        try await inPage("document.documentElement.dataset.look = 'dark'")
        try await until("the page's dark look is told") { self.looks == [nil, .dark] }
        XCTAssertEqual(engine.look, .dark)
        try await inPage("document.documentElement.dataset.look = 'sepia'")
        try await inPage("document.documentElement.dataset.look = 'light'")
        try await until("the page's light look is told, and no other word before it") { self.looks == [nil, .dark, .light] }
        XCTAssertEqual(engine.look, .light)
        engine.leave()
        try await until("nothing is known of a page that was left") { self.looks == [nil, .dark, .light, nil] }
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
        // That load says nothing. The page's own sentence is said, and until then nothing is loaded a
        // third time. (A device that is its person's is loaded again later: EngineHoldsTests.)
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
        // A refusal is Tilo's own, and leaves nothing there.
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

    func testWhatALiveThingDrawsIsSaidByItsOwnMount() async throws {
        _ = try await engine.view.callAsyncJavaScript(
            "location.hash = '#/w/ws-1/so.tilo.chat'; offerBeside(); enter()", arguments: [:], in: nil, contentWorld: .page)
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        // A mount that draws: a canvas in its place, a count of frames, the size its content asks
        // for and the names its semantics hold once they are asked for.
        _ = try await engine.view.callAsyncJavaScript(#"""
            const mounts = globalThis.tiloOpen;
            globalThis.tiloOpen = async (element, what) => {
              const made = await mounts(element, what);
              element.append(document.createElement('canvas'));
              const state = made.state;
              return {...made, requestSemantics: () => { window.askedSemantics = true; },
                state: () => ({...state(), frames: 3, logicalSize: [320, 149], content: {width: 320, height: 149},
                  semantics: {root: {semantics: {name: 'The tide comes in', children: [{value: 'on quiet feet'}]}, children: []}}})};
            };
            """#, arguments: [:], in: nil, contentWorld: .page)
        // The view a message carries: Chat mounted beside and told the message, as a person's app shows it.
        try await engine.show(live: "m7", .messageView(message: "m7", conversation: "c_chat"))
        let drawn = try await engine.drawn(live: "m7")
        XCTAssertEqual(drawn["phase"]?.string, "presented")
        XCTAssertEqual(drawn["frames"]?.number, 3)
        XCTAssertEqual(drawn["content"], .object(["width": .number(320), "height": .number(149)]))
        XCTAssertEqual(drawn["words"], .array([.string("The tide comes in"), .string("on quiet feet")]))
        XCTAssertEqual(drawn["canvas"], .array([.number(300), .number(150)]))
        XCTAssertNil(drawn["picture"], "a canvas read out gives a frame it drew before: no picture of it is handed on")
        let asked = try await engine.view.callAsyncJavaScript(
            "return window.askedSemantics === true", arguments: [:], in: nil, contentWorld: .page) as? Bool
        XCTAssertEqual(asked, true, "the mount is asked for its semantics before they are read")
        // A thing that is not mounted draws nothing, and the engine says so as it says a page that is not in.
        do {
            _ = try await engine.drawn(live: "nothing")
            XCTFail("a thing that is not mounted was read")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .notOpen)
        }
    }

    func testAMessagesViewIsChatMountedBesideToldItsMessageAndItsSizeIsHeardOnce() async throws {
        try await run("location.hash = '#/w/ws-1/so.tilo.chat'; offerBeside(); enter()")
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        var heard: [String] = []
        let listening = Task { @MainActor in
            for await (id, size) in self.engine.contentSizes() { heard.append("\(id) \(Int(size.width))x\(Int(size.height))") }
        }
        defer { listening.cancel() }
        try await engine.show(live: "m7", .messageView(message: "m7", conversation: "c_chat"))
        // Chat is mounted beside as the page itself opens it, and told the message by the action
        // its item offers for the view. The view's text never crossed.
        let did = try await engine.view.callAsyncJavaScript(
            "return JSON.stringify([opened.map(what => [what.app, 'view' in what, what.instance]), window.shownAlone])",
            arguments: [:], in: nil, contentWorld: .page) as? String
        XCTAssertEqual(did, #"[[["so.tilo.chat",false,100]],[{"resource":"chat.workspace/messages/m7","action":"show-view","args":{}}]]"#)
        try await until("the size the view asks for") { !heard.isEmpty }
        try await Task.sleep(for: .milliseconds(120))
        XCTAssertEqual(heard, ["m7 320x149"], "a size said again unchanged is not handed on twice")
        // A message that cannot be named is refused in Tilo's words, and nothing of it is left in the page.
        do {
            try await engine.show(live: "bad", .messageView(message: "no such/message", conversation: "c_chat"))
            XCTFail("a message that cannot be named was taken for shown")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .refused(code: "unknown_action", reason: "no such message"))
        }
        let kept = try await engine.view.callAsyncJavaScript(
            "return [...document.querySelectorAll('[data-tilo-live]')].map(place => place.dataset.tiloLive).join(' ')",
            arguments: [:], in: nil, contentWorld: .page) as? String
        XCTAssertEqual(kept, "m7")
    }

    /// A message's view is Chat mounted with no document named, and such a mount says it opened
    /// `@tilo:package-primary`. The page spells an app's own document of the workspace with no
    /// document at all: `route so.tilo.chat/: ready;`. Both texts stand here as the tour's page
    /// on the build server said them. Compared as they stand they never matched, so every
    /// mount for a message's view was put away again and none could be shown.
    func testAnAppsOwnDocumentHasJoinedWhereThePageNamesItWithNoDocument() async throws {
        let said = """
            workspace 69c55f12-02b3-49f6-be5f-4b18425329ba: joined through 456635f7
            workspace adf5993d-91ce-40c8-a2af-3cf3eddbd095: joined through 456635f7
            route so.tilo.activity/: ready;
            route so.tilo.browser-desktop/desktop: ready;
            route so.tilo.calendar/: ready;
            route so.tilo.canvas/derived:default@tab:0000000000000000: ready;
            route so.tilo.chat/: ready;
            """
        func joined(_ app: String, _ opened: Any, _ routes: String) async throws -> Bool? {
            try await engine.view.callAsyncJavaScript("return (\(EngineScript.joined))(app, opened, routes)",
                arguments: ["app": app, "opened": opened, "routes": routes], in: nil, contentWorld: .page) as? Bool
        }
        let chat = try await joined("so.tilo.chat", "@tilo:package-primary", said)
        XCTAssertEqual(chat, true, "Chat's own document of the workspace, as a mount with no document named says it")
        // A document with a name of its own has joined under that name, and only under it.
        let canvas = try await joined("so.tilo.canvas", "derived:default@tab:0000000000000000", said)
        XCTAssertEqual(canvas, true)
        let another = try await joined("so.tilo.canvas", "derived:default@tab:0000000000000001", said)
        XCTAssertEqual(another, false)
        // An app the page names nothing of has not joined, whatever its mount says.
        let tasks = try await joined("so.tilo.tasks", "@tilo:package-primary", said)
        XCTAssertEqual(tasks, false)
        // A mount that names nothing it opened has nothing to wait for.
        let nothing = try await joined("so.tilo.chat", NSNull(), said)
        XCTAssertEqual(nothing, true)
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
        try await run("location.hash = '#/w/ws-1/so.tilo.chat'; await handOver(); enter()")
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
                       ["tilo.models", "tilo.vault", "tilo.events", "tilo.telling", "tilo.initiative", "tilo.asking", "tilo.learned", "tilo.connections"])
        // With the engine's own pass: what the device could ask its host, and the engine does not.
        let refused = try await engine.view.callAsyncJavaScript(#"""
            const said = [];
            for (const provider of ['tilo.environment', 'tilo.mcp-server', 'tilo.device-pairing', 'tilo.accounts', 'tilo.server', 'agents',
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

    /// Evaluate a script in the page, for its effect.
    private func inPage(_ script: String) async throws {
        _ = try await engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page)
    }

    /// Enter as a page that says what its device heard, which is looked at before a word is handed on.
    private func enterWeighed() async throws {
        try await inPage("window.heardSoFar = 'heard 1 object(s)'; globalThis.tiloChannels = () => window.heardSoFar; location.hash = '#/w/ws-1/so.tilo.chat'; offerChanges(); enter()")
    }

    func testAPageThatSaysAWordAtEveryFrameIsHeardOnlyWhenWhatItsDeviceHeardMoved() async throws {
        try await enterWeighed()
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        try await until("the first word") { self.changes >= 1 }
        try await Task.sleep(for: .milliseconds(500))
        // A word at every frame, with nothing heard and nothing held anew: none is handed on.
        let before = changes
        try await inPage("for (let frame = 0; frame < 40; frame++) told[0]()")
        try await Task.sleep(for: .milliseconds(500))
        XCTAssertEqual(changes, before)
        // What the device heard moved: the page's next word is handed on.
        try await inPage("window.heardSoFar = 'heard 2 object(s)'; told[0]()")
        try await until("the word is handed on once something was heard") { self.changes > before }
        try await Task.sleep(for: .milliseconds(400))
        // While the app expects a change, a word is handed on though nothing was heard.
        let quiet = changes
        engine.expectChanges(for: .seconds(5))
        try await Task.sleep(for: .milliseconds(150))
        try await inPage("told[0]()")
        try await until("a word is handed on while the app expects one") { self.changes > quiet }
    }

    /// A page nobody sees gets its timers late, by up to a second. Here the page's timers never
    /// fire at all: a word of a change is handed on by the page's own message, and needs none.
    func testAWordOfAChangeIsHandedOnWithNoTimerOfThePage() async throws {
        try await inPage("location.hash = '#/w/ws-1/so.tilo.chat'; offerChanges(); enter()")
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        try await until("the first word") { self.changes >= 1 }
        try await Task.sleep(for: .milliseconds(300))
        try await inPage("window.setTimeout = () => 1; window.setInterval = () => 1")
        for word in 1...3 {
            let before = changes
            try await inPage("told[0]()")
            try await until("word \(word) of a change is handed on with no timer of the page") { self.changes > before }
        }
    }

    /// The same where the page says what its device heard, which is looked at before a word is
    /// handed on: what moved is handed on with no timer, and so is every word while the app
    /// expects a change, as it does while an answer is written.
    func testAWordThatIsWeighedIsHandedOnWithNoTimerOfThePage() async throws {
        try await enterWeighed()
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        try await until("the first word") { self.changes >= 1 }
        try await Task.sleep(for: .milliseconds(500))
        try await inPage("window.setTimeout = () => 1; window.setInterval = () => 1")
        // Nothing heard and nothing held anew: the word is not handed on.
        let quiet = changes
        try await inPage("told[0]()")
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(changes, quiet)
        // What the device heard moved: the page's next word is handed on.
        try await inPage("window.heardSoFar = 'heard 2 object(s)'; told[0]()")
        try await until("what the device heard is handed on with no timer of the page") { self.changes > quiet }
        // While the app expects a change, a word is handed on though nothing was heard.
        try await Task.sleep(for: .milliseconds(200))
        let before = changes
        engine.expectChanges(for: .seconds(5))
        try await Task.sleep(for: .milliseconds(150))
        try await inPage("told[0]()")
        try await until("a word is handed on while the app expects one, with no timer of the page") { self.changes > before }
    }

    /// While an answer is written the page says a word for every piece of it, and nothing holds
    /// those words back. The ones that come while the app reads are one word to it: it reads
    /// once more, not once for each.
    func testWordsThatComeWhileTheAppReadsAreOneWordToIt() async throws {
        try await inPage("location.hash = '#/w/ws-1/so.tilo.chat'; offerChanges(); enter()")
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        try await until("the first word") { self.changes >= 1 }
        try await Task.sleep(for: .milliseconds(300))
        let engine = engine!
        let reader = Task {
            for await _ in engine.changes() {
                self.readings += 1
                // A reading takes its time.
                try? await Task.sleep(for: .milliseconds(250))
            }
        }
        defer { reader.cancel() }
        try await Task.sleep(for: .milliseconds(100))
        try await inPage("for (let word = 0; word < 40; word++) told[0]()")
        try await Task.sleep(for: .milliseconds(1000))
        XCTAssertGreaterThanOrEqual(readings, 1, "forty words of a change were never read")
        XCTAssertLessThanOrEqual(readings, 2, "forty words in one breath were read \(readings) times")
    }

    func testWhatTheAppItselfChangedIsReadAgainWithoutThePagesWord() async throws {
        try await inPage("location.hash = '#/w/ws-1/so.tilo.chat'; enter()")
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        try await until("the first word") { self.changes >= 1 }
        try await Task.sleep(for: .milliseconds(300))
        let before = changes
        try await engine.invoke("rename", on: "notes.book/notes/n1", ["title": "Report"])
        try await until("the app's own change is a word") { self.changes > before }
        try await Task.sleep(for: .milliseconds(300))
        // A read changes nothing, and so says nothing: reading at a word never asks for another.
        let settled = changes
        _ = try await engine.read("notes.book")
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(changes, settled)
    }

    func testAnAskThatAPageNeverAnswersEndsWhenThatPageStartsOver() async throws {
        try await inPage("location.hash = '#/w/ws-1/so.tilo.chat'; enter()")
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        try await inPage("tiloMount.invokeAction = () => new Promise(() => {})")
        let engine = engine!
        let asking = Task { @MainActor in try await engine.invoke("rename", on: "notes.book") }
        try await Task.sleep(for: .milliseconds(200))
        // The page starts over under the ask, as it does once more after a device got in.
        try await inPage("document.documentElement.dataset.entry = ''; document.documentElement.dataset.arrival = 'load'")
        do {
            _ = try await asking.value
            XCTFail("an ask of a page that started over was answered")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .noAnswer)
        }
    }

    func testTheAppNamesItsDeviceAndNothingInThePageRenamesIt() async throws {
        // A name is one of the places the script fills; nothing it holds is read as another.
        let named = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing,
                               deviceName: "Tilo Assistant on Ada's Mac %PASS% " + String(repeating: "x", count: 80))
        named.view.loadHTMLString(Self.page, baseURL: URL(string: "\(Self.site)/app/"))
        let read = { (script: String) async -> String in
            ((try? await named.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page)) as? String) ?? ""
        }
        try await until("the page is there") { await read("return typeof globalThis.tiloNativeHost?.device") == "function" }
        let said = await read("try { globalThis.tiloNativeHost.name = 'Someone else'; } catch { /* It cannot be written. */ } return globalThis.tiloNativeHost.name")
        XCTAssertEqual(said, String(("Tilo Assistant on Ada's Mac %PASS% " + String(repeating: "x", count: 80)).prefix(64)))
        XCTAssertFalse(said.contains(named.pass))
        // An app that names nothing leaves the name to the page.
        let unnamed = try await engine.view.callAsyncJavaScript("return globalThis.tiloNativeHost.name", arguments: [:], in: nil, contentWorld: .page) as? String
        XCTAssertEqual(unnamed, "")
    }

    func testAProgramForAMacOrAPhoneTellsThePageNothingOfHowItDraws() {
        // What a simulator's program adds to the script is the compiler's to decide, by its own
        // condition for a simulator: no argument, setting or environment of a program says it.
        let script = EngineScript.source(origin: Self.site, unseen: true, pass: "a pass", providers: ["anthropic"], device: "A device")
        // No place of the script is left unfilled, whichever program this is.
        XCTAssertNil(script.range(of: "%[A-Z_]+%", options: .regularExpression))
        #if targetEnvironment(simulator)
        XCTAssertTrue(script.contains("name: \"A device\", html: true, /* a simulator offers the page no graphics adapter */\n"))
        XCTAssertTrue(script.contains("{...what, dom: true, /* shown as HTML in a simulator */ profile, device, workspace: scope.workspace,"))
        #else
        XCTAssertEqual(EngineScript.simulator.says, "")
        XCTAssertEqual(EngineScript.simulator.mounts, "")
        XCTAssertFalse(script.contains("html:"))
        XCTAssertFalse(script.contains("dom:"))
        // Its two places read as they do without a simulator's words.
        XCTAssertTrue(script.contains("name: \"A device\",\n"))
        XCTAssertTrue(script.contains("{...what, profile, device, workspace: scope.workspace,"))
        #endif
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

    func testAdmittedDeviceReadsAndPostsToTheAgentsChatWithoutAMountedWindow() async throws {
        try await run(#"""
        location.hash = '#/w/ws-1/so.tilo.chat';
        const provider = 'record:so.tilo.chat:';
        const messages = 'chain:derived:messages@derived:default@tab:0000000000000000';
        const offered = verb => ({provider, action: {resource_kind: 'chat.messages',
          address: {resource: messages, action: verb}, params: ['fields', 'where'].map(name => ({name}))}});
        window.sent = [];
        const nonce = crypto.randomUUID();
        if (tiloNativeHost.device({invoke: () => {}})) throw new Error('unconfirmed legacy handoff');
        const confirmed = tiloNativeHost.confirmDevice(nonce);
        __tiloLive.confirmDevice('wrong pass', nonce);
        webkit.messageHandlers.tiloLive.postMessage({kind: 'device-challenge', nonce});
        if (!await confirmed) throw new Error('the native application did not confirm the handoff');
        const entry = {
          person: 'ada', workspace: 'ws-1',
          workspaceRecord: async app => {
            if (app !== 'so.tilo.chat') throw new Error('wrong app');
            return provider;
          },
          catalogue: async query => {
            if (query.provider !== provider) throw new Error('wrong provider');
            if (query.target?.action === 'read' && query.target.resource.startsWith('chat.messages/messages/')) {
              return {rows: [{provider, action: {address: query.target}}]};
            }
            return {rows: ['read', 'post'].map(offered).filter(row =>
              query.target ? row.action.address.action === query.target.action && messages === query.target.resource :
              query.resource_name === 'chat.messages')};
          },
          invoke: async (target, args, options) => {
            if (target.provider !== provider) throw new Error('wrong provider');
            if (target.address.resource !== messages) {
              if (!window.canReadItem) throw new Error('unavailable: The connection was lost');
              const id = target.address.resource.split('/').pop();
              const row = sent.find(row => row.id === id);
              return {state: 'succeeded', output: {value: {...row, author: {person: 'ada'}, ts: 1794900000123}}};
            }
            if (target.address.action === 'post') {
              if (options.invocation !== args.id) throw new Error('the retry would use another invocation');
              if (!sent.some(row => row.id === args.id)) sent.push(args);
              if (window.loseReceipt) throw new Error('unavailable: Lost receipt');
            }
            const value = {messages: sent.map(row => ({id: row.id, value: {channel: row.channel,
              author: {person: 'ada'}, ts: 1794900000123, text: row.text}}))};
            return {state: 'succeeded', output: {value}};
          }
        };
        if (tiloNativeHost.device(entry, 'wrong challenge')) throw new Error('unconfirmed handoff');
        if (!tiloNativeHost.device(entry, nonce)) throw new Error('admitted device lost');
        document.documentElement.dataset.entry = 'open';
        """#)
        try await until("the engine holds the admitted device") { if case .open = self.engine.state { true } else { false } }
        let id = try await engine.chatSend("Hello", to: ChatRecipient(.agent, id: "scout", name: "Scout"))
        let latest = try await engine.chatLatest()
        XCTAssertEqual(latest, ["chat.messages/messages/" + id])
        let read = try await engine.chatConversation(#"direct:[{"person":"ada"},{"agent":"scout"}]"#)
        XCTAssertEqual(read?.first?.id, "chat.messages/messages/" + id)
        XCTAssertEqual(read?.first?.text, "Hello")
        XCTAssertEqual(read?.first?.at, Date(timeIntervalSince1970: 1_794_900_000.123))
        let channel = try await said("return sent[0].channel")
        XCTAssertEqual(channel, #"direct:[{"person":"ada"},{"agent":"scout"}]"#)
        let unauthorised = try await said("return await __tiloLive.invoke('chat.workspace', 'send', {}, false, null, '')")
        XCTAssertTrue(unauthorised.contains("unknown_action"))
        let count = try await said("return String(sent.length)")
        XCTAssertEqual(count, "1")
        let delivery = UUID()
        let recipient = ChatRecipient(.agent, id: "scout", name: "Scout")
        try await run("window.loseReceipt = true")
        do {
            _ = try await engine.chatSend("Again", to: recipient, delivery: delivery)
            XCTFail("An unreadable outcome was acknowledged")
        } catch TiloEngine.Problem.refused(let code, _) {
            XCTAssertEqual(code, "unavailable")
        }
        try await run("window.canReadItem = true")
        let retried = try await engine.chatSend("Again", to: recipient, delivery: delivery)
        XCTAssertEqual(retried, delivery.uuidString.lowercased())
        let posted = try await said("return String(sent.filter(row => row.text === 'Again').length)")
        XCTAssertEqual(posted, "1")

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

    // MARK: What the page asks before it pairs, and the workspace's agents

    /// What a script in the page answers, as text.
    private func said(_ script: String, _ arguments: [String: Any] = [:]) async throws -> String {
        try await engine.view.callAsyncJavaScript(script, arguments: arguments, in: nil, contentWorld: .page) as? String ?? ""
    }

    func testWhatThePageAsksBeforeItPairsIsToldInItsOwnWordsAndOnlyAnAnswerPressesItsButton() async throws {
        let whose = "You are adding this device to the devices of whoever made this link. If you did not make it yourself, stop."
        let server = "This device would join another server, own.example, which is not this site’s own."
        _ = try await said("askWhose(whose, server); return ''", ["whose": whose, "server": server])
        try await until("the page's question is told") { if case .asking = self.engine.state { true } else { false } }
        guard case .asking(let ask) = engine.state else { return XCTFail("the engine does not stand at the page's question") }
        XCTAssertEqual(ask, TiloEngine.PairingAsk(heading: "Add this device?",
                                                  said: [whose, server, "From then on this device acts as them."],
                                                  go: "Add this device", stop: "Stop"))
        // Nothing answers by itself: the card stands for as long as nobody presses.
        try await Task.sleep(for: .milliseconds(400))
        let untouched = try await said("return String(window.agreed)")
        XCTAssertEqual(untouched, "undefined")
        XCTAssertEqual(engine.state, .asking(ask))
        // Without the engine's pass no script answers through the engine's entry.
        let refused = try await said("return String(globalThis.__tiloLive.pairing('', true)) + ' ' + String(window.agreed)")
        XCTAssertEqual(refused, "false undefined")
        // The person's yes is a press of the page's own button, after which the page goes on.
        let pressed = await engine.answerPairing(goOn: true)
        XCTAssertTrue(pressed)
        let agreed = try await said("return String(window.agreed)")
        XCTAssertEqual(agreed, "true")
        try await run("pairing('135790')")
        try await until("the code to compare is told") { self.engine.state == .pairing(code: "135790") }
        // Nothing is left to answer, and nothing is pressed.
        let late = await engine.answerPairing(goOn: true)
        XCTAssertFalse(late)
    }

    func testStoppingPressesThePagesOwnStop() async throws {
        _ = try await said("askWhose('Whose it becomes.', 'Which server adds it.'); return ''")
        try await until("the page's question is told") { if case .asking = self.engine.state { true } else { false } }
        let pressed = await engine.answerPairing(goOn: false)
        XCTAssertTrue(pressed)
        let agreed = try await said("return String(window.agreed)")
        XCTAssertEqual(agreed, "false")
        try await until("the question is gone") { if case .asking = self.engine.state { false } else { true } }
    }

    func testTheWorkspacesAgentsAreAskedOfItsServerAsThePersonAndNothingElseOfAssistantsDocumentIs() async throws {
        try await enterAsADevice()
        // This server offers none of it. Its own refusal comes back, and it was asked as the
        // person, under Assistant's own document of the workspace.
        do {
            try await engine.askAgents("read")
            XCTFail("a server that offers no agents answered")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .refused(code: "unknown_action", reason: "No such action is offered"))
        }
        _ = try? await engine.askAgents("read", agent: "scout")
        // Whatever record the page says it shows, the server is asked for the workspace's own.
        try await run("globalThis.tiloMount.state = () => ({record: 'uuid:1f0e'})")
        _ = try? await engine.askAgents("ensure-personal-assistant")
        _ = try? await engine.askAgents("stop-sub-agent", ["agent": "scout"])
        let targets = try await said("return JSON.stringify(window.asked.map(one => [one.target.provider, one.target.address.resource, one.target.address.action, one.argument]))")
        // Assistant's own document of the workspace, as its server names it: the app and nothing after it.
        let assistant = JSON.string("record:so.tilo.assistant:")
        XCTAssertEqual(JSON(text: targets), [
            [assistant, "tilo.workspace.agent", "read", .null],
            [assistant, "tilo.workspace.agent/agents/scout", "read", .null],
            [assistant, "tilo.workspace.agent", "ensure-personal-assistant", .null],
            [assistant, "tilo.workspace.agent", "stop-sub-agent", ["agent": "scout"]],
        ])
        // Any other action, an action on one agent, a name that is none, and a script without
        // the engine's pass: the server is not asked at all.
        let cases: [(String, String?)] = [("rename-agent", nil), ("ensure-personal-assistant", "scout"), ("retire-agent", nil), ("set-agent-on-host", nil), ("read", "../chat.workspace")]
        for (action, agent) in cases {
            do {
                try await engine.askAgents(action, agent: agent)
                XCTFail("\(action) was asked of the server")
            } catch let problem as TiloEngine.Problem {
                XCTAssertEqual(problem, .refused(code: "unknown_action", reason: "no action addressed by (tilo.workspace.agent, \(action))"))
            }
        }
        let bare = try await said("return await globalThis.__tiloLive.agents('', 'read', null, null)")
        XCTAssertTrue(bare.contains("unknown_action"), bare)
        let count = try await said("return String(window.asked.length)")
        XCTAssertEqual(count, "4")
    }

    func testChatGrantReadsAndRequestsAddressOnlyTheNamedAgentOnTheCurrentDevice() async throws {
        try await enterAsADevice()
        let whose = try await engine.whose()
        let scope = try XCTUnwrap(whose)
        try await run("window.grantAnswer = {state: 'approved', ready: true, apps: ['so.tilo.chat'], expiresAtMs: 1794900000123}")
        let status = try await engine.chatGrantStatus(agent: "scout", forDevice: scope)
        XCTAssertEqual(status.state, .approved)
        XCTAssertTrue(status.ready)
        XCTAssertEqual(status.apps, ["so.tilo.chat"])
        XCTAssertEqual(status.expiresAtMs, 1_794_900_000_123)
        try await run("window.grantAnswer = {grant: 'scout', pairingCode: 'example-code'}")
        let code = try await engine.requestChatGrant(agent: "scout", forDevice: scope)
        XCTAssertEqual(code, "example-code")
        let calls = try await asked()
        XCTAssertEqual(JSON(text: calls), [
            ["target": ["provider": "agents", "address": ["resource": "agent:scout", "action": "grant-status"]], "argument": .null],
            ["target": ["provider": "agents", "address": ["resource": "agent:scout", "action": "request-chat-grant"]], "argument": .null],
        ])

        try await run("window.grantAnswer = {state: 'missing', ready: false, apps: [], expiresAtMs: null}")
        let missing = try await engine.chatGrantStatus(agent: "scout", forDevice: scope)
        XCTAssertEqual(missing.state, .missing)
        XCTAssertFalse(missing.ready)
        XCTAssertEqual(missing.apps, [])
        XCTAssertNil(missing.expiresAtMs)
        for field in ["ready: 1", "state: 'unknown'", "apps: [42]", "expiresAtMs: 12.5", "state: 'pending', ready: true"] {
            try await run("window.grantAnswer = {state: 'approved', ready: false, apps: [], expiresAtMs: null, " + field + "}")
            do {
                _ = try await engine.chatGrantStatus(agent: "scout", forDevice: scope)
                XCTFail("The malformed grant status was accepted: " + field)
            } catch let problem as TiloEngine.Problem {
                XCTAssertEqual(problem, .noAnswer, field)
            }
        }
        try await run("window.grantAnswer = {grant: 'scout', pairingCode: ''}")
        let alreadyApproved = try await engine.requestChatGrant(agent: "scout", forDevice: scope)
        XCTAssertNil(alreadyApproved)

        for answer in ["{grant: 'other', pairingCode: 'example-code'}", "{grant: 'scout'}",
                       "{grant: 'scout', pairingCode: null}", "{grant: 'scout', pairingCode: 42}"] {
            try await run("window.grantAnswer = " + answer)
            do {
                _ = try await engine.requestChatGrant(agent: "scout", forDevice: scope)
                XCTFail("The malformed grant receipt was accepted")
            } catch let problem as TiloEngine.Problem {
                XCTAssertEqual(problem, .noAnswer)
            }
        }
        try await run("window.grantReceipt = {state: 'failed', error: {code: 'not_permitted', message: 'Only the keeper may request this grant'}}")
        do {
            _ = try await engine.requestChatGrant(agent: "scout", forDevice: scope)
            XCTFail("A refused grant was accepted")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .refused(code: "not_permitted", reason: "Only the keeper may request this grant"))
        }
    }

    func testAssistantWorkGrantAndRevocationAddressOnlyTheNamedAgent() async throws {
        try await enterAsADevice()
        let whose = try await engine.whose()
        let scope = try XCTUnwrap(whose)
        try await run("window.grantAnswer = {grant: 'scout', pairingCode: 'example-code'}")
        let code = try await engine.requestAssistantWorkGrant(agent: "scout", forDevice: scope)
        XCTAssertEqual(code, "example-code")
        try await run("window.grantAnswer = {}")
        try await engine.revokeChatGrant(agent: "scout", forDevice: scope)
        let calls = try await asked()
        XCTAssertEqual(JSON(text: calls), [
            ["target": ["provider": "agents", "address": ["resource": "agent:scout", "action": "request-assistant-work-grant"]], "argument": .null],
            ["target": ["provider": "agents", "address": ["resource": "agent:scout", "action": "revoke"]], "argument": .null],
        ])

        try await run("window.grantAnswer = {grant: 'scout', pairingCode: ''}")
        let alreadyApproved = try await engine.requestAssistantWorkGrant(agent: "scout", forDevice: scope)
        XCTAssertNil(alreadyApproved)
        try await run("window.grantAnswer = {grant: 'other', pairingCode: 'example-code'}")
        do {
            _ = try await engine.requestAssistantWorkGrant(agent: "scout", forDevice: scope)
            XCTFail("Another agent's work grant was accepted")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .noAnswer)
        }
        try await run("window.grantReceipt = {state: 'failed', error: {code: 'not_permitted', message: 'Only the keeper may revoke this grant'}}")
        do {
            try await engine.revokeChatGrant(agent: "scout", forDevice: scope)
            XCTFail("A refused revocation was reported as successful")
        } catch let problem as TiloEngine.Problem {
            XCTAssertEqual(problem, .refused(code: "not_permitted", reason: "Only the keeper may revoke this grant"))
        }
    }

    func testAppGrantChoicesComeFromTheNamedAgentsActionAndOnlySelectedAppsAreRequested() async throws {
        try await enterAsADevice()
        let whose = try await engine.whose()
        let scope = try XCTUnwrap(whose)
        try await run(#"""
            window.queries = [];
            const param = (name, choices) => ({name, schema: {type: 'array', items: {type: 'string', oneOf: choices}}});
            window.grantChoices = [{const: 'so.tilo.chat', title: 'Chat'}, {const: 'so.tilo.notes', title: 'Workspace notes'},
              {const: 'so.tilo.sheets', title: 'Sheets'}];
            const other = [{const: 'so.tilo.other', title: 'Another choice'}];
            const row = (provider, resource, action, params) => ({provider, action: {address: {resource, action}, params}});
            window.grantCatalogue = query => {
              queries.push(query);
              return {rows: [
                row('other', 'agent:scout', 'request-app-grant', [param('apps', other)]),
                row('agents', 'agent:other', 'request-app-grant', [param('apps', other)]),
                row('agents', 'agent:scout', 'other-action', [param('apps', other)]),
                row('agents', 'agent:scout', 'request-app-grant', [param('other', other), param('apps', window.grantChoices)]),
              ]};
            };
            window.grantAnswer = {grant: 'scout', pairingCode: 'example-code'};
            """#)
        let apps = try await engine.chatGrantApps(agent: "scout", forDevice: scope)
        let choices = apps.sorted { $0.id < $1.id }
        XCTAssertEqual(choices.map(\.id), ["so.tilo.chat", "so.tilo.notes", "so.tilo.sheets"])
        XCTAssertEqual(choices.map(\.name), ["Chat", "Workspace notes", "Sheets"])
        let queries = try await said("return JSON.stringify(window.queries)")
        XCTAssertEqual(JSON(text: queries), [
            ["provider": "agents", "target": ["resource": "agent:scout", "action": "request-app-grant"]],
        ])

        let selection = ["so.tilo.chat", "so.tilo.notes"]
        let code = try await engine.requestAppGrant(agent: "scout", apps: selection, forDevice: scope)
        XCTAssertEqual(code, "example-code")
        let calls = try await asked()
        XCTAssertEqual(JSON(text: calls), [[
            "target": ["provider": "agents", "address": ["resource": "agent:scout", "action": "request-app-grant"]],
            "argument": ["apps": ["so.tilo.chat", "so.tilo.notes"]],
        ]])
        try await run("window.grantChoices = [{const: 'so.tilo.chat', title: 'Chat'}]")
        do {
            _ = try await engine.requestAppGrant(agent: "scout", apps: selection, forDevice: scope)
            XCTFail("An app the host stopped offering was requested")
        } catch TiloEngine.Problem.refused(let code, _) {
            XCTAssertEqual(code, "invalid_request")
        }
        let forwarded = try await asked()
        XCTAssertEqual(forwarded, calls)
        let checked = try await said("return JSON.stringify(window.queries)")
        let query: JSON = ["provider": "agents", "target": ["resource": "agent:scout", "action": "request-app-grant"]]
        XCTAssertEqual(JSON(text: checked), [query, query, query])
        for choices in ["[{const: 'so.tilo.notes', title: 'Notes'}]", "[{const: 'so.tilo.chat', title: 'Chat'}, {const: 'so.tilo.chat', title: 'Chat'}]"] {
            try await run("window.grantChoices = " + choices)
            do {
                _ = try await engine.chatGrantApps(agent: "scout", forDevice: scope)
                XCTFail("The malformed app grant catalogue was accepted")
            } catch TiloEngine.Problem.refused(let code, _) {
                XCTAssertEqual(code, "unknown_action")
            }
        }
    }

    func testAppGrantBridgeRejectsInvalidSelectionsAndUnscopedCatalogueQueries() async throws {
        try await enterAsADevice()
        let rejected = try await said(#"""
            const scope = {person: 'ad'.repeat(32), workspace: 'ws-1'};
            const apps = {apps: ['so.tilo.chat']};
            let queries = 0;
            window.grantCatalogue = () => { queries++; return {rows: []}; };
            const call = async (given, action, where, argument) =>
              JSON.parse(await __tiloLive.chatGrant(given, action, 'scout', where, argument)).error?.code;
            const catalogue = async (given, agent, where) =>
              JSON.parse(await __tiloLive.chatGrantApps(given, agent, where)).error?.code;
            const badArgs = [null, {}, {apps: []}, {apps: ['so.tilo.notes']}, {apps: ['so.tilo.chat', 'so.tilo.chat']},
              {apps: ['so.tilo.chat', '*']}, {apps: ['so.tilo.chat', 'so.tilo.*']}, {...apps, expiresInSecs: 3600},
              {apps: ['so.tilo.chat', ...Array.from({length: 64}, (_, i) => 'so.tilo.app' + i)]}];
            const invalid = [];
            for (const arg of badArgs) invalid.push(await call(pass, 'request-app-grant', scope, arg));
            for (const action of ['grant-status', 'request-chat-grant', 'request-assistant-work-grant', 'revoke'])
              invalid.push(await call(pass, action, scope, apps));
            return JSON.stringify({
              invalid: [...new Set(invalid)], scope: await call(pass, 'request-app-grant', null, apps),
              catalogue: [await catalogue('', 'scout', scope), await catalogue(pass, 'two agents', scope),
                await catalogue(pass, 'scout', null), await catalogue(pass, 'scout', {...scope, person: 'other'})],
              asked: window.asked.length, queries,
            });
            """#, ["pass": engine.pass])
        XCTAssertEqual(JSON(text: rejected), [
            "invalid": ["invalid_request"], "scope": "wrong_device",
            "catalogue": ["unknown_action", "unknown_action", "wrong_device", "wrong_device"], "asked": 0, "queries": 0,
        ])
    }

    func testChatGrantChecksTheEnginePassActionAgentAndDeviceBeforeAskingTheHost() async throws {
        try await enterAsADevice()
        let rejected = try await said(#"""
            const scope = {person: 'ad'.repeat(32), workspace: 'ws-1'};
            const check = async (given, action, agent, where) =>
              JSON.parse(await __tiloLive.chatGrant(given, action, agent, where)).error?.code;
            return JSON.stringify([
              await check('', 'request-chat-grant', 'scout', scope),
              await check(pass, 'other-action', 'scout', scope),
              await check(pass, 'grant-status', 'two agents', scope),
              await check(pass, 'request-chat-grant', 'scout', null),
              await check(pass, 'request-chat-grant', 'scout', {...scope, person: 'other'}),
              await check(pass, 'grant-status', 'scout', {...scope, workspace: 'ws-2'}),
              await check('', 'revoke', 'scout', scope),
              await check(pass, 'revoke', 'scout', null),
              await check(pass, 'revoke', 'scout', {...scope, workspace: 'ws-2'}),
              await check(pass, 'request-assistant-work-grant', 'scout', {...scope, person: 'other'}),
              window.asked.length,
            ]);
            """#, ["pass": engine.pass])
        XCTAssertEqual(JSON(text: rejected), [
            "unknown_action", "unknown_action", "unknown_action", "wrong_device", "wrong_device", "wrong_device",
            "unknown_action", "wrong_device", "wrong_device", "wrong_device", 0,
        ])
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

    /// As on a slow connection: the page fetches a thing's app the first time it mounts one.
    func testMountingALiveThingMayTakeLongerThanAnAskOfAServerAndIsNotEndedByThatBound() async throws {
        try await inPage("""
            location.hash = '#/w/ws-1/so.tilo.chat'; offerBeside();
            const mounts = globalThis.tiloOpen;
            globalThis.tiloOpen = async (element, what) => { await new Promise(on => setTimeout(on, 1200)); return mounts(element, what); };
            enter()
            """)
        try await until("the engine is in") { if case .open = self.engine.state { true } else { false } }
        engine.askPatience = .milliseconds(300)
        let shown = try await engine.show(live: "draft", .new(app: "so.tilo.word-processor"))
        XCTAssertEqual(shown, "uuid:made-now")
    }

    /// Tilo's page ends an ask of a person's server after thirty seconds, looked at every half
    /// second, and follows the making of a person's own workspace for 160 seconds: the engine's
    /// own bounds are longer, so that the page's word comes first.
    func testTheEnginesBoundsAreLongerThanThePagesOwn() {
        XCTAssertGreaterThan(engine.askPatience, .seconds(30.5))
        XCTAssertGreaterThan(engine.ownPatience, .seconds(160))
        XCTAssertGreaterThanOrEqual(engine.mountPatience, engine.askPatience)
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

    func testAPairingLinkTheSitesOwnComputerMadeIsOpenedUnderTheNameTheAppReachesItBy() throws {
        // The page a person has open on the computer that serves the site writes its code and
        // its link under a name only that computer can use. A phone's app reaches the same site
        // by the name other devices use, and reads the link as that site's.
        let phone = try ShellSettings(site: "https://tilo.wifi.example:24520")
        let made = URL(string: "http://tilo.localhost:24520/#/@127.0.0.1:24500/w/1f0e?pair=\(secret)")!
        XCTAssertEqual(EngineAddress.pairing(made, of: phone)?.absoluteString,
                       "https://tilo.wifi.example:24520/app/#/@tilo.wifi.example:24500/w/1f0e/so.tilo.chat?pair=\(secret)")
        let own = URL(string: "http://localhost:24520/#/me?pair=\(secret)")!
        XCTAssertEqual(EngineAddress.pairing(own, of: phone)?.absoluteString,
                       "https://tilo.wifi.example:24520/app/#/me/so.tilo.chat?pair=\(secret)")
        // Another port is some other program on someone's computer, and another site stays another site.
        for text in ["http://tilo.localhost:24120/#/w/1f0e?pair=\(secret)", "http://tilo.localhost/#/w/1f0e?pair=\(secret)",
                     "https://other.example:24520/#/w/1f0e?pair=\(secret)"] {
            XCTAssertNil(EngineAddress.pairing(URL(string: text)!, of: phone), text)
        }
    }

    func testNothingALinkSaysMakesAnotherSiteThisOne() throws {
        // The site's policy knows the one site the build was given, and the names that site's own
        // computer has for itself on the site's port. A link is judged whole, by where it leads:
        // what it carries inside (a name in its route, a second address, a sign-in before its
        // name) adds no name.
        let phone = try ShellSettings(site: "https://tilo.wifi.example:24520")
        let inside = "https://tilo.wifi.example:24520/#/w/1f0e?pair=\(secret)"
        let own = "http://tilo.localhost:24520/#/w/1f0e?pair=\(secret)"
        for text in ["https://other.example:24520/#/@tilo.wifi.example:24500/w/1f0e?pair=\(secret)",
                     "https://other.example/?next=\(inside)", "HTTPS://other.example/?next=\(inside)",
                     "HTTPS://other.example/?next=\(own)",
                     "tilo-assistant://open?link=\(inside)", "tilo-assistant://open?link=\(own)",
                     "http://tilo.localhost.other.example:24520/#/w/1f0e?pair=\(secret)",
                     "https://tilo.wifi.example:24520@other.example/#/w/1f0e?pair=\(secret)",
                     "http://other.example@tilo.localhost:24520/#/w/1f0e?pair=\(secret)",
                     "https://tilo.wifi.example.other.example:24520/#/w/1f0e?pair=\(secret)"] {
            let link = try XCTUnwrap(URL(string: text), text)
            XCTAssertNil(EngineAddress.pairing(link, of: phone), text)
        }
        // On the site's own computer nothing changes: its own name is its site, another site is not.
        let mac = try ShellSettings(site: "http://tilo.localhost:24520")
        XCTAssertEqual(EngineAddress.pairing(URL(string: own)!, of: mac)?.absoluteString,
                       "http://tilo.localhost:24520/app/#/w/1f0e/so.tilo.chat?pair=\(secret)")
        XCTAssertNil(EngineAddress.pairing(URL(string: inside)!, of: mac), inside)
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
