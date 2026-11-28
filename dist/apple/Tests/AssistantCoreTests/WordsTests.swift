import AssistantCore
import XCTest

final class WordsTests: XCTestCase {
    private var package: URL {
        URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    }

    func testEverySentenceIsSaidWithItsValuesPutIn() {
        XCTAssertEqual(AssistantWords.replyMany.said(["count": "2"]), "2 replies")
        XCTAssertEqual(AssistantWords.counted(1, one: .readyOne, many: .readyMany), "1 ready for review")
        XCTAssertEqual(AssistantWords.counted(2, one: .readyOne, many: .readyMany), "2 ready for review")
        XCTAssertEqual(AssistantWords.getFromManager.said(["manager": "Bitwarden"]), "Get it from Bitwarden")
        XCTAssertFalse(AssistantWords.quit.said.contains("{"))
        for words in AssistantWords.allCases {
            XCTAssertFalse(words.english.isEmpty, words.rawValue)
            XCTAssertEqual(words.english.filter { $0 == "{" }.count, words.english.filter { $0 == "}" }.count, words.rawValue)
            XCTAssertFalse(words.english.hasSuffix(" ") && words != .addConnectionDraft, words.rawValue)
        }
    }

    /// The people and firms of the drawing are its examples. Nothing the app is made of knows them.
    func testNothingInTheAppIsAboutOnePersonsLineOfWork() throws {
        let examples = ["investor", "fundrais", "raising", "northwind", "law firm", "lawyer", "thesis", "maya", "dima",
                        "mento", "jonas", "linkedin", "bitwarden", "github"]
        let sources = package.appendingPathComponent("Sources")
        var read = 0
        for target in ["AssistantCore"] {
            let folder = sources.appendingPathComponent(target)
            for name in try FileManager.default.contentsOfDirectory(atPath: folder.path) where name.hasSuffix(".swift") {
                let text = try String(contentsOf: folder.appendingPathComponent(name), encoding: .utf8).lowercased()
                read += 1
                for example in examples {
                    XCTAssertNil(text.range(of: "\\b\(example)", options: .regularExpression), "\(target)/\(name) names “\(example)”")
                }
            }
        }
        XCTAssertGreaterThan(read, 8)
    }
}
