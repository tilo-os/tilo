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
}
