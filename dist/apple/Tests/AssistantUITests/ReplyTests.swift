#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// Answering one message of a conversation. Reply in a piece's menu puts that piece above the
/// field, with a cross that drops it; the message then sent says which piece it answers, and the
/// conversation shows that line above its words, where a press goes to the answered piece. The
/// app is driven as its program makes it, in a window that is never on a screen; the menu's own
/// row is asked for in PieceMenuTests. Everything said here is made up.
@MainActor
final class ReplyTests: XCTestCase {
    private let said = "Nine mails sorted since seven. None of them needs you."
    private var line: String { "\(AssistantWords.replyingTo.said): \(said)" }

    private func piece(saying words: String, in model: AssistantModel, file: StaticString = #filePath, line: UInt = #line) throws -> (Item, Message) {
        for item in model.conversation(.main).items {
            if case .message(let message) = item.content, message.text == words { return (item, message) }
        }
        XCTFail("no message says “\(words)”", file: file, line: line)
        throw CancellationError()
    }

    /// The elements read out as exactly `words`: by their name, or by what a line of words holds.
    private func read(_ words: String, in drive: Drive) -> [Node] {
        drive.tree().filter { $0.label == words || $0.value == words }
    }

    /// Whether the bubble that says `words` stands inside the window: scrolled away, it stands outside it.
    private func inView(_ words: String, _ started: Started) -> Bool {
        started.drive.tree().contains { node in
            (node.label.contains(words) || node.value.contains(words))
                && !node.label.contains(AssistantWords.replyingTo.said) && !node.value.contains(AssistantWords.replyingTo.said)
                && started.window.frame.contains(CGPoint(x: node.frame.midX, y: node.frame.midY))
        }
    }

    private func says(_ words: String, in model: AssistantModel) -> Bool {
        model.conversation(.main).items.contains { item in
            if case .message(let message) = item.content { return message.text == words }
            return false
        }
    }

    func testReplyPutsAPieceAboveTheFieldAndTheSentMessageSaysWhichPieceItAnswers() async throws {
        // A conversation longer than its window: the answered piece is scrolled away once the reply stands at its foot.
        var content = Drawing.content()
        content.main += (1...40).map { Item(id: Item.ID("more-\($0)"), at: Drawing.now.addingTimeInterval(Double($0 - 60)), .message(Message(.assistant, "More was said, line \($0)."))) }
        let started = try await Started.app(content)
        defer { started.end() }
        let drive = started.drive, model = started.model
        let (answered, _) = try piece(saying: said, in: model)
        XCTAssertFalse(drive.has(AssistantWords.dropReply.said, role: "AXButton"), "nothing is answered before the person says so")
        await drawn(started)
        let tall = try conversationScroll(started.window).documentVisibleRect.height
        XCTAssertEqual(below(try conversationScroll(started.window)), 0, accuracy: 1, "the conversation opened away from its end")
        XCTAssertEqual(read(line, in: drive).count, 0)

        model.take(.reply, of: PieceWords(of: answered.id, said))
        await drive.until("the answered piece above the field") { drive.has(AssistantWords.dropReply.said, role: "AXButton") }
        XCTAssertEqual(read(line, in: drive).count, 1, drive.said())
        XCTAssertEqual(drive.nodes(line, role: "AXButton").count, 0, "above the field the line is nothing to press")
        XCTAssertEqual(model.drafts[.main]?.answers, Answered(answered.id, words: said))
        // Answering alone sends nothing: there are no words yet.
        XCTAssertEqual(started.source.calls("send"), [])
        // The field is taller by the answered line: the conversation gives way and its end comes back above the field.
        await drawn(started)
        let short = try conversationScroll(started.window)
        XCTAssertLessThan(short.documentVisibleRect.height, tall - 10, "the field did not grow by the answered line: this shows nothing")
        XCTAssertEqual(below(short), 0, accuracy: 1, "after Reply the conversation's end lies under the grown field")
        drive.picture("mac-reply-field-light")

        // The person's words go with which piece they answer.
        model.drafts[.main]?.text = "Which nine?"
        model.send(in: .main)
        await drive.until("the reply to reach the source") { started.source.calls("send").count == 1 }
        await drive.until("the reply in the conversation") { self.says("Which nine?", in: model) }
        let (_, reply) = try piece(saying: "Which nine?", in: model)
        XCTAssertEqual(reply.author, .person)
        XCTAssertEqual(reply.answers, Answered(answered.id, words: said))
        await drive.settle(300)
        // Above the field nothing is answered any more. Above the reply's words stands the line, to be pressed.
        XCTAssertNil(model.drafts[.main]?.answers)
        XCTAssertFalse(drive.has(AssistantWords.dropReply.said, role: "AXButton"), drive.said())
        XCTAssertEqual(drive.nodes(line, role: "AXButton").count, 1, drive.said())
        let above = try XCTUnwrap(drive.nodes(line, role: "AXButton").first)
        drive.picture("mac-reply-sent-light")
        started.window.appearance = NSAppearance(named: .darkAqua)
        await drive.settle(300)
        drive.picture("mac-reply-sent-dark")

        // A press on the line goes to the answered piece: scrolled away while the reply stands at the foot, it comes into view.
        XCTAssertTrue(inView("Which nine?", started), "the reply stands in view")
        XCTAssertFalse(inView(said, started), "the answered piece is scrolled away before the press")
        drive.press(above)
        await drive.until("the answered piece in view") { self.inView(self.said, started) }
        XCTAssertNil(model.shownItem, "the conversation went there and keeps nothing of the ask")
    }

