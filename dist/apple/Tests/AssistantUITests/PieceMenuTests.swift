#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// The menu of a piece of words in a conversation. Copy puts the piece's words on the pasteboard
/// as plain text with every line where it stood; Select text lays the same words out for a
/// person to mark a part of them and copy it. The menu is asked for as AppKit asks for one and
/// never opened: nothing of it stands on a screen. No test reads or writes the person's own
/// pasteboard: the model is handed one of the test's own. Not checked here, because it writes
/// that pasteboard: the system's own Copy of a marked part. Everything said here is made up.
@MainActor
final class PieceMenuTests: XCTestCase {
    private let plain = """
        Good morning. Today:
        • 10:30 Northwind, with a rundown on the fund 15 minutes before.
        • 14:00 Atlas, moved from Wednesday.
        • Open two things wait for your yes.
        """

    /// The drawing's first piece, a message with a list in it.
    private func rundown(file: StaticString = #filePath, line: UInt = #line) throws -> PieceWords {
        let piece = Drawing.mainChat[0]
        guard case .message(let message) = piece.content else {
            XCTFail("the rundown is a message", file: file, line: line)
            throw CancellationError()
        }
        return PieceWords(of: piece.id, message.text)
    }

    private func ownBoard() -> NSPasteboard {
        let board = NSPasteboard(name: NSPasteboard.Name("so.tilo.assistant.tests.\(UUID().uuidString)"))
        board.clearContents()
        return board
    }

    private func textView(in view: NSView?) -> NSTextView? {
        guard let view else { return nil }
        if let found = view as? NSTextView { return found }
        for sub in view.subviews { if let found = textView(in: sub) { return found } }
        return nil
    }

    /// A press of the mouse at a place of the window, as the system would hand it over.
    private func press(_ type: NSEvent.EventType, _ flags: NSEvent.ModifierFlags, at point: CGPoint, in window: NSWindow) throws -> NSEvent {
        try XCTUnwrap(NSEvent.mouseEvent(with: type, location: point, modifierFlags: flags, timestamp: ProcessInfo.processInfo.systemUptime,
                                         windowNumber: window.windowNumber, context: nil, eventNumber: 1, clickCount: 1, pressure: 1))
    }

    /// Make `event` the one the app is handling now, as when it took it from its queue. It is sent nowhere.
    private func handling(_ event: NSEvent) {
        NSApp.postEvent(event, atStart: true)
        _ = NSApp.nextEvent(matching: [.rightMouseDown, .leftMouseDown], until: nil, inMode: .default, dequeue: true)
    }

    /// The menu AppKit would open for `event` over `view`: asked for as AppKit asks, from the view
    /// upwards, and never opened.
    private func menu(over view: NSView?, for event: NSEvent) -> NSMenu? {
        var seen = view
        while let held = seen {
            if let menu = held.menu(for: event) { return menu }
            seen = held.superview
        }
        return nil
    }

    func testTheSecondButtonAndControlWithAClickOpenAPiecesMenuAndAPlainPressReachesItsWords() async throws {
        let started = try await Started.app()
        defer { started.end() }
        let drive = started.drive, window = started.window
        let top = try XCTUnwrap(window.contentView?.superview ?? window.contentView)
        // A message of the assistant's, a report, and the person's own message. A message's words
        // can be marked where they stand; a report's cannot.
        for (words, marks) in [("Nine mails sorted since seven.", true), ("Dima at Mento reached out.", false), ("remind me in 30 minutes to call Jonas", true)] {
            let node = try XCTUnwrap(drive.tree().first { $0.label.contains(words) || $0.value.contains(words) }, "nothing says “\(words)”")
            let point = window.convertPoint(fromScreen: CGPoint(x: node.frame.midX, y: node.frame.midY))
            var taker: NSView?
            for (type, flags) in [(NSEvent.EventType.rightMouseDown, NSEvent.ModifierFlags()), (.leftMouseDown, .control)] {
                let event = try press(type, flags, at: point, in: window)
                handling(event)
                taker = top.hitTest(point)
                let menu = try XCTUnwrap(menu(over: taker, for: event), "nothing would open over “\(words)”")
                XCTAssertEqual(menu.items.map(\.title), ["Copy", "Select text"], "over “\(words)”")
            }
            guard marks else { continue }
            // A plain press goes past what took the second button, to the words: a drag marks them where they stand.
            handling(try press(.leftMouseDown, [], at: point, in: window))
            let plain = top.hitTest(point)
            XCTAssertNotNil(plain, "a plain press over “\(words)” reaches nothing")
            XCTAssertFalse(plain === taker, "a plain press over “\(words)” is taken by the menu's own view")
        }
    }

