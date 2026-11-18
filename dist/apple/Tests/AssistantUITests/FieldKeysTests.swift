#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// The system's text view as it comes, set up as the message field sets its own up and with the
/// field's plain look, in a window on no screen: what the field's keys are compared with.
@MainActor
private final class PlainField {
    let window: HeldWindow
    let view: NSTextView
    let drive: Drive

    init(size: CGSize, menu: NSMenu?) {
        let scroll = NSScrollView(frame: NSRect(origin: .zero, size: size))
        scroll.drawsBackground = false
        scroll.borderType = .noBorder
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        let view = NSTextView(usingTextLayoutManager: false)
        view.isRichText = false
        view.allowsUndo = true
        view.drawsBackground = false
        view.textContainerInset = .zero
        view.textContainer?.lineFragmentPadding = 0
        view.textContainer?.widthTracksTextView = true
        view.isVerticallyResizable = true
        view.isHorizontallyResizable = false
        view.autoresizingMask = [.width]
        view.minSize = .zero
        view.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        view.isAutomaticQuoteSubstitutionEnabled = false
        view.isAutomaticDashSubstitutionEnabled = false
        view.isAutomaticTextReplacementEnabled = false
        view.typingAttributes = FieldStyle.base(.light)
        scroll.documentView = view
        let window = HeldWindow.make(NSRect(origin: .zero, size: size), [.titled])
        window.contentView = scroll
        view.frame = NSRect(origin: .zero, size: scroll.contentSize)
        window.makeFirstResponder(view)
        self.window = window
        self.view = view
        self.drive = Drive(window, menu: menu)
    }

    /// These words, in the field's plain look, with the caret at their end.
    func set(_ text: String) {
        let length = (text as NSString).length
        view.string = text
        view.textStorage?.setAttributes(FieldStyle.base(.light), range: NSRange(location: 0, length: length))
        view.typingAttributes = FieldStyle.base(.light)
        view.undoManager?.removeAllActions()
        view.setSelectedRange(NSRange(location: length, length: 0))
    }

    func end() {
        window.contentView = nil
        window.close()
    }
}

/// The arrow keys, by the letter and the number the keyboard gives them.
private enum Arrow: String {
    case up = "\u{F700}", down = "\u{F701}", left = "\u{F702}", right = "\u{F703}"

    var code: UInt16 {
        switch self {
        case .up: 126
        case .down: 125
        case .left: 123
        case .right: 124
        }
    }
}

extension Drive {
    /// An arrow key, pressed and let go, as the keyboard sends it.
    fileprivate func arrow(_ arrow: Arrow, _ flags: NSEvent.ModifierFlags = []) {
        key(arrow.rawValue, code: arrow.code, flags.union([.numericPad, .function]))
    }

    /// The field emptied by hand: everything selected and taken away.
    fileprivate func emptyTheField() {
        key("a", [.command])
        key(Key.delete.0, code: Key.delete.1)
    }
}

/// The keys the message field takes for itself, pressed as real key events in the app as its program makes it,
/// nothing on a screen: Up and Down step through what the person sent in the conversation,
/// Escape lets it go, and no key is taken from a letter that is being put together.
@MainActor
final class FieldKeysTests: XCTestCase {
    private static let first = "first ask"
    private static let second = "a second **ask**\nwith a line under it"
    private static let third = "third ask"

    /// The drawing's content with a main chat in which the person wrote three times, an answer last.
    private func content() -> MemorySource.Content {
        var content = Drawing.content()
        func at(_ minute: Int) -> Date { Drawing.now.addingTimeInterval(TimeInterval(minute - 30) * 60) }
        content.main = [
            Item(id: "ask-1", at: at(1), .message(Message(.person, Self.first))),
            Item(id: "answer-1", at: at(2), .message(Message(.assistant, "an answer"))),
            Item(id: "ask-2", at: at(3), .message(Message(.person, Self.second))),
            Item(id: "ask-3", at: at(4), .message(Message(.person, Self.third))),
            Item(id: "answer-3", at: at(5), .message(Message(.assistant, "My model did not answer"))),
        ]
        return content
    }

