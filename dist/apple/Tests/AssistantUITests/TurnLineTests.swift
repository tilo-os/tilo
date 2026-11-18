#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// The one line that says what a turn does, in the Mac's window on no screen: the newest step
/// in the place of the one before, how long, how many; its steps only when its person opens them.
@MainActor
final class TurnLineTests: XCTestCase {
    private func words(_ step: Int) -> String { "Reads part \(step) of the settings" }

    private func steps(_ count: Int) -> [Message.Turn.Step] {
        (1...count).map { .init(id: "s\($0)", words: words($0), took: $0 == 7 ? nil : 2, outcome: $0 == 7 ? .failed : .done) }
    }

    /// A turn that began two hours ago and is at its `count`th step.
    private func turn(at count: Int) -> Message.Turn {
        Message.Turn(now: .step(words(count)), began: Date().addingTimeInterval(-7200), steps: steps(count))
    }

    private func reply(_ turn: Message.Turn?, saying said: String = "") -> Item {
        Item(id: "at-work", at: Drawing.now, .message(Message(.assistant, said, turn: turn)))
    }

    /// The window's picture in the light look and in the dark one, where a folder keeps pictures.
    private func pictures(_ name: String, _ app: Started) async {
        guard ProcessInfo.processInfo.environment["TILO_SHOTS_DIR"] != nil else { return }
        app.drive.picture(name)
        app.window.appearance = NSAppearance(named: .darkAqua)
        await app.drive.settle()
        app.drive.picture("\(name)-dark")
        app.window.appearance = nil
        await app.drive.settle()
    }

    /// Let the window lay out and draw once, as a window on a screen is drawn again and again.
    private func drawn(_ app: Started) async {
        await app.drive.settle(150)
        if let view = app.window.contentView, let picture = view.bitmapImageRepForCachingDisplay(in: view.bounds) {
            view.cacheDisplay(in: view.bounds, to: picture)
        }
        await app.drive.settle(60)
    }

    /// Every scroll view of the window that holds no text field.
    private func scrolls(_ window: NSWindow) -> [NSScrollView] {
        var found: [NSScrollView] = []
        func walk(_ view: NSView) {
            if let scroll = view as? NSScrollView { found.append(scroll) }
            for sub in view.subviews { walk(sub) }
        }
        func holdsField(_ view: NSView) -> Bool { view is NSTextView || view.subviews.contains(where: holdsField) }
        if let root = window.contentView { walk(root) }
        return found.filter { !holdsField($0) }
    }

    /// The conversation's own scroll view: the one with the tallest content that holds no text
    /// field and stands in no other scroll view.
    private func conversationScroll(_ window: NSWindow) throws -> NSScrollView {
        try XCTUnwrap(scrolls(window).filter { $0.enclosingScrollView == nil }
            .max { ($0.documentView?.frame.height ?? 0) < ($1.documentView?.frame.height ?? 0) })
    }

    /// The part the opened steps scroll in: a scroll view that stands in the conversation's.
    private func stepsScroll(in conversation: NSScrollView, _ window: NSWindow) -> NSScrollView? {
        scrolls(window).first { $0 !== conversation && $0.isDescendant(of: conversation) }
    }

    /// What shows of a scroll view, in the screen's measure.
    private func shown(_ scroll: NSScrollView) -> CGRect {
        scroll.window?.convertToScreen(scroll.convert(scroll.bounds, to: nil)) ?? .zero
    }

    /// The middle of what says `words` in the conversation's column, in the screen's measure.
    private func middle(saying words: String, under sight: CGRect, _ drive: Drive) -> CGPoint? {
        drive.tree().first {
            ($0.label.contains(words) || $0.value.contains(words)) && $0.frame.midX > sight.minX && $0.frame.midX < sight.maxX
        }.map { CGPoint(x: $0.frame.midX, y: $0.frame.midY) }
    }

    /// How tall the conversation is, and how much of it lies under what shows.
    private func place(_ scroll: NSScrollView) -> (tall: CGFloat, below: CGFloat) {
        guard let document = scroll.documentView else { return (-1, -1) }
        let visible = scroll.documentVisibleRect
        let tall = document.bounds.height
        return (tall, document.isFlipped ? tall - visible.maxY : visible.minY)
    }

    private func counted(_ steps: Int) -> String {
        AssistantWords.counted(steps, one: .stepOne, many: .stepMany)
    }