    func testTheCrossDropsWhatIsAnsweredAndKeepsTheWords() async throws {
        let started = try await Started.app()
        defer { started.end() }
        let drive = started.drive, model = started.model
        let (answered, _) = try piece(saying: said, in: model)
        model.take(.reply, of: PieceWords(of: answered.id, said))
        model.drafts[.main]?.text = "Which nine?"
        await drive.until("the answered piece above the field") { drive.has(AssistantWords.dropReply.said, role: "AXButton") }
        drive.press(AssistantWords.dropReply.said, role: "AXButton")
        await drive.until("nothing is answered") { model.drafts[.main]?.answers == nil }
        await drive.settle(200)
        XCTAssertEqual(model.drafts[.main]?.text, "Which nine?")
        XCTAssertEqual(read(line, in: drive).count, 0, drive.said())
        // Sent now, the words answer no message in particular.
        model.send(in: .main)
        await drive.until("the message in the conversation") { self.says("Which nine?", in: model) }
        let (_, sent) = try piece(saying: "Which nine?", in: model)
        XCTAssertNil(sent.answers)
    }

    private func field(in view: NSView?) -> NSTextView? {
        guard let view else { return nil }
        if let found = view as? FieldTextView { return found }
        for sub in view.subviews { if let found = field(in: sub) { return found } }
        return nil
    }

