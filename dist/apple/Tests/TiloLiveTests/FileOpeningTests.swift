#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// Opening a file on Tilo itself. A file a message carries is fetched by a link Tilo's page
/// hands the app: an address on the app's own site, good for a short time. The system
/// downloads it into a file of this device; here a download of this test's own making stands
/// in for the system's, so nothing is fetched from anywhere. Against a page that speaks as
/// Tilo's does. No window is opened and nothing is shown.
@MainActor
final class FileOpeningTests: XCTestCase {
    private static let site = "https://tilo.test"

    /// What stands in for the system's download: it says which links it was handed.
    private final class Downloads: @unchecked Sendable {
        var asked: [URL] = []
        var bytes = Data("What the file says.".utf8)
        var fails = false
    }

    private var source: TiloSource!
    private var suite = ""
    private var loaded: [URL] = []
    private var made: [URL] = []
    private let downloads = Downloads()
    private var sessions: [Session] = []
    private var changes: [ConversationChange] = []
    private var listening: [Task<Void, Never>] = []

    private func start() async throws {
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { [weak self] in self?.loaded.append($0) }
        source = TiloSource(engine: engine, mainChat: .conversation(id: "c_chat", name: "chat"), livesInOwn: false, defaults: defaults)
        source.download = { [downloads] link in
            downloads.asked.append(link)
            if downloads.fails { throw URLError(.networkConnectionLost) }
            let file = FileManager.default.temporaryDirectory.appendingPathComponent("tilo-fetched-\(UUID().uuidString)")
            try downloads.bytes.write(to: file)
            return file
        }
        let source = source!
        listening = [
            Task { for await session in source.session() { self.sessions.append(session) } },
            Task { for await change in source.conversation(.main) { self.changes.append(change) } },
        ]
        engine.view.loadHTMLString(Self.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page is there") {
            (try? await engine.view.callAsyncJavaScript("return typeof window.enter", arguments: [:], in: nil, contentWorld: .page)) as? String == "function"
        }
    }

    override func tearDown() async throws {
        for task in listening { task.cancel() }
        UserDefaults.standard.removePersistentDomain(forName: suite)
        // Only what this test itself opened is removed: each lies in a folder of its own.
        for file in made { try? FileManager.default.removeItem(at: file.deletingLastPathComponent()) }
        source = nil
    }

