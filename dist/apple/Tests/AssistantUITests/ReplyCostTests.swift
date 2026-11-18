#if os(macOS)
import AppKit
import AssistantCore
@testable import AssistantUI
import SwiftUI
import XCTest

/// What a change of a growing reply costs its bubble: how many of its blocks are read again.
@MainActor
final class ReplyCostTests: XCTestCase {
    /// A long answer: forty paragraphs and forty lists of three entries, each with its own words.
    private func answer(blocks: Int) -> String {
        (0..<blocks).map { index in
            "Paragraph \(index) says that **usage** went up by \(index + 3) percent and `sheet-\(index)` has the rest."
                + "\n\n- sign-ups of week \(index)\n- churned in week \(index)\n- open in week \(index)"
        }.joined(separator: "\n\n")
    }

    /// Draw a bubble's Markdown as the conversation does, on no screen, and answer how many blocks were read for it.
    private func readings(drawing text: String) -> Int {
        let before = InlineReadings.read
        let view = NSHostingView(rootView: MarkdownText(text: text).frame(width: 472))
        view.layoutSubtreeIfNeeded()
        _ = view.fittingSize
        return InlineReadings.read - before
    }

    func testAChangeOfALongReplyReadsOnlyTheBlockThatGrew() {
        let long = answer(blocks: 40)
        XCTAssertGreaterThanOrEqual(readings(drawing: long), 160, "the first drawing reads every block: 40 paragraphs and 120 entries")
        // Its model writes on: four more words in its last entry.
        XCTAssertEqual(readings(drawing: long + " and four more words"), 1, "a change read more than the one block that grew")
        // A new paragraph begins: one more block.
        XCTAssertEqual(readings(drawing: long + " and four more words\n\nThen"), 1)
        // Drawn again as it stands, nothing is read.
        XCTAssertEqual(readings(drawing: long + " and four more words\n\nThen"), 0)
    }

    func testWhatIsKeptIsBoundedAndABlockIsStillDrawnWhenItsReadingWasLetGo() {
        for index in 0..<(InlineReadings.most + 40) {
            XCTAssertNotNil(InlineReadings.reading(of: "one of many blocks, number \(index)"))
        }
        // The oldest were let go: read again, they are read anew and say the same.
        let before = InlineReadings.read
        let again = InlineReadings.reading(of: "one of many blocks, number 0")
        XCTAssertEqual(InlineReadings.read, before + 1)
        XCTAssertEqual(again.map { String($0.characters) }, "one of many blocks, number 0")
        XCTAssertEqual(InlineReadings.reading(of: "a **strong** word").map { String($0.characters) }, "a strong word")
    }
}
#endif
