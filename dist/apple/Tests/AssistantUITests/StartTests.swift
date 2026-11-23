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

    /// A person's words on 18 November 2026: the app said "Waiting for Tilo" and then "Setting up
    /// your assistant", at a start that set nothing up. From opening the app until the main chat
    /// stands a person reads one sentence, through every step the app takes behind it.
    func testFromOpeningTheAppUntilTheMainChatStandsAPersonReadsOneSentence() async throws {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        content.setup = .findingAssistant
        let started = try await Started.app(content)
        defer { started.end() }
        let drive = started.drive, source = started.source.inner
        let waiting = AssistantWords.pairing.said, settingUp = "Setting up your assistant"

        // The page and the session: nothing is known yet.
        source.set(Reach.unknown)
        await drive.until("the window holds that nothing is known yet") { started.model.reach == .unknown }
        await drive.until("the one sentence") { drive.shows(waiting) }
        XCTAssertFalse(drive.shows(settingUp), "a start that sets nothing up said that it does")
        drive.picture("start-page-and-session")

        // The page is in, and the app looks for its assistant, as every start does: the same
        // sentence, and no other.
        source.set(Reach.reachable)
        await drive.until("the window holds that Tilo answers") { started.model.reach == .reachable }
        await drive.settle(300)
        XCTAssertTrue(drive.shows(waiting), "the one sentence went before the main chat stood:\n\(drive.said())")
        XCTAssertFalse(drive.shows(settingUp), "the look for an assistant was said as a setting up")
        XCTAssertFalse(drive.shows(AssistantWords.nothingYet.said), "a chat that is still waited for was said to be empty")
        drive.picture("start-the-look")

        // The assistant is found and what answers for it is read: the main chat stands, and
        // nothing is waited for.
        source.set(Setup.ready)
        await drive.until("the sentence to go") { !drive.shows(waiting) }
        XCTAssertFalse(drive.shows(settingUp))
        drive.picture("start-the-main-chat-stands")
    }

    /// The first time, while an assistant is made for a person who keeps none: only then does
    /// the main chat say that their assistant is being set up, and it is the one sentence that stands.
    func testThatAnAssistantIsBeingSetUpIsSaidOnlyWhileOneIsMade() async throws {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        content.setup = .makingAssistant
        let started = try await Started.app(content)
        defer { started.end() }
        let drive = started.drive, source = started.source.inner
        await drive.until("the main chat to say that an assistant is being set up") { drive.shows(AssistantWords.makingAssistant.said) }
        XCTAssertFalse(drive.shows(AssistantWords.pairing.said), "two sentences stood at once:\n\(drive.said())")
        XCTAssertFalse(drive.shows(AssistantWords.nothingYet.said), "a chat that says what it waits for is not empty")
        drive.picture("first-time-an-assistant-is-made")
        // It is made, and no provider is linked yet: the step that follows takes the sentence's place.
        source.set(Setup.needsProvider(Drawing.providers))
        await drive.until("the sentence to go") { !drive.shows(AssistantWords.makingAssistant.said) }
        XCTAssertFalse(drive.shows(AssistantWords.pairing.said))
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
