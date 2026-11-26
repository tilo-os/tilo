#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// The Tasks panel follows what the workspace's tracker keeps. The page tells the app that
/// something Tasks keeps in the workspace changed, by name and with nothing of what changed, and
/// the panel reads the assistant's tasks again then: a task the workspace's server moves on, or
/// one a colleague's device changes, shows without the person doing anything. Against the page
/// the Tasks panel's own tests speak to, whose entry also answers which documents changed. No
/// window is opened, nothing is fetched, nothing kept.
@MainActor
final class TasksFollowTests: XCTestCase {
    private static let site = "https://tilo.test"
    private var source: TiloSource!
    private var suite = ""
    private var sessions: [Session] = []
    private var changes: [ConversationChange] = []
    private var lists: [[AssistantTask]] = []
    private var listening: [Task<Void, Never>] = []

    private func start() async throws {
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { _ in }
        source = TiloSource(engine: engine, mainChat: .agent(id: "assistant", name: "Assistant"), livesInOwn: false, defaults: defaults)
        // An ask her server failed at is made again soon: no test waits a second for it.
        source.firstPause = .milliseconds(40)
        let source = source!
        listening = [
            Task { for await session in source.session() { self.sessions.append(session) } },
            Task { for await change in source.conversation(.main) { self.changes.append(change) } },
            Task { for await list in source.tasks() { self.lists.append(list) } },
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

    /// As Tilo's page that shows Chat alone, with its device's entry handed to the app. Its mount
    /// reads one conversation's messages. Behind the entry the workspace's server keeps the
    /// tracker (`window.issues`, in the tracker's order) and what it told her.
    ///
    /// The server answers Tasks as the workspace's server does: under the tracker's own name
    /// (`tracker` below), the issues' chain with its `read` (rows of `item`, `name` and
    /// the fields that were asked for, narrowed by `where`, in pages) and its `create`.
    ///   `window.trackerless`  a server that keeps no tracker: it knows no such ask
    ///   `window.fails`        how many of the next reads her server fails at
    ///   `window.pageMost`     the most rows it answers in one page
    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status"></span><div id="semantics"></div>
    <script>
      \#(testDeviceHandoff)
      const root = document.documentElement;
      // A write by another device or by the workspace's server, as the workspace's feed tells of it.
      window.feed = []; window.held = false;
      window.write = (app, document) => { window.feed.push({app, document}); window.woke?.(); };
      const ADA = 'ad'.repeat(32);
      const DIRECT = 'direct:[{"person":"' + ADA + '"},{"agent":"assistant"}]';
      // The tracker's own document of the workspace, as the workspace's server names what it offers of it.
      const tracker = provider => typeof provider === 'string' && provider.endsWith(':so.tilo.tasks:');
      const ISSUES = 'chain:derived:issues@derived:default@tab:0000000000000000';
      const hex = text => [...text].map(letter => letter.charCodeAt(0).toString(16).padStart(2, '0')).join('');
      window.touch = () => { document.getElementById('semantics').textContent = String(Math.random()); };
      const said = value => ({state: 'completed', output: {value}});
      const done = value => ({invocation: 'i', state: 'succeeded', output: {value, resources: []}});
      const noSuch = () => { throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'}); };
      window.messages = []; window.told = []; window.asks = []; window.tellingAsks = 0; window.issues = [];
      // An issue as the tracker keeps one: given to the assistant by her, unless `more` says otherwise.
      window.issue = (id, title, status, more = {}) => ({id, title, status, number: 0, assignee: 'agent:assistant',
        assigned_by: {person: ADA}, parent: null, work: null, ...more});
      window.post = message => {
        window.messages.push({ts: 1794900000 + window.messages.length, conversation: DIRECT, question: null, share: null, attachments: [], ...message});
        window.touch();
      };
      window.tell = words => {
        const seq = window.told.length + 1;
        window.told.push({seq, id: 't-' + seq, atMs: 1794900100000 + seq, kind: 'news', title: '', words,
          mark: {important: false, why: {code: 'judged'}}, reaches: true});
        window.logMoved?.();
      };
      const device = Object.freeze({
        person: ADA, workspace: 'ws-1',
        catalogue: async () => ({actions: []}), status: async invocation => ({invocation, state: 'succeeded'}),
        ownWorkspace: async () => ({id: 'own-7', name: 'Mine', origin: 'https://tilo.example:8443'}),
        // The workspace's feed of changes as it tells this person of them (`window.feed`), answered as
        // the page's own entry answers: from now where no cursor is given, else what was asked for
        // and changed since, held until something did.
        changed: async ({of, after, waitMs}) => {
          if (after === null) return {changed: [], cursor: String(window.feed.length)};
          const named = () => window.feed.slice(Number(after))
            .filter(one => of.some(asked => asked.app === one.app && (asked.document == null || asked.document === one.document)));
          if (!named().length && waitMs > 0) {
            window.held = true;
            await new Promise(over => { window.woke = () => { if (named().length) over(); }; setTimeout(over, waitMs); });
            window.held = false;
          }
          return {changed: [...new Map(named().map(one => [one.app + '\n' + one.document, one])).values()], cursor: String(window.feed.length)};
        },
        invoke: async (target, argument) => {
          const {resource, action} = target.address;
          if (target.provider === 'tilo.telling' && action === 'list') {
            window.tellingAsks += 1;
            // An ask that waits is held until her server's log moves.
            const newer = () => window.told.filter(one => one.seq > (argument?.after ?? 0));
            if (!newer().length && argument?.waitMs) await new Promise(moved => { window.logMoved = moved; setTimeout(moved, argument.waitMs); });
            return done({told: newer(), newest: Math.max(0, ...window.told.map(one => one.seq))});
          }
          if (!tracker(target.provider) || resource !== ISSUES) noSuch();
          window.asks.push(action + ' ' + JSON.stringify(action === 'read' ? argument?.where ?? null : argument ?? null));
          if (window.trackerless) noSuch();
          if (action === 'read') {
            if (window.fails > 0) {
              window.fails -= 1;
              return {invocation: 'i', state: 'failed', error: {code: 'write_timeout', message: 'The write did not finish in time'}};
            }
            const wanted = Object.entries(argument?.where ?? {});
            const all = window.issues.filter(issue => wanted.every(([field, value]) => issue[field] === value));
            const from = Number(argument?.cursor ?? 0);
            const most = Math.min(argument?.pageSize ?? 50, window.pageMost ?? 200);
            const rows = all.slice(from, from + most).map(issue => ({item: 'tasks.issues/issues/' + issue.id, name: issue.title,
              ...Object.fromEntries((argument?.fields ?? []).map(field => [field, issue[field] ?? null]))}));
            return done({kind: 'tasks.issues', issues: rows, ...(from + most < all.length ? {nextCursor: String(from + most)} : {})});
          }
          if (action === 'create') {
            // Tasks keeps who gave it: the person whose device asked.
            window.issues.push(window.issue('i-new-' + window.issues.length, argument.title, 'todo', {assignee: argument.assignee ?? null}));
            return done(null);
          }
          noSuch();
        },
      });
      window.enter = async () => {
        // Tilo's page hands the entry of its device to the app's own script, which stands in the
        // document to take it (Tilo's page calls it by its name).
        await giveDevice(device);
        root.dataset.arrival = 'done';
        root.dataset.entry = 'open';
        globalThis.tiloMount = {
          semantics: document.getElementById('semantics'),
          state: () => ({workspaceName: 'Ada\'s workspace'}),
          actions: () => [
            {address: {resource: 'chain:messages', action: 'read'}, resource_kind: 'chat.messages',
              params: [{name: 'cursor'}, {name: 'pageSize'}, {name: 'fields'}, {name: 'where'}]},
            {address: {resource: 'chat.workspace/people/' + hex(ADA), action: 'read'}, resource_kind: 'chat.workspace.people'}],
          invokeAction: async (resource, action, args) => {
            if (resource === 'chain:messages' && action === 'read') {
              // The read names the one conversation it wants, under Chat's own name for that field.
              const [named, wanted] = Object.entries(args.where ?? {})[0] ?? [];
              const listed = [...window.messages].reverse().filter(m => m.conversation === wanted).slice(0, args.pageSize ?? 50);
              return said({kind: 'chat.messages', messages: listed.map(m => ({item: 'chat.messages/messages/' + m.id, name: m.text,
                ...Object.fromEntries((args.fields ?? []).map(field => [field, field === named ? m.conversation : m[field] ?? null]))}))});
            }
            if (resource === 'chat.workspace/people/' + hex(ADA) && action === 'read') return said({id: {person: ADA}, name: 'Ada Lovelace'});
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

    private func page(_ script: String) async throws -> String {
        try await source.engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page) as? String ?? ""
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

    /// The page gets in, and the app is signed in as its person.
    private func enter(_ before: String = "") async throws {
        try await inPage("\(before); location.hash = '#/w/ws-1/so.tilo.chat'; enter()")
        try await until("the app is signed in") { if case .signedIn = self.sessions.last { true } else { false } }
    }

    /// The way in is over once her server holds the ask that follows what it tells her: it is
    /// the last thing the way in begins.
    private func wayInIsOver() async throws {
        try await until("the way in is over") { (try? await self.page("return String(window.tellingAsks)")).flatMap { Int($0) } ?? 0 >= 2 }
    }

    /// What her server was asked of the tracker: each ask's action, and what narrowed a read.
    private func trackerAsks() async throws -> [String] {
        let text = try await page("return JSON.stringify(window.asks)")
        return (try? JSONDecoder().decode([String].self, from: Data(text.utf8))) ?? ["not read: \(text)"]
    }

    /// What one tab or group of the panel lists now: each task's name, and the line under it.
    private func shown(_ state: AssistantTask.State) -> [String] {
        (lists.last ?? []).filter { $0.state == state }.map { task in task.note.map { "\(task.title): \($0)" } ?? task.title }
    }

    // MARK: Following

    /// A change made by another device or by the workspace's server shows in the panel within a
    /// few seconds, at none of the moments the panel reads at by itself: the device does not get
    /// in again, the app calls nothing, her server tells her nothing, no answer ends.
    func testATaskChangedElsewhereShowsInThePanelWithinSecondsAndAtNoOtherMoment() async throws {
        try await start()
        try await enter("window.issues = [issue('i-atlas', 'Prepare the Atlas call', 'todo')]")
        try await until("the task is listed") { self.shown(.upNext) == ["Prepare the Atlas call"] }
        try await wayInIsOver()
        try await until("the page holds the app's ask for what changed") { (try? await self.page("return String(window.held)")) == "true" }
        let asksBefore = try await trackerAsks()
        let waysIn = sessions.count
        // The workspace's server moves the task on and writes it: a part of the tracker's issues
        // changes, and the workspace's feed tells of that part.
        let began = ContinuousClock.now
        try await inPage("""
            window.issues[0].status = 'in-review';
            write('so.tilo.tasks', 'derived:part.0@derived:issues@derived:default@tab:0000000000000000')
            """)
        try await until("the panel shows the task where it stands now", within: .seconds(5)) { self.shown(.readyForReview) == ["Prepare the Atlas call"] }
        XCTAssertLessThan(ContinuousClock.now - began, .seconds(5))
        XCTAssertEqual(shown(.upNext), [])
        // One more read of the tracker, made at the page's word, and none of the four moments.
        let asksAfter = try await trackerAsks()
        XCTAssertEqual(asksAfter.count, asksBefore.count + 1, "\(asksAfter)")
        let quiet = try await page("return [window.told.length, window.messages.length].join(' ')")
        XCTAssertEqual(quiet, "0 0", "her server told her nothing and nothing was said in the conversation")
        XCTAssertEqual(sessions.count, waysIn, "the device did not get in again")
        // A change of something the panel does not show reads nothing again.
        try await until("the page holds the next ask") { (try? await self.page("return String(window.held)")) == "true" }
        try await inPage("write('so.tilo.chat', 'derived:part.0@derived:messages@derived:default@tab:0000000000000000')")
        try await Task.sleep(for: .milliseconds(500))
        let asksLater = try await trackerAsks()
        XCTAssertEqual(asksLater.count, asksAfter.count)
    }
}
#endif
