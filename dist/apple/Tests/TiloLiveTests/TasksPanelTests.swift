#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// The Tasks panel on Tilo itself. A task of the person's assistant is one issue of the
/// workspace's tracker that is given to it. The page the app holds shows Chat and keeps nothing
/// of Tasks, so the app asks the workspace's server, as the person on this device. Against a
/// page that speaks as Tilo's does and a server that keeps a tracker. No window is opened,
/// nothing is fetched, nothing kept.
@MainActor
final class TasksPanelTests: XCTestCase {
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
    /// (`TRACKER` below), the issues' chain with its `read` (rows of `item`, `name` and
    /// the fields that were asked for, narrowed by `where`, in pages) and its `create`.
    ///   `window.trackerless`  a server that keeps no tracker: it knows no such ask
    ///   `window.fails`        how many of the next reads her server fails at
    ///   `window.pageMost`     the most rows it answers in one page
    ///   `window.refuses`      the sentence it refuses a new issue with
    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status"></span><div id="semantics"></div>
    <script>
      \#(testDeviceHandoff)
      const root = document.documentElement;
      const ADA = 'ad'.repeat(32);
      const DIRECT = 'direct:[{"person":"' + ADA + '"},{"agent":"assistant"}]';
      const TRACKER = 'record:so.tilo.tasks:';
      const ISSUES = 'chain:derived:issues@derived:default@tab:0000000000000000';
      const hex = text => [...text].map(letter => letter.charCodeAt(0).toString(16).padStart(2, '0')).join('');
      window.touch = () => { document.getElementById('semantics').textContent = String(Math.random()); };
      const said = value => ({state: 'completed', output: {value}});
      const done = value => ({invocation: 'i', state: 'succeeded', output: {value, resources: []}});
      const noSuch = () => { throw Object.assign(new Error('No such action is offered'), {code: 'unknown_action'}); };
      window.creates = [];
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
        get person() { return window.certifiedPerson ?? ADA; },
        get workspace() { return window.certifiedWorkspace ?? 'ws-1'; },
        catalogue: async query => window.initiativeCatalogue?.(query) ?? {actions: []},
        status: async invocation => ({invocation, state: 'succeeded'}),
        ownWorkspace: async () => ({id: 'own-7', name: 'Mine', origin: 'https://tilo.example:8443'}),
        invoke: async (target, argument, options) => {
          const {resource, action} = target.address;
          if (target.provider === 'tilo.initiative' && window.initiative) return window.initiative(resource, action, argument);
          if (target.provider === 'tilo.telling' && action === 'list') {
            window.tellingAsks += 1;
            // An ask that waits is held until her server's log moves.
            const newer = () => window.told.filter(one => one.seq > (argument?.after ?? 0));
            if (!newer().length && argument?.waitMs) await new Promise(moved => { window.logMoved = moved; setTimeout(moved, argument.waitMs); });
            return done({told: newer(), newest: Math.max(0, ...window.told.map(one => one.seq))});
          }
          if (target.provider !== TRACKER || resource !== ISSUES) noSuch();
          window.asks.push(action === 'read' ? 'read ' + JSON.stringify(argument?.where ?? null)
            : action + ' ' + argument?.title + ' for ' + argument?.assignee);
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
            if (window.refuses) return {invocation: 'i', state: 'failed', error: {code: 'invalid_argument', message: window.refuses}};
            const receipt = options?.invocation;
            const applied = window.creates.includes(receipt);
            window.creates.push(receipt);
            if (receipt && applied) return done(null);
            // Tasks keeps who gave it: the person whose device asked.
            window.issues.push(window.issue('i-new-' + window.issues.length, argument.title, 'todo', {assignee: argument.assignee ?? null}));
            if (window.loseReceipt) {
              window.loseReceipt = false;
              return {invocation: receipt, state: 'accepted'};
            }
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

    // MARK: Proposed work

    private func enterWithProposals() async throws -> Person {
        try await start()
        try await enter("""
            const proposed = (id, title, extra) => ({
              id, title, kind: 'follow-up', why: 'The signed contract arrived.',
              state: 'proposed', proposedAtMs: 1794800000000, ...extra
            });
            window.proposals = [
              proposed('file', 'File the contract', {task: {issue: 'i-file', number: 17}}),
              proposed('renew', 'Review renewals', {event: {name: 'Review renewals', when: 'every 7 days',
                onlyIf: 'a contract renewed', instruction: 'Summarize the changes'}}),
              proposed('old', 'Previously decided', {state: 'accepted'})
            ];
            window.writes = [];
            window.offersLook = true;
            window.offersAlways = true;
            window.preferences = [{kind: 'invoice', declined: 2, quiet: true, always: false}];
            const offered = (resource, action, params = []) =>
              ({provider: 'tilo.initiative', action: {address: {resource, action}, params}});
            window.initiativeCatalogue = query => ({rows: query.provider !== 'tilo.initiative' ? [] : [
              offered('proposals', 'accept', window.offersAlways ? [{name: 'always'}] : []),
              offered('proposals', 'ask-first'), offered('proposals', 'propose-again'),
              ...(window.offersLook ? [offered('look-round', 'look-now')] : [])
            ]});
            const list = () => done({proposals: window.proposals, ...(window.preferences.length ? {kinds: window.preferences} : {})});
            window.initiative = (resource, action, argument) => {
              if (resource === 'proposals' && action === 'list') return list();
              window.writes.push([resource, action, argument ?? {}]);
              if (window.decisionDenied) return {state: 'failed', error: {code: 'permission_denied', message: window.decisionDenied}};
              if (resource === 'look-round' && action === 'look-now') {
                return done({on: false, when: 'every 1800 seconds', dayLimitMicros: 200000,
                  spentTodayMicros: 0, day: '2026-11-25', looks: []});
              }
              if (resource !== 'proposals') noSuch();
              if (action === 'ask-first' || action === 'propose-again') {
                const preference = window.preferences.find(row => row.kind === argument?.kind);
                if (!preference) noSuch();
                if (action === 'ask-first') preference.always = false;
                else { preference.declined = 0; preference.quiet = false; }
                window.preferences = window.preferences.filter(row => row.always || row.declined > 0);
                return list();
              }
              if (!['accept', 'decline'].includes(action)) noSuch();
              const proposal = window.proposals.find(row => row.id === argument?.proposal);
              if (!proposal) noSuch();
              if (proposal.state === 'accepted' && action === 'accept') return done(proposal);
              proposal.state = action === 'accept' ? 'accepted' : 'declined';
              if (argument.always) window.preferences.push({kind: proposal.kind, declined: 0, quiet: false, always: true});
              if (window.loseDecision) return {invocation: 'unanswered', state: 'accepted'};
              return done(proposal);
            }
            """)
        try await wayInIsOver()
        guard case .signedIn(let person) = sessions.last else { throw SourceProblem.signedOut }
        return person
    }

    func testWaitingProposalsAreReviewedAndLookNowDoesNotEnableASchedule() async throws {
        let person = try await enterWithProposals()
        let offered = try await source.proposalReview(for: person)
        let review = try XCTUnwrap(offered)
        XCTAssertTrue(review.canLookNow)
        XCTAssertEqual(review.pending.map(\.title), ["File the contract", "Review renewals"])
        let task = try XCTUnwrap(review.pending.first)
        let event = try XCTUnwrap(review.pending.last)
        XCTAssertFalse(task.isEvent)
        XCTAssertTrue(event.isEvent)
        XCTAssertEqual(event.why, "The signed contract arrived.")
        XCTAssertEqual(event.eventWhen, "every 7 days")
        XCTAssertEqual(event.eventInstruction, "Summarize the changes")
        XCTAssertEqual(event.eventCondition, "a contract renewed")
        try await source.review(task, accept: true, for: person)
        try await source.review(event, accept: false, for: person)
        try await source.lookNow(for: person)
        let writes = try await page("return JSON.stringify(window.writes)")
        XCTAssertEqual(JSON(text: writes), [
            ["proposals", "accept", ["proposal": "file"]],
            ["proposals", "decline", ["proposal": "renew"]],
            ["look-round", "look-now", [:]],
        ])
        try await inPage("window.offersLook = false; window.offersAlways = false")
        let after = try await source.proposalReview(for: person)
        XCTAssertEqual(after?.pending, [])
        XCTAssertEqual(after?.canLookNow, false)
        XCTAssertEqual(after?.canAlwaysAllow, false, "an older accept action offers no standing permission")
    }

    func testStandingPermissionCanBeRevokedAndAMutedKindCanBeSuggestedAgain() async throws {
        let person = try await enterWithProposals()
        let before = try await source.proposalReview(for: person)
        XCTAssertEqual(before?.canAlwaysAllow, true)
        let task = try XCTUnwrap(before?.pending.first)
        XCTAssertEqual(task.kind, "follow-up")
        let muted = try XCTUnwrap(before?.preferences.first { $0.kind == "invoice" && $0.quiet })
        try await source.alwaysAllow(task, for: person)
        let afterApproval = try await source.proposalReview(for: person)
        let standing = try XCTUnwrap(afterApproval?.preferences.first { $0.kind == "follow-up" && $0.always })
        // The name of the invoice kind alone grants no authority in a different workspace.
        try await inPage("window.certifiedWorkspace = 'ws-2'")
        do {
            try await source.changePreference(muted, suggestAgain: true, for: person)
            XCTFail("a preference change was sent to a different workspace")
        } catch SourceProblem.refused {
        } catch SourceProblem.signedOut {}
        try await inPage("delete window.certifiedWorkspace")
        try await source.changePreference(standing, suggestAgain: false, for: person)
        try await source.changePreference(muted, suggestAgain: true, for: person)
        let after = try await source.proposalReview(for: person)
        let preferences = try XCTUnwrap(after?.preferences)
        XCTAssertFalse(preferences.contains { $0.kind == "follow-up" && $0.always })
        XCTAssertFalse(preferences.contains { $0.kind == "invoice" && $0.quiet })
        let writes = try await page("return JSON.stringify(window.writes)")
        XCTAssertEqual(JSON(text: writes), [
            ["proposals", "accept", ["proposal": "file", "always": true]],
            ["proposals", "ask-first", ["kind": "follow-up"]],
            ["proposals", "propose-again", ["kind": "invoice"]],
        ])
    }

    func testAnApprovalOnAnotherDeviceDoesNotImplyAStandingPermission() async throws {
        let person = try await enterWithProposals()
        let before = try await source.proposalReview(for: person)
        let task = try XCTUnwrap(before?.pending.first)
        try await inPage("window.proposals[0].state = 'accepted'")
        do {
            try await source.alwaysAllow(task, for: person)
            XCTFail("one earlier decision cannot grant future permission")
        } catch SourceProblem.refused {}
        let after = try await source.proposalReview(for: person)
        let preferences = try XCTUnwrap(after?.preferences)
        XCTAssertFalse(preferences.contains { $0.kind == task.kind && $0.always })
        let writes = try await page("return JSON.stringify(window.writes)")
        XCTAssertEqual(JSON(text: writes), [["proposals", "accept", ["proposal": "file", "always": true]]])
    }

    func testARefusedProposalStaysPendingAndALostReceiptIsReconciledByReading() async throws {
        let person = try await enterWithProposals()
        let offered = try await source.proposalReview(for: person)
        let proposal = try XCTUnwrap(offered?.pending.first)
        try await inPage("window.decisionDenied = 'You cannot start this work'")
        do {
            try await source.review(proposal, accept: true, for: person)
            XCTFail("the host refused the decision")
        } catch let error as SourceProblem {
            XCTAssertEqual(error, .refused("You cannot start this work"))
        }
        let refused = try await source.proposalReview(for: person)
        XCTAssertEqual(refused?.pending.first?.id, proposal.id)
        try await inPage("window.decisionDenied = null; window.loseDecision = true")
        do {
            try await source.review(proposal, accept: false, for: person)
            XCTFail("the host applied the decision but did not confirm it")
        } catch SourceProblem.unreachable {}
        let after = try await source.proposalReview(for: person)
        XCTAssertEqual(after?.pending.map(\.title), ["Review renewals"])
        let writes = try await page("return JSON.stringify(window.writes)")
        XCTAssertEqual(JSON(text: writes), [
            ["proposals", "accept", ["proposal": "file"]],
            ["proposals", "decline", ["proposal": "file"]],
        ])
    }

    func testAProposalFromAnotherPersonOrWorkspaceCannotBeDecided() async throws {
        let person = try await enterWithProposals()
        let offered = try await source.proposalReview(for: person)
        let proposal = try XCTUnwrap(offered?.pending.first)
        for changed in ["window.certifiedPerson = 'be'.repeat(32)", "window.certifiedWorkspace = 'ws-2'"] {
            try await inPage(changed)
            do {
                try await source.review(proposal, accept: true, for: person)
                XCTFail("a decision reached a different account or workspace")
            } catch SourceProblem.signedOut {
                // The app has since paired with a different identity.
            } catch SourceProblem.refused {
                // The peer refused to act on a stale selection.
            }
            try await inPage("delete window.certifiedPerson; delete window.certifiedWorkspace")
        }
        let writes = try await page("return JSON.stringify(window.writes)")
        XCTAssertEqual(JSON(text: writes), [])
        let after = try await source.proposalReview(for: person)
        XCTAssertEqual(after?.pending.first?.id, proposal.id)
    }

    // MARK: The list

    func testTheAssistantsIssuesFillTheTabsByHowEachStands() async throws {
        try await start()
        try await enter("""
            const waits = (kind, what) => ({folder: '', note: '', waits: [{kind, what, on: null, event: null}], until_ms: 1795000000000, stopped: null});
            window.issues = [
              issue('i-maya', 'Reply to Maya at Northwind', 'in-review'),
              issue('i-deck', 'Board deck for Friday: first draft', {$variant: 'in-review'}),
              issue('i-q3', 'Find the Q3 board deck', 'in-progress', {work: waits('answer', 'Two versions exist. Which one?')}),
              // Work that stopped is never ready for review, whatever its status was set to since.
              issue('i-vendors', 'Compare the three vendors', 'in-review', {work: {folder: '', note: '', waits: [], until_ms: null,
                stopped: {why: 'budget', words: 'I used up what you set for this task.', at_ms: 1794900000000}}}),
              issue('i-usage', 'Find out why usage dropped', 'in-progress'),
              issue('i-yc', 'Shortlist the YC batch for the thesis', 'in-progress', {work: waits({$variant: 'reply'}, 'An answer from the fund')}),
              // A piece of a task that is listed stands under it in Tasks: the panel lists the task.
              issue('i-yc-1', 'Read the batch list', 'done', {parent: 'i-yc'}),
              issue('i-atlas', 'Prepare the Atlas call', 'todo'),
              issue('i-folder', 'Tidy the shared folder', 'backlog'),
              issue('i-sent', 'Send the October numbers', 'done'),
              issue('i-offsite', 'Book the offsite', 'cancelled'),
              // Her own issue is no task of her assistant's.
              issue('i-bank', 'Call the bank', 'todo', {assignee: 'ad'.repeat(32)}),
            ]
            """)
        try await until("the tasks are listed") { self.lists.last?.isEmpty == false }
        XCTAssertEqual(shown(.readyForReview), ["Reply to Maya at Northwind", "Board deck for Friday: first draft"])
        XCTAssertEqual(shown(.needsInput), ["Find the Q3 board deck: Two versions exist. Which one?",
                                            "Compare the three vendors: I used up what you set for this task."])
        XCTAssertEqual(shown(.inProgress), ["Find out why usage dropped", "Shortlist the YC batch for the thesis: An answer from the fund"])
        XCTAssertEqual(shown(.upNext), ["Prepare the Atlas call", "Tidy the shared folder"])
        XCTAssertEqual(shown(.resolved), ["Send the October numbers", "Book the offsite"])
        XCTAssertEqual(lists.last?.first?.id, AssistantTask.ID("i-maya"), "a task is named by its issue")
        // One read, as the person, of the issues that are given to her assistant.
        let asked = try await trackerAsks()
        XCTAssertEqual(asked, [#"read {"assignee":"agent:assistant"}"#])
    }

    func testALongListIsReadPageByPageInTheTrackersOrder() async throws {
        try await start()
        try await enter("window.pageMost = 2; window.issues = ['a', 'b', 'c', 'd', 'e'].map(letter => issue('i-' + letter, 'Task ' + letter, 'todo'))")
        try await until("all five are listed") { self.lists.last?.count == 5 }
        XCTAssertEqual(lists.last?.map(\.title), ["Task a", "Task b", "Task c", "Task d", "Task e"])
        let asked = try await trackerAsks()
        XCTAssertEqual(asked.count, 3, "two pages of two and one of one")
    }

    func testTheListFollowsWhatHerServerTellsHerAndWhatAnAnswerLeftBehind() async throws {
        try await start()
        try await enter("window.issues = [issue('i-usage', 'Find out why usage dropped', 'in-progress')]")
        try await until("the task is listed as in work") { self.shown(.inProgress) == ["Find out why usage dropped"] }
        try await wayInIsOver()
        // Her server finished it while every page of hers showed Chat, and told her so.
        try await inPage("window.issues[0].status = 'in-review'; tell('The usage report is ready for you.')")
        try await until("it is ready for review") { self.shown(.readyForReview) == ["Find out why usage dropped"] }
        // An answer in the main chat made a task: the list is read again when the answer has ended.
        try await inPage("""
            post({id: 'm1', author: {agent: 'assistant'}, text: 'I will look into the churn numbers.',
              work: {stream: null, state: {state: {$variant: 'doing', did: '', about: ''}, since: 1794900002}}, stopped: null})
            """)
        try await until("the answer is being written") { self.changes.contains(.answering(true)) }
        try await inPage("""
            window.issues.push(issue('i-churn', 'Look into the churn numbers', 'todo'));
            window.messages[0].work.state.state = {$variant: 'done'};
            touch()
            """)
        try await until("the new task is up next") { self.shown(.upNext) == ["Look into the churn numbers"] }
        // Her server was asked when she got in, when it told her something and when the answer
        // ended: a message that came and grew asked it for nothing.
        let asked = try await trackerAsks()
        XCTAssertEqual(asked.count, 3)
    }

    func testAReadHerServerFailedAtIsMadeAgainByItselfAndWhatWasListedStandsMeanwhile() async throws {
        try await start()
        try await enter("window.fails = 2; window.issues = [issue('i-atlas', 'Prepare the Atlas call', 'todo')]")
        try await until("the task is listed after all") { self.shown(.upNext) == ["Prepare the Atlas call"] }
        var asked = try await trackerAsks()
        XCTAssertEqual(asked.count, 3, "two reads her server failed at, and the one it answered")
        try await wayInIsOver()
        try await inPage("window.fails = 1; window.issues[0].status = 'in-progress'; tell('I began the Atlas call.')")
        try await until("the task is in work") { self.shown(.inProgress) == ["Prepare the Atlas call"] }
        asked = try await trackerAsks()
        XCTAssertEqual(asked.count, 5)
        XCTAssertFalse(lists.dropFirst().contains([]), "what was listed stood while her server failed at a read")
    }

    func testAServerThatKeepsNoTrackerListsNothingAndIsNotAskedWithoutEnd() async throws {
        try await start()
        try await enter("window.trackerless = true")
        try await until("her server was asked for the tasks") { (try? await self.trackerAsks().count) == 1 }
        try await wayInIsOver()
        // It is asked again when something may have changed, and not in between.
        try await inPage("tell('Good morning.')")
        try await until("her server was asked once more") { ((try? await self.trackerAsks().count) ?? 0) >= 2 }
        let asked = try await trackerAsks()
        XCTAssertEqual(asked.count, 2)
        XCTAssertEqual(lists, [[]], "nothing was listed, and nothing was said to be")
    }

    // MARK: The plus

    func testWhatIsTypedUnderThePlusIsGivenToTheAssistantAsAnIssueOfHersAndStandsInTheList() async throws {
        try await start()
        try await enter()
        try await until("her server was asked for the tasks") { (try? await self.trackerAsks().count) == 1 }
        try await source.addTask("  Plan the offsite \n")
        // Tasks' own action, as the person: the issue is given to her assistant, and the list is read again.
        let asked = try await trackerAsks()
        XCTAssertEqual(asked.dropFirst().first, "create Plan the offsite for agent:assistant")
        try await until("the task is up next") { self.shown(.upNext) == ["Plan the offsite"] }
        // Nothing typed is no task, and her server is asked for nothing.
        try await source.addTask("  \n ")
        let after = try await trackerAsks()
        XCTAssertEqual(after.filter { $0.hasPrefix("create") }.count, 1)
    }

    func testRetryingATaskDraftUsesItsOriginalInvocation() async throws {
        try await start()
        try await enter("window.loseReceipt = true")
        try await wayInIsOver()
        let draft = Draft(text: "Plan the offsite")
        do {
            try await source.addTask(draft.text, delivery: draft.delivery)
            XCTFail("the create receipt was lost")
        } catch TiloEngine.Problem.noAnswer {}
        try await source.addTask(draft.text, delivery: draft.delivery)
        let another = Draft(text: draft.text)
        try await source.addTask(another.text, delivery: another.delivery)
        let calls = try await page("return JSON.stringify(window.creates)")
        XCTAssertEqual(JSON(text: calls), .array([draft, draft, another].map { .string($0.delivery.uuidString.lowercased()) }))
        let count = try await page("return String(window.issues.length)")
        XCTAssertEqual(count, "2")
    }

    func testATaskHerServerDoesNotTakeIsRefusedInItsOwnWordsAndNothingIsListed() async throws {
        try await start()
        try await enter("window.refuses = 'A title holds 240 characters at most'")
        try await until("her server was asked for the tasks") { (try? await self.trackerAsks().count) == 1 }
        do {
            try await source.addTask("Plan the offsite")
            XCTFail("a task her server refused was taken")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused("A title holds 240 characters at most"))
        }
        XCTAssertEqual(lists, [[]])
    }

    func testWhereHerServerKeepsNoTrackerThePlusSaysSoInASentence() async throws {
        try await start()
        try await enter("window.trackerless = true")
        try await until("her server was asked for the tasks") { (try? await self.trackerAsks().count) == 1 }
        do {
            try await source.addTask("Plan the offsite")
            XCTFail("a task was taken where nothing keeps one")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused("Tilo keeps no tasks for your assistant in this workspace yet"))
        }
    }
}
#endif
