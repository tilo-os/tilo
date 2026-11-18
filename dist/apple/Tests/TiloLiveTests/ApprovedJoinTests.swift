#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// A device whose pairing was approved and whose page then stopped before the device was
/// anybody's here: the page cannot draw, its join stops, its site goes away, or the app is
/// ended. Its store holds the device that was approved. At its next start the app stood at the
/// plain way in as if nothing had happened, and its person would pair a second time and meet
/// the same stop. It goes on as the device it has, and where that stops again the way in says
/// the page's own sentence for it. Against a page that speaks as Tilo's does. No window is
/// opened and nothing is fetched; what the app keeps is kept in a suite of the test's own.
@MainActor
final class ApprovedJoinTests: XCTestCase {
    static let site = "https://tilo.test"
    static let secret = String(repeating: "c7", count: 48)
    /// What Tilo's page says where it was let in and cannot draw.
    static let noAdapter = "Tilo draws with WebGPU. This browser has it, but offered this page no graphics adapter."
    /// What Tilo's page says of a workspace that is no longer there.
    static let gone = "Lounge no longer exists. It was reset since this browser last opened it."
    /// The page of the workspace the link named, asked for as the device the store already is:
    /// it carries nothing to pair with.
    static let asItself = "https://tilo.test/app/#/w/ws-2/so.tilo.chat"
    var source: TiloSource!
    var engine: TiloEngine!
    var defaults: UserDefaults!
    var suite = ""
    var sessions: [Session] = []
    /// Every word of why a try ended, as the source said them: nil where it says none.
    var ended: [String?] = []
    /// Every page this start's engine asked for. It fetches nothing: what it asks for is written down.
    var loaded: [URL] = []
    var listening: [Task<Void, Never>] = []
    /// Which start of the app is heard: what an ended one still says is written down nowhere.
    var started = 0

