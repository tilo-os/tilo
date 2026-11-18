#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// A device that was paired before, and whose site was set up again since: the workspace it was
/// let into is no longer there. Against a page that speaks as Tilo's does when it says so
/// (`web/failure-screen.mjs`, `web/peer.mjs`, `web/session-refusal.mjs`): what the app keeps,
/// where it stands, and how it comes back. No window is opened, nothing is fetched, nothing kept.
@MainActor
final class WorkspaceGoneTests: XCTestCase {
    private static let site = "https://tilo.test"
    private static let ada = String(repeating: "ad", count: 32)
    private static let enter = "location.hash = '#/w/ws-1/so.tilo.chat'; enter(false)"
    private static let stopped = "document.documentElement.dataset.arrival = 'stopped'; document.documentElement.dataset.entry = 'refused'"
    /// Tilo's page at the first start after its site was set up again: its failure screen's
    /// case on its root element, its two sentences in its status line.
    private static let gone = """
        document.getElementById('status').textContent = 'Lounge no longer exists. It was reset since this browser last opened it.';
        document.documentElement.dataset.failure = 'gone'; \(stopped)
        """
    /// Tilo's page at every later start: what the device kept was certified by another server.
    private static let changed = """
        document.getElementById('status').textContent = 'Couldn’t open Lounge. This workspace was reset since this browser last opened it. Reload the page to join it again.';
        document.documentElement.dataset.failure = 'refused'; \(stopped)
        """
    /// Its owner deleted the workspace.
    private static let deleted = """
        document.getElementById('status').textContent = 'Lounge was deleted. Its owner deleted it, so it can’t be opened anymore.';
        document.documentElement.dataset.failure = 'deleted'; \(stopped)
        """
    /// A refusal that says nothing of a reset: the person was taken out of the workspace.
    private static let removed = """
        document.getElementById('status').textContent = 'You were removed from Lounge. To come back, ask someone in it for a new invitation.';
        document.documentElement.dataset.failure = 'removed'; \(stopped)
        """
    private var source: TiloSource!
    private var engine: TiloEngine!
    private var defaults: UserDefaults!
    private var suite = ""
    private var sessions: [Session] = []
    private var reaches: [Reach] = []
    private var conversation = Conversation()
    private var loaded: [URL] = []
    private var listening: [Task<Void, Never>] = []
    private var sendEnded = false
    /// Whom the main chat is with. A test that posts into it takes the person's conversation with their assistant.
    private var mainChat = TiloSource.MainChat.conversation(id: "c_chat", name: "chat")

