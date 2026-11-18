import AssistantCore
import AssistantDrawing
import XCTest

/// The reply a turn leaves when what its person wrote next takes the turn along: it has no
/// words and nothing with it, and the conversation draws nothing for it.
final class ReplyLeftTests: XCTestCase {
    private let first = Item(id: "first", at: Drawing.at(9, 40), .message(Message(.person, "Draft a note to Maya about Friday.")))
    private let left = Item(id: "left", at: Drawing.at(9, 40), .message(Message(.assistant, "")))
    private let second = Item(id: "second", at: Drawing.at(9, 41), .message(Message(.person, "And say that I bring the numbers.")))
    private let answer = Item(id: "answer", at: Drawing.at(9, 42), .message(Message(.assistant, "The note is drafted, with the numbers.")))

    private func rows(_ items: [Item], in chat: Chat = .main) -> [String] {
        var conversation = Conversation()
        conversation.apply(.all(items))
        return conversation.rows(in: chat, Drawing.wording).map(\.id)
    }

    func testAReplyWithoutWordsHasNoRow() {
        XCTAssertEqual(rows([first, left, second, answer]), ["mark-first", "piece-first", "piece-second", "piece-answer"],
                       "their second message follows their first, and the one answer follows it")
    }

    func testNoMarkNoLineAndNoCaptionStandForItAlone() {
        // It came after a pause: the mark of that time stands before the next piece that is drawn.
        var late = left
        late.at = Drawing.at(10, 30)
        var after = second
        after.at = Drawing.at(10, 31)
        XCTAssertEqual(rows([first, late, after]), ["mark-first", "piece-first", "mark-second", "piece-second"])
        // Under a task of its own, no line names the task over nothing.
        var tasked = left
        tasked.task = TaskLine(task: "note", title: "Note to Maya")
        XCTAssertEqual(rows([first, tasked, second]), ["mark-first", "piece-first", "piece-second"])
        // In a sub-agent's window the main agent's empty word has no caption, and the piece
        // after it is the window's first: it needs no mark.
        let word = Item(id: "word", at: Drawing.at(9, 40), .message(Message(.mainAgent(.instruction), " \n")))
        XCTAssertEqual(rows([word, answer], in: .subAgent("usage-drop")), ["piece-answer"])
    }

    func testAReplyThatCarriesSomethingKeepsItsRow() {
        let file = Item(id: "file", at: Drawing.at(9, 42),
                        .message(Message(.assistant, "", attachments: [Attachment(id: "note", name: "note.pdf", type: "com.adobe.pdf")])))
        let thing = Item(id: "thing", at: Drawing.at(9, 42),
                         .message(Message(.assistant, "", live: [LiveThing(id: "areas", title: "Tasks", reference: "view:areas:main")])))
        let fault = Item(id: "fault", at: Drawing.at(9, 42), .message(Message(.assistant, "", keyFault: .noProvider)))
        XCTAssertEqual(rows([first, file, thing, fault]), ["mark-first", "piece-first", "piece-file", "piece-thing", "piece-fault"])
    }

    func testWhatCameSinceThePersonLeftStandsBeforeAPieceThatIsDrawn() {
        var late = left
        late.at = Drawing.at(9, 50)
        var after = answer
        after.at = Drawing.at(9, 51)
        var conversation = Conversation()
        conversation.apply(.all([first, late, after]))
        XCTAssertEqual(conversation.first(since: Drawing.at(9, 45)), "answer")
    }
}
