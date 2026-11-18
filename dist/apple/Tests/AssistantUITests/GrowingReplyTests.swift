#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// A conversation while its newest reply grows, in the Mac's window on no screen: what stays
/// in sight, what stays put, and what says that more came below.
@MainActor
final class GrowingReplyTests: XCTestCase {
    private func reply(_ words: Int) -> Item {
        Item(id: "growing", at: Drawing.now, .message(Message(.assistant, (0..<words).map { "word\($0)" }.joined(separator: " "))))
    }

    /// The drawing's content with enough before it that the conversation is taller than its window.
    private var long: MemorySource.Content {
        var content = Drawing.content()
        let earlier = (0..<30).map { index in
            Item(id: Item.ID("earlier-\(index)"), at: Drawing.at(7, 0).addingTimeInterval(Double(index) * 30),
                 .message(Message(index % 2 == 0 ? .person : .assistant, "Earlier line \(index), long enough to stand as a line of its own.")))
        }
        content.main = earlier + content.main
        return content
    }

    /// The conversation's own scroll view: the one with the tallest content that holds no text field.
    private func conversationScroll(_ window: NSWindow) throws -> NSScrollView {
        var found: [NSScrollView] = []
        func walk(_ view: NSView) {
            if let scroll = view as? NSScrollView { found.append(scroll) }
            for sub in view.subviews { walk(sub) }
        }
        func holdsField(_ view: NSView) -> Bool { view is NSTextView || view.subviews.contains(where: holdsField) }
        if let root = window.contentView { walk(root) }
        return try XCTUnwrap(found.filter { !holdsField($0) }.max { ($0.documentView?.frame.height ?? 0) < ($1.documentView?.frame.height ?? 0) })
    }

    /// How far the conversation's top is above what shows, and how much of it lies under what shows.
    private func place(_ scroll: NSScrollView) -> (above: CGFloat, below: CGFloat) {
        guard let document = scroll.documentView else { return (-1, -1) }
        let visible = scroll.documentVisibleRect
        let tall = document.bounds.height
        return document.isFlipped ? (visible.minY, tall - visible.maxY) : (tall - visible.maxY, visible.minY)
    }

    /// Let the window lay out and draw once, as a window on a screen is drawn again and again.
    private func drawn(_ app: Started) async {
        await app.drive.settle(150)
        if let view = app.window.contentView, let picture = view.bitmapImageRepForCachingDisplay(in: view.bounds) {
            view.cacheDisplay(in: view.bounds, to: picture)
        }
        await app.drive.settle(60)
    }

    private func wheelUp(_ scroll: NSScrollView, by points: Int32) throws {
        let wheel = try XCTUnwrap(CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1, wheel1: points, wheel2: 0, wheel3: 0))
        scroll.scrollWheel(with: try XCTUnwrap(NSEvent(cgEvent: wheel)))
    }

    func testTheEndStaysInSightWhileTheLastPieceGrowsAndNothingMovesWhenTheAnswerEnds() async throws {
        let app = try await Started.app(long)
        defer { app.end() }
        let source = app.source.inner
        source.setAnswering(true)
        source.post(reply(0))
        await drawn(app)
        let scroll = try conversationScroll(app.window)
        XCTAssertGreaterThan(place(scroll).above, 100, "the conversation is no taller than its window: this shows nothing")
        for words in stride(from: 12, through: 120, by: 12) {
            source.change(reply(words))
            await drawn(app)
            XCTAssertEqual(place(scroll).below, 0, accuracy: 1, "with \(words) words the end left the view")
        }
        XCTAssertFalse(app.drive.has(AssistantWords.moreBelow.said), "a mark said more came below while the end was in sight")
        let grown = place(scroll)
        source.setAnswering(false)
        await drawn(app)
        let ended = place(scroll)
        XCTAssertEqual(ended.below, 0, accuracy: 1)
        XCTAssertEqual(ended.above, grown.above, accuracy: 0.5, "what stands above the reply moved when its writing ended")
    }

    func testScrolledUpTheViewStaysPutAndAQuietMarkSaysMoreCameBelow() async throws {
        let app = try await Started.app(long)
        defer { app.end() }
        let drive = app.drive, source = app.source.inner
        source.setAnswering(true)
        source.post(reply(4))
        await drawn(app)
        let scroll = try conversationScroll(app.window)
        // The person turns the wheel: 300 points up.
        try wheelUp(scroll, by: 300)
        await drawn(app)
        let up = place(scroll)
        XCTAssertEqual(up.below, 300, accuracy: 2, "the wheel did not scroll the conversation")
        XCTAssertFalse(drive.has(AssistantWords.moreBelow.said), "a mark stood there before anything came below")
        for words in stride(from: 16, through: 160, by: 16) {
            source.change(reply(words))
            await drawn(app)
            XCTAssertEqual(place(scroll).above, up.above, accuracy: 0.5, "with \(words) words the view left where the person put it")
        }
        XCTAssertGreaterThan(place(scroll).below, 320, "the reply did not grow below")
        // A quiet mark says so, and a press on it brings the end back.
        drive.press(drive.node(AssistantWords.moreBelow.said, role: "AXButton"))
        await drawn(app)
        await drawn(app)
        XCTAssertEqual(place(scroll).below, 0, accuracy: 1, "the mark's press did not bring the end back")
        XCTAssertFalse(drive.has(AssistantWords.moreBelow.said), "the mark stayed with the end in sight")
        // A piece that comes does not pull a person away from what they scrolled up to.
        try wheelUp(scroll, by: 300)
        await drawn(app)
        let again = place(scroll)
        XCTAssertEqual(again.below, 300, accuracy: 2)
        source.setAnswering(false)
        source.post(Item(id: "next", at: Drawing.now, .report("Dima wrote back.")))
        await drawn(app)
        await drawn(app)
        XCTAssertEqual(place(scroll).above, again.above, accuracy: 0.5, "a piece that came pulled the view to the end")
        XCTAssertTrue(drive.has(AssistantWords.moreBelow.said), "nothing said that a piece came below")
    }
}
#endif
