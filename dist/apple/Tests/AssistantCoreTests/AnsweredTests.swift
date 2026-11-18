import AssistantCore
import XCTest

/// What a message answers, as the app holds it. Everything said here is made up.
final class AnsweredTests: XCTestCase {
    func testAnAnsweredPiecesLineIsItsWordsAsOneLineOfPlainText() {
        let whole = Answered("rundown", words: "I will remind you at **10:14**.\n\nNot before.")
        XCTAssertEqual(whole.line, "I will remind you at 10:14. Not before.")
        XCTAssertFalse(whole.isPart)
        XCTAssertEqual(Answered("rundown", words: "at **10:14**", isPart: true).line, "at 10:14")
        // A piece this device does not hold has no words to show.
        XCTAssertEqual(Answered("gone", words: "").line, "")
    }

    func testWhatIsAnsweredAloneIsNothingToSend() {
        var draft = Draft(text: "", answers: Answered("rundown", words: "Nine mails sorted since seven."))
        XCTAssertTrue(draft.isEmpty)
        draft.text = "Which nine?"
        XCTAssertFalse(draft.isEmpty)
        XCTAssertEqual(draft.answers?.piece, "rundown")
    }

    func testAMarkedPartStandsWhereItsWordsStandInThePiecesOwnWords() {
        let words = "Nine mails sorted since seven. None of them **needs** you: all of it can wait."
        // Marked as it stands: the stretch itself.
        XCTAssertEqual(Answered.span(of: "sorted since seven", in: words), 11..<29)
        // Marked across a sign of Markdown's: from its first word to its last.
        let across = Answered.span(of: "them needs you", in: words)
        XCTAssertEqual(String(decoding: Array(words.utf8)[across], as: UTF8.self), "them **needs** you")
        // A sign that is more than one byte before it: the stretch is counted in bytes.
        XCTAssertEqual(Answered.span(of: "wait", in: "Café: wait"), 7..<11)
        // Words that do not stand in it: the words whole.
        XCTAssertEqual(Answered.span(of: "nothing of the kind", in: words), 0..<words.utf8.count)
    }
}
