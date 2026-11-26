#if os(macOS)
import AssistantCore
@testable import AssistantTilo
import ShellCore
@testable import TiloLive
import WebKit
import XCTest

/// The assistant view's figures of the day, against the page that speaks as Chat's does
/// (`TiloSourceTests.page`) and a workspace that keeps its agents as Tilo's server keeps them:
/// what an agent spent on a day under that day's date, the most it may spend in a day, and a
/// count of messages or of tokens only where a test's server keeps one. No window is opened,
/// nothing is fetched, nothing kept.
@MainActor
final class AssistantViewTests: XCTestCase {
    private static let site = "https://tilo.test"
    private var source: TiloSource!
    private var suite = ""
    private var sessions: [Session] = []
    private var changes: [ConversationChange] = []
    private var supervisions: [Supervision] = []
    private var listening: [Task<Void, Never>] = []
    /// The time it is on this device, as a test says it.
    private var clock = Date(timeIntervalSince1970: 0)
    /// The lines as they are said in English, on whatever machine the tests are built.
    private let wording = Wording(locale: Locale(identifier: "en_US"))

    /// A calendar that dates a moment the same on every machine the tests are built on.
    private static let calendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }()

    private static func time(_ written: String) -> Date {
        ISO8601DateFormatter().date(from: written)!
    }

    private func start(at written: String, and fraction: TimeInterval = 0) async throws {
        clock = Self.time(written).addingTimeInterval(fraction)
        suite = "so.tilo.assistant.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { _ in }
        source = TiloSource(engine: engine, mainChat: .assistant, livesInOwn: false, defaults: defaults, closedKeys: nil)
        let source = source!
        source.assistantDay.calendar = Self.calendar
        source.assistantDay.now = { [unowned self] in self.clock }
        listening = [
            Task { for await session in source.session() { self.sessions.append(session) } },
            Task { for await change in source.conversation(.main) { self.changes.append(change) } },
            Task { for await facts in source.supervision() { self.supervisions.append(facts) } },
        ]
        engine.view.loadHTMLString(TiloSourceTests.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page is there") {
            (try? await engine.view.callAsyncJavaScript("return typeof window.enter", arguments: [:], in: nil, contentWorld: .page)) as? String == "function"
        }
    }

    override func tearDown() async throws {
        for task in listening { task.cancel() }
        UserDefaults.standard.removePersistentDomain(forName: suite)
        source = nil
    }

    private func inPage(_ script: String) async throws {
        _ = try await source.engine.view.callAsyncJavaScript(script, arguments: [:], in: nil, contentWorld: .page)
    }

    private func until(_ what: String, _ holds: @MainActor () async -> Bool) async throws {
        for _ in 0..<240 {
            if await holds() { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        XCTFail("waited twelve seconds for \(what)")
        throw CancellationError()
    }

    /// The device gets in, where the workspace keeps `agents`: its server says them to the
    /// person's device, and the page's own Chat holds the same ones.
    private func enter(_ agents: String) async throws {
        try await inPage("window.observeAgents = true; window.byKind = true; window.agents = window.pageAgents = \(agents); location.hash = '#/w/ws-1/so.tilo.chat'; enter(true)")
        try await until("the app is signed in") { if case .signedIn = self.sessions.last { true } else { false } }
        try await until("the conversation is told") { !self.changes.isEmpty }
    }

    /// What the assistant view was last told of the day.
    private var usage: Supervision.Usage? { supervisions.last?.usage }

    /// Ada's assistant as Tilo's server keeps one today: what it spent on 17 November and the
    /// most it may spend in a day. No count of messages and none of tokens.
    private static let reed = "kept('reed', {name: 'Reed', spend_limit_micros: 10000000, spent: {day: '2026-11-17', cost_micros: 3120000}})"

    func testWhereHerServerCountsNoMessagesAndNoTokensTheViewSaysSoAndShowsNoNought() async throws {
        try await start(at: "2026-11-17T12:00:00Z")
        try await enter("[\(Self.reed)]")
        try await until("the assistant's day is read") { self.usage?.dailyLimitMicros == 10_000_000 }
        let usage = try XCTUnwrap(usage)
        // The money is what her server counted for this day, of the most it may spend.
        XCTAssertEqual(wording.spent(usage), Wording.Line(first: "3.12 $", second: "Of 10 $ a day"))
        // Her server keeps no count of messages and none of tokens: neither is shown as counted.
        XCTAssertNil(usage.messagesToday, "a count of messages that nobody kept is shown")
        XCTAssertNil(usage.tokensToday, "a count of tokens that nobody kept is shown")
        XCTAssertEqual(wording.messages(usage), "Not counted yet")
        XCTAssertEqual(wording.tokens(usage), "Not counted yet")
    }

    func testWhatHerServerCountedOnADayThatIsOverIsNotSaidAsTodays() async throws {
        try await start(at: "2026-11-18T09:00:00Z")
        try await enter("[\(Self.reed)]")
        try await until("the assistant's day is read") { self.usage?.dailyLimitMicros == 10_000_000 }
        // The count is dated the day before. Today her server has charged nothing yet, and reads it so itself.
        XCTAssertEqual(usage?.spentTodayMicros, 0, "what was spent the day before is shown as today's")
        XCTAssertEqual(wording.spent(try XCTUnwrap(usage)), Wording.Line(first: "0 $", second: "Of 10 $ a day"))
        // The new day's first charge is the day's.
        try await inPage("window.agents[0].spent = {day: '2026-11-18', cost_micros: 40000}; changeRoster()")
        try await until("the new day's first charge") { self.usage?.spentTodayMicros == 40_000 }
        // A server whose own day turned before this device's counts on its day: that is its today.
        try await inPage("window.agents[0].spent = {day: '2026-11-19', cost_micros: 70000}; changeRoster()")
        try await until("a count dated ahead of this device") { self.usage?.spentTodayMicros == 70_000 }
    }

    func testTheDaysFiguresStartAgainWhenTheDayTurnsWhileTheAppIsOpen() async throws {
        // A fifth of a second before this device's day turns.
        try await start(at: "2026-11-17T23:59:59Z", and: 0.8)
        try await enter("[\(Self.reed)]")
        try await until("the day's money") { self.usage?.spentTodayMicros == 3_120_000 }
        // The day turns. Nothing is written anywhere, and the page says nothing.
        clock = Self.time("2026-11-18T00:00:01Z")
        try await until("the day's count to start again") { self.usage?.spentTodayMicros == 0 }
        XCTAssertEqual(usage?.dailyLimitMicros, 10_000_000)
    }

    func testWhereHerServerCountsMessagesAndTokensTheViewShowsItsCountsOfTheDay() async throws {
        try await start(at: "2026-11-17T12:00:00Z")
        try await enter("[kept('reed', {name: 'Reed', spent: {day: '2026-11-17', cost_micros: 3120000, messages: 42, tokens: 1800000}})]")
        try await until("the day's counts") { self.usage?.messagesToday == 42 }
        XCTAssertEqual(usage?.tokensToday, 1_800_000)
        XCTAssertEqual(wording.messages(try XCTUnwrap(usage)), "42")
        XCTAssertEqual(wording.tokens(try XCTUnwrap(usage)), "1.8M")
        // The counts follow what her server writes while the app is open.
        try await inPage("window.agents[0].spent = {day: '2026-11-17', cost_micros: 3200000, messages: 43, tokens: 1850000}; changeRoster()")
        try await until("the next message to be counted") { self.usage?.messagesToday == 43 }
        XCTAssertEqual(usage?.tokensToday, 1_850_000)
        XCTAssertEqual(usage?.spentTodayMicros, 3_200_000)
        // On a day her server has not charged yet it counted none: nought, and counted.
        clock = Self.time("2026-11-18T08:00:00Z")
        try await inPage("touch()")
        try await until("the new day") { self.usage?.messagesToday == 0 }
        XCTAssertEqual(usage?.tokensToday, 0)
        XCTAssertEqual(usage?.spentTodayMicros, 0)
    }

    // MARK: Where it works

    /// The line of the assistant view that says where the assistant works, as it was last told.
    private var places: Wording.Line? { supervisions.last.map { wording.places($0) } }

    func testPlacementFollowsAvailableDevicesAndRetiringTheAgentClearsThePlace() async throws {
        try await start(at: "2026-11-17T12:00:00Z")
        // Legacy host hints do not describe which device will claim a turn.
        try await enter("[kept('reed', {name: 'Reed', on_host: true})]")
        try await until("where the assistant works") {
            self.supervisions.last?.state == .active && self.places == Wording.Line(first: "Your workspace, on an available device")
        }
        // Retired, it works nowhere, and the line says nothing of a place.
        try await inPage("window.agents[0].state = {$variant: 'retired'}; changeRoster()")
        try await until("the retired assistant has no place") {
            self.supervisions.last?.state == .retired && self.places == Wording.Line(first: "Nothing yet")
        }
        try await inPage("window.agents = []; changeRoster()")
        try await until("the absent assistant has no reported state") { self.supervisions.last?.state == nil }
    }
}
#endif
