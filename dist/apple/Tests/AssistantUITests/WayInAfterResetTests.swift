#if os(macOS)
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import XCTest

/// The window of an app that was told that the workspace it was let into is gone: its way in,
/// with one sentence that says why, nothing of the frame, and the person's unsent words kept
/// for when they are back. Driven with nothing on a screen.
@MainActor
final class WayInAfterResetTests: XCTestCase {
    func testAWindowWhoseWorkspaceIsGoneShowsItsWayInSaysWhyAndKeepsWhatWasTyped() async throws {
        let started = try await Started.app()
        defer { started.end() }
        XCTAssertFalse(started.drive.has(AssistantWords.pasteLink.said), "a window that is signed in showed the way in")
        XCTAssertTrue(started.drive.has(AssistantWords.newSubAgent.said))
        // The person stands in a sub-agent's conversation, and has words in the main chat's field.
        let agent = try XCTUnwrap(started.model.subAgents.first)
        started.model.open(.subAgent(agent.id))
        started.model.drafts[.main] = Draft(text: "Remind me at nine")
        started.source.inner.set(.reset)
        await started.drive.until("the way in to stand") { started.model.session == .reset && started.drive.has(AssistantWords.pasteLink.said) }
        XCTAssertTrue(started.drive.shows(AssistantWords.wayInReset.said), "the way in does not say why it stands there")
        XCTAssertFalse(started.drive.shows(AssistantWords.wayInLead.said), "the way in promises what a reset took away")
        XCTAssertFalse(started.drive.has(AssistantWords.newSubAgent.said), "the frame of the workspace that is gone still stands")
        XCTAssertFalse(started.drive.shows(AssistantWords.notReachable.said))
        XCTAssertNil(started.model.person)
        XCTAssertEqual(started.model.place, .chat(.main))
        XCTAssertEqual(started.model.drafts[.main]?.text, "Remind me at nine")
        // Paired again, by the person's own press where Tilo asks: the frame stands, and the words are in the field.
        started.model.pair(with: "https://tilo.example/#/w/1f0e?pair=abc")
        await started.drive.until("Tilo's question, or the device in") {
            if case .asked = started.model.session { true } else { started.model.person != nil }
        }
        if case .asked(let ask) = started.model.session { started.drive.press(ask.go, role: "AXButton") }
        await started.drive.until("the device to be in again") { started.model.person == Drawing.person && started.drive.has(AssistantWords.newSubAgent.said) }
        XCTAssertFalse(started.drive.has(AssistantWords.pasteLink.said))
        XCTAssertEqual(started.model.drafts[.main]?.text, "Remind me at nine")
        XCTAssertEqual(try started.field().string, "Remind me at nine")
    }

    func testAFirstStartsWayInSaysNothingOfAReset() async throws {
        let started = try await Started.app(MemorySource.Content())
        defer { started.end() }
        await started.drive.until("the way in to stand") { started.drive.has(AssistantWords.pasteLink.said) }
        XCTAssertTrue(started.drive.shows(AssistantWords.wayInLead.said))
        XCTAssertFalse(started.drive.shows(AssistantWords.wayInReset.said))
    }
}
#endif
