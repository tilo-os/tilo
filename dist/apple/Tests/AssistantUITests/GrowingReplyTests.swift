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

    // MARK: Where a conversation rests

    /// A conversation where it is shown, drawn by this Mac on no screen.
    private struct Shown {
        let window: HeldWindow
        let drive: Drive
        let source: MemorySource
        let model: AssistantModel
        let size: CGSize
        /// How much of the window's height a keyboard takes: an iPhone's, and what the Mac's smallest window leaves.
        let keyboard: CGFloat
        let end: () -> Void
    }

    /// The newest piece of every conversation here is the assistant's: nothing the person sent
    /// brings the end into view.
    private var settled: MemorySource.Content {
        var content = long
        content.main.append(Item(id: "newest", at: Drawing.now, .message(Message(.assistant, "The newest piece of all."))))
        return content
    }

    /// The Mac's window as its program makes it, or the phone's conversation over its field at
    /// an iPhone's size.
    private func shown(_ layout: String, _ content: MemorySource.Content) async throws -> Shown {
        if layout == "mac" {
            let app = try await Started.app(content)
            return Shown(window: app.window, drive: app.drive, source: app.source.inner, model: app.model, size: CGSize(width: 1280, height: 820), keyboard: 240) { app.end() }
        }
        NSApplication.shared.setActivationPolicy(.prohibited)
        let source = MemorySource(content, now: { Drawing.now })
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
        model.start()
        let size = CGSize(width: 402, height: 874)
        let window = HeldWindow.make(NSRect(origin: .zero, size: size), [.borderless])
        window.contentView = NSHostingView(rootView: AssistantRoot(model: model) {
            VStack(spacing: 0) {
                ConversationView(chat: .main)
                Composer(chat: .main).layoutPriority(1)
            }
            .background(Token.content)
        })
        let drive = Drive(window)
        await drive.until("the source's first word", { model.session != nil && model.conversation(.main).isLoaded })
        await drive.settle(300)
        return Shown(window: window, drive: drive, source: source, model: model, size: size, keyboard: 294) {
            window.contentView = nil
            window.close()
        }
    }

    private func drawn(_ held: Shown) async {
        for _ in 0..<2 {
            await held.drive.settle(150)
            if let view = held.window.contentView, let picture = view.bitmapImageRepForCachingDisplay(in: view.bounds) {
                view.cacheDisplay(in: view.bounds, to: picture)
            }
            await held.drive.settle(60)
        }
    }

    /// The window's height: a keyboard that comes takes the lower part of it and gives it back.
    private func make(_ held: Shown, shorter: Bool) async {
        held.window.setContentSize(NSSize(width: held.size.width, height: held.size.height - (shorter ? held.keyboard : 0)))
        await drawn(held)
    }

    func testAConversationOpensOnItsNewestPiece() async throws {
        for layout in ["mac", "phone"] {
            let held = try await shown(layout, settled)
            defer { held.end() }
            await drawn(held)
            let scroll = try conversationScroll(held.window)
            XCTAssertGreaterThan(place(scroll).above, 100, "\(layout): the conversation is no taller than its window: this shows nothing")
            XCTAssertEqual(place(scroll).below, 0, accuracy: 1, "\(layout): the conversation opened away from its newest piece")
        }
    }

    /// Its pieces may come after its window opened, as they do on a device that has just got in.
    func testAConversationWhosePiecesComeAfterItOpenedRestsOnItsNewestPiece() async throws {
        for layout in ["mac", "phone"] {
            var empty = settled
            let pieces = empty.main
            empty.main = []
            let held = try await shown(layout, empty)
            defer { held.end() }
            await drawn(held)
            for piece in pieces { held.source.post(piece) }
            await drawn(held)
            await drawn(held)
            let scroll = try conversationScroll(held.window)
            XCTAssertGreaterThan(place(scroll).above, 100, "\(layout): the conversation is no taller than its window: this shows nothing")
            XCTAssertEqual(place(scroll).below, 0, accuracy: 1, "\(layout): the pieces came and the conversation rests away from its newest")
        }
    }

    /// An iPhone's keyboard takes the lower part of the window and gives it back; a Mac's window
    /// is made shorter and taller. The newest piece stays in sight through both.
    func testTheNewestPieceStaysInSightWhenTheWindowGrowsShorterAndTallerAgain() async throws {
        for layout in ["mac", "phone"] {
            let held = try await shown(layout, settled)
            defer { held.end() }
            await drawn(held)
            let tall = try conversationScroll(held.window).documentVisibleRect.height
            XCTAssertEqual(place(try conversationScroll(held.window)).below, 0, accuracy: 1, "\(layout): before the window changed")
            for round in 1...2 {
                await make(held, shorter: true)
                let short = try conversationScroll(held.window)
                XCTAssertLessThan(short.documentVisibleRect.height, tall - 200, "\(layout): the window did not grow shorter: this shows nothing")
                XCTAssertEqual(place(short).below, 0, accuracy: 1, "\(layout), time \(round): with the window shorter the newest piece went out of sight")
                await make(held, shorter: false)
                XCTAssertEqual(place(try conversationScroll(held.window)).below, 0, accuracy: 1, "\(layout), time \(round): with the window tall again the conversation rests away from its newest piece")
            }
        }
    }

    /// Scrolled up, a person stays where they put the conversation when the window changes: its
    /// upper edge does not move, and nothing pulls them to the end.
    func testScrolledUpTheViewStaysPutWhenTheWindowGrowsShorterAndTallerAgain() async throws {
        for layout in ["mac", "phone"] {
            let held = try await shown(layout, settled)
            defer { held.end() }
            await drawn(held)
            try wheelUp(try conversationScroll(held.window), by: 300)
            await drawn(held)
            let up = place(try conversationScroll(held.window))
            XCTAssertEqual(up.below, 300, accuracy: 2, "\(layout): the wheel did not scroll the conversation")
            await make(held, shorter: true)
            XCTAssertEqual(place(try conversationScroll(held.window)).above, up.above, accuracy: 1, "\(layout): with the window shorter the view left where the person put it")
            await make(held, shorter: false)
            XCTAssertEqual(place(try conversationScroll(held.window)).above, up.above, accuracy: 1, "\(layout): with the window tall again the view left where the person put it")
        }
    }

    /// In the Mac's window one view shows whichever conversation is chosen. Coming back to one
    /// that was left scrolled up, it shows its newest piece.
    func testAConversationThatIsOpenedAgainShowsItsNewestPiece() async throws {
        let held = try await shown("mac", settled)
        defer { held.end() }
        await drawn(held)
        try wheelUp(try conversationScroll(held.window), by: 300)
        await drawn(held)
        XCTAssertEqual(place(try conversationScroll(held.window)).below, 300, accuracy: 2, "the wheel did not scroll the conversation")
        let other = try XCTUnwrap(settled.subAgents.first?.id, "the drawing has no sub-agent to go to")
        held.model.place = .chat(.subAgent(other))
        await drawn(held)
        held.model.place = .chat(.main)
        await drawn(held)
        let back = try conversationScroll(held.window)
        XCTAssertGreaterThan(place(back).above, 100, "the conversation is no taller than its window: this shows nothing")
        XCTAssertEqual(place(back).below, 0, accuracy: 1, "opened again, the conversation rests away from its newest piece")
    }
}
#endif
