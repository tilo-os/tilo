import AssistantCore
import AssistantDrawing
import XCTest

final class MarkdownTests: XCTestCase {
    private typealias Mark = Markdown.Mark

    private func marks(_ text: String, _ kind: Mark.Kind) -> [String] {
        let units = Array(text.utf16)
        return Markdown.marks(in: text).filter { $0.kind == kind }.map { String(decoding: units[$0.range], as: UTF16.self) }
    }

    func testStrongWordsAreMarkedAndTheirSignsStayInTheText() {
        let text = "**Tomorrow** before the call"
        XCTAssertEqual(Markdown.marks(in: text), [
            Mark(.syntax, 0..<2), Mark(.strong, 2..<10), Mark(.syntax, 10..<12),
        ])
        XCTAssertEqual(marks("__so__ and *this* and _that_ and ~~gone~~", .strong), ["so"])
        XCTAssertEqual(marks("__so__ and *this* and _that_ and ~~gone~~", .emphasis), ["this", "that"])
        XCTAssertEqual(marks("__so__ and *this* and _that_ and ~~gone~~", .strike), ["gone"])
    }

    func testWhatTheDrawingTypesIsFormattedAsDrawn() {
        let text = Drawing.typed
        XCTAssertEqual(marks(text, .strong), ["Tomorrow"])
        XCTAssertEqual(marks(text, .code), ["billing#retry"])
        XCTAssertEqual(marks(text, .listItem(ordered: true)),
                       ["1. pull the churn numbers", "2. check `billing#retry` is live"])
        XCTAssertEqual(marks(text, .marker), ["1.", "2."])
        // Every character typed is still there: only these are Markdown's own.
        XCTAssertEqual(marks(text, .syntax), ["**", "**", "`", "`"])
    }

    func testASignInsideAWordOrWithoutItsPartnerStaysAsTyped() {
        for text in ["use snake_case_name here", "2 * 3 * 4", "**open", "a ** b ** c", "`open", "price_", "[words] (not a link)"] {
            let found = Markdown.marks(in: text).map(\.kind)
            XCTAssertEqual(found, [], text)
        }
    }

    func testStrongWordsMayHoldEmphasisAndCode() {
        let text = "**all *of* `it`**"
        XCTAssertEqual(marks(text, .strong), ["all *of* `it`"])
        XCTAssertEqual(marks(text, .emphasis), ["of"])
        XCTAssertEqual(marks(text, .code), ["it"])
        // Code keeps its signs to itself.
        XCTAssertEqual(marks("`a*b*c`", .emphasis), [])
        XCTAssertEqual(marks("\\*not\\*", .emphasis), [])
    }

    func testHeadingsListsQuotesAndFences() {
        let text = "## Plan\n- one\n* two\n3) three\n> said\n```\nlet **x** = 1\n```\nafter **it**"
        XCTAssertEqual(marks(text, .heading(2)), ["## Plan"])
        XCTAssertEqual(marks(text, .listItem(ordered: false)), ["- one", "* two"])
        XCTAssertEqual(marks(text, .listItem(ordered: true)), ["3) three"])
        XCTAssertEqual(marks(text, .quote), ["> said"])
        XCTAssertEqual(marks(text, .codeBlock), ["```", "let **x** = 1", "```"])
        // Nothing is formatted inside a fence, and formatting goes on after it.
        XCTAssertEqual(marks(text, .strong), ["it"])
        XCTAssertEqual(marks("#notaheading and 1.5 litres and -dash", .heading(1)), [])
        XCTAssertEqual(Markdown.marks(in: "#notaheading and 1.5 litres and -dash"), [])
    }

    func testLinksAndAddresses() {
        let text = "see [the site](https://tilo.sh/a) or https://tilo.sh/b."
        XCTAssertEqual(marks(text, .link("https://tilo.sh/a")), ["the site"])
        XCTAssertEqual(marks(text, .link("https://tilo.sh/b")), ["https://tilo.sh/b"])
        XCTAssertEqual(marks(text, .syntax), ["[", "](https://tilo.sh/a)"])
    }

    func testRangesCountAsTheSystemsTextViewsDo() {
        // The thumb is two units, so the strong words begin at the sixth.
        XCTAssertEqual(Markdown.marks(in: "👍 **ok**"), [
            Mark(.syntax, 3..<5), Mark(.strong, 5..<7), Mark(.syntax, 7..<9),
        ])
        XCTAssertEqual(Markdown.marks(in: ""), [])
        XCTAssertEqual(Markdown.marks(in: "\n\n"), [])
    }

    func testPlainWordsForANotification() {
        XCTAssertEqual(Markdown.plain("**Maya** asks for `x`,\nsee [this](https://tilo.sh)."), "Maya asks for x, see this.")
        XCTAssertEqual(Markdown.plain("Call Jonas."), "Call Jonas.")
    }