    /// The app on a device that is its person's already: it keeps who they are and where it
    /// was let in, and its page is on its way in from the first moment.
    private func startAgain() async throws {
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)!
        defaults.set("/w/ws-1", forKey: "TiloLiveRoute")
        defaults.set([Self.ada, "Ada Lovelace", "Ada's workspace"], forKey: "TiloLivePerson")
        try await start()
        try await showPage()
    }

    /// The app started on what `defaults` keeps. Its engine fetches nothing: what it asks for is written down.
    private func start() async throws {
        if defaults == nil {
            suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
            defaults = UserDefaults(suiteName: suite)!
        }
        for task in listening { task.cancel() }
        sessions = []
        reaches = []
        loaded = []
        conversation = Conversation()
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { [weak self] in self?.loaded.append($0) }
        engine.stopPause = .milliseconds(100)
        engine.stopPatience = .milliseconds(100)
        self.engine = engine
        source = TiloSource(engine: engine, mainChat: mainChat, livesInOwn: false, defaults: defaults)
        let source = source!
        listening = [
            Task { for await session in source.session() { self.sessions.append(session) } },
            Task { for await reach in source.reach() { self.reaches.append(reach) } },
            Task { for await change in source.conversation(.main) { self.conversation.apply(change) } },
        ]
        try await until("the source says where it stands") { !self.sessions.isEmpty }
    }

    /// The page the engine asked for stands in its view, as far as a page that fetches nothing can.
    private func showPage() async throws {
        engine.view.loadHTMLString(TiloSourceTests.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page is there") {
            (try? await self.engine.view.callAsyncJavaScript("return typeof window.enter", arguments: [:], in: nil, contentWorld: .page)) as? String == "function"
        }
    }

    override func tearDown() async throws {
        for task in listening { task.cancel() }
        UserDefaults.standard.removePersistentDomain(forName: suite)
        source = nil
        engine = nil
        defaults = nil
    }

    private func inPage(_ script: String) async throws {
        _ = try await engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page)
    }

    private func page(_ script: String) async throws -> String {
        try await engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page) as? String ?? ""
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

    private var isSignedIn: Bool {
        if case .signedIn = sessions.last { true } else { false }
    }

    /// What every case of a workspace that is gone comes to: the device is nobody's here any
    /// more, nothing of the old workspace is kept for a next start, its page is put away and
    /// is not loaded again, and nothing is said to be unreachable where nothing is tried.
    private func standsAtItsWayIn(after said: String, _ what: String) async throws {
        try await startAgain()
        XCTAssertTrue(isSignedIn, "a device that was paired before starts as its person's")
        try await inPage(said)
        try await until("the app to stand at its way in after \(what)") { !self.isSignedIn }
        XCTAssertNil(defaults.string(forKey: "TiloLiveRoute"), "the workspace that is gone is still kept")
        XCTAssertNil(defaults.array(forKey: "TiloLivePerson"), "who the device was there is still kept")
        XCTAssertEqual(engine.state, .away, "the page of the workspace that is gone still stands")
        let asked = loaded.count
        try await Task.sleep(for: .milliseconds(600))
        XCTAssertEqual(loaded.count, asked, "the workspace that is gone was asked for again")
        XCTAssertFalse(isSignedIn)
        XCTAssertEqual(reaches.last, .unknown, "at a way in nothing is tried, so nothing is said of reach")
        XCTAssertEqual(sessions.last, .reset, "the way in does not say why the device stands there")
    }

    func testADeviceWhoseSiteWasSetUpAgainStandsAtItsWayInAtItsFirstStartAfter() async throws {
        try await standsAtItsWayIn(after: Self.gone, "the page said its workspace no longer exists")
    }

    /// A build of before stood on this at every later start, with a person signed in and nothing to press.
    func testADeviceThatKeptAnotherServersCertificateStandsAtItsWayInToo() async throws {
        try await standsAtItsWayIn(after: Self.changed, "the page said its workspace was reset")
    }

    func testADeviceWhoseWorkspaceWasDeletedStandsAtItsWayIn() async throws {
        try await standsAtItsWayIn(after: Self.deleted, "the page said its workspace was deleted")
    }

    /// The site is set up again while the app is open: its server turns the open page's session
    /// away, and the page says so on its root while it still shows what it showed. The app asks
    /// for its page again, and what that page says is where the device stands.
    func testAnOpenPageItsServerTurnsAwayIsAskedForAgainAndTheWorkspaceItThenFindsGonePutsTheAppAtItsWayIn() async throws {
        mainChat = .agent(id: "assistant", name: "Assistant")
        try await startAgain()
        // As a page that names a member by its kind: the person's conversation with their assistant holds one message.
        try await inPage("window.byKind = true; direct({id: 'm1', author: {person: ADA}, text: 'Good morning'}); \(Self.enter)")
        try await until("the device is in") { if case .open = self.engine.state { true } else { false } }
        try await until("the conversation is held") { self.conversation.items.count == 1 }
        let asked = loaded.count
        try await inPage("document.documentElement.dataset.refusal = 'workspace-deleted'")
        try await until("the page to be asked for again") { self.loaded.count > asked }
        XCTAssertEqual(loaded.last?.absoluteString, "\(Self.site)/app/#/w/ws-1/so.tilo.chat")
        // The page that is loaded again finds the workspace gone.
        try await inPage(Self.gone)
        try await until("the app to stand at its way in") { !self.isSignedIn }
        try await until("nothing of the old conversation to be shown") { self.conversation.items.isEmpty }
        XCTAssertNil(defaults.string(forKey: "TiloLiveRoute"))
        XCTAssertEqual(engine.state, .away)
    }

    /// A page its server turned away that gets in again when it is loaded again stays its
    /// person's, and is not asked for once more without a new word of its server.
    func testAnOpenPageItsServerTurnsAwayThatGetsInAgainStaysItsPersons() async throws {
        try await startAgain()
        try await inPage(Self.enter)
        try await until("the device is in") { if case .open = self.engine.state { true } else { false } }
        let asked = loaded.count
        try await inPage("document.documentElement.dataset.refusal = 'certificate-rejected'")
        try await until("the page to be asked for again") { self.loaded.count > asked }
        try await inPage("delete document.documentElement.dataset.refusal; document.documentElement.dataset.arrival = 'done again'")
        try await until("the device to be in again") { if case .open = self.engine.state { true } else { false } }
        let then = loaded.count
        try await Task.sleep(for: .milliseconds(500))
        XCTAssertEqual(loaded.count, then, "a page that is in again was asked for once more")
        XCTAssertTrue(isSignedIn)
        XCTAssertEqual(defaults.string(forKey: "TiloLiveRoute"), "/w/ws-1")
    }

    /// What the app showed of the workspace that is gone is not shown as if it were live.
    func testWhatTheAppHeldOfAWorkspaceThatIsGoneIsPutAway() async throws {
        mainChat = .agent(id: "assistant", name: "Assistant")
        try await startAgain()
        // As a page that names a member by its kind: the person's conversation with their assistant holds one message.
        try await inPage("window.byKind = true; direct({id: 'm1', author: {person: ADA}, text: 'Good morning'}); \(Self.enter)")
        try await until("the device is in") { if case .open = self.engine.state { true } else { false } }
        try await until("the conversation is held") { self.conversation.items.count == 1 }
        // Its page loaded itself again and found the workspace gone.
        try await inPage(Self.gone)
        try await until("the app to stand at its way in") { !self.isSignedIn }
        try await until("nothing of the old conversation to be shown") { self.conversation.items.isEmpty }
    }

    /// A message sent in the first seconds of such a start is not taken for sent, and its
    /// send ends: the window puts the words back into the field.
    func testAMessageSentAsSuchAStartBeginsIsSaidNotToHaveGoneAndNothingWaitsForEver() async throws {
        try await startAgain()
        let source = source!
        let sending = Task { @MainActor () -> SourceProblem? in
            do {
                try await source.send(Draft(text: "Hello"), to: .main)
                return nil
            } catch {
                return error as? SourceProblem ?? .refused("\(error)")
            }
        }
        try await Task.sleep(for: .milliseconds(200))
        try await inPage(Self.gone)
        let waiting = Task { @MainActor in
            let problem = await sending.value
            self.sendEnded = true
            return problem
        }
        try await until("the send to end") { self.sendEnded }
        let problem = await waiting.value
        XCTAssertEqual(problem, .signedOut, "the message's send did not end as one from a device nobody is signed in on")
    }

    /// The next start of the app loads nothing of the workspace that is gone.
    func testTheNextStartAsksForNothingOfTheWorkspaceThatIsGone() async throws {
        try await standsAtItsWayIn(after: Self.gone, "the page said its workspace no longer exists")
        try await start()
        XCTAssertFalse(isSignedIn, "the app came up as the person of a workspace that is gone")
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertTrue(loaded.isEmpty, "the app asked for \(loaded.map(\.absoluteString)) at a start after its workspace was gone")
        XCTAssertEqual(engine.state, .away)
    }

    /// From that way in a pairing link brings the device back, by the same steps as a first
    /// pairing: Tilo's question, the person's press, and nothing before it.
    func testAPairingLinkTakenAtThatWayInBringsTheDeviceBackByThePersonsOwnPress() async throws {
        try await standsAtItsWayIn(after: Self.gone, "the page said its workspace no longer exists")
        let secret = String(repeating: "c7", count: 48)
        let link = try XCTUnwrap(PairingLink(in: "\(Self.site)/#/w/ws-2?pair=\(secret)"))
        try await source.pair(with: link)
        XCTAssertEqual(loaded.last?.absoluteString, "\(Self.site)/app/#/w/ws-2/so.tilo.chat?pair=\(secret)")
        try await showPage()
        try await inPage("askWhose('You are adding this device to Ada Lovelace’s devices. If that is not you, stop.', 'The server that adds it is this site’s own, tilo.test.')")
        try await until("the app to stand at Tilo's question") { if case .asked = self.sessions.last { true } else { false } }
        // Nothing answers for the person.
        try await Task.sleep(for: .milliseconds(300))
        let untouched = try await page("return String(window.agreed)")
        XCTAssertEqual(untouched, "undefined")
        await source.answerPairing(goOn: true)
        let agreed = try await page("return String(window.agreed)")
        XCTAssertEqual(agreed, "true")
        // A device of theirs approved: the page that paired is in, and is loaded again without the link's secret.
        let before = loaded.count
        try await inPage("location.hash = '#/w/ws-2/so.tilo.chat'; enter(false)")
        try await until("the workspace to be asked for again") { self.loaded.count == before + 1 }
        XCTAssertEqual(loaded.last?.absoluteString, "\(Self.site)/app/#/w/ws-2/so.tilo.chat")
        try await inPage("document.documentElement.dataset.arrival = 'done again'")
        try await until("the device to be its person's again") { self.isSignedIn }
        XCTAssertEqual(defaults.string(forKey: "TiloLiveRoute"), "/w/ws-2")
        // Its way in has nothing left to say of the workspace that was gone, now or at a later start.
        XCTAssertFalse(defaults.bool(forKey: "TiloLiveReset"))
    }

    /// A refusal that says nothing of a reset changes nothing of whose the device is.
    func testARefusalThatSaysNothingOfAResetLeavesTheDeviceItsPersons() async throws {
        try await startAgain()
        try await inPage(Self.removed)
        try await until("the refusal to be said") { self.reaches.last == .unreachable }
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertTrue(isSignedIn)
        XCTAssertEqual(defaults.string(forKey: "TiloLiveRoute"), "/w/ws-1")
        XCTAssertNotNil(defaults.array(forKey: "TiloLivePerson"))
    }

    /// A pairing link that leads into a workspace that is gone is a refused link, as it was:
    /// nobody was signed in, so nothing is put away, and the page's sentence is the engine's.
    func testALinkIntoAWorkspaceThatIsGoneIsARefusedLinkAsBefore() async throws {
        try await start()
        XCTAssertEqual(sessions.last, .signedOut)
        let link = try XCTUnwrap(PairingLink(in: "\(Self.site)/#/w/ws-9?pair=\(String(repeating: "c7", count: 48))"))
        try await source.pair(with: link)
        try await showPage()
        try await inPage(Self.gone)
        try await until("the refusal to be the engine's") {
            self.engine.state == .refused("Lounge no longer exists. It was reset since this browser last opened it.")
        }
        try await until("the plain way in to stand") { self.sessions.last == .signedOut }
    }

    // MARK: What the way in says

    /// The way in says why the device stands there at every start until it is paired again;
    /// signing out of such a device leaves the plain way in.
    func testTheWayInSaysThatTheWorkspaceIsGoneAtEveryStartUntilThePersonSignsOutOrPairsAgain() async throws {
        try await standsAtItsWayIn(after: Self.changed, "the page said its workspace was reset")
        try await start()
        XCTAssertEqual(sessions.last, .reset, "at its next start the way in no longer says why it stands there")
        await source.signOut()
        try await until("the plain way in") { self.sessions.last == .signedOut }
        try await start()
        XCTAssertEqual(sessions.last, .signedOut)
    }

    /// The names and the one sentence the engine knows a workspace that is gone by are the page's own.
    func testWhatTheEngineKnowsAWorkspaceThatIsGoneByIsWhatTilosPageSays() throws {
        var repository = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { repository.deleteLastPathComponent() }
        let web = repository.appendingPathComponent("crates/browser/tilo-browser-runtime/web")
        let peer = try String(contentsOf: web.appendingPathComponent("peer.mjs"), encoding: .utf8)
        XCTAssertTrue(peer.contains("'\(TiloEngine.resetSaid) Reload the page to join it again.'"),
                      "web/peer.mjs says a server that was set up again in other words than the engine knows")
        let screen = try String(contentsOf: web.appendingPathComponent("failure-screen.mjs"), encoding: .utf8)
        XCTAssertTrue(screen.contains("root.dataset.failure = screen.cause"), "the failure screen no longer names its case on the page's root")
        for name in ["gone", "deleted"] {
            XCTAssertTrue(TiloEngine.gone(failure: name, status: ""))
            XCTAssertTrue(screen.contains("\n  \(name): ["), "web/failure-screen.mjs has no case \(name)")
        }
        // An open page its server turned away names the refusal on its root, whatever the refusal is.
        let entry = try String(contentsOf: web.appendingPathComponent("entry-page.mjs"), encoding: .utf8)
        XCTAssertTrue(entry.contains("document.documentElement.dataset.refusal = code;"), "an open page no longer names its server's refusal on its root")
        // What says nothing of a workspace that is gone.
        for name in ["", "refused", "removed", "no-access", "invitation-only", "unreachable", "stopped"] {
            XCTAssertFalse(TiloEngine.gone(failure: name, status: "Couldn’t open Lounge. The workspace did not let you in."), name)
        }
    }

    // MARK: A stranger's link

    /// A link of another site than the app's own is refused in words: nothing of it is opened,
    /// no card is shown, and the device stays at its way in.
    func testAStrangersLinkOfAnotherSiteIsRefusedInWordsAndShowsNoCard() async throws {
        try await standsAtItsWayIn(after: Self.gone, "the page said its workspace no longer exists")
        let asked = loaded.count
        let link = try XCTUnwrap(PairingLink(in: "https://other.example/#/w/1f0e?pair=\(String(repeating: "e5", count: 48))"))
        do {
            try await source.pair(with: link)
            XCTFail("a link of another site was taken")
        } catch let problem as SourceProblem {
            XCTAssertEqual(problem, .refused(AssistantWords.codeFromElsewhere.said(["from": "other.example", "site": Self.site])))
        }
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(loaded.count, asked, "something of another site's link was opened")
        XCTAssertEqual(sessions.last, .reset)
        XCTAssertFalse(sessions.contains { if case .asked = $0 { true } else { false } }, "a card stood for a link of another site")
    }

    /// A link of the app's own site that somebody else made meets Tilo's question, which names
    /// whose devices this one would join and the app's own site in full, and lets nobody in
    /// without the person's press: stopped, the device is nobody's and stands at its way in.
    func testAStrangersLinkOfThisSiteMeetsTilosQuestionAndLetsNobodyInWithoutThePersonsPress() async throws {
        try await standsAtItsWayIn(after: Self.gone, "the page said its workspace no longer exists")
        let link = try XCTUnwrap(PairingLink(in: "\(Self.site)/#/w/ws-6?pair=\(String(repeating: "e5", count: 48))"))
        try await source.pair(with: link)
        try await showPage()
        let whose = "You are adding this device to Eve Stranger’s devices. If that is not you, stop."
        let server = "The server that adds it is this site’s own, tilo.test."
        try await inPage("askWhose('\(whose)', '\(server)')")
        let ask = PairingAsk(heading: "Add this device?", said: [whose, server], go: "Add this device", stop: "Stop", site: Self.site)
        try await until("Tilo's question to stand, with the app's own site in full") { self.sessions.last == .asked(ask) }
        try await Task.sleep(for: .milliseconds(400))
        let untouched = try await page("return String(window.agreed)")
        XCTAssertEqual(untouched, "undefined", "something answered for the person")
        XCTAssertFalse(isSignedIn)
        await source.answerPairing(goOn: false)
        let stopped = try await page("return String(window.agreed)")
        XCTAssertEqual(stopped, "false")
        // The page says that nothing was paired, as Tilo's does.
        try await inPage("""
            document.getElementById('status').textContent = 'Nothing was paired. This device was added to nobody’s devices.';
            document.documentElement.dataset.arrival = 'stopped'; document.documentElement.dataset.entry = 'failed'
            """)
        try await until("the way in to stand again") { self.sessions.last == .reset }
        XCTAssertFalse(isSignedIn)
        XCTAssertNil(defaults.string(forKey: "TiloLiveRoute"))
        XCTAssertNil(defaults.array(forKey: "TiloLivePerson"))
    }
}
#endif
