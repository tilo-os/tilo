#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// A pairing that ends before the device is anybody's: its site refuses the link, or the page
/// that was let in cannot go on. The device stands at its way in again, and the source says
/// the page's own sentence for why, until another link is taken. Against a page that speaks as
/// Tilo's does. No window is opened, nothing is fetched, nothing kept.
@MainActor
final class PairingEndedTests: XCTestCase {
    private static let site = "https://tilo.test"
    private static let ada = String(repeating: "ad", count: 32)
    private static let secret = String(repeating: "c7", count: 48)
    /// What Tilo's page says of a link into a workspace that is no longer there.
    private static let refused = "Lounge no longer exists. It was reset since this browser last opened it."
    private var source: TiloSource!
    private var engine: TiloEngine!
    private var defaults: UserDefaults!
    private var suite = ""
    private var sessions: [Session] = []
    /// Every word of why a pairing ended, as the source said them: nil where it says none.
    private var ended: [String?] = []
    /// The technical details under that sentence, as the source said them: nil where it says none.
    private var endedDetails: [String?] = []
    private var loaded: [URL] = []
    private var listening: [Task<Void, Never>] = []

    /// The app on a device nobody paired, or on one that is its person's already. Its engine
    /// fetches nothing: what it asks for is written down.
    private func start(kept: Bool = false) async throws {
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)!
        if kept {
            defaults.set("/w/ws-1", forKey: "TiloLiveRoute")
            defaults.set([Self.ada, "Ada Lovelace", "Ada's workspace"], forKey: "TiloLivePerson")
        }
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { [weak self] in self?.loaded.append($0) }
        engine.stopPause = .milliseconds(100)
        engine.stopPatience = .milliseconds(100)
        self.engine = engine
        source = TiloSource(engine: engine, mainChat: .conversation(id: "c_chat", name: "chat"), livesInOwn: false, defaults: defaults)
        let source = source!
        listening = [
            Task { for await session in source.session() { self.sessions.append(session) } },
            Task { for await why in source.pairingEnded() { self.ended.append(why) } },
            Task { for await details in source.pairingEndedDetails() { self.endedDetails.append(details) } },
        ]
        try await until("the source says where it stands") { !self.sessions.isEmpty && !self.ended.isEmpty }
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

