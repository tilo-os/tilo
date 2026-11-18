#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// The first seconds of a device that was paired before, against a page that speaks as Chat's
/// does: what the source says of whether Tilo answers while its page is on its way in, after
/// a try that failed and once a try gets in, and what becomes of a message sent meanwhile.
/// No window is opened, nothing is fetched, nothing kept.
@MainActor
final class ReachTests: XCTestCase {
    private static let site = "https://tilo.test"
    private static let ada = String(repeating: "ad", count: 32)
    /// The page gets in, in the workspace the device was let into.
    private static let enter = "location.hash = '#/w/ws-1/so.tilo.chat'; enter(false)"
    /// The page stops on its way in and says why, as Tilo's does.
    private static let fail = """
        document.getElementById('status').textContent = 'Tilo is not answering';
        document.documentElement.dataset.arrival = 'stopped'; document.documentElement.dataset.entry = 'failed'
        """
    private static let sent = "return JSON.stringify(window.messages.map(message => message.text))"
    private var source: TiloSource!
    private var suite = ""
    private var said: [Reach] = []
    private var listening: [Task<Void, Never>] = []

    /// The app started again on a device that is its person's already: it keeps who they are
    /// and where it was let in, and its page is on its way in from the first moment.
    private func startAgain() async throws {
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.set("/w/ws-1", forKey: "TiloLiveRoute")
        defaults.set([Self.ada, "Ada Lovelace", "Ada's workspace"], forKey: "TiloLivePerson")
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { _ in }
        engine.stopPause = .milliseconds(100)
        engine.stopPatience = .milliseconds(100)
        let chat = TiloSource.MainChat.conversation(id: "c_chat", name: "chat")
        source = TiloSource(engine: engine, mainChat: chat, livesInOwn: false, defaults: defaults)
        let source = source!
        listening = [Task { for await reach in source.reach() { self.said.append(reach) } }]
        engine.view.loadHTMLString(TiloSourceTests.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page is there") {
            (try? await engine.view.callAsyncJavaScript("return typeof window.enter", arguments: [:], in: nil, contentWorld: .page)) as? String == "function"
        }
        try await until("the source says something of whether Tilo answers") { !self.said.isEmpty }
    }

    override func tearDown() async throws {
        for task in listening { task.cancel() }
        UserDefaults.standard.removePersistentDomain(forName: suite)
        source = nil
    }

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

    private var isOnItsWayIn: Bool {
        if case .arriving = source.engine.state { true } else { false }
    }

    /// The owner's start: the app comes up signed in, its page takes its seconds to load, and
    /// in the end it gets in. Nothing had failed, so nothing is said to have.
    func testAtTheStartNothingIsSaidOfWhetherTiloAnswersUntilTheFirstTryHasEnded() async throws {
        try await startAgain()
        XCTAssertEqual(said, [.unknown], "the source said that Tilo cannot be reached before a try had ended")
        XCTAssertTrue(isOnItsWayIn)
        // The page takes its time. No count of seconds ends the try: still nothing is said.
        try await Task.sleep(for: .milliseconds(1500))
        XCTAssertEqual(said, [.unknown])
        XCTAssertTrue(isOnItsWayIn)
        // It gets in: Tilo answers, and that it cannot be reached was never said.
        try await inPage(Self.enter)
        try await until("the source says that Tilo answers") { self.said.last == .reachable }
        XCTAssertEqual(said, [.unknown, .reachable])
    }

    func testATryThatFailedSaysThatTiloCannotBeReachedAndALaterTryThatGetsInTakesItBack() async throws {
        try await startAgain()
        try await inPage(Self.fail)
        try await until("the source says that Tilo cannot be reached") { self.said.last == .unreachable }
        XCTAssertEqual(said, [.unknown, .unreachable])
        // The page is loaded again and again meanwhile: nothing new is said of it.
        try await Task.sleep(for: .milliseconds(600))
        XCTAssertEqual(said, [.unknown, .unreachable])
        try await inPage(Self.enter)
        try await until("the source says that Tilo answers") { self.said.last == .reachable }
        XCTAssertEqual(said, [.unknown, .unreachable, .reachable])
    }

    /// What a person sends in the first seconds waits, as a send waits where Chat's send is
    /// not offered yet, and goes out once the page is in and the main chat's other side is known.
    func testAMessageSentWhileTheDeviceIsOnItsWayInWaitsAndGoesOutOnceItIsIn() async throws {
        try await startAgain()
        let source = source!
        let sending = Task { @MainActor () -> String? in
            do {
                try await source.send(Draft(text: "Hello"), to: .main)
                return nil
            } catch {
                return "\(error)"
            }
        }
        try await Task.sleep(for: .milliseconds(300))
        let early = try await page(Self.sent)
        XCTAssertEqual(early, "[]")
        try await inPage(Self.enter)
        let problem = await sending.value
        XCTAssertNil(problem, "a message sent in the first seconds was said not to have gone")
        let sent = try await page(Self.sent)
        XCTAssertEqual(sent, #"["Hello"]"#)
        XCTAssertEqual(said, [.unknown, .reachable])
    }

    /// Where the first try fails, the message that waited is said not to have gone, and the
    /// reason is that Tilo did not answer: not that its person has no assistant.
    func testAMessageSentWhileTheFirstTryFailsIsSaidNotToHaveGoneBecauseTiloDidNotAnswer() async throws {
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
        try await inPage(Self.fail)
        let problem = await sending.value
        XCTAssertEqual(problem, .unreachable)
        XCTAssertEqual(said, [.unknown, .unreachable])
        let sent = try await page(Self.sent)
        XCTAssertEqual(sent, "[]")
    }
}
#endif
