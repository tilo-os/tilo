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
    private var stood: [TiloEngine.Standing] = []
    private var changes = 0
    private var listening: [Task<Void, Never>] = []

    override func setUp() async throws {
        engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        stood = []
        changes = 0
        let engine = engine!
        listening = [
            Task { for await standing in engine.standings() { self.stood.append(standing) } },
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
        try await until("the engine is in") { self.engine.standing == .open(route: "/@host.example/w/ws-1/so.tilo.chat") }
        // The code stays on the page after the approval: it is no longer what the page waits for.
        XCTAssertEqual(stood.last, .open(route: "/@host.example/w/ws-1/so.tilo.chat"))
    }

    func testARefusalIsSaidInTheSitesOwnWords() async throws {
        try await run("refuse('Lounge is invite-only for now.')")
        try await until("the refusal is told") { self.engine.standing == .refused("Lounge is invite-only for now.") }
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
        try await until("the engine is in") { if case .open = self.engine.standing { true } else { false } }
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
        try await until("the engine is in") { if case .open = self.engine.standing { true } else { false } }
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
        try await until("the engine is in") { if case .open = self.engine.standing { true } else { false } }
        try await until("the first word") { self.changes >= 1 }
        let before = changes
        try await run("told[0]()")
        try await until("the page's own word is passed on") { self.changes > before }
    }

    func testAnotherSitesAddressIsNeverOpened() throws {
        XCTAssertThrowsError(try engine.pair(with: URL(string: "https://elsewhere.example/#/w/ws-1?pair=\(String(repeating: "a", count: 96))")!))
        XCTAssertThrowsError(try engine.open(route: "/somewhere/else"))
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
