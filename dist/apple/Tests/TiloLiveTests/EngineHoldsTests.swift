#if os(macOS)
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// What the engine does when its page stops after it was in, and when an ask is never answered:
/// against a page that speaks as Tilo's does. No window is opened, nothing is fetched and
/// nothing is kept.
@MainActor
final class EngineHoldsTests: XCTestCase {
    private static let site = "https://tilo.test"
    private static let inside = "/w/ws-1/so.tilo.chat"
    private var engine: TiloEngine!
    private var loaded = 0
    private var stood: [TiloEngine.State] = []
    private var listening: [Task<Void, Never>] = []

    override func setUp() async throws {
        engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        loaded = 0
        stood = []
        let engine = engine!
        listening = [Task { for await state in engine.states() { self.stood.append(state) } }]
        engine.view.loadHTMLString(Self.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page says where it stands") { self.stood.contains(.arriving("load")) }
        engine.loads = { [weak self] _ in self?.loaded += 1 }
    }

    override func tearDown() async throws {
        for task in listening { task.cancel() }
        engine = nil
    }

    /// As Tilo's page on its way in and once it is in: its step on the root element, its words in
    /// the status line, its mount named once it is in. `silent` makes it a page that takes an
    /// ask and never answers it.
    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status">Loading Tilo…</span><div id="semantics"></div>
    <script>
      const root = document.documentElement;
      window.owed = [];
      const status = words => { document.getElementById('status').textContent = words; };
      window.fail = words => { status(words); root.dataset.arrival = 'stopped'; root.dataset.entry = 'failed'; };
      window.leave = () => { status(''); root.dataset.entry = ''; root.dataset.arrival = 'load'; };
      window.enter = () => {
        status('');
        location.hash = '#/w/ws-1/so.tilo.chat';
        root.dataset.arrival = 'done';
        root.dataset.entry = 'open';
        globalThis.tiloMount = {
          semantics: document.getElementById('semantics'),
          actions: () => [],
          // An answer that is never given stays owed: the system itself ends an ask whose answer
          // nothing could give any more.
          invokeAction: async () => window.silent ? new Promise(answer => { window.owed.push(answer); })
            : {state: 'completed', output: {value: {words: 'read'}}},
        };
      };
    </script></html>
    """#

    private func inPage(_ script: String) async throws {
        _ = try await engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page)
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

    private func getIn() async throws {
        try engine.open(route: "/w/ws-1")
        try await inPage("enter()")
        try await until("the engine is in") { self.engine.state == .open(route: Self.inside) }
        XCTAssertEqual(loaded, 1)
    }

    /// An ask whose answer never comes ends by itself, so whoever asked can ask again; the page
    /// stays in.
    func testAnAskThatIsNeverAnsweredEndsByItselfAndThePageStaysIn() async throws {
        try await getIn()
        engine.askPatience = .milliseconds(300)
        try await inPage("window.silent = true")
        let engine = engine!
        var ended: String?
        let asking = Task { @MainActor in
            do {
                _ = try await engine.read("notes.book")
                ended = "answered"
            } catch TiloEngine.Problem.noAnswer {
                ended = "no answer"
            } catch {
                ended = "\(error)"
            }
        }
        defer { asking.cancel() }
        do {
            try await until("the ask ends", within: .seconds(5)) { ended != nil }
        } catch {
            // The page is put away, so that the ask does not outlive the test.
            try await inPage("leave()")
            throw error
        }
        XCTAssertEqual(ended, "no answer")
        XCTAssertEqual(engine.state, .open(route: Self.inside))
        // The next ask of the same page is answered.
        try await inPage("window.silent = false")
        let read = try await engine.read("notes.book")
        XCTAssertEqual(read["words"]?.string, "read")
    }
}
#endif
