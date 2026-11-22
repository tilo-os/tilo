#if os(macOS)
import Foundation
import XCTest

/// A turn of three steps as the person's Tilo keeps it with the reply: what a read of the
/// reply's work gives once the turn's server wrote a listing, a change that went through and
/// the same change refused. The file (`dist/apple/Fixtures/turn_steps_read.json`) is what the
/// native Chat gave a reader of a reply's work, kept as it last read: the app's reader is tested
/// against what a reader was given, never against rows written here.
enum KeptTurn {
    /// The reply's work, as JSON text.
    static func work(file: StaticString = #filePath, line: UInt = #line) throws -> String {
        var repository = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { repository.deleteLastPathComponent() }
        let kept = try Data(contentsOf: repository.appendingPathComponent("dist/apple/Fixtures/turn_steps_read.json"))
        let whole = try XCTUnwrap(try JSONSerialization.jsonObject(with: kept) as? [String: Any], file: file, line: line)
        let read = try XCTUnwrap(whole["read"] as? [String: Any], "the kept turn holds no read", file: file, line: line)
        let text = try JSONSerialization.data(withJSONObject: read, options: [.sortedKeys])
        return try XCTUnwrap(String(data: text, encoding: .utf8), file: file, line: line)
    }
}
#endif