    private func until(_ what: String, within patience: Duration = .seconds(12), _ holds: @MainActor () async -> Bool) async throws {
        let end = ContinuousClock.now.advanced(by: patience)
        while ContinuousClock.now < end {
            if await holds() { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        XCTFail("waited \(patience) for \(what)")
        throw CancellationError()
    }

    /// Tilo's page refusing: its sentence in its status line, and that it stopped, on its root.
    private static func refuses(_ words: String, failure: String = "") -> String {
        """
        document.getElementById('status').textContent = '\(words)';
        \(failure.isEmpty ? "" : "document.documentElement.dataset.failure = '\(failure)';")
        document.documentElement.dataset.arrival = 'stopped'; document.documentElement.dataset.entry = 'refused'
        """
    }

    /// Tilo's page where it cannot draw: it says that it stopped, empties its status line, and says why in a card.
    private static func cannotDraw(_ words: String) -> String {
        """
        document.documentElement.dataset.arrival = 'stopped'; document.documentElement.dataset.entry = 'unsupported';
        document.getElementById('status').textContent = '';
        const card = document.createElement('form'); card.className = 'tilo-entry-card'; card.dataset.card = 'renderer';
        const message = document.createElement('p'); message.textContent = '\(words)';
        card.append(message); document.body.append(card)
        """
    }

    private func link(into workspace: String) throws -> PairingLink {
        try XCTUnwrap(PairingLink(in: "\(Self.site)/#/w/\(workspace)?pair=\(Self.secret)"))
    }

    /// A link taken, and its site refuses it.
    private func refusedLink() async throws {
        try await start()
        XCTAssertEqual(sessions.last, .signedOut)
        XCTAssertEqual(ended, [nil], "a device nobody paired is told of a pairing that ended")
        try await source.pair(with: link(into: "ws-9"))
        try await showPage()
        try await inPage(Self.refuses(Self.refused))
        try await until("the refusal to be the engine's") { self.engine.state == .refused(Self.refused) }
        try await until("the plain way in to stand") { self.sessions.last == .signedOut }
    }

    func testALinkItsSiteRefusesLeavesTheDeviceAtItsWayInWithThePagesOwnSentence() async throws {
        try await refusedLink()
        try await until("the way in to be told why the pairing ended", within: .seconds(3)) { self.ended.last == Self.refused }
        XCTAssertEqual(sessions.last, .signedOut)
    }

    /// As in a simulator, and wherever a device offers Tilo's page nothing to draw with: the
    /// person pressed Add this device, a device of theirs approved, the page was let in, and
    /// the page that then comes cannot draw. The app stood at its way in with no word of why.
    func testADeviceThatWasLetInAndWhosePageCannotGoOnStandsAtItsWayInAndSaysWhy() async throws {
        try await start()
        try await source.pair(with: link(into: "ws-2"))
        try await showPage()
        try await inPage("askWhose('You are adding this device to Ada Lovelace’s devices. If that is not you, stop.', 'The server that adds it is this site’s own, tilo.test.')")
        try await until("Tilo's question to stand") { if case .asked = self.sessions.last { true } else { false } }
        await source.answerPairing(goOn: true)
        // A device of theirs approved: the page that paired is in, and is loaded again without the link's secret.
        let before = loaded.count
        try await inPage("location.hash = '#/w/ws-2/so.tilo.chat'; enter(false)")
        try await until("the workspace to be asked for again") { self.loaded.count == before + 1 }
        // The page that comes then cannot draw here, and says so in its own sentence.
        try await inPage(Self.cannotDraw("Tilo needs a graphics adapter to draw, and this device offers none."))
        try await until("the page's sentence to be the engine's") { if case .failed = self.engine.state { true } else { false } }
        try await until("the way in to stand again") { self.sessions.last == .signedOut }
        try await until("the way in to be told why the pairing ended", within: .seconds(3)) {
            if case .failed(let said) = self.engine.state, !said.isEmpty { self.ended.last == said } else { false }
        }
        // What the page says a moment later is what the way in says then.
        try await Task.sleep(for: .milliseconds(400))
        guard case .failed(let said) = engine.state else { return XCTFail("the engine no longer holds the page's sentence") }
        XCTAssertEqual(ended.last, said)
        XCTAssertEqual(sessions.last, .signedOut)
        XCTAssertNil(defaults.array(forKey: "TiloLivePerson"), "a device that never got in is kept as somebody's")
    }

    /// What Tilo's page says where its join stopped, and the details it shows folded under it.
    private static let stopped = "Couldn’t open Tilo. It stopped while joining. Try again. If it stops again, copy the technical details below."
    private static let details = ["Cause: loading", "Stopped at: join", "Error: DataCloneError: The object can not be cloned.", "Page build: test"]

    /// Tilo's page where its join stopped: its failure screen's details under its sentence, the
    /// sentence in its status line, and its case and that it stopped on its root.
    private static func stops(_ words: String, details: [String]) -> String {
        """
        const folded = document.createElement('details'); folded.id = 'arrival-failure-details';
        const text = document.createElement('pre'); text.id = 'arrival-failure-text';
        text.textContent = [\(details.map { "'\($0)'" }.joined(separator: ", "))].join(String.fromCharCode(10));
        folded.append(text); document.body.append(folded);
        document.getElementById('status').textContent = '\(words)';
        document.documentElement.dataset.failure = 'loading';
        document.documentElement.dataset.arrival = 'stopped'; document.documentElement.dataset.entry = 'failed'
        """
    }

    /// Tilo's page says "copy the technical details below" where its join stopped, and shows
    /// them folded under its sentence. The way in said the sentence and was handed nothing to
    /// show below it.
    func testAPairingWhosePageStoppedHandsTheWayInThePagesTechnicalDetailsWithItsSentence() async throws {
        try await start()
        try await until("the source says that it has no details") { !self.endedDetails.isEmpty }
        XCTAssertEqual(endedDetails, [nil], "a device nobody paired is handed details of a pairing that ended")
        try await source.pair(with: link(into: "ws-2"))
        try await showPage()
        try await inPage(Self.stops(Self.stopped, details: Self.details))
        // The first stop is not said: the page is asked for once more, and what stopped it is said then.
        try await until("the page's sentence to be the engine's", within: .seconds(3)) { self.engine.state == .failed(Self.stopped) }
        try await until("the way in to be told why the pairing ended", within: .seconds(3)) { self.ended.last == Self.stopped }
        let said = Self.details.joined(separator: "\n")
        try await until("the way in to be handed the page's technical details", within: .seconds(3)) { self.endedDetails.last == said }
        XCTAssertEqual(engine.stopDetails, said)
        XCTAssertEqual(sessions.last, .signedOut)
        // Another link is taken: neither the sentence nor its details are said any more.
        try await source.pair(with: link(into: "ws-3"))
        try await until("the details to be said no more") { self.endedDetails.last.map { $0 == nil } == true }
        XCTAssertEqual(ended.last.map { $0 == nil }, true, "the sentence of the pairing before is still said")
    }

    func testAnotherLinkTakenSaysTheLastPairingsEndNoMore() async throws {
        try await refusedLink()
        try await until("the way in to be told why the pairing ended", within: .seconds(3)) { self.ended.last == Self.refused }
        try await source.pair(with: link(into: "ws-2"))
        try await until("the sentence to be said no more") { self.ended.last.map { $0 == nil } == true }
    }

    /// A device that is its person's already stands at no way in: a refusal of its page is said
    /// as before (Tilo is not reached), and nothing is said of a pairing.
    func testADeviceThatIsItsPersonsAlreadyIsToldOfNoPairingThatEnded() async throws {
        try await start(kept: true)
        try await showPage()
        try await inPage(Self.refuses("You were removed from Lounge. To come back, ask someone in it for a new invitation.", failure: "removed"))
        try await until("the refusal to be the engine's") { if case .refused = self.engine.state { true } else { false } }
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(ended.compactMap { $0 }, [], "a device that is its person's was told of a pairing that ended")
        if case .signedIn = sessions.last {} else { XCTFail("the device is no longer its person's") }
    }
}
#endif
