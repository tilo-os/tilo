#if os(macOS)
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import XCTest

/// The way in of a device that was approved and then stopped before it got in. It stood as the
/// plain way in, and its person would have paired a second time. It says that the device was
/// approved and when, says what stopped in Tilo's own sentence with the technical details
/// under it, and its first button goes on as the device that was approved: nothing is paired
/// twice. Pairing afresh stays at hand, also while a try is in work. Driven with nothing on a
/// screen; with TILO_SHOTS_DIR set, the light and the dark look are kept as pictures.
@MainActor
final class WayInApprovedTests: XCTestCase {
    private static let why = "Tilo draws with WebGPU. This browser has it, but offered this page no graphics adapter."
    private static let details = "Cause: unsupported\nStopped at: draw\nPage build: test"

    private func approved() -> MemorySource.Content {
        var content = MemorySource.Content()
        content.session = .signedOut
        content.pairsAs = Drawing.person
        content.joinApproved = Drawing.now.addingTimeInterval(-240)
        content.pairingEnded = Self.why
        content.pairingEndedDetails = Self.details
        return content
    }

    private func pictures(_ started: Started, _ name: String) async {
        for dark in [false, true] {
            started.window.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
            await started.drive.settle(200)
            started.drive.picture("\(name)-\(dark ? "dark" : "light")")
        }
        started.window.appearance = nil
        await started.drive.settle(100)
    }

    func testAWayInAfterAnApprovedJoinThatStoppedSaysSoAndGoesOnAsThatDeviceWhenItsPersonPresses() async throws {
        let started = try await Started.app(approved())
        defer { started.end() }
        await started.drive.until("the way in to stand") { started.drive.has(AssistantWords.pasteLink.said) }
        // Not the plain way in: it says that the device was approved, when, and what stopped.
        XCTAssertTrue(started.drive.shows(AssistantWords.wayInApproved.said), "the way in does not say that this device was approved")
        XCTAssertFalse(started.drive.shows(AssistantWords.wayInLead.said), "the way in promises a first pairing to a device that was approved")
        // `shows` asks whether some element holds these words: the line that says when is there, and says a time.
        XCTAssertTrue(started.drive.shows(AssistantWords.approvedWhen.said(["when": ""])), "the way in does not say when the device was approved")
        XCTAssertFalse(started.drive.has(AssistantWords.approvedWhen.said(["when": ""])), "the way in says no time for the approval")
        XCTAssertNotNil(started.model.joinApproved)
        XCTAssertTrue(started.drive.shows(Self.why), "the way in does not say what stopped")
        XCTAssertTrue(started.drive.has(AssistantWords.technicalDetails.said, role: "AXButton"), "the details are not where the way in keeps them")
        XCTAssertTrue(started.drive.shows(AssistantWords.wayInApprovedHelp.said))
        guard started.drive.has(AssistantWords.tryAgain.said, role: "AXButton") else {
            return XCTFail("the way in of a device that was approved offers no way to go on as that device")
        }
        await pictures(started, "way-in-approved")

        started.drive.press(AssistantWords.tryAgain.said, role: "AXButton")
        await started.drive.until("the device to be on its way in") {
            started.model.session == .pairing && started.drive.shows(AssistantWords.wayInGoesOn.said)
        }
        XCTAssertEqual(started.source.calls("goOn").count, 1, "the press did not go on once")
        XCTAssertEqual(started.source.calls("pair"), [], "going on paired the device a second time")
        XCTAssertTrue(started.drive.shows(AssistantWords.wayInGoesOn.said), "the way in does not say that Tilo is opening on the approved device")
        XCTAssertTrue(started.drive.shows(AssistantWords.pairing.said))
        XCTAssertFalse(started.drive.shows(Self.why), "what stopped the try before is still said while the next is in work")
        XCTAssertFalse(started.drive.has(AssistantWords.tryAgain.said, role: "AXButton"), "Try again stands while the try is in work")
        XCTAssertTrue(started.drive.has(AssistantWords.pasteLink.said), "a device on its way in from an approval cannot be paired afresh")
        await pictures(started, "way-in-goes-on")

        // The page gets in: the device is its person's, and the way in is gone.
        started.source.inner.set(joinApproved: nil)
        started.source.inner.set(.signedIn(Drawing.person))
        await started.drive.until("the frame to stand") { started.drive.has(AssistantWords.newSubAgent.said) && !started.drive.has(AssistantWords.pasteLink.said) }
        XCTAssertFalse(started.drive.has(AssistantWords.pasteLink.said))
        XCTAssertNil(started.model.joinApproved)
    }

    /// The try that goes on stops again: the way in says so again, and offers the same ways on.
    func testATryThatStopsAgainIsSaidAtTheSameWayIn() async throws {
        var content = approved()
        content.session = .pairing
        content.pairingEnded = nil
        content.pairingEndedDetails = nil
        let started = try await Started.app(content)
        defer { started.end() }
        await started.drive.until("the device to be on its way in") { started.drive.shows(AssistantWords.wayInGoesOn.said) }
        XCTAssertFalse(started.drive.has(AssistantWords.tryAgain.said, role: "AXButton"))
        started.source.inner.endPairing(saying: Self.why, details: Self.details)
        await started.drive.until("the way in to say what stopped") {
            started.drive.shows(Self.why) && started.drive.has(AssistantWords.tryAgain.said, role: "AXButton")
        }
        XCTAssertTrue(started.drive.shows(AssistantWords.wayInApproved.said))
        XCTAssertTrue(started.drive.has(AssistantWords.technicalDetails.said, role: "AXButton"))
        XCTAssertTrue(started.drive.has(AssistantWords.pasteLink.said))
        XCTAssertNotNil(started.model.joinApproved, "a try that stopped took back that the device was approved")
    }

    /// A device nobody approved stands at the plain way in, with nothing to go on with.
    func testAPlainWayInOffersNothingToGoOnWith() async throws {
        var content = MemorySource.Content()
        content.session = .signedOut
        content.pairsAs = Drawing.person
        let started = try await Started.app(content)
        defer { started.end() }
        await started.drive.until("the way in to stand") { started.drive.has(AssistantWords.pasteLink.said) }
        XCTAssertTrue(started.drive.shows(AssistantWords.wayInLead.said))
        XCTAssertFalse(started.drive.has(AssistantWords.tryAgain.said, role: "AXButton"), "a plain way in offers to go on as a device nobody approved")
        XCTAssertFalse(started.drive.shows(AssistantWords.wayInApproved.said))
        XCTAssertFalse(started.drive.shows(AssistantWords.wayInApprovedHelp.said))
    }
}
#endif
