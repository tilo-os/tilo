#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// Files in a conversation on Tilo itself. Chat keeps what a message carries beside its words:
/// a file is named there by the name it was posted under, by what the workspace keeps its
/// content under, and by its size. The app shows it so, and says in plain words what it cannot
/// do with it yet. Against a page that speaks as Tilo's does. No window is opened, nothing is
/// fetched, nothing kept.
@MainActor
final class FilesTests: XCTestCase {
    private static let site = "https://tilo.test"
    private var source: TiloSource!
    private var suite = ""
    private var asked: [URL] = []
    private var sessions: [Session] = []
    private var changes: [ConversationChange] = []
    private var listening: [Task<Void, Never>] = []

    private func start() async throws {
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { [weak self] in self?.asked.append($0) }
        source = TiloSource(engine: engine, mainChat: .conversation(id: "c_chat", name: "chat"), livesInOwn: false, defaults: defaults)
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
        source = nil
    }

    /// As Tilo's page that shows Chat alone, with its device's entry handed to the app. Its
    /// mount reads one conversation's messages, each with the fields that were asked for, and
    /// offers nothing else; behind the entry her server answers every ask as one it does not
    /// know. So nothing here takes a file's content or hands one over, as on Tilo today.
    /// `post(id, author, text, carried)` puts a message into the conversation the app shows;
    /// `kept(letter)` is a name content is kept under, made up as everything here. `heard` is
    /// what the mount was asked, `asks` what her server was.
    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status"></span><div id="semantics"></div>
    <script>
      \#(testDeviceHandoff)
      const root = document.documentElement;
      const ADA = 'ad'.repeat(32);
      window.kept = letter => letter.repeat(64);
      window.messages = [];
      window.heard = [];
      const said = value => ({state: 'completed', output: {value}});
      window.post = (id, author, text, carried = []) => {
        window.messages.push({id, author, text, ts: 1794900000 + window.messages.length, question: null, share: null, attachments: carried});
        document.getElementById('semantics').textContent = String(Math.random());
      };
      // What the app's device asked of its person's server, which offers the app nothing here.
      window.asks = [];
      const noSuch = () => { throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'}); };
      const device = Object.freeze({
        person: ADA, workspace: 'ws-1',
        catalogue: async () => ({actions: []}), status: async invocation => ({invocation, state: 'succeeded'}),
        ownWorkspace: async () => noSuch(),
        invoke: async target => { window.asks.push(target.provider + ' ' + target.address.action); return noSuch(); },
      });
      window.enter = async () => {
        // Tilo's page hands the entry of its device to the app's own script, which stands in the
        // document to take it.
        await giveDevice(device);
        root.dataset.arrival = 'done';
        root.dataset.entry = 'open';
        globalThis.tiloMount = {
          semantics: document.getElementById('semantics'),
          state: () => ({workspaceName: 'Ada\'s workspace'}),
          actions: () => [{address: {resource: 'chain:messages', action: 'read'}, resource_kind: 'chat.messages',
            params: [{name: 'cursor'}, {name: 'pageSize'}, {name: 'fields'}, {name: 'where'}]}],
          invokeAction: async (resource, action, args) => {
            window.heard.push(resource + ' ' + action);
            if (resource === 'chain:messages' && action === 'read') {
              // The read names the one conversation it wants, under Chat's own name for that field.
              const [named, wanted] = Object.entries(args.where ?? {})[0] ?? [];
              return said({kind: 'chat.messages', messages: [...window.messages].reverse().slice(0, args.pageSize ?? 50)
                .map(m => ({item: 'chat.messages/messages/' + m.id, name: m.text,
                  ...Object.fromEntries((args.fields ?? []).map(field => [field, field === named ? wanted : m[field] ?? null]))}))});
            }
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

    private func enter() async throws {
        try await inPage("location.hash = '#/w/ws-1/so.tilo.chat'; enter()")
        try await until("the app is signed in") { if case .signedIn = self.sessions.last { true } else { false } }
        try await until("the conversation is told") { !self.changes.isEmpty }
    }

    private var held: [Item] {
        var conversation = Conversation()
        for change in changes { conversation.apply(change) }
        return conversation.items
    }

    /// The files of the piece at `index`, as the views are given them.
    private func files(_ index: Int) -> [Attachment] {
        guard held.indices.contains(index), case .message(let message) = held[index].content else { return [] }
        return message.attachments
    }

    func testAFileThatCameWithAMessageStandsInItWithTheNameAndTheSizeChatGives() async throws {
        try await start()
        try await enter()
        try await inPage(#"""
          post('m1', {agent: 'assistant'}, 'The report is ready.', [{kind: 'file', name: 'report.pdf', blob: kept('a'), size: 48213}]);
          post('m2', {person: ADA}, '', [{kind: 'image', name: 'photo.png', blob: kept('b'), w: 640, h: 480},
                                         {kind: 'video', name: 'clip.mov', blob: kept('c'), w: 0, h: 0, size: 1500000}]);
        """#)
        try await until("both messages stand in the conversation") { self.held.count == 2 }
        XCTAssertEqual(files(0), [Attachment(id: "chat.messages/messages/m1#0", name: "report.pdf", type: "com.adobe.pdf", bytes: 48_213)])
        // Chat says no size of a picture, and the app makes none up.
        XCTAssertEqual(files(1), [
            Attachment(id: "chat.messages/messages/m2#0", name: "photo.png", type: "public.png"),
            Attachment(id: "chat.messages/messages/m2#1", name: "clip.mov", type: "com.apple.quicktime-movie", bytes: 1_500_000),
        ])
        // A message that is files alone, with no words, is its person's own all the same.
        guard case .message(let mine) = held[1].content else { return XCTFail("no message") }
        XCTAssertEqual(mine.author, .person)
        XCTAssertEqual(mine.text, "")
    }

    func testWhatAMessageCarriesThatIsNoFileIsNotShownAsOneAndAFileKeepsItsPlace() async throws {
        try await start()
        try await enter()
        try await inPage(#"""
          post('m3', {agent: 'assistant'}, 'Look at these.', [
            {kind: 'text', text: 'words quoted from a document', from: null},
            {kind: 'resource', at: {app: 'so.tilo.tasks', kind: 'tasks.issue', id: 'i-1', label: 'Plan the launch'}, anchor: null, label: 'Plan the launch'},
            {kind: 'file', name: 'notes.txt', blob: kept('d'), size: 12},
            {kind: 'file', name: '', blob: kept('e'), size: 3},
            {kind: 'file', name: 'kept-nowhere.bin', size: 3},
            {kind: 'file', name: 'odd.zzq', blob: kept('f'), size: -1}]);
        """#)
        try await until("the message stands in the conversation") { self.held.count == 1 }
        XCTAssertEqual(files(0), [
            Attachment(id: "chat.messages/messages/m3#2", name: "notes.txt", type: "public.plain-text", bytes: 12),
            // A name whose ending the system does not know is plain data, and a size below nothing is no size.
            Attachment(id: "chat.messages/messages/m3#5", name: "odd.zzq", type: "public.data"),
        ])
    }

    func testAFileIsReadHoweverTheReadSpellsItsKind() throws {
        let carried: JSON = [
            ["kind": "file", "name": "a.txt", "blob": "aa", "size": 1],
            ["$variant": "image", "name": "b.png", "blob": "bb", "w": 2, "h": 2],
            ["video": ["name": "c.mov", "blob": "cc", "w": 0, "h": 0, "size": 9]],
            // Neither is a file: something a document keeps, and a kind Chat does not have.
            ["resource": ["at": ["app": "so.tilo.tasks", "kind": "tasks.issue", "id": "i-1", "label": "Plan"], "label": "Plan"]],
            ["kind": "parcel", "name": "d.bin", "blob": "dd", "size": 4],
            // A size that is no whole number of bytes is no size.
            ["kind": "file", "name": "e.bin", "blob": "ee", "size": 1.5],
        ]
        // Chat's own name for the field that says which conversation a message is in.
        let conversation = ChatMessage.drawn[0]
        let message = try XCTUnwrap(ChatMessage(id: "chat.messages/messages/m4", fields: [
            conversation: "c_chat", "author": ["person": "ad"], "ts": 1_794_900_000, "text": "", "attachments": carried,
        ]))
        XCTAssertEqual(message.files, [
            ChatFile(kind: .file, index: 0, name: "a.txt", content: "aa", bytes: 1),
            ChatFile(kind: .image, index: 1, name: "b.png", content: "bb"),
            ChatFile(kind: .video, index: 2, name: "c.mov", content: "cc", bytes: 9),
            ChatFile(kind: .file, index: 5, name: "e.bin", content: "ee"),
        ])
    }

    func testPressingAFileSaysInASentenceThatItCannotBeFetchedHereYetAndAsksTiloForNothing() async throws {
        try await start()
        try await enter()
        try await inPage("post('m1', {agent: 'assistant'}, 'The report is ready.', [{kind: 'file', name: 'report.pdf', blob: kept('a'), size: 48213}])")
        try await until("the message stands in the conversation") { self.held.count == 1 }
        let file = try XCTUnwrap(files(0).first)
        let loaded = asked
        do {
            _ = try await source.file(of: file.id)
            XCTFail("a file was handed over that Tilo's page gave the app no way to")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused("A file cannot be fetched into this app yet"))
        }
        // The page was asked for nothing but what it reads, her server for nothing of a file,
        // and nothing was fetched from anywhere.
        let others = try await page("return JSON.stringify(window.heard.filter(one => !one.endsWith(' read')))")
        XCTAssertEqual(others, "[]")
        let ofAFile = try await page("return JSON.stringify(window.asks.filter(one => /content|file|link|put|get/i.test(one)))")
        XCTAssertEqual(ofAFile, "[]")
        XCTAssertEqual(asked, loaded)
    }
}
#endif
