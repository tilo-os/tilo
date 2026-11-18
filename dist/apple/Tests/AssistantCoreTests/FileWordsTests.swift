import AssistantCore
import XCTest

/// What the tag of a file in a message says.
final class FileWordsTests: XCTestCase {
    private static let english = Locale(identifier: "en_US")

    func testAFilesTagSaysItsNameAndItsSizeAsItsSourceGivesThem() {
        let small = Attachment(id: "m1#0", name: "report.pdf", type: "com.adobe.pdf", bytes: 48_213).words(locale: Self.english)
        // 48 213 bytes are 48 kilobytes, as the system writes a file's size.
        XCTAssertTrue(small.hasPrefix("report.pdf · 48"), small)
        XCTAssertTrue(small.hasSuffix("B"), small)
        let large = Attachment(id: "m1#1", name: "clip.mov", type: "com.apple.quicktime-movie", bytes: 1_500_000).words(locale: Self.english)
        XCTAssertEqual(large, "clip.mov · 1.5 MB")
        // A file of no bytes has a size all the same, said as a number.
        let empty = Attachment(id: "m1#2", name: "empty.txt", type: "public.plain-text", bytes: 0).words(locale: Self.english)
        XCTAssertTrue(empty.hasPrefix("empty.txt · 0"), empty)
    }

    func testAFileWhoseSizeNobodySaidIsSaidByItsNameAlone() {
        XCTAssertEqual(Attachment(id: "m2#0", name: "photo.png", type: "public.png").words(locale: Self.english), "photo.png")
        XCTAssertEqual(Attachment(id: "m2#1", name: "odd.bin", type: "public.data", bytes: -1).words(locale: Self.english), "odd.bin")
    }
}