    override func setUp() async throws {
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)!
    }

    override func tearDown() async throws {
        end()
        UserDefaults.standard.removePersistentDomain(forName: suite)
        defaults = nil
    }

    /// The app is started: on a device nobody paired, or on what an earlier start kept.
    func start() async throws {
        started += 1
        let mine = started
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { [weak self] in
            guard let self, self.started == mine else { return }
            self.loaded.append($0)
        }
        engine.stopPause = .milliseconds(100)
        engine.stopPatience = .milliseconds(100)
        self.engine = engine
        source = TiloSource(engine: engine, mainChat: .conversation(id: "c_chat", name: "chat"), livesInOwn: false, defaults: defaults)
        let source = source!
        listening = [
            Task { for await session in source.session() where self.started == mine { self.sessions.append(session) } },
            Task { for await why in source.pairingEnded() where self.started == mine { self.ended.append(why) } },
        ]
        try await until("the source says where it stands") { !self.sessions.isEmpty && !self.ended.isEmpty }
    }

    /// The app is ended: what it held is gone, what it kept stays.
    func end() {
        started += 1
        for task in listening { task.cancel() }
        listening = []
        source = nil
        engine = nil
        sessions = []
        ended = []
        loaded = []
    }

    /// The page the engine asked for stands in its view, as far as a page that fetches nothing can.
    func showPage() async throws {
        // A page that stood in the view before is not the one that was asked for: it is marked,
        // and the page that comes is waited for.
        _ = try? await engine.view.callAsyncJavaScript("window.stoodBefore = true", arguments: [:], in: nil, contentWorld: .page)
        engine.view.loadHTMLString(TiloSourceTests.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page is there") {
            (try? await self.engine.view.callAsyncJavaScript("return typeof window.enter === 'function' && window.stoodBefore !== true",
                                                             arguments: [:], in: nil, contentWorld: .page)) as? Bool == true
        }
    }

    func inPage(_ script: String) async throws {
        _ = try await engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page)
    }

    func until(_ what: String, within patience: Duration = .seconds(12), _ holds: @MainActor () async -> Bool) async throws {
        let end = ContinuousClock.now.advanced(by: patience)
        while ContinuousClock.now < end {
            if await holds() { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        XCTFail("waited \(patience) for \(what)")
        throw CancellationError()
    }

    func link(into workspace: String) throws -> PairingLink {
        try XCTUnwrap(PairingLink(in: "\(Self.site)/#/w/\(workspace)?pair=\(Self.secret)"))
    }

    /// Tilo's page refusing: its sentence in its status line, and that it stopped, on its root.
    static func refuses(_ words: String, failure: String = "") -> String {
        """
        document.getElementById('status').textContent = '\(words)';
        \(failure.isEmpty ? "" : "document.documentElement.dataset.failure = '\(failure)';")
        document.documentElement.dataset.arrival = 'stopped'; document.documentElement.dataset.entry = 'refused'
        """
    }

    /// Tilo's page where it cannot draw: it says that it stopped, empties its status line, and says why in a card.
    static func cannotDraw(_ words: String) -> String {
        """
        document.documentElement.dataset.arrival = 'stopped'; document.documentElement.dataset.entry = 'unsupported';
        document.getElementById('status').textContent = '';
        const card = document.createElement('form'); card.className = 'tilo-entry-card'; card.dataset.card = 'renderer';
        const message = document.createElement('p'); message.textContent = '\(words)';
        card.append(message); document.body.append(card)
        """
    }

    /// The page shows the code a device where the person is signed in asks about, at its join.
    static let showsCode = "document.documentElement.dataset.pairingCode = '481207'; document.documentElement.dataset.arrival = 'join'"

    /// A link is taken, the person presses Add this device, the code stands, and a device where
    /// they are signed in approves: the page is let in and gets past its join.
    func pairUntilApproved() async throws {
        try await source.pair(with: link(into: "ws-2"))
        try await showPage()
        try await inPage("askWhose('You are adding this device to Ada Lovelace’s devices. If that is not you, stop.', 'The server that adds it is this site’s own, tilo.test.')")
        try await until("Tilo's question to stand") { if case .asked = self.sessions.last { true } else { false } }
        await source.answerPairing(goOn: true)
        try await inPage(Self.showsCode)
        try await until("the code to stand") { if case .confirming = self.sessions.last { true } else { false } }
        try await inPage("document.documentElement.dataset.arrival = 'open'")
        try await until("the page to be past its join") { self.engine.state == .arriving("open") }
    }

    /// As in a simulator of a phone's kind: the pairing was approved, and the page that was let
    /// in cannot draw. The way in says the page's sentence, as it did before.
    func approvedAndStopped() async throws {
        try await start()
        try await pairUntilApproved()
        try await inPage(Self.cannotDraw(Self.noAdapter))
        try await until("the way in to stand again") { self.sessions.last == .signedOut }
        try await until("the way in to say what stopped", within: .seconds(3)) { self.ended.last == Self.noAdapter }
        XCTAssertNil(defaults.array(forKey: "TiloLivePerson"), "a device that never got in is kept as somebody's")
    }

    func testADeviceThatWasApprovedAndThenStoppedGoesOnAsItselfAtTheNextStartAndSaysWhatStops() async throws {
        try await approvedAndStopped()

        // The app ends and starts again.
        end()
        try await start()
        try await until("the app to ask for its workspace's page again, as the device that was approved", within: .seconds(3)) { !self.loaded.isEmpty }
        XCTAssertEqual(loaded.map(\.absoluteString), [Self.asItself], "the app went on otherwise than as the device its store holds")
        XCTAssertEqual(sessions.first, .pairing, "the app's first word at its start is the plain way in")
        XCTAssertEqual(sessions.last, .pairing)
        // The page that comes cannot draw either: the way in says so, in the page's sentence.
        try await showPage()
        try await inPage(Self.cannotDraw(Self.noAdapter))
        try await until("the way in to stand again") { self.sessions.last == .signedOut }
        try await until("the way in to say what stopped this time", within: .seconds(3)) { self.ended.last == Self.noAdapter }

        // Ended and started once more: what was approved is still what the app goes on with.
        end()
        try await start()
        try await until("the app to go on once more", within: .seconds(3)) { !self.loaded.isEmpty }
        XCTAssertEqual(loaded.map(\.absoluteString), [Self.asItself])
        // This time its page gets in: the device is its person's, and nothing was paired twice.
        try await showPage()
        try await inPage("location.hash = '#/w/ws-2/so.tilo.chat'; enter(true)")
        try await until("the device to be its person's") { if case .signedIn = self.sessions.last { true } else { false } }
        XCTAssertEqual(ended.compactMap { $0 }, [], "the way in still says what stopped a try before")
        XCTAssertNotNil(defaults.array(forKey: "TiloLivePerson"))
        XCTAssertNil(defaults.object(forKey: "TiloLiveApproved"), "a device that is its person's is still kept as one that waits to get in")
    }

    /// A workspace that is gone ends it: the way in says that, and nothing is gone on with after.
    func testAnApprovedDeviceWhoseWorkspaceIsGoneStandsAtTheWayInThatSaysSoAndGoesOnNoMore() async throws {
        try await approvedAndStopped()
        end()
        try await start()
        try await until("the app to go on as the device that was approved", within: .seconds(3)) { !self.loaded.isEmpty }
        try await showPage()
        try await inPage(Self.refuses(Self.gone, failure: "gone"))
        try await until("the way in to say that its Tilo was reset") { self.sessions.last == .reset }

        end()
        try await start()
        try await Task.sleep(for: .milliseconds(400))
        XCTAssertEqual(loaded, [], "the app went on with a device whose workspace is gone")
        XCTAssertEqual(sessions.last, .reset)
    }

    /// A pairing that ended before anybody approved it left no device in the store: the next
    /// start stands at the plain way in and asks for nothing.
    func testAPairingThatEndedBeforeItWasApprovedLeavesThePlainWayInAtTheNextStart() async throws {
        try await start()
        try await source.pair(with: link(into: "ws-2"))
        try await showPage()
        try await inPage(Self.showsCode)
        try await until("the code to stand") { if case .confirming = self.sessions.last { true } else { false } }
        try await inPage(Self.refuses("This device was not approved."))
        try await until("the plain way in to stand") { self.sessions.last == .signedOut }

        end()
        try await start()
        try await Task.sleep(for: .milliseconds(400))
        XCTAssertEqual(loaded, [], "the app went on as a device nobody approved")
        XCTAssertEqual(sessions, [.signedOut])
        XCTAssertEqual(ended.compactMap { $0 }, [])
    }

    /// Whoever signs the device out, or takes another link, gives up what was approved before.
    func testASignOutAndAnotherLinkEachForgetWhatWasApproved() async throws {
        try await approvedAndStopped()
        await source.signOut()
        end()
        try await start()
        try await Task.sleep(for: .milliseconds(400))
        XCTAssertEqual(loaded, [], "the app went on as a device its person signed out")
        XCTAssertEqual(sessions, [.signedOut])

        try await pairUntilApproved()
        try await inPage(Self.cannotDraw(Self.noAdapter))
        try await until("the way in to stand again") { self.sessions.last == .signedOut }
        // Another link is taken, and its site refuses it before anybody is asked.
        try await source.pair(with: link(into: "ws-3"))
        try await showPage()
        try await inPage(Self.refuses("This link was used already."))
        try await until("the refusal to be said", within: .seconds(3)) { self.ended.last == "This link was used already." }
        try await until("the plain way in to stand") { self.sessions.last == .signedOut }
        end()
        try await start()
        try await Task.sleep(for: .milliseconds(400))
        XCTAssertEqual(loaded, [], "the app went on with what was approved before another link was taken")
        XCTAssertEqual(sessions, [.signedOut])
    }
}
#endif