    private func end(_ words: String) -> NSRange {
        NSRange(location: (words as NSString).length, length: 0)
    }

    /// What the person wrote in the main chat, as the app holds the conversation.
    private func own(_ started: Started) -> [String] {
        started.model.conversation(.main).items.compactMap { item in
            if case .message(let message) = item.content, message.author == .person { message.text } else { nil }
        }
    }

    func testUpInAnEmptyFieldBringsBackWhatThePersonSentAndDownGoesForwardAgain() async throws {
        continueAfterFailure = false
        let started = try await Started.app(content())
        defer { started.end() }
        let drive = started.drive, field = try started.field()
        drive.click(field)
        XCTAssertEqual(field.string, "")

        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.third, "Up in the empty field brings back the last message the person sent here, not the answer after it")
        XCTAssertEqual(field.selectedRange(), end(Self.third), "with the caret at its end")
        await drive.settle()
        XCTAssertEqual(started.model.drafts[.main]?.text, Self.third, "it is the draft, as if it had been typed")
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.second, "Up again brings the one before, its Markdown and its lines as they were sent")
        XCTAssertEqual(field.selectedRange(), end(Self.second))
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.first)
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.first, "nothing was sent before the first: it stays")
        drive.arrow(.down)
        XCTAssertEqual(field.string, Self.second, "Down goes forward again")
        drive.arrow(.down)
        XCTAssertEqual(field.string, Self.third)
        drive.arrow(.down)
        XCTAssertEqual(field.string, "", "past the newest the field is empty again")
        await drive.settle()
        XCTAssertTrue(started.model.drafts[.main]?.isEmpty ?? true, "and so is the draft")
        drive.arrow(.down)
        XCTAssertEqual(field.string, "")
        XCTAssertEqual(started.source.calls("send"), [], "stepping sends nothing")

        // Brought back and sent with Return: it goes out as it stood, once.
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.third)
        drive.key(Drive.Key.returnKey.0, code: Drive.Key.returnKey.1)
        await drive.until("the message to be sent again") { started.source.calls("send").count == 1 }
        XCTAssertEqual(started.source.calls("send"), ["send(\(String(reflecting: Self.third)), files: [], main)"])
        await drive.until("the field to be empty again") { field.string.isEmpty }
        await drive.until("the message in the conversation") { self.own(started).count == 4 }
        // What was sent twice in a row is one step, not two that look alike.
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.third)
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.second, "a message sent again right after itself is one step")
    }

    func testTheSteppingReadsTheConversationItselfWhereverAMessageWasSentFrom() async throws {
        continueAfterFailure = false
        let started = try await Started.app(content())
        defer { started.end() }
        let drive = started.drive
        var field = try started.field()
        drive.click(field)
        // The person sends from another device meanwhile: the conversation is told, this app sent nothing.
        started.source.inner.post(Item(id: "ask-elsewhere", at: Drawing.now, .message(Message(.person, "sent from the phone"))))
        await drive.until("the message from elsewhere") { self.own(started).last == "sent from the phone" }
        drive.arrow(.up)
        XCTAssertEqual(field.string, "sent from the phone", "what the conversation holds comes back, wherever it was sent from")
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.third)
        XCTAssertEqual(started.source.calls("send"), [])

        // A sub-agent's window steps through what the person wrote to that sub-agent, and nothing of the main chat.
        drive.press("Usage drop: investigate, Coder, working", role: "AXButton")
        await drive.until("the sub-agent's window") { started.model.place == .chat(.subAgent("usage-drop")) }
        await drive.settle(300)
        field = try started.field()
        drive.click(field)
        XCTAssertEqual(field.string, "")
        drive.arrow(.up)
        XCTAssertEqual(field.string, "is it only Safari?")
        drive.arrow(.up)
        XCTAssertEqual(field.string, "is it only Safari?", "the job the main agent gave is not the person's: nothing older")
    }

    func testTypingMovingTheCaretOrEscapeLeavesTheSteppingAndTheArrowsAreATextFieldsAgain() async throws {
        continueAfterFailure = false
        let started = try await Started.app(content())
        defer { started.end() }
        let drive = started.drive, field = try started.field()
        drive.click(field)

        // Escape lets what was brought back go: the field is empty as before.
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.third)
        drive.key(Drive.Key.escape.0, code: Drive.Key.escape.1)
        XCTAssertEqual(field.string, "", "Escape leaves the stepping with the field empty as it was")
        drive.arrow(.up)
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.second, "and Up begins at the newest again")

        // Typing makes the words the person's own: the arrows move the caret in them, as in any text field.
        drive.type("!")
        let edited = Self.second + "!"
        XCTAssertEqual(field.string, edited)
        drive.arrow(.up)
        XCTAssertEqual(field.string, edited, "once typed in, Up brings nothing back")
        XCTAssertLessThanOrEqual(field.selectedRange().location, (Self.second as NSString).range(of: "\n").location, "it moves the caret to the line above")
        drive.arrow(.up)
        XCTAssertEqual(field.selectedRange(), NSRange(location: 0, length: 0), "and on the first line to the start")
        drive.arrow(.down)
        drive.arrow(.down)
        XCTAssertEqual(field.selectedRange(), end(edited), "Down on the last line goes to the end")
        drive.arrow(.down)
        XCTAssertEqual(field.string, edited)

        // Emptied by hand, the field steps again. A move of the caret alone leaves the stepping too.
        drive.emptyTheField()
        XCTAssertEqual(field.string, "")
        await drive.settle()
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.third)
        drive.arrow(.left)
        drive.arrow(.right)
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.third, "the caret was moved: Up is the text field's")
        XCTAssertEqual(field.selectedRange(), NSRange(location: 0, length: 0))
        drive.arrow(.down)
        XCTAssertEqual(field.string, Self.third)
        XCTAssertEqual(field.selectedRange(), end(Self.third))

        // Undo takes back what Up brought, and only that.
        drive.emptyTheField()
        await drive.settle()
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.third)
        await drive.settle()
        XCTAssertTrue(drive.key("z", [.command]).hasPrefix("the menu"), "Undo is the menu's line")
        await drive.settle()
        XCTAssertEqual(field.string, "", "Undo takes back what Up brought")
        XCTAssertTrue(started.model.drafts[.main]?.isEmpty ?? true)

        // Shift Return in what was brought back makes a line in it and sends nothing.
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.third)
        drive.key(Drive.Key.returnKey.0, code: Drive.Key.returnKey.1, [.shift])
        XCTAssertEqual(field.string, Self.third + "\n")
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.third + "\n", "the words are the person's own now")
        XCTAssertEqual(started.source.calls("send"), [])
    }

    func testUndoAfterAMessageWasSentTakesNothingBackAndBreaksNothing() async throws {
        let started = try await Started.app(content())
        defer { started.end() }
        let drive = started.drive, field = try started.field()
        drive.click(field)
        drive.type("Hello?")
        await drive.settle()
        drive.key(Drive.Key.returnKey.0, code: Drive.Key.returnKey.1)
        await drive.until("the message to be sent") { started.source.calls("send").count == 1 && field.string.isEmpty }
        await drive.settle(300)
        // What Undo remembered was said of words that went out with the message: nothing of the empty field is taken back.
        drive.key("z", [.command])
        await drive.settle()
        XCTAssertEqual(field.string, "", "Undo after a message was sent takes nothing back")
        XCTAssertTrue(started.model.drafts[.main]?.isEmpty ?? true)
        XCTAssertEqual(started.source.calls("send").count, 1)

        // What the person writes after it is still taken back.
        drive.type("again")
        await drive.settle()
        XCTAssertEqual(field.string, "again")
        drive.key("z", [.command])
        await drive.settle()
        XCTAssertEqual(field.string, "", "Undo takes back what was typed since")

        // The same after a message that Up brought back and Return sent again.
        drive.arrow(.up)
        XCTAssertEqual(field.string, "Hello?")
        drive.key(Drive.Key.returnKey.0, code: Drive.Key.returnKey.1)
        await drive.until("the message to be sent again") { started.source.calls("send").count == 2 && field.string.isEmpty }
        await drive.settle(300)
        drive.key("z", [.command])
        await drive.settle()
        XCTAssertEqual(field.string, "")
        XCTAssertEqual(started.source.calls("send").count, 2)
    }

    func testWhileALetterIsBeingPutTogetherTheFieldTakesNoKeyForItself() async throws {
        let started = try await Started.app(content())
        defer { started.end() }
        let drive = started.drive, field = try started.field()
        drive.click(field)
        let plain = PlainField(size: CGSize(width: field.frame.width, height: 66), menu: started.app.mainMenu)
        defer { plain.end() }
        let both: [(keys: Drive, view: NSTextView)] = [(plain.drive, plain.view), (drive, field)]
        let nowhere = NSRange(location: NSNotFound, length: 0)
        /// What an input method does while a letter is put together: it hands the letter over, marked.
        func compose(_ letter: String) {
            for one in both { one.view.setMarkedText(letter, selectedRange: NSRange(location: 1, length: 0), replacementRange: nowhere) }
        }

        // Return while a letter is being put together is the input method's and the system's: nothing is sent.
        for one in both { one.keys.type("ab") }
        compose("か")
        XCTAssertTrue(field.hasMarkedText())
        for one in both { one.keys.key(Drive.Key.returnKey.0, code: Drive.Key.returnKey.1) }
        await drive.settle(200)
        XCTAssertEqual(started.source.calls("send"), [], "Return while a letter is being put together sends nothing")
        XCTAssertEqual(field.string, plain.view.string, "the key does what it does in any text view")
        XCTAssertEqual(field.hasMarkedText(), plain.view.hasMarkedText())

        // Up in a field that holds only a letter being put together brings nothing back over it.
        for one in both { one.keys.emptyTheField() }
        await drive.settle()
        XCTAssertEqual(field.string, "")
        compose("か")
        for one in both { one.keys.arrow(.up) }
        XCTAssertEqual(field.string, "か")
        XCTAssertEqual(field.string, plain.view.string)
        XCTAssertEqual(field.selectedRange(), plain.view.selectedRange())
        XCTAssertEqual(field.hasMarkedText(), plain.view.hasMarkedText())

        // A letter begun while a sent message stands in the field: Up is not the stepping's.
        for one in both { one.keys.emptyTheField() }
        await drive.settle()
        drive.arrow(.up)
        XCTAssertEqual(field.string, Self.third)
        plain.set(Self.third)
        compose("か")
        for one in both { one.keys.arrow(.up) }
        XCTAssertEqual(field.string, Self.third + "か", "the message before is not brought over the letter")
        XCTAssertEqual(field.string, plain.view.string)
        XCTAssertEqual(field.selectedRange(), plain.view.selectedRange())

        // An accent chosen by a long press takes the letter's place, as the system hands it over.
        for one in both { one.keys.emptyTheField() }
        await drive.settle()
        for one in both { one.keys.type("e") }
        for one in both { one.view.insertText("é", replacementRange: NSRange(location: 0, length: 1)) }
        await drive.settle()
        XCTAssertEqual(field.string, "é")
        XCTAssertEqual(field.string, plain.view.string)
        XCTAssertEqual(started.model.drafts[.main]?.text, "é", "the draft holds the letter with its accent")
        XCTAssertEqual(started.source.calls("send"), [])
    }
}
#endif
