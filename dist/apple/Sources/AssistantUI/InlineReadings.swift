import SwiftUI

/// The words of the blocks a bubble drew, read once. A reply that grows is drawn again at
/// every change, five times a second, and all its blocks but the one that grew hold the words
/// they held before: a long answer is more than a hundred blocks, and each of them was read
/// again at every change. What was read is kept by its words, the newest few hundred blocks.
@MainActor
enum InlineReadings {
    private static var kept: [String: AttributedString] = [:]
    private static var order: [String] = []
    /// How many blocks were read anew since the program began: what a test counts a change by.
    private(set) static var read = 0
    /// How many readings are kept at most.
    static let most = 512

    /// A block's words with their inline Markdown read. Nil where they cannot be read.
    static func reading(of words: String) -> AttributedString? {
        if let known = kept[words] { return known }
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        guard let made = try? AttributedString(markdown: words, options: options) else { return nil }
        read += 1
        kept[words] = made
        order.append(words)
        if order.count > most { kept[order.removeFirst()] = nil }
        return made
    }
}
