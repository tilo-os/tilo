import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// The message field's formatting, as the system's text view is given it.
@MainActor
final class FieldStyleTests: XCTestCase {
    private func styled(_ text: String, caret: Int? = nil) -> NSTextStorage {
        let storage = NSTextStorage(string: text)
        FieldStyle.apply(to: storage, selection: caret.map { NSRange(location: $0, length: 0) }, scheme: .light)
        return storage
    }

    private func font(_ storage: NSTextStorage, at index: Int) throws -> NSFont {
        try XCTUnwrap(storage.attribute(.font, at: index, effectiveRange: nil) as? NSFont)
    }

    private func isBold(_ font: NSFont) -> Bool {
        NSFontManager.shared.traits(of: font).contains(.boldFontMask)
    }

    private func place(of words: String, in text: String) -> Int {
        (text as NSString).range(of: words).location
    }

    func testWhatTheDrawingTypesIsFormattedInPlaceAndEveryCharacterKept() throws {
        let typed = Drawing.typed
        let storage = styled(typed)
        XCTAssertEqual(storage.string, typed, "formatting changes how the words look, never the words")

        // "**Tomorrow**": the word is bold at the field's size, its signs are out of sight and take no room.
        let word = place(of: "Tomorrow", in: typed)
        XCTAssertTrue(isBold(try font(storage, at: word)))
        XCTAssertEqual(try font(storage, at: word).pointSize, 15)
        XCTAssertLessThan(try font(storage, at: 0).pointSize, 1)
        XCTAssertEqual(storage.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor, NSColor.clear)
        XCTAssertFalse(isBold(try font(storage, at: place(of: "before", in: typed))))

        // The list's lines hang under their numbers, and the numbers stay in sight.
        let entry = place(of: "1. pull", in: typed)
        let paragraph = try XCTUnwrap(storage.attribute(.paragraphStyle, at: entry, effectiveRange: nil) as? NSParagraphStyle)
        XCTAssertEqual(paragraph.firstLineHeadIndent, 4.5)
        // The drawing's twenty points, or more where the face at hand sets a number wider than the drawing's does.
        XCTAssertGreaterThanOrEqual(paragraph.headIndent, 20)
        XCTAssertLessThan(paragraph.headIndent, 23)
        XCTAssertEqual(try font(storage, at: entry).pointSize, 15)
        // A number is set in figures of one width, as a page sets a list's, so the words follow it closely.
        let figures = try font(storage, at: entry)
        func wide(_ figure: String, _ face: NSFont) -> CGFloat { (figure as NSString).size(withAttributes: [.font: face]).width }
        XCTAssertEqual(wide("1", figures), wide("0", figures), accuracy: 0.01)
        XCTAssertEqual(wide("1", figures), wide("8", figures), accuracy: 0.01)
        let after = storage.attribute(.kern, at: entry + 2, effectiveRange: nil) as? CGFloat ?? 0
        XCTAssertEqual(4.5 + wide("1. ", figures) + after, paragraph.headIndent, accuracy: 0.1, "the words begin where their lines go on under them")

        // Code is set in the fixed face on a ground of its own, which reaches over its two signs.
        let code = place(of: "billing#retry", in: typed)
        XCTAssertTrue(try font(storage, at: code).isFixedPitch)
        XCTAssertEqual(storage.attribute(FieldStyle.ground, at: code, effectiveRange: nil) as? NSColor, Token.raised.platform(.light))
        XCTAssertNotNil(storage.attribute(FieldStyle.ground, at: code - 1, effectiveRange: nil))
        XCTAssertNotNil(storage.attribute(FieldStyle.ground, at: code + "billing#retry".count, effectiveRange: nil))
        XCTAssertNil(storage.attribute(FieldStyle.ground, at: place(of: "is live", in: typed), effectiveRange: nil))
        XCTAssertNil(storage.attribute(FieldStyle.ground, at: place(of: "check", in: typed), effectiveRange: nil))
        // Out of sight, its signs leave the room the drawing pads code with.
        XCTAssertEqual(storage.attribute(.kern, at: code - 1, effectiveRange: nil) as? CGFloat, FieldStyle.groundPad)
        XCTAssertEqual(storage.attribute(.kern, at: code + "billing#retry".count, effectiveRange: nil) as? CGFloat, FieldStyle.groundPad)
    }

    func testMarkdownsSignsShowWhereThePersonWrites() throws {
        let text = "**bold** and `code`"
        let away = styled(text)
        XCTAssertLessThan(try font(away, at: 0).pointSize, 1)
        // The caret in the bold word: its signs show, quietly, and the code's stay out of sight.
        let writing = styled(text, caret: 4)
        XCTAssertEqual(try font(writing, at: 0).pointSize, 15)
        XCTAssertEqual(writing.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor, Token.ink3.platform(.light))
        XCTAssertLessThan(try font(writing, at: place(of: "`", in: text)).pointSize, 1)
    }