    func testTheMenusRowsCopyThePieceAndLayItsWordsOut() async throws {
        let started = try await Started.app()
        defer { started.end() }
        let drive = started.drive, window = started.window
        let board = ownBoard()
        defer { board.releaseGlobally() }
        started.model.pasteboard = board
        let general = NSPasteboard.general.changeCount
        let top = try XCTUnwrap(window.contentView?.superview ?? window.contentView)
        // The drawing's first piece: a message with a list in it.
        let node = try XCTUnwrap(drive.tree().first { $0.label.contains("Good morning. Today:") || $0.value.contains("Good morning. Today:") })
        let point = window.convertPoint(fromScreen: CGPoint(x: node.frame.midX, y: node.frame.midY))
        let event = try press(.rightMouseDown, [], at: point, in: window)
        handling(event)
        let menu = try XCTUnwrap(menu(over: top.hitTest(point), for: event))
        XCTAssertEqual(menu.items.map(\.title), [AssistantWords.copy.said, AssistantWords.selectText.said])
        // Copy: the whole piece, as plain text with its lines.
        menu.performActionForItem(at: 0)
        XCTAssertEqual(board.string(forType: .string), plain)
        XCTAssertNil(started.model.selecting)
        // Select text: the same words, laid out.
        menu.performActionForItem(at: 1)
        await drive.until("the words laid out") { window.heldSheets.count == 1 }
        XCTAssertEqual(started.model.selecting?.plain, plain)
        let sheet = try XCTUnwrap(window.heldSheets.first)
        drive.key(Drive.Key.escape.0, code: Drive.Key.escape.1, in: sheet)
        await drive.until("the sheet to be put away") { window.heldSheets.isEmpty }
        XCTAssertEqual(NSPasteboard.general.changeCount, general, "the person's own pasteboard was not written")
    }

    func testTheMenuOffersCopyThenSelectText() {
        XCTAssertEqual(PieceAction.allCases, [.copy, .selectText])
        XCTAssertEqual(PieceAction.allCases.map(\.words.english), ["Copy", "Select text"])
    }

    func testCopyPutsAPiecesPlainWordsOnThePasteboardWithEveryLine() throws {
        let model = AssistantModel(source: MemorySource(MemorySource.Content()))
        let board = ownBoard()
        defer { board.releaseGlobally() }
        model.pasteboard = board
        let general = NSPasteboard.general.changeCount
        model.take(.copy, of: try rundown())
        XCTAssertEqual(board.string(forType: .string), plain)
        XCTAssertNil(model.selecting, "Copy lays nothing out")
        // Copied again, the pasteboard holds the newer words alone.
        model.take(.copy, of: PieceWords(of: "remind-answer", "I will remind you at **10:14**."))
        XCTAssertEqual(board.string(forType: .string), "I will remind you at 10:14.")
        XCTAssertEqual(NSPasteboard.general.changeCount, general, "the person's own pasteboard was not written")
    }

    func testSelectTextLaysTheWordsOutToBeMarkedAndCopiedAndChangesNothing() async throws {
        let started = try await Started.app()
        defer { started.end() }
        let drive = started.drive
        let words = try rundown()
        let general = NSPasteboard.general.changeCount
        let calls = started.source.calls
        started.model.take(.selectText, of: words)
        await drive.until("the words laid out") { started.window.heldSheets.count == 1 }
        await drive.settle(300)
        let sheet = try XCTUnwrap(started.window.heldSheets.first)
        XCTAssertTrue(drive.has(AssistantWords.selectText.said, role: "AXHeading", in: sheet.contentView), drive.said(of: sheet.contentView))
        XCTAssertTrue(drive.has(AssistantWords.close.said, role: "AXButton", in: sheet.contentView), drive.said(of: sheet.contentView))
        let text = try XCTUnwrap(textView(in: sheet.contentView), "the sheet has no text view")
        XCTAssertEqual(text.string, plain, "the words stand as plain text, every line where it stood")
        XCTAssertTrue(text.isSelectable, "the words can be marked")
        XCTAssertFalse(text.isEditable, "and not changed")
        drive.picture("mac-select-text-light", of: sheet.contentView)

        // A part can be marked: the text view holds it as its marked words.
        let part = "14:00 Atlas, moved from Wednesday."
        let marked = (text.string as NSString).range(of: part)
        text.setSelectedRange(marked)
        XCTAssertEqual(text.selectedRange(), marked)
        XCTAssertEqual((text.string as NSString).substring(with: text.selectedRange()), part)
        // Typing changes nothing.
        sheet.makeFirstResponder(text)
        drive.type("x", in: sheet)
        await drive.settle()
        XCTAssertEqual(text.string, plain)

        let dark = NSAppearance(named: .darkAqua)
        started.window.appearance = dark
        sheet.appearance = dark
        await drive.settle(300)
        drive.picture("mac-select-text-dark", of: sheet.contentView)

        // Escape puts the words away. Nothing was sent, nothing was asked of the source, nothing is kept.
        drive.key(Drive.Key.escape.0, code: Drive.Key.escape.1, in: sheet)
        await drive.until("the sheet to be put away") { started.window.heldSheets.isEmpty }
        XCTAssertNil(started.model.selecting)
        XCTAssertEqual(started.source.calls, calls)
        XCTAssertEqual(NSPasteboard.general.changeCount, general, "the person's own pasteboard was not written")
    }
}
#endif
