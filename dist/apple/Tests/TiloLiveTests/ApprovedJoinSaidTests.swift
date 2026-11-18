#if os(macOS)
import AssistantCore
@testable import AssistantTilo
@testable import TiloLive
import XCTest

/// What the source says of when this device was approved, as it says it one after the other.
@MainActor
private final class Approvals {
    var said: [Date?] = []
    private var hearing: Task<Void, Never>?

    init(of source: TiloSource) {
        hearing = Task { [weak self] in
            for await at in source.joinApproved() { self?.said.append(at) }
        }
    }

    /// When the device was approved, as it was said last. Nil: nothing was said yet, or that no approved join waits.
    var last: Date? { said.last.flatMap { $0 } }

    func stop() {
        hearing?.cancel()
    }
}

/// What the way in is told of an approved join that stopped, across the app's starts, and its
/// person's press to go on as the device that was approved.
extension ApprovedJoinTests {
    func testTheWayInIsToldWhenTheDeviceWasApprovedAcrossStartsAndAPressGoesOnAsThatDevice() async throws {
        let before = Date()
        try await start()
        var approvals = Approvals(of: source)
        try await until("the source says whether an approved join waits") { !approvals.said.isEmpty }
        XCTAssertEqual(approvals.said, [nil], "a device nobody paired is said to have been approved")
        try await pairUntilApproved()
        try await until("the way in to be told that the device was approved", within: .seconds(3)) { approvals.last != nil }
        let at = try XCTUnwrap(approvals.last)
        XCTAssertTrue(before <= at && at <= Date(), "the device was approved at another time than now")
        XCTAssertEqual(engine.letInto, "/w/ws-2")
        try await inPage(Self.cannotDraw(Self.noAdapter))
        try await until("the way in to stand again") { self.sessions.last == .signedOut }
        try await until("the way in to say what stopped", within: .seconds(3)) { self.ended.last == Self.noAdapter }
        XCTAssertEqual(approvals.last, at, "what stopped the page took back that the device was approved")

        // The app ends and starts again: the way in is told the same, and the device goes on.
        approvals.stop()
        end()
        try await start()
        approvals = Approvals(of: source)
        try await until("the way in to be told again that the device was approved") { !approvals.said.isEmpty }
        XCTAssertEqual(approvals.last, at, "the next start says another time, or none")
        try await until("the app to go on as the device that was approved", within: .seconds(3)) { !self.loaded.isEmpty }
        try await showPage()
        try await inPage(Self.cannotDraw(Self.noAdapter))
        try await until("the way in to say what stopped this time", within: .seconds(3)) { self.ended.last == Self.noAdapter }
        XCTAssertEqual(approvals.last, at)

        // Its person presses Try again: the same page is asked for once more, with nothing to pair with.
        let asked = loaded.count
        await source.goOn()
        try await until("the page to be asked for again") { self.loaded.count == asked + 1 }
        XCTAssertEqual(loaded.last?.absoluteString, Self.asItself, "the press asked for another page than the approved device's")
        try await until("the device to be on its way in") { self.sessions.last == .pairing }
        XCTAssertEqual(ended.last.map { $0 == nil }, true, "what stopped the try before is still said while the next is in work")

        // This time the page gets in: no approved join waits any more.
        try await showPage()
        try await inPage("location.hash = '#/w/ws-2/so.tilo.chat'; enter(true)")
        try await until("the device to be its person's") { if case .signedIn = self.sessions.last { true } else { false } }
        try await until("the way in to be told that no approved join waits") { approvals.said.last.map { $0 == nil } == true }
        approvals.stop()
    }

    /// A page of the try before may stand in the view for a moment after another link is taken.
    /// Its word says nothing of the new pairing: nobody approved that one.
    func testAPageOfTheTryBeforeDoesNotSayThatTheNextPairingWasLetIn() async throws {
        try await approvedAndStopped()
        XCTAssertNotNil(defaults.object(forKey: "TiloLiveApproved"), "the app keeps nothing of the pairing that was approved")
        try await source.pair(with: link(into: "ws-3"))
        XCTAssertNil(defaults.object(forKey: "TiloLiveApproved"), "another link was taken, and what was approved before is still kept")
        // The page of the try before speaks once more, as a page does that is on its way out.
        try await inPage("document.documentElement.dataset.entry = ''")
        try await until("the page's word to be heard") { self.engine.state == .arriving("stopped") }
        try await inPage("document.documentElement.dataset.entry = 'unsupported'")
        try await until("the page's sentence to be the engine's") { if case .failed = self.engine.state { true } else { false } }
        try await until("the way in to stand") { self.sessions.last == .signedOut }
        XCTAssertNil(engine.letInto, "a page of the try before is taken as the new pairing's page, let in")
        XCTAssertNil(defaults.object(forKey: "TiloLiveApproved"), "the app keeps an approval nobody gave")
    }

    /// A page that pairs and stops at its join was not let in: nothing is held of an approval.
    func testAPageThatStopsAtItsJoinIsNotSaidToBeLetIn() async throws {
        XCTAssertFalse(TiloEngine.letIn(entry: "", arrival: "join"))
        XCTAssertFalse(TiloEngine.letIn(entry: "refused", arrival: "stopped"))
        XCTAssertFalse(TiloEngine.letIn(entry: "failed", arrival: "stopped"))
        XCTAssertFalse(TiloEngine.letIn(entry: "", arrival: "load"))
        XCTAssertTrue(TiloEngine.letIn(entry: "", arrival: "open"))
        XCTAssertTrue(TiloEngine.letIn(entry: "", arrival: "draw"))
        XCTAssertTrue(TiloEngine.letIn(entry: "open", arrival: "done"))
        XCTAssertTrue(TiloEngine.letIn(entry: "unsupported", arrival: "stopped"))
        try await start()
        try await source.pair(with: link(into: "ws-2"))
        try await showPage()
        try await inPage(Self.showsCode)
        try await until("the code to stand") { if case .confirming = self.sessions.last { true } else { false } }
        XCTAssertNil(engine.letInto, "a page that waits at its join is held as let in")
        try await inPage(Self.refuses("This device was not approved."))
        try await until("the plain way in to stand") { self.sessions.last == .signedOut }
        XCTAssertNil(engine.letInto)
        XCTAssertNil(defaults.object(forKey: "TiloLiveApproved"))
    }
}
#endif