    func testEmphasisHeadingsLinksAndStruckWords() throws {
        let text = "# Head\n*slanted* ~~gone~~ [site](https://tilo.sh) **_both_**"
        let storage = styled(text)
        XCTAssertEqual(try font(storage, at: place(of: "Head", in: text)).pointSize, 20)
        XCTAssertTrue(isBold(try font(storage, at: place(of: "Head", in: text))))
        let slanted = place(of: "slanted", in: text)
        let traits = NSFontManager.shared.traits(of: try font(storage, at: slanted))
        XCTAssertTrue(traits.contains(.italicFontMask) || storage.attribute(.obliqueness, at: slanted, effectiveRange: nil) != nil)
        XCTAssertNotNil(storage.attribute(.strikethroughStyle, at: place(of: "gone", in: text), effectiveRange: nil))
        XCTAssertEqual(storage.attribute(.foregroundColor, at: place(of: "site", in: text), effectiveRange: nil) as? NSColor,
                       Token.asst.platform(.light))
        XCTAssertTrue(isBold(try font(storage, at: place(of: "both", in: text))))
    }

    func testTheFieldIsAsTallAsItsLines() {
        let one = FieldStyle.height(of: styled("one"), width: 300, scheme: .light)
        XCTAssertEqual(one, 22, accuracy: 1, "one line of the drawing: 15 points set 1.45 apart")
        XCTAssertEqual(FieldStyle.height(of: styled(""), width: 300, scheme: .light), one, "an empty field is one line tall")
        XCTAssertEqual(FieldStyle.height(of: styled("one\ntwo\nthree"), width: 300, scheme: .light), 3 * one, accuracy: 1)
        let long = String(repeating: "words ", count: 40)
        XCTAssertGreaterThan(FieldStyle.height(of: styled(long), width: 300, scheme: .light), 2 * one)
    }

    func testUnorderedBulletsDrawLiveDotsWhenCaretIsAwayAndRevealMarkerWhenEditing() throws {
        let text = "- **first** item\n* *second* `code` item"
        let away = styled(text)
        XCTAssertEqual(away.string, text, "live bullet styling keeps every character intact")
        // Caret away: dash and asterisk render as crisp bullet dots via FieldStyle.bullet.
        XCTAssertNotNil(away.attribute(FieldStyle.bullet, at: 0, effectiveRange: nil))
        XCTAssertEqual(away.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor, NSColor.clear)
        let secondLine = place(of: "* *second*", in: text)
        XCTAssertNotNil(away.attribute(FieldStyle.bullet, at: secondLine, effectiveRange: nil))
        // Inline bold, italic, and code all style inside bullet items too.
        XCTAssertTrue(isBold(try font(away, at: place(of: "first", in: text))))
        let secondWord = place(of: "second", in: text)
        let traits = NSFontManager.shared.traits(of: try font(away, at: secondWord))
        XCTAssertTrue(traits.contains(.italicFontMask) || away.attribute(.obliqueness, at: secondWord, effectiveRange: nil) != nil)
        XCTAssertTrue(try font(away, at: place(of: "code", in: text)).isFixedPitch)

        // Caret on the first bullet marker: raw dash reappears softly and dot attribute is omitted.
        let editing = styled(text, caret: 0)
        XCTAssertNil(editing.attribute(FieldStyle.bullet, at: 0, effectiveRange: nil))
        XCTAssertEqual(editing.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor, Token.ink3.platform(.light))
        // Second bullet stays styled as a dot.
        XCTAssertNotNil(editing.attribute(FieldStyle.bullet, at: secondLine, effectiveRange: nil))
    }

    func testWrapSelectionFormatsBoldItalicAndInlineCode() {
        let view = FieldTextView(frame: NSRect(x: 0, y: 0, width: 200, height: 40))
        view.string = "hello world"
        view.setSelectedRange(NSRange(location: 6, length: 5))
        view.wrapSelection(with: "**")
        XCTAssertEqual(view.string, "hello **world**")
        XCTAssertEqual(view.selectedRange(), NSRange(location: 8, length: 5))

        view.wrapSelection(with: "*")
        XCTAssertEqual(view.string, "hello ***world***")

        view.string = "check status"
        view.setSelectedRange(NSRange(location: 6, length: 6))
        view.wrapSelection(with: "`")
        XCTAssertEqual(view.string, "check `status`")
    }
}
