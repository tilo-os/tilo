#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// Sending a file on Tilo itself. Tilo's page hands the app a way to put a file into its
/// person's workspace a piece at a time; the app reads the picked file as it goes and then
/// sends one message that carries the words and names each file. Against a page that speaks
/// as Tilo's does and whose device takes a put as Tilo's page does. No window is opened; the
/// files are made here, in a folder of this test's own, and removed with it.
@MainActor
final class FileSendingTests: XCTestCase {
    private static let site = "https://tilo.test"
    private var source: TiloSource!
    private var suite = ""
    private var folder: URL!
    private var sessions: [Session] = []
    private var changes: [ConversationChange] = []
    private var listening: [Task<Void, Never>] = []

    private func start() async throws {
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        folder = FileManager.default.temporaryDirectory.appendingPathComponent("tilo-file-sending-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let defaults = UserDefaults(suiteName: suite)!
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
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
        if let folder { try? FileManager.default.removeItem(at: folder) }
        source = nil
    }

    /// As Tilo's page that shows Chat alone, with its device's entry handed to the app. The
    /// entry takes a file as Tilo's page does: `put.begin(size)`, `put.append(upload, offset,
    /// piece)` with the piece as base64 text, `put.finish(upload)`. `steps` is every step it
    /// was asked, in order, a piece by its length alone. What it keeps a file under is a sum
    /// over the file's bytes in their order, so a piece that is missing, doubled or out of its
    /// place gives another name. `most` is the largest file it takes, `failAt` the piece it
    /// does not take, and with `offersPut` false the entry hands no put, as an older page's.
    /// The mount reads one conversation and takes Chat's `send`: `heard` is what it was asked.
    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status"></span><div id="semantics"></div>
    <script>
      \#(testDeviceHandoff)
      const root = document.documentElement;
      const ADA = 'ad'.repeat(32);
      window.messages = [];
      window.heard = [];
      window.steps = [];
      window.asks = [];
      window.most = 32 * 1024 * 1024;
      window.failAt = -1;
      window.offersPut = true;
      window.keepsAnswers = false;
      const said = value => ({state: 'completed', output: {value}});
      window.post = (id, author, text, carried = [], answers = null, quoted = null) => {
        window.messages.push({id, author, text, ts: 1794900000 + window.messages.length, question: null, share: null, attachments: carried,
          answers, quoted});
        document.getElementById('semantics').textContent = String(Math.random());
      };
      const uploads = new Map();
      const sum = (from, bytes) => {
        let now = from;
        for (let at = 0; at < bytes.length; at++) now = Math.imul(now ^ bytes.charCodeAt(at), 0x01000193) >>> 0;
        return now;
      };
      const put = Object.freeze({
        begin: async size => {
          window.steps.push(['begin', size]);
          if (!Number.isSafeInteger(size) || size < 0) throw new TypeError('size is a whole number, 0 or more');
          if (size > window.most) throw Object.assign(new Error(`one file is at most ${window.most} bytes here, and this one is ${size}`), {code: 'invalid_request'});
          const upload = uploads.size + 1;
          uploads.set(upload, {size, at: 0, sum: 0x811c9dc5, pieces: 0});
          return {upload};
        },
        append: async (upload, offset, piece) => {
          const bytes = atob(piece);
          window.steps.push(['append', upload, offset, bytes.length]);
          const held = uploads.get(upload);
          if (!held) throw new Error('unknown_item: no such upload');
          if (held.pieces === window.failAt) throw new Error('unavailable: The server did not take this piece');
          if (offset !== held.at) throw new Error('invalid_request: A piece begins where the piece before it ended');
          if (bytes.length > 393216) throw new Error('invalid_request: A piece carries at most 393216 bytes');
          held.sum = sum(held.sum, bytes);
          held.at += bytes.length;
          held.pieces += 1;
          return {written: bytes.length};
        },
        finish: async upload => {
          window.steps.push(['finish', upload]);
          const held = uploads.get(upload);
          if (!held || held.at !== held.size) throw new Error('invalid_request: The file is not whole');
          return {content: 'tilo.content:' + held.sum.toString(16).padStart(8, '0').repeat(8)};
        },
      });
      const noSuch = () => { throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'}); };
      window.enter = async () => {
        const device = Object.freeze({
          person: ADA, workspace: 'ws-1',
          catalogue: async () => ({actions: []}), status: async invocation => ({invocation, state: 'succeeded'}),
          ownWorkspace: async () => noSuch(),
          invoke: async target => { window.asks.push(target.provider + ' ' + target.address.action); return noSuch(); },
          ...(window.offersPut ? {put} : {}),
        });
        // Tilo's page hands the entry of its device to the app's own script, which stands in the
        // document to take it.
        await giveDevice(device);
        root.dataset.arrival = 'done';
        root.dataset.entry = 'open';
        globalThis.tiloMount = {
          semantics: document.getElementById('semantics'),
          state: () => ({workspaceName: 'Ada\'s workspace'}),
          actions: () => [{address: {resource: 'chain:messages', action: 'read'}, resource_kind: 'chat.messages',
            params: [{name: 'cursor'}, {name: 'pageSize'}, {name: 'fields'}, {name: 'where'}]},
            // A Chat that keeps which message a message answers offers a post that takes it.
            ...(window.keepsAnswers ? [{address: {resource: 'chain:messages', action: 'post'}, resource_kind: 'chat.messages',
              params: [{name: 'text'}, {name: 'answers'}, {name: 'quoted'}, {name: 'attachments'}]}] : [])],
          invokeAction: async (resource, action, args) => {
            window.heard.push({resource, action, args});
            if (resource === 'chain:messages' && action === 'read') {
              // The read names the one conversation it wants, under Chat's own name for that field.
              const [named, wanted] = Object.entries(args.where ?? {})[0] ?? [];
              // The name the app's own read gives the field that says a message's conversation.
              if (named) window.named = named;
              // A Chat that keeps a field on no message refuses a read that names it.
              if (!window.keepsAnswers && (args.fields ?? []).includes('answers')) throw new Error('unknown_field: a message has no field answers');
              return said({kind: 'chat.messages', messages: [...window.messages].reverse().slice(0, args.pageSize ?? 50)
                .map(m => ({item: 'chat.messages/messages/' + m.id, name: m.text,
                  ...Object.fromEntries((args.fields ?? []).map(field => [field, field === named ? wanted : m[field] ?? null]))}))});
            }
            if (resource === 'chain:messages' && action === 'post' && window.keepsAnswers) {
              const id = 's' + window.messages.length;
              window.post(id, {person: ADA}, args.text, args.attachments ?? [], args.answers ?? null, args.quoted ?? null);
              return said({accepted: true, message: id});
            }
            if (resource === 'chat.workspace' && action === 'send') {
              const id = 's' + window.messages.length;
              window.post(id, {person: ADA}, args.text, args.attached);
              return said({accepted: true, message: id, conversation: args.to[0].id});
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

    private func page(_ script: String, _ arguments: [String: Any] = [:]) async throws -> String {
        try await source.engine.view.callAsyncJavaScript(script, arguments: arguments, in: nil, contentWorld: .page) as? String ?? ""
    }

    private func until(_ what: String, _ holds: @MainActor () async -> Bool) async throws {
        for _ in 0..<240 {
            if await holds() { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        XCTFail("waited twelve seconds for \(what)")
        throw CancellationError()
    }

    private func enter(_ before: String = "") async throws {
        try await inPage("\(before); location.hash = '#/w/ws-1/so.tilo.chat'; enter()")
        try await until("the app is signed in") { if case .signedIn = self.sessions.last { true } else { false } }
        try await until("the conversation is told") { !self.changes.isEmpty }
    }

    private var held: [Item] {
        var conversation = Conversation()
        for change in changes { conversation.apply(change) }
        return conversation.items
    }

    /// A file of this test's own making, of `count` bytes that repeat nowhere soon.
    private func file(_ name: String, bytes count: Int) throws -> (picked: Draft.File, bytes: Data) {
        var bytes = Data(count: count)
        var state: UInt32 = 0x2545_f491
        for at in 0..<count {
            state = state &* 1_664_525 &+ 1_013_904_223
            bytes[at] = UInt8(truncatingIfNeeded: state >> 24)
        }
        let url = folder.appendingPathComponent(name)
        try bytes.write(to: url)
        return (Draft.File(url: url), bytes)
    }

    /// What the page's device keeps these bytes under: its sum over them, in their order.
    private func kept(_ bytes: Data) -> String {
        var sum: UInt32 = 0x811c_9dc5
        for byte in bytes { sum = (sum ^ UInt32(byte)) &* 0x0100_0193 }
        return String(repeating: String(format: "%08x", sum), count: 8)
    }

    private func steps() async throws -> JSON? { JSON(text: try await page("return JSON.stringify(window.steps)")) }

    /// What Chat's send was asked: the resource, the words and what the message carries.
    private func sent() async throws -> JSON? {
        JSON(text: try await page("return JSON.stringify(window.heard.filter(call => call.action === 'send').map(call => [call.resource, call.args.text, call.args.attached]))"))
    }

    private func refusal(of draft: Draft) async -> SourceProblem? {
        do {
            try await source.send(draft, to: .main)
            XCTFail("a draft was taken for sent that could not be")
            return nil
        } catch let problem as SourceProblem {
            return problem
        } catch {
            XCTFail("not a sentence for a person: \(error)")
            return nil
        }
    }

    func testAPickedFileGoesInAPieceAtATimeAndOneMessageCarriesItWithTheWords() async throws {
        try await start()
        try await enter()
        let plan = try file("plan.pdf", bytes: 2 * 393_216 + 1_000)
        let note = try file("note.txt", bytes: 12)
        try await source.send(Draft(text: "Here is the plan", files: [plan.picked, note.picked]), to: .main)
        // The size is said before any piece. Every piece is 393216 bytes at most and begins
        // where the piece before it ended. The second file follows the first.
        let asked = try await steps()
        XCTAssertEqual(asked, [
            ["begin", 787_432], ["append", 1, 0, 393_216], ["append", 1, 393_216, 393_216], ["append", 1, 786_432, 1_000], ["finish", 1],
            ["begin", 12], ["append", 2, 0, 12], ["finish", 2],
        ])
        // ONE message: the words, and each file by its name, what her server keeps it under
        // (the pieces together are the file, byte for byte and in order) and its size.
        let message = try await sent()
        XCTAssertEqual(message, [["chat.workspace", "Here is the plan", [
            ["kind": "file", "name": "plan.pdf", "blob": .string(kept(plan.bytes)), "size": 787_432],
            ["kind": "file", "name": "note.txt", "blob": .string(kept(note.bytes)), "size": 12],
        ]]])
        // It stands in the conversation as the person's own, with a tag for each file.
        try await until("the message stands in the conversation") { self.held.count == 1 }
        guard case .message(let mine) = held[0].content else { return XCTFail("no message") }
        XCTAssertEqual(mine.author, .person)
        XCTAssertEqual(mine.text, "Here is the plan")
        XCTAssertEqual(mine.attachments.map(\.name), ["plan.pdf", "note.txt"])
        XCTAssertEqual(mine.attachments.map(\.bytes), [787_432, 12])
        // Beside the put her server was asked for nothing of a file (what the app asks of it as
        // it enters is none of this), and the app keeps nothing of a file beside it.
        let others = try await page("return JSON.stringify(window.asks.filter(one => /content|file|put|link/i.test(one)))")
        XCTAssertEqual(others, "[]")
        let beside = String(describing: UserDefaults.standard.persistentDomain(forName: suite) ?? [:])
        XCTAssertFalse(beside.contains(kept(plan.bytes)) || beside.contains("plan.pdf"))
    }

    func testAFileTooLargeIsRefusedByItsSizeBeforeAnyOfItCrossesAndNothingIsSent() async throws {
        try await start()
        try await enter("window.most = 1000")
        let large = try file("large.bin", bytes: 1_500)
        let problem = await refusal(of: Draft(text: "Too much", files: [large.picked]))
        // How large it is and how large one may be, each as the system writes a file's size.
        let written = { (bytes: Int) in Int64(bytes).formatted(.byteCount(style: .file)) }
        XCTAssertEqual(problem, .refused("large.bin is \(written(1_500)), and a file sent from here can be \(written(1_000)) at most"))
        let asked = try await steps()
        XCTAssertEqual(asked, [["begin", 1_500]])
        let message = try await sent()
        XCTAssertEqual(message, [])
        XCTAssertEqual(held.count, 0)
    }

    func testAPieceThatDoesNotGoInSendsNothing() async throws {
        try await start()
        try await enter("window.failAt = 1")
        let plan = try file("plan.pdf", bytes: 393_216 + 500)
        let problem = await refusal(of: Draft(text: "Here is the plan", files: [plan.picked]))
        XCTAssertEqual(problem, .refused("The server did not take this piece"))
        // The file was not ended, and no message says that it is there.
        let asked = try await steps()
        XCTAssertEqual(asked, [["begin", 393_716], ["append", 1, 0, 393_216], ["append", 1, 393_216, 500]])
        let message = try await sent()
        XCTAssertEqual(message, [])
        XCTAssertEqual(held.count, 0)
    }

    func testAPageThatHandsNoWayToPutAFileSaysSoAndNothingOfTheFileIsTouched() async throws {
        try await start()
        try await enter("window.offersPut = false")
        // No such file is there: were anything of it asked for, the sentence would be another.
        let gone = Draft.File(url: folder.appendingPathComponent("gone.pdf"))
        let problem = await refusal(of: Draft(text: "With a file", files: [gone]))
        XCTAssertEqual(problem, .refused("A file cannot be sent from this app yet"))
        let asked = try await steps()
        XCTAssertEqual(asked, [])
        let others = try await page("return JSON.stringify(window.asks.filter(one => /content|file|put|link/i.test(one)))")
        XCTAssertEqual(others, "[]")
        let message = try await sent()
        XCTAssertEqual(message, [])
    }

    func testAFileThatIsEmptyOrCannotBeReadIsNotSentAndSaysWhy() async throws {
        try await start()
        try await enter()
        let empty = try file("empty.txt", bytes: 0)
        let whole = try file("whole.txt", bytes: 20)
        let first = await refusal(of: Draft(text: "", files: [empty.picked]))
        XCTAssertEqual(first, .refused("empty.txt is empty, and an empty file cannot be sent"))
        let second = await refusal(of: Draft(text: "", files: [Draft.File(url: folder.appendingPathComponent("gone.pdf"))]))
        XCTAssertEqual(second, .refused("gone.pdf cannot be read on this device"))
        // A folder is no file.
        let third = await refusal(of: Draft(text: "", files: [Draft.File(url: folder)]))
        XCTAssertEqual(third, .refused("\(folder.lastPathComponent) cannot be read on this device"))
        // None of them was begun. A file after one that cannot be sent is not sent alone.
        let fourth = await refusal(of: Draft(text: "Two", files: [empty.picked, whole.picked]))
        XCTAssertEqual(fourth, .refused("empty.txt is empty, and an empty file cannot be sent"))
        let asked = try await steps()
        XCTAssertEqual(asked, [])
        let message = try await sent()
        XCTAssertEqual(message, [])
    }

    func testAStepOfAPutIsTakenOnlyWithTheEnginesOwnPassAndOnlyAStepThereIs() async throws {
        try await start()
        try await enter()
        // Whatever else stands in the page reaches no put through the app's script.
        let withoutPass = try await page("return globalThis.__tiloLive.put('0'.repeat(64), 'begin', {size: 5})")
        XCTAssertEqual(JSON(text: withoutPass)?["error"]?["code"]?.string, "unknown_action")
        let noStep = try await page("return globalThis.__tiloLive.put(given, 'erase', {upload: 1})", ["given": source.engine.pass])
        XCTAssertEqual(JSON(text: noStep)?["error"]?["code"]?.string, "unknown_action")
        let asked = try await steps()
        XCTAssertEqual(asked, [])
        // With the pass a step is the page's own answer, and nothing of the piece comes back.
        let begun = try await page("return globalThis.__tiloLive.put(given, 'begin', {size: 3})", ["given": source.engine.pass])
        XCTAssertEqual(JSON(text: begun), ["ok": ["upload": 1]])
        let appended = try await page("return globalThis.__tiloLive.put(given, 'append', {upload: 1, offset: 0, piece: 'YWJj'})", ["given": source.engine.pass])
        XCTAssertEqual(JSON(text: appended), ["ok": ["written": 3]])
    }
}

/// Answering one message, on Tilo itself: against the same page, which with `keepsAnswers` is a
/// Chat that keeps which message a message answers (its post takes `answers`, a read gives it).
extension FileSendingTests {
    /// What Chat was asked by `action`: the resource, the conversation, the words, the answered message.
    private func asked(_ action: String) async throws -> JSON? {
        JSON(text: try await page("return JSON.stringify(window.heard.filter(call => call.action === '\(action)').map(call => [call.resource, call.args[window.named] ?? '', call.args.text, call.args.answers ?? '']))"))
    }

    private func message(saying words: String) -> Message? {
        for item in held {
            if case .message(let message) = item.content, message.text == words { return message }
        }
        return nil
    }

    func testAReplyIsPostedWithTheMessageItAnswersWhereChatKeepsThat() async throws {
        try await start()
        try await enter("window.keepsAnswers = true; window.post('m1', {person: 'b0'.repeat(32)}, 'Nine mails sorted since seven.')")
        try await until("the answered message is held") { self.message(saying: "Nine mails sorted since seven.") != nil }
        let answered = Answered(Item.ID("chat.messages/messages/m1"), words: "Nine mails sorted since seven.")
        try await source.send(Draft(text: "Which nine?", answers: answered), to: .main)
        // One post, into the conversation the answered message stands in, by the name Chat's actions take for it.
        let posted = try await asked("post")
        XCTAssertEqual(posted, [["chain:messages", "c_chat", "Which nine?", "m1"]])
        let sent = try await asked("send")
        XCTAssertEqual(sent, [])
        // What Chat keeps comes back as what the message answers, with the answered message's words.
        try await until("the reply is told") { self.message(saying: "Which nine?") != nil }
        XCTAssertEqual(message(saying: "Which nine?")?.answers, answered)
        XCTAssertNil(message(saying: "Nine mails sorted since seven.")?.answers)
    }

    func testAReplyToAChatThatKeepsNoAnswersCarriesTheAnsweredLineAsAQuoteAboveTheWords() async throws {
        try await start()
        try await enter("window.post('m1', {person: 'b0'.repeat(32)}, 'Nine mails sorted since seven.')")
        try await until("the answered message is held") { self.message(saying: "Nine mails sorted since seven.") != nil }
        // A message answered whole: one line of it. A marked part: the part whole.
        let whole = Answered(Item.ID("chat.messages/messages/m1"), words: "Nine mails sorted since seven.\n\nNone of them needs you.")
        try await source.send(Draft(text: "Which nine?", answers: whole), to: .main)
        let part = Answered(Item.ID("chat.messages/messages/m1"), words: "sorted since seven", isPart: true)
        try await source.send(Draft(text: "Since when exactly?", answers: part), to: .main)
        let posted = try await asked("post")
        XCTAssertEqual(posted, [])
        let sent = try await asked("send")
        XCTAssertEqual(sent, [["chat.workspace", "", "> Nine mails sorted since seven.\n\nWhich nine?", ""],
                              ["chat.workspace", "", "> sorted since seven\n\nSince when exactly?", ""]])
        // Words that answer no message go as they always did.
        try await source.send(Draft(text: "Thank you"), to: .main)
        let all = try await asked("send")
        XCTAssertEqual(all?.array?.last, ["chat.workspace", "", "Thank you", ""])
    }

    func testAQuoteIsPostedWithItsWordsAndWhereTheyStandInTheAnsweredMessage() async throws {
        try await start()
        try await enter("window.keepsAnswers = true; window.post('m1', {person: 'b0'.repeat(32)}, 'Nine mails sorted since seven.')")
        try await until("the answered message is held") { self.message(saying: "Nine mails sorted since seven.") != nil }
        let quoted = Answered(Item.ID("chat.messages/messages/m1"), words: "sorted since seven", isPart: true, span: 11..<29)
        try await source.send(Draft(text: "Since when exactly?", answers: quoted), to: .main)
        let posted = try await asked("post")
        XCTAssertEqual(posted, [["chain:messages", "c_chat", "Since when exactly?", "m1"]])
        let spans = JSON(text: try await page("return JSON.stringify(window.heard.filter(call => call.action === 'post').map(call => call.args.quoted ?? {}))"))
        XCTAssertEqual(spans, [["text": "sorted since seven", "start": 11, "end": 29]])
        // Chat's quote comes back as the marked words and where they stand.
        try await until("the reply is told") { self.message(saying: "Since when exactly?") != nil }
        XCTAssertEqual(message(saying: "Since when exactly?")?.answers, quoted)
    }
}
#endif
