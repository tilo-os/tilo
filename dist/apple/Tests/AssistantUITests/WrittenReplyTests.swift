#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// A reply while its model writes it, in the Mac's window on no screen.
@MainActor
final class WrittenReplyTests: XCTestCase {
    private func reply(_ words: String) -> Item {
        Item(id: "written", at: Drawing.now, .message(Message(.assistant, words)))
    }

    func testWhileAReplyIsWrittenItsOpenSignsAreDrawnAsTheyWillStand() async throws {
        let app = try await Started.app()
        defer { app.end() }
        let drive = app.drive, source = app.source.inner
        source.setAnswering(true)
        source.post(reply("The deck is **almost"))
        await drive.until("the reply's words without its two stars") { drive.shows("The deck is almost") }
        XCTAssertFalse(drive.shows("**"), "two stars stood in a reply that is being written")
        source.change(reply("The deck is **almost ready** and\n\n-"))
        await drive.until("the next words") { drive.shows("The deck is almost ready and") }
        XCTAssertFalse(drive.tree().contains { $0.value == "-" || $0.label == "-" }, "a list's dash stood alone on its line")
        // Written to its end, a message is read as it stands: its model meant the two stars.
        source.change(reply("Type two stars: **"))
        source.setAnswering(false)
        await drive.until("the finished words as they stand") { drive.shows("Type two stars: **") }
    }
}
#endif