    /// As Tilo's page that shows Chat alone, with its device's entry handed to the app. The
    /// entry hands a link as Tilo's page does: `link(content, name)` answers a path on the
    /// site, good for a short time. `links` is every link it was asked for; `linkPath` makes
    /// it answer that path instead; with `offersLink` false it hands none, as an older page's.
    /// The mount reads one conversation and each of its messages: `heard` is what it was asked.
    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status"></span><div id="semantics"></div>
    <script>
      \#(testDeviceHandoff)
      const root = document.documentElement;
      const ADA = 'ad'.repeat(32);
      window.kept = letter => letter.repeat(64);
      window.messages = [];
      window.heard = [];
      window.links = [];
      window.asks = [];
      window.offersLink = true;
      window.linkPath = null;
      const said = value => ({state: 'completed', output: {value}});
      window.post = (id, author, text, carried = []) => {
        window.messages.push({id, author, text, ts: 1794900000 + window.messages.length, question: null, share: null, attachments: carried});
        document.getElementById('semantics').textContent = String(Math.random());
      };
      const noSuch = () => { throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'}); };
      const link = async (content, name) => {
        window.links.push([content, name]);
        return {path: window.linkPath ?? `/api/content/tok-${window.links.length}/${encodeURIComponent(name)}`, expiresInSecs: 120};
      };
      window.enter = async () => {
        const device = Object.freeze({
          person: ADA, workspace: 'ws-1',
          catalogue: async () => ({actions: []}), status: async invocation => ({invocation, state: 'succeeded'}),
          ownWorkspace: async () => noSuch(),
          invoke: async target => { window.asks.push(target.provider + ' ' + target.address.action); return noSuch(); },
          ...(window.offersLink ? {link} : {}),
        });
        // Tilo's page hands the entry of its device to the app's own script, which stands in the
        // document to take it.
        await giveDevice(device);
        root.dataset.arrival = 'done';
        root.dataset.entry = 'open';
        // Chat's own name for the field that says which conversation a message is in: the read says it.
        let named = '';
        globalThis.tiloMount = {
          semantics: document.getElementById('semantics'),
          state: () => ({workspaceName: 'Ada\'s workspace'}),
          actions: () => [{address: {resource: 'chain:messages', action: 'read'}, resource_kind: 'chat.messages',
            params: [{name: 'cursor'}, {name: 'pageSize'}, {name: 'fields'}, {name: 'where'}]}],
          invokeAction: async (resource, action, args) => {
            window.heard.push(resource + ' ' + action);
            if (resource === 'chain:messages' && action === 'read') {
              const [field, wanted] = Object.entries(args.where ?? {})[0] ?? [];
              named = field ?? named;
              return said({kind: 'chat.messages', messages: [...window.messages].reverse().slice(0, args.pageSize ?? 50)
                .map(m => ({item: 'chat.messages/messages/' + m.id, name: m.text,
                  ...Object.fromEntries((args.fields ?? []).map(one => [one, one === field ? wanted : m[one] ?? null]))}))});
            }
            const message = resource.startsWith('chat.messages/messages/') ? window.messages.find(m => m.id === resource.split('/').pop()) : null;
            if (message && action === 'read') return said({...message, [named]: 'c_chat'});
            throw new Error(`unknown_action: no action addressed by (${resource}, ${action})`);
          },
        };
      };
    </script></html>
    """#

    private func inPage(_ script: String) async throws {
        _ = try await source.engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page)
    }

    private func page(_ script: String) async throws -> String {
        try await source.engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page) as? String ?? ""
    }

    private func until(_ what: String, _ holds: @MainActor () async -> Bool) async throws {
        for _ in 0..<240 {
            if await holds() { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        XCTFail("waited twelve seconds for \(what)")
        throw CancellationError()
    }

    private var held: [Item] {
        var conversation = Conversation()
        for change in changes { conversation.apply(change) }
        return conversation.items
    }

    /// The page is in, and one message with one file stands in the conversation: its tag.
    private func enter(_ before: String = "") async throws -> Attachment {
        try await inPage("\(before); location.hash = '#/w/ws-1/so.tilo.chat'; enter()")
        try await until("the app is signed in") { if case .signedIn = self.sessions.last { true } else { false } }
        try await until("the conversation is told") { !self.changes.isEmpty }
        try await inPage("post('m1', {agent: 'assistant'}, 'The report is ready.', [{kind: 'file', name: 'report.pdf', blob: kept('a'), size: 19}])")
        try await until("the message stands in the conversation") { self.held.count == 1 }
        guard case .message(let message) = held[0].content, let tag = message.attachments.first else {
            XCTFail("the message carries no file")
            throw CancellationError()
        }
        return tag
    }

    private func open(_ file: Attachment.ID) async throws -> URL {
        let opened = try await source.file(of: file)
        made.append(opened)
        return opened
    }

    private func refusal(of file: Attachment.ID) async -> SourceProblem? {
        do {
            made.append(try await source.file(of: file))
            XCTFail("a file was handed over that could not be")
            return nil
        } catch let problem as SourceProblem {
            return problem
        } catch {
            XCTFail("not a sentence for a person: \(error)")
            return nil
        }
    }

    private func links() async throws -> JSON? { JSON(text: try await page("return JSON.stringify(window.links)")) }

    func testAFileInAMessageIsFetchedByALinkAndLiesOnThisDeviceUnderItsName() async throws {
        try await start()
        let tag = try await enter()
        let before = loaded
        let file = try await open(tag.id)
        // The file lies where opened files lie, under its own name, with the bytes that came.
        XCTAssertEqual(file.lastPathComponent, "report.pdf")
        XCTAssertEqual(try Data(contentsOf: file), downloads.bytes)
        XCTAssertEqual(file.deletingLastPathComponent().deletingLastPathComponent().path, TiloFiles.opened.path)
        // Her server was asked for one link, to what the message says the file is kept under.
        let asked = try await links()
        XCTAssertEqual(asked, [[.string("tilo.content:" + String(repeating: "a", count: 64)), "report.pdf"]])
        // It was fetched once, at that path on the app's own site, and the view went nowhere.
        XCTAssertEqual(downloads.asked, [URL(string: "https://tilo.test/api/content/tok-1/report.pdf")!])
        XCTAssertEqual(loaded, before)
        let others = try await page("return JSON.stringify(window.asks.filter(one => /content|file|put|link/i.test(one)))")
        XCTAssertEqual(others, "[]")
        // Nothing of the link is in what the app keeps, nor in where the file lies.
        let beside = String(describing: UserDefaults.standard.persistentDomain(forName: suite) ?? [:])
        XCTAssertFalse(beside.contains("tok-1") || beside.contains("api/content"))
        XCTAssertFalse(file.path.contains("tok-1"))
        // A link is good for a short time and is not kept: a second press asks for a new one.
        let again = try await open(tag.id)
        XCTAssertEqual(downloads.asked.last, URL(string: "https://tilo.test/api/content/tok-2/report.pdf")!)
        XCTAssertEqual(downloads.asked.count, 2)
        XCTAssertNotEqual(again, file)
    }

    func testALinkThatLeadsOutOfTheAppsOwnSiteIsNotFollowed() async throws {
        try await start()
        let tag = try await enter()
        let outside = ["https://elsewhere.example/api/content/t/report.pdf", "//elsewhere.example/t/report.pdf",
                       "api/content/t/report.pdf", "", "/\\elsewhere.example/t/report.pdf", "javascript:alert(1)"]
        for path in outside {
            try await inPage("window.linkPath = \(JSON.string(path).text)")
            let problem = await refusal(of: tag.id)
            XCTAssertEqual(problem, .refused("The link to this file does not lead into this Tilo site"), path)
        }
        XCTAssertEqual(downloads.asked, [])
        // A path of the site is one, whatever stands in it.
        XCTAssertEqual(TiloEngine.link(on: "https://tilo.test", at: "/api/content/t/@elsewhere.example/a%20b.pdf")?.absoluteString,
                       "https://tilo.test/api/content/t/@elsewhere.example/a%20b.pdf")
        XCTAssertEqual(TiloEngine.link(on: "http://tilo.localhost:24520", at: "/api/content/t/a.pdf")?.absoluteString,
                       "http://tilo.localhost:24520/api/content/t/a.pdf")
    }

    func testAnOpenedFileIsKeptUnderTheLastPartOfItsNameAlone() throws {
        let fetched = { () throws -> URL in
            let file = FileManager.default.temporaryDirectory.appendingPathComponent("tilo-fetched-\(UUID().uuidString)")
            try Data("x".utf8).write(to: file)
            return file
        }
        for (name, last) in [("../../Library/LaunchAgents/so.example.plist", "so.example.plist"), ("/etc/passwd", "passwd"),
                             ("..", "file"), ("", "file"), ("notes.txt", "notes.txt")] {
            let kept = try TiloFiles.keep(try fetched(), as: name)
            made.append(kept)
            XCTAssertEqual(kept.lastPathComponent, last, name)
            // Each in a folder of its own, directly under where opened files lie.
            XCTAssertEqual(kept.deletingLastPathComponent().deletingLastPathComponent().path, TiloFiles.opened.path, name)
        }
    }

    func testAFileThatIsNoLongerInItsMessageSaysSoAndNoLinkIsAskedFor() async throws {
        try await start()
        _ = try await enter()
        for gone in ["chat.messages/messages/m1#3", "chat.messages/messages/m9#0", "chat.messages/messages/m1", "f1"] {
            let problem = await refusal(of: .init(gone))
            XCTAssertEqual(problem, .refused("This file is no longer in the conversation"), gone)
        }
        let asked = try await links()
        XCTAssertEqual(asked, [])
        XCTAssertEqual(downloads.asked, [])
    }

    func testADownloadThatFailsSaysSoByTheFilesNameAlone() async throws {
        try await start()
        let tag = try await enter()
        downloads.fails = true
        let problem = await refusal(of: tag.id)
        XCTAssertEqual(problem, .refused("report.pdf could not be fetched"))
        XCTAssertEqual(downloads.asked.count, 1)
    }

    func testAPageThatHandsNoLinkSaysTheSentenceAndIsAskedForNothing() async throws {
        try await start()
        let tag = try await enter("window.offersLink = false")
        let problem = await refusal(of: tag.id)
        XCTAssertEqual(problem, .refused("A file cannot be fetched into this app yet"))
        // The message was not read again, her server was asked for nothing, nothing was fetched.
        let reads = try await page("return JSON.stringify(window.heard.filter(one => one.startsWith('chat.messages/messages/')))")
        XCTAssertEqual(reads, "[]")
        let others = try await page("return JSON.stringify(window.asks.filter(one => /content|file|put|link/i.test(one)))")
        XCTAssertEqual(others, "[]")
        XCTAssertEqual(downloads.asked, [])
    }

    func testALinkIsHandedOnlyWithTheEnginesOwnPass() async throws {
        try await start()
        _ = try await enter()
        // Whatever else stands in the page is handed no link through the app's script.
        let withoutPass = try await page("return globalThis.__tiloLive.link('0'.repeat(64), {content: 'tilo.content:' + kept('a'), name: 'report.pdf'})")
        XCTAssertEqual(JSON(text: withoutPass)?["error"]?["code"]?.string, "unknown_action")
        let asked = try await links()
        XCTAssertEqual(asked, [])
    }
}
#endif