    /// The conversation and the field alone at an iPhone's width, as the phone lays them out. A
    /// piece whose words are longer than a phone is wide: its line is cut before its cross.
    func testAtAPhonesWidthALongAnsweredLineIsCutBeforeItsCrossAndStandsAboveTheField() async throws {
        NSApplication.shared.setActivationPolicy(.prohibited)
        if let faces = ProcessInfo.processInfo.environment["TILO_FONTS_DIR"] { Typeface.register(in: URL(fileURLWithPath: faces)) }
        let model = AssistantModel(source: MemorySource(Drawing.content(), now: { Drawing.now }), snapshots: cacheOfTheCheck())
        model.start()
        let window = HeldWindow.make(NSRect(origin: .zero, size: CGSize(width: 402, height: 874)), [.borderless])
        window.contentView = NSHostingView(rootView: AssistantRoot(model: model) {
            VStack(spacing: 0) {
                ConversationView(chat: .main)
                Composer(chat: .main).layoutPriority(1)
            }
            .background(Token.content)
        })
        defer {
            window.contentView = nil
            window.close()
        }
        let drive = Drive(window)
        await drive.until("the source's first word", { model.session != nil && model.conversation(.main).isLoaded })
        await drive.settle(300)
        // The drawing's first piece: a message with a list in it, far longer than one line of a phone.
        let rundown = Drawing.mainChat[0]
        guard case .message(let message) = rundown.content else { return XCTFail("the rundown is a message") }
        model.take(.reply, of: PieceWords(of: rundown.id, message.text))
        await drive.until("the answered piece above the field") { drive.has(AssistantWords.dropReply.said, role: "AXButton") }
        await drive.settle(300)
        let whole = "\(AssistantWords.replyingTo.said): \(Answered(rundown.id, words: message.text).line)"
        XCTAssertGreaterThan(whole.count, 120, "the line is longer than a phone is wide")
        let shown = try XCTUnwrap(read(whole, in: drive).first, drive.said())
        let cross = try XCTUnwrap(drive.node(AssistantWords.dropReply.said, role: "AXButton"), drive.said())
        let words = try XCTUnwrap(field(in: window.contentView), "the window has no message field")
        let fieldTop = window.convertToScreen(words.convert(words.bounds, to: nil)).maxY
        // On a screen's measure the origin is at the foot.
        XCTAssertGreaterThanOrEqual(shown.frame.minY, fieldTop - 1, "the line stands above the field's words")
        XCTAssertLessThanOrEqual(shown.frame.maxX, cross.frame.minX, "the line ends before its cross")
        XCTAssertGreaterThanOrEqual(shown.frame.minX, window.frame.minX, "the line begins inside the window")
        XCTAssertLessThanOrEqual(cross.frame.maxX, window.frame.maxX, "the cross stands inside the window")
        XCTAssertLessThan(shown.frame.height, 24, "the line is one line")
        drive.picture("phone-width-reply-field-light")
        window.appearance = NSAppearance(named: .darkAqua)
        await drive.settle(300)
        drive.picture("phone-width-reply-field-dark")
    }

    private func textView(in view: NSView?) -> NSTextView? {
        guard let view else { return nil }
        if let found = view as? NSTextView { return found }
        for sub in view.subviews { if let found = textView(in: sub) { return found } }
        return nil
    }

