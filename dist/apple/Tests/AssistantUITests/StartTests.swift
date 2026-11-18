#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// Wait until something holds, a little while at most.
@MainActor
private func until(_ holds: () -> Bool, file: StaticString = #filePath, line: UInt = #line) async throws {
    for _ in 0..<400 {
        if holds() { return }
        try await Task.sleep(for: .milliseconds(5))
    }
    XCTFail("it never came", file: file, line: line)
}

/// The app's first seconds. Until a first try to get in has ended, nothing is known of whether
/// Tilo answers, and nothing is said of it; what a person sends meanwhile waits.
@MainActor
final class StartTests: XCTestCase {
    func testUntilTheSourceHasSaidItNothingIsKnownOfWhetherTiloAnswers() async {
        let model = AssistantModel(source: MemorySource())
        XCTAssertEqual(model.reach, .unknown, "the window held whether Tilo answers before anything had said it")
        await model.prime()
        XCTAssertEqual(model.reach, .reachable)
    }

    /// The owner's picture: the app right after its start, an empty conversation, and over the
    /// message field "Tilo cannot be reached right now", though nothing had failed.
    func testTheNoticeIsSaidOnlyAfterATryThatFailedAndGoesTheMomentALaterOneGetsIn() async throws {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        let started = try await Started.app(content)
        defer { started.end() }
        let drive = started.drive, source = started.source.inner
        let notice = AssistantWords.notReachable.said, waiting = AssistantWords.pairing.said

        // The start: not known yet. The quiet word for waiting, and no word of a failure.
        source.set(Reach.unknown)
        await drive.until("the window holds that nothing is known yet") { started.model.reach == .unknown }
        await drive.until("the quiet word for waiting") { drive.shows(waiting) }
        XCTAssertFalse(drive.shows(notice), "the window said that Tilo cannot be reached before a try had ended")
        drive.picture("start-not-known-yet")

        // A try failed: now it is said, and nothing is waited for.
        source.set(Reach.unreachable)
        await drive.until("the notice, after a try that failed") { drive.shows(notice) }
        XCTAssertFalse(drive.shows(waiting), "a try that failed was still said to be waited for")
        drive.picture("start-a-try-failed")

        // A later try gets in: the notice goes with it.
        source.set(Reach.reachable)
        await drive.until("the notice to go") { !drive.shows(notice) }
        XCTAssertFalse(drive.shows(waiting))
        drive.picture("start-in")
    }

    func testAMessageSentBeforeItIsKnownWaitsAndGoesOutOnceTheTryGetsIn() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source)
        model.start()
        try await until { model.conversation(.main).isLoaded }
        source.set(Reach.unknown)
        try await until { model.reach == .unknown }

        model.drafts[.main] = Draft(text: "are you there")
        model.send(in: .main)
        XCTAssertNil(model.drafts[.main], "the field is empty at once")
        // It waits: nothing has gone out, and nothing is said to have failed.
        try await Task.sleep(for: .milliseconds(200))
        XCTAssertNil(model.problem, "a message sent in the first seconds was said not to have gone")
        XCTAssertNil(model.drafts[.main])
        XCTAssertEqual(source.items().count, Drawing.mainChat.count)

        source.set(Reach.reachable)
        try await until { source.items().count == Drawing.mainChat.count + 1 }
        XCTAssertNil(model.problem)
        XCTAssertNil(model.drafts[.main])
    }

    func testAMessageSentBeforeItIsKnownIsBackInTheFieldWithTheReasonWhereTheTryFails() async throws {
        let source = Drawing.source()
        let model = AssistantModel(source: source)
        model.start()
        try await until { model.conversation(.main).isLoaded }
        source.set(Reach.unknown)
        try await until { model.reach == .unknown }

        model.drafts[.main] = Draft(text: "are you there")
        model.send(in: .main)
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertNil(model.problem, "a message sent in the first seconds was said not to have gone before the try ended")
        source.set(Reach.unreachable)
        try await until { model.problem != nil }
        XCTAssertEqual(model.problem, AssistantWords.unreachable.said)
        XCTAssertEqual(model.drafts[.main]?.text, "are you there", "what did not go through is back in the field")
        XCTAssertEqual(source.items().count, Drawing.mainChat.count)
    }
}
#endif
