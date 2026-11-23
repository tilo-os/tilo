#if os(macOS)
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import XCTest

/// The way in of a device whose pairing stopped on Tilo's page. The page's sentence says "copy
/// the technical details below", and the page shows them folded under it; the way in said the
/// sentence and showed nothing below it. It shows them as the page does: folded, opened by a
/// press, with Copy details. Driven with nothing on a screen; the pasteboard is the test's own.
@MainActor
final class WayInDetailsTests: XCTestCase {
    private static let why = "Couldn’t open Tilo. It stopped while joining. Try again. If it stops again, copy the technical details below."
    private static let details = "Cause: loading\nStopped at: join\nError: DataCloneError: The object can not be cloned.\nPage build: test"

    private func wayIn() -> MemorySource.Content {
        var content = MemorySource.Content()
        content.session = .signedOut
        content.pairsAs = Drawing.person
        return content
    }

    func testAWayInWhosePairingStoppedShowsThePagesTechnicalDetailsUnderItsSentenceAndCopiesThem() async throws {
        let started = try await Started.app(wayIn())
        defer { started.end() }
        let board = NSPasteboard.withUniqueName()
        defer { board.releaseGlobally() }
        started.model.pasteboard = board
        await started.drive.until("the way in to stand") { started.drive.has(AssistantWords.pasteLink.said) }
        XCTAssertFalse(started.drive.has(AssistantWords.technicalDetails.said, role: "AXButton"), "a way in with nothing to say offers details")
        started.source.inner.endPairing(saying: Self.why, details: Self.details)
        await started.drive.until("the sentence and its details to stand") {
            started.drive.shows(Self.why) && started.drive.has(AssistantWords.technicalDetails.said, role: "AXButton")
        }
        XCTAssertTrue(started.drive.shows(Self.why), "the way in does not say why the pairing ended")
        guard started.drive.has(AssistantWords.technicalDetails.said, role: "AXButton") else {
            return XCTFail("the way in says 'copy the technical details below' and shows none below it")
        }
        // Folded, as on the page: nothing of them stands before the person's press.
        XCTAssertFalse(started.drive.has(AssistantWords.copyDetails.said, role: "AXButton"))
        started.drive.press(AssistantWords.technicalDetails.said, role: "AXButton")
        await started.drive.until("the details to be laid open") {
            started.drive.shows(Self.details) && started.drive.has(AssistantWords.copyDetails.said, role: "AXButton")
        }
        XCTAssertTrue(started.drive.shows(Self.details), "the details laid open are not the page's")
        started.drive.press(AssistantWords.copyDetails.said, role: "AXButton")
        XCTAssertEqual(board.string(forType: .string), Self.details, "Copy details put something else on the pasteboard")
        // A sentence its page shows no details under has none below it here either.
        started.source.inner.endPairing(saying: "Lounge no longer exists. It was reset since this browser last opened it.")
        await started.drive.until("the details to go") { !started.drive.has(AssistantWords.technicalDetails.said, role: "AXButton") }
        XCTAssertFalse(started.drive.has(AssistantWords.technicalDetails.said, role: "AXButton"), "details of the pairing before stand under another sentence")
        XCTAssertEqual(started.model.session, .signedOut)
    }
}
#endif
