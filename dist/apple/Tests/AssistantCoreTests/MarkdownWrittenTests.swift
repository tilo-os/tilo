import AssistantCore
import Foundation
import XCTest

/// A message that is still being written, as a bubble draws it meanwhile.
final class MarkdownWrittenTests: XCTestCase {
    /// One character a bubble draws, and the form it is drawn in.
    private struct Drawn: Equatable {
        let character: Character
        let form: String
    }

    /// Everything a bubble draws of a text, character by character with its form: which kind of
    /// block it stands in, and whether it is strong, emphasised, struck, code or a link.
    private func drawn(_ text: String, whileWritten: Bool) -> [Drawn] {
        var out: [Drawn] = []
        func inline(_ words: String, _ form: String) {
            let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
            guard let read = try? AttributedString(markdown: words, options: options) else {
                out += words.map { Drawn(character: $0, form: form) }
                return
            }
            let looks = NSAttributedString(read)
            looks.enumerateAttributes(in: NSRange(location: 0, length: looks.length)) { attributes, range, _ in
                let intent = (attributes[.inlinePresentationIntent] as? NSNumber)?.uintValue ?? 0
                let look = "\(form) \(intent)\(attributes[NSAttributedString.Key("NSLink")] == nil ? "" : " link")"
                out += (looks.string as NSString).substring(with: range).map { Drawn(character: $0, form: look) }
            }
        }
        func entries(_ all: [String], _ form: String) {
            for (index, entry) in all.enumerated() {
                if index > 0 { out.append(Drawn(character: "\n", form: "entry")) }
                inline(entry, form)
            }
        }
        for (index, block) in Markdown.blocks(in: text, whileWritten: whileWritten).enumerated() {
            if index > 0 { out.append(Drawn(character: "\n", form: "block")) }
            switch block {
            case .paragraph(let words): inline(words, "paragraph")
            case .heading(let level, let words): inline(words, "heading \(level)")
            case .bullets(let all): entries(all, "bullet")
            case .numbered(let first, let all): entries(all, "numbered from \(first)")
            case .code(let code): out += code.map { Drawn(character: $0, form: "code") }
            case .quote(let words): inline(words, "quote")
            }
        }
        return out
    }

    private let answer = """
    The launch is **planned for May** and the `metrics` sheet says *usage* dropped ~~a lot~~ a little.

    ## What to check

    - **Sign-ups**: 412 this week
    - the `billing#retry` job
    - churn, *by plan*

    1. Pull the numbers
    2. Check retries

    > Maya asked for this.

    ```swift
    let retries = 3 * 2
    ```

    Done: 2 * 3 is 6, and snake_case stays.
    """

    /// The rule a person sees: wherever the writing stands, what is drawn is the beginning of
    /// what is drawn a character later. Nothing drawn is taken away, nothing changes its form.
    func testWhatIsDrawnOfAnAnswerOnlyGrowsWhereverItsWritingStands() {
        var before: [Drawn] = []
        var broken: [String] = []
        for end in answer.indices.dropFirst() + [answer.endIndex] {
            let written = String(answer[..<end])
            let now = drawn(written, whileWritten: true)
            if !now.starts(with: before) {
                broken.append("after “\(written.suffix(14).replacingOccurrences(of: "\n", with: "⏎"))”: “\(String(before.suffix(12).map(\.character)))” became “\(String(now.suffix(12).map(\.character)))”")
            }
            before = now
        }
        XCTAssertEqual(broken.count, 0, "what was drawn changed at \(broken.count) places, first: \(broken.prefix(3).joined(separator: "; "))")
        // Written to its end, it is drawn as the finished message is.
        XCTAssertEqual(before, drawn(answer, whileWritten: false))
        XCTAssertEqual(Markdown.whileWritten(answer), answer)
    }

    func testWhatStandsOpenAtTheEndIsClosedAndASignAloneWaits() {
        let read: [(String, String)] = [
            ("so **bo", "so **bo**"), ("so **bold wor ", "so **bold wor** "), ("so **bold*", "so **bold**"),
            ("so **", "so "), ("so *", "so "), ("a `co", "a `co`"), ("a `", "a "), ("gone ~~str", "gone ~~str~~"),
            ("*a **b", "*a **b***"),
            ("- one\n-", "- one"), ("- one\n- ", "- one"), ("- one\n- **", "- one"), ("Steps:\n\n1.", "Steps:\n"),
            ("Intro\n\n#", "Intro\n"), ("Intro\n\n## Pl", "Intro\n\n## Pl"),
            ("see [the sheet](https://exa", "see the sheet"), ("see [the she", "see the she"),
            ("```swift\nlet x = 1 *\n", "```swift\nlet x = 1 *"), ("```\nlet x\n``", "```\nlet x"),
            ("2 * 3", "2 * 3"), ("snake_case and price_", "snake_case and price"),
            ("done **here** and", "done **here** and"), ("all `code` here", "all `code` here"),
        ]
        for (typed, wanted) in read {
            XCTAssertEqual(Markdown.whileWritten(typed), wanted, "“\(typed)”")
        }
    }

    func testAFinishedMessageIsReadAsItStands() {
        for text in ["**open", "a `tick", "- one\n-", "see [the she"] {
            XCTAssertEqual(Markdown.blocks(in: text, whileWritten: false), Markdown.blocks(in: text), text)
        }
    }
}
