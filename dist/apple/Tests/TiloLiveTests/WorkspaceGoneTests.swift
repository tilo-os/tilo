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
    /// Tilo's page where its join stopped otherwise than by its server's refusal, as on a device
    /// that kept nothing to hold the answering server against: it names its entry `failed`, asks
    /// its site what became of the workspace it remembers, and names its failure screen's case
    /// after that.
    private static let goneAfterAStop = """
        document.documentElement.dataset.arrival = 'stopped'; document.documentElement.dataset.entry = 'failed';
        await new Promise(resolve => setTimeout(resolve, 30));
        document.getElementById('status').textContent = 'Lounge no longer exists. It was reset since this browser last opened it.';
        document.documentElement.dataset.failure = 'gone'
        """
    /// Its join stopped and its site said nothing of the workspace: a moment's failure.
    private static let stoppedJoining = """
        document.getElementById('status').textContent = 'Couldn’t open Lounge. It stopped while joining. Try again, or reload the page.';
        document.documentElement.dataset.failure = 'loading';
        document.documentElement.dataset.arrival = 'stopped'; document.documentElement.dataset.entry = 'failed'
        """
    /// Tilo's page where its site answered nothing once the page stopped. Its failure screen's
    /// case says so; its sentence is, on purpose, the one of any stop on the way in.
    private static let noAnswer = """
        document.getElementById('status').textContent = 'Couldn’t open Lounge. It stopped while joining. Try again, or reload the page.';
        document.documentElement.dataset.failure = 'no-connection';
        document.documentElement.dataset.arrival = 'stopped'; document.documentElement.dataset.entry = 'failed'
        """
    /// The page that was asked for again, on its way: it names no entry yet.
    private static let onItsWay = "document.documentElement.dataset.entry = ''; document.documentElement.dataset.arrival = 'load'; document.documentElement.dataset.failure = ''"
    private var source: TiloSource!
    private var engine: TiloEngine!
    private var defaults: UserDefaults!
    private var suite = ""
    private var sessions: [Session] = []
    private var reaches: [Reach] = []
    private var joins: [Bool] = []
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
        joins = []
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
            Task { for await stops in source.joinStops() { self.joins.append(stops) } },
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

    /// The page's join stopped otherwise than by its server's refusal, and its site then said
    /// that the workspace it remembers is gone. The page says so once: it has forgotten the
    /// workspace at its next load. A build of before took it for a page that stopped and loaded
    /// it again for ever, its person signed in and nothing to press.
    func testADeviceWhosePageStoppedAndThenNamedItsWorkspaceGoneStandsAtItsWayIn() async throws {
        try await standsAtItsWayIn(after: Self.goneAfterAStop, "the page stopped and then said its workspace no longer exists")
    }

    /// A page whose join stopped and that names no workspace that is gone is asked for again, and
    /// the device stays its person's: its server may only be away.
    func testAPageWhoseJoinStoppedAndNamesNoWorkspaceThatIsGoneIsAskedForAgain() async throws {
        try await startAgain()
        let asked = loaded.count
        try await inPage(Self.stoppedJoining)
        try await until("the page to be asked for again") { self.loaded.count > asked }
        XCTAssertTrue(isSignedIn, "a device whose page stopped for a moment is nobody's any more")
        XCTAssertNotNil(defaults.string(forKey: "TiloLiveRoute"), "the workspace of a page that stopped for a moment was put away")
    }

    // MARK: A site that answers and does not let the device in

    /// The page of a device that is its person's stops; it is asked for again, and the page
    /// that comes is on its way and then says the same. Nothing but a page's own word ends a
    /// try here.
    private func stopTwice(saying said: String) async throws {
        try await startAgain()
        engine.stopPatience = .seconds(60)
        let asked = loaded.count
        try await inPage(said)
        try await until("the page to be asked for again") { self.loaded.count > asked }
        try await inPage(Self.onItsWay)
        try await inPage(said)
        try await until("the app to say that the try failed") { self.reaches.last == .unreachable }
    }

    /// A device that is its person's, whose site serves its page and whose page's join stops at
    /// every load: a store that missed the one time its page said that its workspace is gone
    /// stands so for ever. The app says that Tilo answers and does not let this device in, so
    /// that its window can offer the way in. Nothing of the device is given up by that: it
    /// stays its person's and keeps where it was let in until its person presses, which does
    /// what "Sign out of this device" does.
    func testADeviceWhoseSiteAnswersAndWhoseJoinStopsIsOfferedItsWayInAndGivesNothingUpBeforeItsPersonPresses() async throws {
        try await stopTwice(saying: Self.stoppedJoining)
        try await until("the app to say that Tilo answers and does not let this device in") { self.joins.last == true }
        XCTAssertTrue(isSignedIn, "a device that is only offered its way in is nobody's any more")
        XCTAssertEqual(defaults.string(forKey: "TiloLiveRoute"), "/w/ws-1", "where the device was let in was put away before its person pressed")
        XCTAssertNotNil(defaults.array(forKey: "TiloLivePerson"), "who the device is was put away before its person pressed")
        // Its page is asked for again as before, and the offer stands through the try.
        let asked = loaded.count
        try await until("the page to be asked for once more") { self.loaded.count > asked }
        XCTAssertEqual(joins.last, true, "the offer went away while a try was in work")
        XCTAssertTrue(isSignedIn)
        // The person presses.
        await source.signOut()
        try await until("the device to stand at its way in") { self.sessions.last == .signedOut }
        XCTAssertEqual(joins.last, false, "a device at its way in is still told that it is not let in")
        XCTAssertNil(defaults.string(forKey: "TiloLiveRoute"))
        XCTAssertEqual(engine.state, .away)
    }

    /// A site that does not answer offers no way in: signing out brings nobody nearer to a
    /// server that is away.
    func testADeviceWhoseSiteDoesNotAnswerIsOfferedNoWayIn() async throws {
        try await stopTwice(saying: Self.noAnswer)
        let asked = loaded.count
        try await until("the page to be asked for once more") { self.loaded.count > asked }
        XCTAssertFalse(joins.contains(true), "a device whose site did not answer was offered its way in")
        XCTAssertTrue(isSignedIn)
        XCTAssertEqual(reaches.last, .unreachable)
    }

    /// A page that names another case in the same sentence ends no new try and changes no
    /// sentence: its word is heard all the same.
    func testAPageThatSaysOnlyLaterThatItsSiteAnsweredIsHeardAlthoughItsSentenceIsTheSame() async throws {
        try await stopTwice(saying: Self.noAnswer)
        XCTAssertNotEqual(joins.last, true)
        try await inPage("document.documentElement.dataset.failure = 'loading'")
        try await until("the app to say that Tilo answers and does not let this device in") { self.joins.last == true }
        XCTAssertTrue(isSignedIn)
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

    /// The site is set up again while the app is open, and for a while no server answers at
    /// all: nothing turns the open page's session away. The page only says on its root that it
    /// lost its server, and goes on showing what it showed. The app asks for its page again, as
    /// for a page that stopped, and what the page that comes says is where the device stands.
    func testAnOpenPageThatLostItsServerIsAskedForAgainAndTheWorkspaceItThenFindsGonePutsTheAppAtItsWayIn() async throws {
        mainChat = .agent(id: "assistant", name: "Assistant")
        try await startAgain()
        try await inPage("window.byKind = true; direct({id: 'm1', author: {person: ADA}, text: 'Good morning'}); \(Self.enter)")
        try await until("the device is in") { if case .open = self.engine.state { true } else { false } }
        try await until("the conversation is held") { self.conversation.items.count == 1 }
        let asked = loaded.count
        try await inPage("document.documentElement.dataset.connection = 'offline'")
        try await until("the page to be asked for again") { self.loaded.count > asked }
        XCTAssertEqual(loaded.last?.absoluteString, "\(Self.site)/app/#/w/ws-1/so.tilo.chat")
        XCTAssertTrue(isSignedIn, "a device whose server is away for a while is still its person's")
        // Another server answers at the site now: the page that is loaded again finds the workspace gone.
        try await inPage(Self.gone)
        try await until("the app to stand at its way in") { !self.isSignedIn }
        try await until("nothing of the old conversation to be shown") { self.conversation.items.isEmpty }
        XCTAssertNil(defaults.string(forKey: "TiloLiveRoute"))
        XCTAssertEqual(engine.state, .away)
        XCTAssertEqual(sessions.last, .reset)
    }

    /// A server that was only away for a while: the page that is asked for again gets in, the
    /// device stays its person's, and nothing is asked for once more.
    func testAnOpenPageThatLostItsServerAndGetsInAgainStaysItsPersons() async throws {
        try await startAgain()
        try await inPage(Self.enter)
        try await until("the device is in") { if case .open = self.engine.state { true } else { false } }
        let asked = loaded.count
        try await inPage("document.documentElement.dataset.connection = 'offline'")
        try await until("the page to be asked for again") { self.loaded.count > asked }
        // The page that comes is a new document, which says nothing of a connection that was cut.
        try await inPage("delete document.documentElement.dataset.connection; document.documentElement.dataset.arrival = 'done again'")
        try await until("the device to be in again") { if case .open = self.engine.state { true } else { false } }
        let then = loaded.count
        try await Task.sleep(for: .milliseconds(500))
        XCTAssertEqual(loaded.count, then, "a page that is in again was asked for once more")
        XCTAssertTrue(isSignedIn)
        XCTAssertEqual(defaults.string(forKey: "TiloLiveRoute"), "/w/ws-1")
    }

    /// A connection that is back before the page is asked for again: the page found its server
    /// by itself, and is left as it is.
    func testAPageWhoseConnectionIsBackBeforeItIsAskedForAgainIsLeftAsItIs() async throws {
        try await startAgain()
        engine.stopPause = .milliseconds(700)
        try await inPage(Self.enter)
        try await until("the device is in") { if case .open = self.engine.state { true } else { false } }
        let asked = loaded.count
        try await inPage("document.documentElement.dataset.connection = 'offline'")
        try await until("the engine to hold that its page is not in") { if case .arriving = self.engine.state { true } else { false } }
        try await inPage("document.documentElement.dataset.connection = 'online'")
        try await until("the device to be in again") { if case .open = self.engine.state { true } else { false } }
        try await Task.sleep(for: .milliseconds(1000))
        XCTAssertEqual(loaded.count, asked, "a page that found its server again by itself was asked for anew")
        XCTAssertTrue(isSignedIn)
    }

    /// What a page says of its store, its files or its drawing says nothing of its server:
    /// nothing is asked for again.
    func testWhatAPageSaysOfItsStoreOrItsDrawingAsksForNothingAgain() async throws {
        try await startAgain()
        try await inPage(Self.enter)
        try await until("the device is in") { if case .open = self.engine.state { true } else { false } }
        let asked = loaded.count
        for word in ["connecting", "online", "saving", "files", "renderer"] {
            try await inPage("document.documentElement.dataset.connection = '\(word)'")
            try await Task.sleep(for: .milliseconds(300))
            XCTAssertEqual(loaded.count, asked, "the page was asked for again at \(word)")
            XCTAssertTrue({ if case .open = engine.state { true } else { false } }(), "the device is no longer in at \(word)")
        }
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
        let web = repository.appendingPathComponent("crates/core/tilo-browser-runtime/web")
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
        // A join that stopped otherwise than by a refusal is named `failed`, and what became of the workspace is asked after that.
        XCTAssertTrue(entry.contains("? 'refused' : 'failed';"), "the page no longer names a join that stopped without a refusal `failed`")
        // What says nothing of a site that answered and stopped the page's join is the failure screen's own names.
        for name in TiloEngine.unanswered.sorted() {
            XCTAssertFalse(TiloEngine.answered(failure: name), name)
            XCTAssertTrue(screen.contains("return '\(name)';"), "web/failure-screen.mjs has no case \(name)")
        }
        for name in ["loading", "desktop", "stopped"] {
            XCTAssertTrue(TiloEngine.answered(failure: name), name)
        }
        XCTAssertFalse(TiloEngine.answered(failure: ""), "a page that has not named its case says nothing of its site")
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