    func testOneLineSaysTheNewestStepHowLongAndHowManyAndNeverAList() async throws {
        let app = try await Started.app()
        defer { app.end() }
        let drive = app.drive, source = app.source.inner
        source.setAnswering(true)
        source.post(reply(turn(at: 1)))
        await drive.until("the line says the first step") { drive.shows("\(self.words(1))…") }
        XCTAssertTrue(drive.shows(counted(1)), drive.said())
        XCTAssertTrue(drive.shows("2:00:"), "the line does not say how long the turn has gone on:\n\(drive.said())")
        await pictures("turn-line-step-1", app)
        source.change(reply(turn(at: 10)))
        await drive.until("the tenth step took the first one's place") { drive.shows("\(self.words(10))…") }
        XCTAssertTrue(drive.shows(counted(10)), drive.said())
        for older in [1, 9] { XCTAssertFalse(drive.shows(words(older)), "step \(older) still stands: a list grew") }
        await pictures("turn-line-step-10", app)
        source.change(reply(turn(at: 30)))
        await drive.until("the thirtieth step took the tenth one's place") { drive.shows("\(self.words(30))…") }
        XCTAssertTrue(drive.shows(counted(30)), drive.said())
        for older in [1, 10, 29] { XCTAssertFalse(drive.shows(words(older)), "step \(older) still stands: a list grew") }
        XCTAssertFalse(drive.shows("\(AssistantWords.answering.said)…"), "the line says only that an answer is written")
        await pictures("turn-line-step-30", app)
    }

    func testAPressOpensTheStepsSoFarEachOneLineAndAPressClosesThem() async throws {
        let app = try await Started.app()
        defer { app.end() }
        let drive = app.drive, source = app.source.inner
        source.setAnswering(true)
        source.post(reply(turn(at: 30)))
        let count = counted(30)
        await drive.until("the line stands") { drive.shows(count) }
        XCTAssertFalse(drive.shows(words(7)), "the steps stand open unasked")
        await drawn(app)
        let scroll = try conversationScroll(app.window)
        let closed = place(scroll)
        XCTAssertEqual(closed.below, 0, accuracy: 1, "the conversation does not rest on its end, where the line stands")
        drive.press(drive.tree().first { $0.label.contains(count) })
        await drive.until("the steps so far stand over the line") { drive.shows(self.words(7)) }
        await drive.settle(500)
        await drawn(app)
        await drawn(app)
        let said = drive.tree().map { "\($0.label) \($0.value)" }
        for step in [1, 7, 29] {
            XCTAssertEqual(said.filter { $0.contains(words(step)) }.count, 1, "step \(step) is not one line:\n\(drive.said())")
        }
        XCTAssertTrue(said.contains { $0.contains(words(7)) && $0.contains(AssistantWords.stepFailed.said) },
                      "the step that failed does not say so:\n\(drive.said())")
        XCTAssertTrue(said.contains { $0.contains(words(3)) && $0.contains(TurnStepList.said(2)) },
                      "a step does not say how long it took:\n\(drive.said())")
        XCTAssertFalse(said.contains { $0.contains(words(3)) && $0.contains(AssistantWords.stepFailed.said) },
                       "a step that went through says it failed")
        // The steps stand over the line, in a part that takes no more than a third of what shows of
        // the conversation, and the line keeps its place at the conversation's end, under the hand
        // that pressed it.
        let opened = place(scroll)
        XCTAssertGreaterThan(opened.tall - closed.tall, 100, "thirty steps over the line made the conversation no taller")
        XCTAssertLessThanOrEqual(opened.tall - closed.tall, shown(scroll).height / 3 + 8,
                                 "thirty opened steps take more than a third of what shows of the conversation")
        XCTAssertEqual(opened.below, 0, accuracy: 1, "the line left the foot, where it was pressed")
        await pictures("turn-line-steps-open", app)
        drive.press(drive.tree().first { $0.label.contains(count) })
        await drive.until("the steps are closed again") { !drive.shows(self.words(7)) }
        await drive.settle(500)
        await drawn(app)
        await drawn(app)
        XCTAssertEqual(place(scroll).tall, closed.tall, accuracy: 1, "the closed steps still make the conversation taller")
        XCTAssertEqual(place(scroll).below, 0, accuracy: 1, "the line left the foot when its steps closed")
    }

    func testThinkingShowsItsNewestWordsAndTheLineGivesWayWhileTheReplysWordsCome() async throws {
        let app = try await Started.app()
        defer { app.end() }
        let drive = app.drive, source = app.source.inner
        let began = Date().addingTimeInterval(-30)
        source.setAnswering(true)
        // Nothing says what the turn does: the line says that an answer is written, as it always did.
        source.post(reply(nil))
        await drive.until("the line says an answer is written") { drive.shows("\(AssistantWords.answering.said)…") }
        source.change(reply(Message.Turn(now: .thinking(""), began: began)))
        await drive.until("the line says the model thinks") { drive.shows("\(AssistantWords.thinking.said)…") }
        source.change(reply(Message.Turn(now: .thinking("weighing the two mail servers"), began: began)))
        await drive.until("the line shows the thought's newest words") { drive.shows("weighing the two mail servers…") }
        await pictures("turn-line-thinking", app)
        source.change(reply(Message.Turn(now: .step(nil), began: began)))
        await drive.until("a step without words of its own is said as work") { drive.shows("\(AssistantWords.atWork.said)…") }
        source.change(reply(Message.Turn(now: .writing, began: began, steps: steps(3)), saying: "Your mail account is set up."))
        await drive.until("the reply's words stand") { drive.shows("Your mail account is set up.") }
        await drive.settle()
        for gone in [AssistantWords.atWork.said, AssistantWords.thinking.said, AssistantWords.answering.said] {
            XCTAssertFalse(drive.shows("\(gone)…"), "the line still speaks while the reply's words come:\n\(drive.said())")
        }
        XCTAssertFalse(drive.shows(counted(3)), "the line still counts while the reply's words come:\n\(drive.said())")
        // A step after the first words: the line speaks again.
        source.change(reply(Message.Turn(now: .step("Add an account: Mail"), began: began, steps: steps(4)), saying: "Your mail account is set up."))
        await drive.until("the line speaks again at the next step") { drive.shows("Add an account: Mail…") }
        XCTAssertTrue(drive.shows(counted(4)), drive.said())
    }

