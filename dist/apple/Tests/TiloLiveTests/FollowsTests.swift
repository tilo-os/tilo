#if os(macOS)
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// A native app is told that something it shows has changed: the engine's ask for which of the
/// followed documents changed, and the one follower that holds that ask for every part of the
/// window. Against a page that speaks as Tilo's does, whose workspace keeps a feed of its
/// changes. No window is opened, nothing is fetched, nothing kept.
@MainActor
final class FollowsTests: XCTestCase {
    private static let site = "https://tilo.test"
    private static let inside = "/w/ws-1/so.tilo.chat"
    private static let tasks = Followed(app: "so.tilo.tasks")
    private static let note = Followed(app: "so.tilo.notes", document: "n-1")
    private var engine: TiloEngine!
    private var follower: ChangeFollower!
    private var stood: [TiloEngine.State] = []
    /// Which part read again, in the order they did.
    private var read: [String] = []
    private var listening: [Task<Void, Never>] = []

    override func setUp() async throws {
        engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        stood = []
        read = []
        let engine = engine!
        listening = [Task { for await state in engine.states() { self.stood.append(state) } }]
        engine.view.loadHTMLString(Self.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page says where it stands") { self.stood.contains(.arriving("load")) }
        engine.loads = { _ in }
    }

    override func tearDown() async throws {
        for task in listening { task.cancel() }
        follower = nil
        engine = nil
    }

    /// As Tilo's page once it is in, with the entry of its device handed to the app. Behind the
    /// entry the workspace keeps a feed of its changes as it tells this person of them
    /// (`window.feed`), and `changed` answers as the page's own does: from now where no cursor
    /// is given, else what was asked for and changed since, held until something did.
    ///   `write(app, document, kept)`  another device or the workspace's server writes; `kept`
    ///                                 is a write to what the workspace keeps from this person:
    ///                                 it wakes whoever waits and the feed tells them nothing
    ///   `window.tellsNothing`         a page whose entry has no such ask
    ///   `window.refuses`              a page whose entry says it cannot tell
    ///   `window.fails`                how many of the next asks go unanswered
    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status">Loading Tilo…</span><div id="semantics"></div>
    <script>
      \#(testDeviceHandoff)
      const root = document.documentElement;
      window.feed = []; window.asks = []; window.fails = 0;
      window.write = (app, document, kept) => { if (!kept) window.feed.push({app, document}); window.woke?.(); };
      window.enter = async () => {
        const device = {
          person: 'ad'.repeat(32), workspace: 'ws-1',
          catalogue: async () => ({actions: []}), status: async invocation => ({invocation, state: 'succeeded'}),
          ownWorkspace: async () => ({id: 'own-7', name: 'Mine', origin: 'https://tilo.example:8443'}),
          invoke: async () => { throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'}); },
        };
        if (!window.tellsNothing) device.changed = async ({of, after, waitMs}) => {
          window.asks.push({of, after, waitMs});
          if (window.refuses) throw new Error('unknown_action: This page cannot tell when a document changed');
          if (window.fails > 0) { window.fails -= 1; throw new Error('unanswered: The workspace\'s server did not say what changed'); }
          if (window.out) throw new Error('busy: One ask for changes is out already');
          if (after === null) return {changed: [], cursor: String(window.feed.length)};
          const named = () => window.feed.slice(Number(after))
            .filter(one => of.some(asked => asked.app === one.app && (asked.document == null || asked.document === one.document)));
          if (!named().length && waitMs > 0) {
            window.out = true;
            await new Promise(over => { window.woke = () => { if (named().length) over(); }; setTimeout(over, waitMs); });
            window.out = false;
          }
          return {changed: [...new Map(named().map(one => [one.app + '\n' + one.document, one])).values()], cursor: String(window.feed.length)};
        };
        // Tilo's page hands the entry of its device to the app's own script, which stands in the
        // document to take it (Tilo's page calls it by its name).
        await giveDevice(Object.freeze(device));
        document.getElementById('status').textContent = '';
        location.hash = '#/w/ws-1/so.tilo.chat';
        root.dataset.arrival = 'done';
        root.dataset.entry = 'open';
        globalThis.tiloMount = {semantics: document.getElementById('semantics'), actions: () => [],
          invokeAction: async () => ({state: 'completed', output: {value: null}})};
      };
    </script></html>
    """#

    private func inPage(_ script: String) async throws {
        _ = try await engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page)
    }

    private func number(_ script: String) async -> Int {
        ((try? await engine.view.callAsyncJavaScript("return \(script)", arguments: [:], in: nil, contentWorld: .page)) as? Int) ?? -1
    }

    /// What the page was asked, an ask a part: where from, how long held, and of what.
    private func asked() async -> String {
        let script = """
            return window.asks.map(one => one.after + '/' + one.waitMs + '/'
              + one.of.map(thing => thing.app + ':' + (thing.document ?? '*')).sort().join(',')).join(' | ')
            """
        return ((try? await engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page)) as? String) ?? ""
    }

    private func until(_ what: String, within patience: Duration = .seconds(12), _ holds: @MainActor () async -> Bool) async throws {
        let end = ContinuousClock.now.advanced(by: patience)
        while ContinuousClock.now < end {
            if await holds() { return }
            try await Task.sleep(for: .milliseconds(25))
        }
        XCTFail("waited \(patience) for \(what)")
        throw CancellationError()
    }

    private func getIn() async throws {
        try engine.open(route: "/w/ws-1")
        try await inPage("enter()")
        try await until("the engine is in") { self.engine.state == .open(route: Self.inside) }
    }

    /// The one follower of this test, with a part for the tasks and a part for one note.
    private func follow(holding hold: Duration = .seconds(2)) -> ChangeFollower {
        let follower = ChangeFollower(engine: engine)
        follower.hold = hold
        follower.firstPause = .milliseconds(40)
        follower.follow(Self.tasks) { [weak self] in self?.read.append("tasks") }
        follower.follow(Self.note) { [weak self] in self?.read.append("note") }
        self.follower = follower
        return follower
    }

    private func held() async throws {
        try await until("the page holds an ask") { await self.number("window.out === true ? 1 : 0") == 1 }
    }

    func testTheEngineSaysWhichOfTheFollowedDocumentsChangedByNameAndFromWhereToAskOn() async throws {
        try await getIn()
        // Nothing that happened before the first ask is news: its answer is where to ask on from.
        try await inPage("write('so.tilo.tasks', 'old')")
        let first = try await engine.changed(of: [Self.tasks])
        XCTAssertEqual(first.changed, [])
        XCTAssertEqual(first.cursor, "1")
        try await inPage("write('so.tilo.tasks', 'part-1'); write('so.tilo.tasks', 'part-1'); write('so.tilo.chat', ''); write('so.tilo.tasks', '')")
        let told = try await engine.changed(of: [Self.tasks], after: first.cursor)
        XCTAssertEqual(told.changed, [Followed(app: "so.tilo.tasks", document: "part-1"), Followed(app: "so.tilo.tasks", document: "")])
        XCTAssertEqual(told.cursor, "5")
        // One document of an app is followed by its id, and an ask may be held.
        let one = try await engine.changed(of: [Followed(app: "so.tilo.tasks", document: "")], after: first.cursor, waiting: .milliseconds(1500))
        XCTAssertEqual(one.changed, [Followed(app: "so.tilo.tasks", document: "")])
        let asks = await asked()
        XCTAssertEqual(asks, "null/0/so.tilo.tasks:* | 1/0/so.tilo.tasks:* | 1/1500/so.tilo.tasks:")
    }

    func testAPageThatHasNoSuchAskIsSaidSoAndIsNoFailure() async throws {
        try await inPage("window.tellsNothing = true")
        try await getIn()
        do {
            _ = try await engine.changed(of: [Self.tasks])
            XCTFail("a page with no such ask answered one")
        } catch TiloEngine.Problem.refused(let code, _) {
            XCTAssertEqual(code, "unknown_action")
        }
    }

    /// A change made by another device or by the workspace's server shows within a few seconds,
    /// with none of the moments a part reads at by itself: the device does not get in, the app
    /// calls nothing, its person is told nothing, no answer ends.
    func testAPartReadsAgainWhenWhatItShowsWasChangedElsewhereAndAtNoOtherWord() async throws {
        try await getIn()
        _ = follow()
        try await held()
        let began = ContinuousClock.now
        try await inPage("write('so.tilo.tasks', 'part-1')")
        try await until("the tasks' part read again", within: .seconds(3)) { self.read == ["tasks"] }
        XCTAssertLessThan(ContinuousClock.now - began, .seconds(3))
        // What no part shows reads nothing again; the one note a part shows does, and only it.
        try await held()
        try await inPage("write('so.tilo.chat', 'part-0'); write('so.tilo.notes', 'n-2')")
        try await Task.sleep(for: .milliseconds(400))
        XCTAssertEqual(read, ["tasks"])
        try await inPage("write('so.tilo.notes', 'n-1')")
        try await until("the note's part read again", within: .seconds(3)) { self.read == ["tasks", "note"] }
        // Many changes of what one part shows, written at once, are one read.
        try await held()
        try await inPage("for (let at = 0; at < 20; at++) write('so.tilo.tasks', 'part-' + (at % 3))")
        try await until("the tasks' part read again", within: .seconds(3)) { self.read == ["tasks", "note", "tasks"] }
        try await held()
        XCTAssertEqual(read, ["tasks", "note", "tasks"])
    }

    func testAWriteToWhatIsKeptFromThePersonReadsNothingAgainAndEndsNoAsk() async throws {
        try await getIn()
        _ = follow(holding: .seconds(3))
        try await held()
        let before = await number("window.asks.length")
        try await inPage("write('so.tilo.tasks', 'part-9', true)")
        try await Task.sleep(for: .milliseconds(600))
        XCTAssertEqual(read, [])
        // The ask that was out is still the one that is out: the write was no word.
        let after = await number("window.asks.length")
        XCTAssertEqual(after, before)
        let out = await number("window.out === true ? 1 : 0")
        XCTAssertEqual(out, 1)
    }

    func testAnAskThatGoesUnansweredIsMadeAgainByItselfAndOneThatEndedWithNothingIsAskedOn() async throws {
        try await getIn()
        try await inPage("window.fails = 2")
        _ = follow(holding: .milliseconds(300))
        try await held()
        // Two asks went unanswered, one said where the feed stands, and the page holds the next.
        let first = await asked()
        XCTAssertEqual(first, "null/0/so.tilo.notes:n-1,so.tilo.tasks:* | null/0/so.tilo.notes:n-1,so.tilo.tasks:* | null/0/so.tilo.notes:n-1,so.tilo.tasks:* | 0/300/so.tilo.notes:n-1,so.tilo.tasks:*")
        // An ask that ended with nothing named is asked again, from where it ended.
        try await until("the page was asked on") { await self.number("window.asks.length") >= 6 }
        XCTAssertEqual(read, [])
        try await inPage("write('so.tilo.tasks', 'part-1')")
        try await until("the tasks' part read again", within: .seconds(3)) { self.read == ["tasks"] }
    }

    func testAPageThatSaysItCannotTellIsAskedOnceAndNoMore() async throws {
        try await getIn()
        try await inPage("window.refuses = true")
        let follower = follow()
        try await until("the follower knows") { follower.isRefused }
        try await Task.sleep(for: .milliseconds(400))
        let asks = await number("window.asks.length")
        XCTAssertEqual(asks, 1)
        XCTAssertEqual(read, [])
    }

    func testWhatIsFollowedWhileAnAskIsOutIsAskedForFromWhereThatAskBegan() async throws {
        try await getIn()
        let follower = ChangeFollower(engine: engine)
        follower.hold = .milliseconds(500)
        follower.follow(Self.tasks) { [weak self] in self?.read.append("tasks") }
        self.follower = follower
        try await held()
        // A part that begins to follow while the page holds an ask of the tasks alone, and a
        // change of what it shows a moment later, before that ask ended.
        follower.follow(Self.note) { [weak self] in self?.read.append("note") }
        try await inPage("write('so.tilo.notes', 'n-1')")
        try await until("the note's part read again", within: .seconds(4)) { self.read == ["note"] }
        let asks = await asked()
        XCTAssertTrue(asks.hasPrefix("null/0/so.tilo.tasks:* | 0/500/so.tilo.tasks:* | 0/500/so.tilo.notes:n-1,so.tilo.tasks:*"), asks)
    }
}
#endif