    func testQuoteAnswersTheMarkedPartOfAPieceAndTheSentMessageSaysWhichWords() async throws {
        let started = try await Started.app()
        defer { started.end() }
        let drive = started.drive, model = started.model
        let (answered, _) = try piece(saying: said, in: model)
        // Quote in the piece's menu lays its words out and says what to do with them.
        model.take(.quote, of: PieceWords(of: answered.id, said))
        await drive.until("the words laid out") { started.window.heldSheets.count == 1 }
        await drive.settle(300)
        let sheet = try XCTUnwrap(started.window.heldSheets.first)
        XCTAssertTrue(drive.has(AssistantWords.selectToQuote.said, role: "AXHeading", in: sheet.contentView), drive.said(of: sheet.contentView))
        // With nothing marked there is nothing to quote.
        let quote = try XCTUnwrap(drive.node(AssistantWords.quote.said, role: "AXButton", in: sheet.contentView), drive.said(of: sheet.contentView))
        XCTAssertFalse(quote.isEnabled, "Quote is on with nothing marked")
        let text = try XCTUnwrap(textView(in: sheet.contentView), "the sheet has no text view")
        let part = "sorted since seven"
        let marked = (text.string as NSString).range(of: part)
        sheet.makeFirstResponder(text)
        text.setSelectedRange(marked)
        await drive.settle(200)
        // Over the marked words the text's own menu offers Quote first: asked for as AppKit asks, never opened.
        let place = sheet.convertPoint(fromScreen: CGPoint(x: text.firstRect(forCharacterRange: marked, actualRange: nil).midX,
                                                           y: text.firstRect(forCharacterRange: marked, actualRange: nil).midY))
        let event = try XCTUnwrap(NSEvent.mouseEvent(with: .rightMouseDown, location: place, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                                                     windowNumber: sheet.windowNumber, context: nil, eventNumber: 1, clickCount: 1, pressure: 1))
        XCTAssertEqual(text.menu(for: event)?.items.first?.title, AssistantWords.quote.said)
        XCTAssertEqual((text.string as NSString).substring(with: text.selectedRange()), part, "asking for the menu left the marked words marked")
        drive.picture("mac-quote-sheet-light", of: sheet.contentView)
        // Quote: the marked words stand above the field, the sheet is put away, nothing is sent.
        drive.press(AssistantWords.quote.said, role: "AXButton", in: sheet.contentView)
        await drive.until("the sheet to be put away") { started.window.heldSheets.isEmpty }
        let quoted = Answered(answered.id, words: part, isPart: true, span: 11..<29)
        XCTAssertEqual(model.drafts[.main]?.answers, quoted)
        await drive.until("the marked words above the field") { !self.read("\(AssistantWords.replyingTo.said): \(part)", in: drive).isEmpty }
        XCTAssertEqual(started.source.calls("send"), [])
        await drawn(started)
        drive.picture("mac-quote-field-light")
        model.drafts[.main]?.text = "Since when exactly?"
        model.send(in: .main)
        await drive.until("the reply in the conversation") { self.says("Since when exactly?", in: model) }
        let (_, reply) = try piece(saying: "Since when exactly?", in: model)
        XCTAssertEqual(reply.answers, quoted)
        await drive.settle(300)
        XCTAssertEqual(drive.nodes("\(AssistantWords.replyingTo.said): \(part)", role: "AXButton").count, 1, drive.said())
        drive.picture("mac-quote-sent-light")

        // The same in the dark look: the sent reply, the words laid out with a part marked, that part above the field.
        let dark = NSAppearance(named: .darkAqua)
        started.window.appearance = dark
        await drive.settle(300)
        drive.picture("mac-quote-sent-dark")
        model.take(.quote, of: PieceWords(of: answered.id, said))
        await drive.until("the words laid out again") { started.window.heldSheets.count == 1 }
        let again = try XCTUnwrap(started.window.heldSheets.first)
        again.appearance = dark
        await drive.settle(300)
        let laid = try XCTUnwrap(textView(in: again.contentView), "the sheet has no text view")
        again.makeFirstResponder(laid)
        laid.setSelectedRange((laid.string as NSString).range(of: part))
        await drive.settle(200)
        drive.picture("mac-quote-sheet-dark", of: again.contentView)
        drive.press(AssistantWords.quote.said, role: "AXButton", in: again.contentView)
        await drive.until("the sheet to be put away") { started.window.heldSheets.isEmpty }
        await drive.until("the marked words above the field") { model.drafts[.main]?.answers == quoted && drive.has(AssistantWords.dropReply.said, role: "AXButton") }
        await drawn(started)
        drive.picture("mac-quote-field-dark")
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

    /// How much of the conversation lies under what shows of it.
    private func below(_ scroll: NSScrollView) -> CGFloat {
        guard let document = scroll.documentView else { return -1 }
        let visible = scroll.documentVisibleRect
        return document.isFlipped ? document.bounds.height - visible.maxY : visible.minY
    }

    /// Let the window lay out and draw once, as a window on a screen is drawn again and again.
    private func drawn(_ app: Started) async {
        await app.drive.settle(150)
        if let view = app.window.contentView, let picture = view.bitmapImageRepForCachingDisplay(in: view.bounds) {
            view.cacheDisplay(in: view.bounds, to: picture)
        }
        await app.drive.settle(60)
    }
}
#endif