    func testOpenedStepsScrollInAPartOfTheirOwnAndWhatStoodAboveTheLineStaysInSight() async throws {
        let app = try await Started.app()
        defer { app.end() }
        let drive = app.drive, source = app.source.inner
        let asked = "Sort my mail into folders by who wrote it"
        source.post(Item(id: "asked", at: Drawing.now, .message(Message(.person, asked))))
        source.setAnswering(true)
        // As many steps as the owner's own turn had taken when they asked what it was doing.
        source.post(reply(turn(at: 58)))
        let count = counted(58)
        await drive.until("the line stands") { drive.shows(count) }
        await drawn(app)
        let scroll = try conversationScroll(app.window)
        let closed = place(scroll)
        let sight = shown(scroll)
        let before = try XCTUnwrap(middle(saying: asked, under: sight, drive), "the person's message is not said:\n\(drive.said())")
        XCTAssertTrue(sight.contains(before), "the person's message is out of sight before anything is opened")
        drive.press(drive.tree().first { $0.label.contains(count) })
        await drive.until("the steps so far stand over the line") { drive.shows(self.words(7)) }
        await drive.settle(500)
        await drawn(app)
        await drawn(app)
        // No step is left out.
        let said = drive.tree().map { "\($0.label) \($0.value)" }
        for step in 1...57 {
            XCTAssertEqual(said.filter { $0.contains(words(step)) }.count, 1, "step \(step) is not one line")
        }
        // What stood above the line stays where a person reads it, and the line stays under the hand.
        let opened = place(scroll)
        let after = try XCTUnwrap(middle(saying: asked, under: sight, drive), "the person's message is not said any more")
        XCTAssertTrue(sight.contains(after), "the opened steps pushed the person's message out of sight: it stands at \(after), what shows is \(sight)")
        XCTAssertLessThanOrEqual(opened.tall - closed.tall, sight.height / 3 + 8,
                                 "the opened steps take more than a third of what shows of the conversation")
        XCTAssertEqual(opened.below, 0, accuracy: 1, "the line left the foot, where it was pressed")
        // The steps stand in a part of their own, which scrolls and rests on its newest step.
        let part = try XCTUnwrap(stepsScroll(in: scroll, app.window), "the opened steps stand in no part of their own that scrolls")
        let rest = place(part)
        XCTAssertGreaterThan(rest.tall, part.frame.height + 300, "the part holds less than its fifty-eight steps")
        XCTAssertEqual(rest.below, 0, accuracy: 1, "the part does not rest on its newest step")
        let newest = try XCTUnwrap(middle(saying: words(57), under: sight, drive))
        let first = try XCTUnwrap(middle(saying: words(1), under: sight, drive))
        XCTAssertTrue(shown(part).contains(newest), "the newest steps are not in sight: \(newest) in \(shown(part))")
        XCTAssertFalse(shown(part).contains(first), "the first step shows though the part rests on its newest")
        await pictures("turn-line-steps-open-58", app)
        // The line is still what stands at its own centre: a press on it closes the steps.
        drive.press(drive.tree().first { $0.label.contains(count) })
        await drive.until("the steps are closed again") { !drive.shows(self.words(7)) }
    }

    func testAFewOpenedStepsTakeOnlyTheHeightOfTheirLinesAndNothingScrolls() async throws {
        let app = try await Started.app()
        defer { app.end() }
        let drive = app.drive, source = app.source.inner
        source.setAnswering(true)
        source.post(reply(turn(at: 3)))
        let count = counted(3)
        await drive.until("the line stands") { drive.shows(count) }
        await drawn(app)
        let scroll = try conversationScroll(app.window)
        let closed = place(scroll)
        drive.press(drive.tree().first { $0.label.contains(count) })
        await drive.until("the steps so far stand over the line") { drive.shows(self.words(2)) }
        await drive.settle(500)
        await drawn(app)
        await drawn(app)
        let opened = place(scroll)
        XCTAssertGreaterThan(opened.tall - closed.tall, 30, "three steps over the line made the conversation no taller")
        XCTAssertLessThan(opened.tall - closed.tall, 90, "three steps stand taller than their three lines")
        let part = try XCTUnwrap(stepsScroll(in: scroll, app.window), "the opened steps stand in no part of their own that scrolls")
        XCTAssertEqual(place(part).tall, part.frame.height, accuracy: 1, "three steps scroll though they need no more than their own height")
    }
}
#endif
