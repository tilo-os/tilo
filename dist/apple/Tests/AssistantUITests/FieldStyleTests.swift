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
        XCTAssertEqual(paragraph.headIndent, 20)
        XCTAssertEqual(try font(storage, at: entry).pointSize, 15)

        // Code is set in the fixed face on a ground of its own.
        let code = place(of: "billing#retry", in: typed)
        XCTAssertTrue(try font(storage, at: code).isFixedPitch)
        XCTAssertNotNil(storage.attribute(.backgroundColor, at: code, effectiveRange: nil))
        XCTAssertNil(storage.attribute(.backgroundColor, at: place(of: "is live", in: typed), effectiveRange: nil))
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
}