    func testAMessageIsCutIntoBlocks() {
        guard case .message(let rundown) = Drawing.mainChat[0].content else { return XCTFail("the rundown is a message") }
        XCTAssertEqual(Markdown.blocks(in: rundown.text), [
            .paragraph("Good morning. Today:"),
            .bullets(["**10:30** Northwind, with a rundown on the fund 15 minutes before.",
                      "**14:00** Atlas, moved from Wednesday.", "**Open** two things wait for your yes."]),
        ])
        XCTAssertEqual(Markdown.blocks(in: "# Title\n\none\ntwo\n\n3. c\n4. d\n> q\n```\ncode\n  more\n```"), [
            .heading(1, "Title"), .paragraph("one\ntwo"), .numbered(3, ["c", "d"]), .quote("q"), .code("code\n  more"),
        ])
        XCTAssertEqual(Markdown.blocks(in: ""), [])
        XCTAssertEqual(Markdown.blocks(in: "just words"), [.paragraph("just words")])
    }
}

final class ConversationTests: XCTestCase {
    func testTheMainChatIsDrawnInTheDrawingsOrder() {
        var conversation = Conversation()
        XCTAssertFalse(conversation.isLoaded)
        conversation.apply(.all(Drawing.mainChat))
        XCTAssertTrue(conversation.isLoaded)
        let rows = conversation.rows(in: .main, Drawing.wording).map { row -> String in
            switch row {
            case .mark(let words, _): "mark \(words)"
            case .task(let line, let important, _): "task \(line.title)\(important ? " !" : "")"
            case .caption(let words, _): "caption \(words)"
            case .piece(let item, let marked): "piece \(item.id)\(marked ? " !" : "")"
            }
        }
        XCTAssertEqual(rows, [
            "mark Today 7:30", "task Morning rundown", "piece rundown-1", "piece rundown-2",
            "mark Today 9:12", "task Answer investor inbound", "piece dima", "task Reply to Maya at Northwind !", "piece maya",
            "mark Today 9:44", "piece remind-ask", "piece remind-answer",
        ])
        XCTAssertEqual(Set(conversation.rows(in: .main, Drawing.wording).map(\.id)).count, rows.count)
    }

    func testASubAgentsWindowNamesTheMainAgentsWordsAndNeedsNoFirstMark() {
        var conversation = Conversation()
        conversation.apply(.all(Drawing.usageDropChat))
        let rows = conversation.rows(in: .subAgent("usage-drop"), Drawing.wording).compactMap { row -> String? in
            switch row {
            case .mark(let words, _): "mark \(words)"
            case .caption(let words, _): words
            default: nil
            }
        }
        XCTAssertEqual(rows, ["The job, from the main agent", "Main agent, 9:31, while it worked"])
    }

    func testChangesKeepTheConversationCurrent() {
        var conversation = Conversation()
        conversation.apply(.all(Drawing.mainChat))
        let reply = Item(id: "reply", at: Drawing.now, .message(Message(.assistant, "Done.")), isImportant: true)
        conversation.apply(.added([reply]))
        conversation.apply(.added([reply]))
        XCTAssertEqual(conversation.items.count, Drawing.mainChat.count + 1, "a piece told twice stands once")
        guard case .piece(_, let marked) = conversation.rows(in: .main, Drawing.wording).last else { return XCTFail("a piece") }
        XCTAssertTrue(marked, "an important piece under no task carries the mark itself")
        var changed = reply
        changed.isImportant = false
        conversation.apply(.changed(changed))
        XCTAssertEqual(conversation.items.last, changed)
        conversation.apply(.answering(true))
        XCTAssertTrue(conversation.isAnswering)
        conversation.apply(.removed("reply"))
        XCTAssertEqual(conversation.items, Drawing.mainChat)
        conversation.prepend([Item(id: "old", at: Drawing.at(6, 0), .report("Earlier.")), Drawing.mainChat[0]])
        XCTAssertEqual(conversation.items.map(\.id).prefix(2), ["old", "rundown-1"])
        XCTAssertEqual(conversation.items.count, Drawing.mainChat.count + 1)
    }

    func testAFieldHidesMarkdownsSignsButWhereThePersonWrites() {
        let text = "**Tomorrow** and `code`\n## Head"
        let marks = Markdown.marks(in: text)
        func hidden(_ selection: Range<Int>?) -> [String] {
            let units = Array(text.utf16)
            return Markdown.hidden(in: marks, selection: selection).map { String(decoding: units[$0], as: UTF16.self) }
        }
        // Not written in: every sign is out of sight, and still in the text.
        XCTAssertEqual(hidden(nil), ["**", "**", "`", "`", "## "])
        // The caret in the strong word shows its signs and no others.
        XCTAssertEqual(hidden(4..<4), ["`", "`", "## "])
        // Right after the closing signs still counts as in it.
        XCTAssertEqual(hidden(12..<12), ["`", "`", "## "])
        XCTAssertEqual(hidden(14..<14), ["**", "**", "`", "`", "## "])
        XCTAssertEqual(hidden(19..<19), ["**", "**", "## "])
        // On the heading's line its signs show.
        XCTAssertEqual(hidden(30..<30), ["**", "**", "`", "`"])
        // A selection over everything shows everything.
        XCTAssertEqual(hidden(0..<text.utf16.count), [])
    }
}
