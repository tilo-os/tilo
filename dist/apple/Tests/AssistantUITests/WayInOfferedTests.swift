#if os(macOS)
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import XCTest

/// The window of an app whose Tilo answers and does not let it in: where it would say that
/// Tilo cannot be reached it says so in one sentence and offers the way in. Nothing happens
/// until the person presses; the press does what "Sign out of this device" does. Driven with
/// nothing on a screen.
@MainActor
final class WayInOfferedTests: XCTestCase {
    func testAWindowWhoseTiloAnswersAndDoesNotLetItInOffersTheWayInAndGoesThereWhenItsPersonPresses() async throws {
        let started = try await Started.app()
        defer { started.end() }
        XCTAssertFalse(started.drive.has(AssistantWords.signOutToPair.said, role: "AXButton"), "a window that is in offers its way in")
        started.source.inner.set(.unreachable)
        started.source.inner.set(joinStops: true)
        await started.drive.until("the offer to stand") {
            started.drive.shows(AssistantWords.joinStops.said) && started.drive.has(AssistantWords.signOutToPair.said, role: "AXButton")
        }
        guard started.drive.has(AssistantWords.signOutToPair.said, role: "AXButton") else {
            return XCTFail("the window offers no way in where its Tilo answers and does not let it in")
        }
        XCTAssertFalse(started.drive.shows(AssistantWords.notReachable.said), "the window says that Tilo answers and that it cannot be reached")
        // Offered, and nothing more: the device is its person's and its frame stands.
        XCTAssertEqual(started.model.person, Drawing.person)
        XCTAssertTrue(started.drive.has(AssistantWords.newSubAgent.said))
        XCTAssertFalse(started.drive.has(AssistantWords.pasteLink.said), "the way in stood before its person pressed")
        started.drive.press(AssistantWords.signOutToPair.said, role: "AXButton")
        await started.drive.until("the way in to stand") { started.model.session == .signedOut && started.drive.has(AssistantWords.pasteLink.said) }
        XCTAssertFalse(started.drive.shows(AssistantWords.joinStops.said), "the way in still says that the device is not let in")
        XCTAssertNil(started.model.person)
    }

    func testAWindowWhoseTiloDoesNotAnswerOffersNoWayIn() async throws {
        let started = try await Started.app()
        defer { started.end() }
        started.source.inner.set(.unreachable)
        await started.drive.until("the window to say that Tilo cannot be reached") { started.drive.shows(AssistantWords.notReachable.said) }
        XCTAssertFalse(started.drive.has(AssistantWords.signOutToPair.said, role: "AXButton"), "a window whose Tilo does not answer offers its way in")
        XCTAssertFalse(started.drive.shows(AssistantWords.joinStops.said))
    }

    /// A device that is let in again by itself keeps everything: the offer goes, and nothing was given up.
    func testTheOfferGoesWhenTheDeviceIsLetInAgain() async throws {
        let started = try await Started.app()
        defer { started.end() }
        started.source.inner.set(.unreachable)
        started.source.inner.set(joinStops: true)
        await started.drive.until("the offer to stand") { started.drive.has(AssistantWords.signOutToPair.said, role: "AXButton") }
        started.source.inner.set(joinStops: false)
        started.source.inner.set(.reachable)
        await started.drive.until("the offer to go") { !started.drive.has(AssistantWords.signOutToPair.said, role: "AXButton") }
        XCTAssertFalse(started.drive.shows(AssistantWords.joinStops.said))
        XCTAssertEqual(started.model.person, Drawing.person)
        XCTAssertFalse(started.drive.has(AssistantWords.pasteLink.said))
    }
}
#endif
