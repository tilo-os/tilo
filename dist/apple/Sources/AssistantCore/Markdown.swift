import Foundation

/// Markdown as the message field and the conversation read it. Two readings of the same words:
/// `marks` says where each kind of formatting lies, so a field can show the words formatted in
/// place while every character the person typed stays; `blocks` cuts a finished message into
/// paragraphs, lists, headings and code for a bubble.
public enum Markdown {
    /// One stretch of formatting. Ranges count UTF-16 units, as the system's text views do.
    public struct Mark: Equatable, Sendable {
        public var range: Range<Int>
        public var kind: Kind

        public init(_ kind: Kind, _ range: Range<Int>) {
            self.kind = kind
            self.range = range
        }

        public enum Kind: Equatable, Sendable {
            case strong
            case emphasis
            case strike
            case code
            /// The link's words, or an address written out.
            case link(String)
            /// A whole line that is a heading of this level.
            case heading(Int)
            /// A whole line that is one entry of a list.
            case listItem(ordered: Bool)
            /// A whole line that is quoted.
            case quote
            /// A whole line inside a fence of code, the fence's own lines included.
            case codeBlock
            /// Markdown's own characters around formatted words. They stay in the text; a field
            /// shows them only where the person is writing.
            case syntax
            /// A list's number or dash, a quote's sign, a fence: always shown, quietly.
            case marker
        }
    }

    /// Where the formatting lies in what was typed.
    public static func marks(in text: String) -> [Mark] {
        var marks: [Mark] = []
        let units = Array(text.utf16)
        var fenced = false
        var start = 0
        while start <= units.count {
            var end = start
            while end < units.count, units[end] != newline { end += 1 }
            line(units, start..<end, fenced: &fenced, into: &marks)
            start = end + 1
        }
        return marks.sorted { ($0.range.lowerBound, $0.range.upperBound) < ($1.range.lowerBound, $1.range.upperBound) }
    }

    /// The words without Markdown's own characters, on one line: for a notification or a label.
    public static func plain(_ text: String) -> String {
        let units = Array(text.utf16)
        var hidden = IndexSet()
        for mark in marks(in: text) {
            // A list's dash and a quote's sign are for the eye; a list's number says something and stays.
            if mark.kind == .syntax || (mark.kind == .marker && mark.range.count == 1) {
                hidden.insert(integersIn: mark.range)
            }
        }
        var kept: [UInt16] = []
        kept.reserveCapacity(units.count)
        for (index, unit) in units.enumerated() where !hidden.contains(index) { kept.append(unit) }
        return String(decoding: kept, as: UTF16.self)
            .split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
            .joined(separator: " ")
    }

    // MARK: Blocks

    public enum Block: Equatable, Sendable {
        case paragraph(String)
        case heading(Int, String)
        case bullets([String])
        /// A numbered list and the number it begins with.
        case numbered(Int, [String])
        case code(String)
        case quote(String)
    }

    /// A finished message, cut into what a bubble draws one under the other. The words inside
    /// each block keep their inline Markdown.
    public static func blocks(in text: String) -> [Block] {
        var blocks: [Block] = []
        var paragraph: [String] = []
        var code: [String]?
        func endParagraph() {
            if !paragraph.isEmpty { blocks.append(.paragraph(paragraph.joined(separator: "\n"))) }
            paragraph = []
        }
        for raw in text.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(raw)
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("```") {
                if let lines = code {
                    blocks.append(.code(lines.joined(separator: "\n")))
                    code = nil
                } else {
                    endParagraph()
                    code = []
                }
                continue
            }
            if code != nil {
                code?.append(line)
                continue
            }
            if trimmed.isEmpty {
                endParagraph()
            } else if let (level, words) = heading(trimmed) {
                endParagraph()
                blocks.append(.heading(level, words))
            } else if let (number, words) = entry(trimmed) {
                endParagraph()
                switch (number, blocks.last) {
                case (nil, .bullets(let before)):
                    blocks[blocks.count - 1] = .bullets(before + [words])
                case (nil, _):
                    blocks.append(.bullets([words]))
                case (_?, .numbered(let first, let before)):
                    blocks[blocks.count - 1] = .numbered(first, before + [words])
                case (let number?, _):
                    blocks.append(.numbered(number, [words]))
                }
            } else if trimmed.hasPrefix(">") {
                endParagraph()
                let words = String(trimmed.dropFirst()).trimmingCharacters(in: .whitespaces)
                if case .quote(let before) = blocks.last {
                    blocks[blocks.count - 1] = .quote(before + "\n" + words)
                } else {
                    blocks.append(.quote(words))
                }
            } else {
                paragraph.append(trimmed)
            }
        }
        if let lines = code { blocks.append(.code(lines.joined(separator: "\n"))) }
        endParagraph()
        return blocks
    }

    private static func heading(_ line: String) -> (Int, String)? {
        let hashes = line.prefix { $0 == "#" }.count
        guard (1...6).contains(hashes) else { return nil }
        let rest = line.dropFirst(hashes)
        guard rest.first == " " else { return nil }
        return (hashes, rest.trimmingCharacters(in: .whitespaces))
    }

    /// A list's entry: its number when it has one, and its words.
    private static func entry(_ line: String) -> (Int?, String)? {
        if let first = line.first, "-*+".contains(first), line.dropFirst().first == " " {
            return (nil, line.dropFirst(2).trimmingCharacters(in: .whitespaces))
        }
        let digits = line.prefix { $0.isASCII && $0.isNumber }
        guard !digits.isEmpty, digits.count <= 9, let number = Int(digits) else { return nil }
        let rest = line.dropFirst(digits.count)
        guard let sign = rest.first, sign == "." || sign == ")", rest.dropFirst().first == " " else { return nil }
        return (number, rest.dropFirst(2).trimmingCharacters(in: .whitespaces))
    }

    // MARK: Marks, line by line

    /// A sign as the UTF-16 unit it is typed as.
    private static func u16(_ sign: Unicode.Scalar) -> UInt16 {
        UInt16(truncatingIfNeeded: sign.value)
    }

    private static let newline = u16("\n")
    private static let space = u16(" ")
    private static let tab = u16("\t")
    private static let star = u16("*")
    private static let underscore = u16("_")
    private static let tilde = u16("~")
    private static let backtick = u16("`")
    private static let backslash = u16("\\")

    private static func line(_ units: [UInt16], _ range: Range<Int>, fenced: inout Bool, into marks: inout [Mark]) {
        var first = range.lowerBound
        while first < range.upperBound, units[first] == space || units[first] == tab { first += 1 }
        let rest = first..<range.upperBound
        if starts(units, rest, with: "```") {
            marks.append(Mark(.codeBlock, range))
            marks.append(Mark(.marker, rest))
            fenced.toggle()
            return
        }
        if fenced {
            marks.append(Mark(.codeBlock, range))
            return
        }
        guard !rest.isEmpty else { return }
        var words = rest
        if let hashes = headingHashes(units, rest) {
            marks.append(Mark(.heading(hashes), range))
            marks.append(Mark(.syntax, first..<(first + hashes + 1)))
            words = (first + hashes + 1)..<range.upperBound
        } else if let (length, ordered) = listSign(units, rest) {
            marks.append(Mark(.listItem(ordered: ordered), range))
            marks.append(Mark(.marker, first..<(first + length)))
            words = min(first + length + 1, range.upperBound)..<range.upperBound
        } else if units[first] == u16(">") {
            marks.append(Mark(.quote, range))
            marks.append(Mark(.marker, first..<(first + 1)))
            words = (first + 1)..<range.upperBound
        }
        inline(units, words, into: &marks)
    }

    private static func starts(_ units: [UInt16], _ range: Range<Int>, with prefix: String) -> Bool {
        let wanted = Array(prefix.utf16)
        guard range.count >= wanted.count else { return false }
        return Array(units[range.lowerBound..<(range.lowerBound + wanted.count)]) == wanted
    }

    private static func headingHashes(_ units: [UInt16], _ range: Range<Int>) -> Int? {
        var count = 0
        while range.lowerBound + count < range.upperBound, units[range.lowerBound + count] == u16("#") { count += 1 }
        guard (1...6).contains(count), range.lowerBound + count < range.upperBound,
              units[range.lowerBound + count] == space
        else { return nil }
        return count
    }

    /// The length of a list's sign at the start of the range, and whether it is a number.
    private static func listSign(_ units: [UInt16], _ range: Range<Int>) -> (Int, Bool)? {
        let first = units[range.lowerBound]
        func spaceFollows(_ length: Int) -> Bool {
            range.lowerBound + length < range.upperBound && units[range.lowerBound + length] == space
        }
        if first == u16("-") || first == star || first == u16("+") {
            return spaceFollows(1) ? (1, false) : nil
        }
        var digits = 0
        while range.lowerBound + digits < range.upperBound, isDigit(units[range.lowerBound + digits]) { digits += 1 }
        guard digits > 0, digits <= 9, range.lowerBound + digits < range.upperBound else { return nil }
        let sign = units[range.lowerBound + digits]
        guard sign == u16(".") || sign == u16(")") else { return nil }
        return spaceFollows(digits + 1) ? (digits + 1, true) : nil
    }

    private static func isDigit(_ unit: UInt16) -> Bool {
        unit >= u16("0") && unit <= u16("9")
    }

    private static func isBlank(_ unit: UInt16) -> Bool {
        unit == space || unit == tab || unit == newline
    }

    private static func isWord(_ unit: UInt16) -> Bool {
        guard let scalar = Unicode.Scalar(unit) else { return true }
        return CharacterSet.alphanumerics.contains(scalar)
    }

    private static func isSign(_ unit: UInt16) -> Bool {
        guard unit < 128, let scalar = Unicode.Scalar(unit) else { return false }
        return CharacterSet.punctuationCharacters.contains(scalar) || CharacterSet.symbols.contains(scalar)
    }

    private static func inline(_ units: [UInt16], _ range: Range<Int>, into marks: inout [Mark]) {
        var index = range.lowerBound
        while index < range.upperBound {
            let unit = units[index]
            if unit == backslash, index + 1 < range.upperBound, isSign(units[index + 1]) {
                marks.append(Mark(.syntax, index..<(index + 1)))
                index += 2
            } else if unit == backtick, let close = next(backtick, in: units, (index + 1)..<range.upperBound),
                      close > index + 1 {
                marks.append(Mark(.syntax, index..<(index + 1)))
                marks.append(Mark(.code, (index + 1)..<close))
                marks.append(Mark(.syntax, close..<(close + 1)))
                index = close + 1
            } else if unit == star || unit == underscore || unit == tilde {
                index = emphasis(units, at: index, in: range, into: &marks)
            } else if unit == u16("["), let end = link(units, at: index, in: range, into: &marks) {
                index = end
            } else if let end = address(units, at: index, in: range, into: &marks) {
                index = end
            } else {
                index += 1
            }
        }
    }

    private static func next(_ wanted: UInt16, in units: [UInt16], _ range: Range<Int>) -> Int? {
        guard range.lowerBound <= range.upperBound else { return nil }
        return range.first { units[$0] == wanted }
    }

    private static func run(of unit: UInt16, in units: [UInt16], at index: Int, before end: Int) -> Int {
        var length = 0
        while index + length < end, units[index + length] == unit { length += 1 }
        return length
    }

    /// Strong, emphasised or struck words that begin at `index`. Returns where to read on.
    private static func emphasis(_ units: [UInt16], at index: Int, in range: Range<Int>, into marks: inout [Mark]) -> Int {
        let sign = units[index]
        let length = run(of: sign, in: units, at: index, before: range.upperBound)
        // A sign inside a word is part of the word: snake_case stays as typed.
        let insideWord = sign == underscore && index > range.lowerBound && isWord(units[index - 1])
        let widths = sign == tilde ? [2] : [2, 1]
        for width in widths where length >= width && !insideWord {
            let inner = index + width
            guard inner < range.upperBound, !isBlank(units[inner]) else { continue }
            guard let close = closing(sign, width, in: units, inner..<range.upperBound), close > inner else { continue }
            let kind: Mark.Kind = sign == tilde ? .strike : (width == 2 ? .strong : .emphasis)
            marks.append(Mark(.syntax, index..<inner))
            marks.append(Mark(kind, inner..<close))
            marks.append(Mark(.syntax, close..<(close + width)))
            inline(units, inner..<close, into: &marks)
            return close + width
        }
        return index + max(length, 1)
    }

    /// Where a run of `sign`, `width` long, closes what was opened: after a word, not before one.
    private static func closing(_ sign: UInt16, _ width: Int, in units: [UInt16], _ range: Range<Int>) -> Int? {
        var index = range.lowerBound
        while index < range.upperBound {
            guard units[index] == sign else {
                // Code keeps its signs to itself.
                if units[index] == backtick, let close = next(backtick, in: units, (index + 1)..<range.upperBound) {
                    index = close + 1
                } else {
                    index += units[index] == backslash ? 2 : 1
                }
                continue
            }
            let length = run(of: sign, in: units, at: index, before: range.upperBound)
            let afterWord = index > range.lowerBound && !isBlank(units[index - 1])
            let beforeWord = sign == underscore && index + length < range.upperBound && isWord(units[index + length])
            if afterWord, !beforeWord, width == 2 ? length >= 2 : length == 1 {
                return width == 2 ? index + length - 2 : index
            }
            index += length
        }
        return nil
    }

    /// `[words](address)` that begins at `index`. Returns where it ends.
    private static func link(_ units: [UInt16], at index: Int, in range: Range<Int>, into marks: inout [Mark]) -> Int? {
        guard let close = next(u16("]"), in: units, (index + 1)..<range.upperBound), close > index + 1,
              close + 1 < range.upperBound, units[close + 1] == u16("("),
              let end = next(u16(")"), in: units, (close + 2)..<range.upperBound), end > close + 2
        else { return nil }
        let target = String(decoding: units[(close + 2)..<end], as: UTF16.self)
        guard !target.contains(" ") else { return nil }
        marks.append(Mark(.syntax, index..<(index + 1)))
        marks.append(Mark(.link(target), (index + 1)..<close))
        marks.append(Mark(.syntax, close..<(end + 1)))
        inline(units, (index + 1)..<close, into: &marks)
        return end + 1
    }

    /// An address written out, such as https://tilo.sh, that begins at `index`.
    private static func address(_ units: [UInt16], at index: Int, in range: Range<Int>, into marks: inout [Mark]) -> Int? {
        guard units[index] == u16("h"), index == range.lowerBound || !isWord(units[index - 1]) else { return nil }
        let rest = index..<range.upperBound
        let prefix = starts(units, rest, with: "https://") ? 8 : (starts(units, rest, with: "http://") ? 7 : 0)
        guard prefix > 0 else { return nil }
        var end = index + prefix
        while end < range.upperBound, !isBlank(units[end]), units[end] != u16("<"), units[end] != u16(">") { end += 1 }
        // A sentence's own sign after the address is not the address's.
        while end > index + prefix, ".,;:!?)]\"'".utf16.contains(units[end - 1]) { end -= 1 }
        guard end > index + prefix else { return nil }
        marks.append(Mark(.link(String(decoding: units[index..<end], as: UTF16.self)), index..<end))
        return end
    }
}

extension Markdown {
    /// Which of Markdown's own characters a field keeps out of sight: all of them, but those of
    /// the formatted words the person is writing in. `selection` is the caret or what is
    /// selected, in UTF-16 units; nil when the field is not being written in.
    public static func hidden(in marks: [Mark], selection: Range<Int>?) -> [Range<Int>] {
        let syntax = marks.filter { $0.kind == .syntax }.map(\.range)
        guard let selection else { return syntax }
        var shown: [Range<Int>] = []
        for mark in marks {
            switch mark.kind {
            case .strong, .emphasis, .strike, .code, .link:
                // The words with their signs on both sides.
                let before = syntax.first { $0.upperBound == mark.range.lowerBound }
                let after = syntax.first { $0.lowerBound == mark.range.upperBound }
                let whole = (before?.lowerBound ?? mark.range.lowerBound)..<(after?.upperBound ?? mark.range.upperBound)
                if touches(selection, whole) { shown += [before, after].compactMap { $0 } }
            case .heading:
                // A heading's signs show while the person writes on its line.
                if touches(selection, mark.range) { shown += syntax.filter { mark.range.contains($0.lowerBound) } }
            default:
                break
            }
        }
        // A sign that stands alone (an escape) shows while the caret is at it.
        return syntax.filter { range in !shown.contains(range) && !(range.count == 1 && touches(selection, range) && alone(range, marks)) }
    }

    private static func touches(_ selection: Range<Int>, _ range: Range<Int>) -> Bool {
        selection.lowerBound <= range.upperBound && selection.upperBound >= range.lowerBound
    }

    private static func alone(_ range: Range<Int>, _ marks: [Mark]) -> Bool {
        !marks.contains { $0.kind != .syntax && ($0.range.lowerBound == range.upperBound || $0.range.upperBound == range.lowerBound) }
    }
}

extension Markdown {
    /// A message's words as a person takes them elsewhere: plain text, every line where it stood.
    /// Markdown's own characters are gone. A list's dash is the round sign a bubble draws, and its
    /// number stays; a line of code stays as it was written and its fence's lines are left out; a
    /// link's address follows its words.
    public static func copied(_ text: String) -> String {
        let units = Array(text.utf16)
        var gone = IndexSet()
        var put: [Int: [UInt16]] = [:]
        for mark in marks(in: text) {
            switch mark.kind {
            case .syntax:
                gone.insert(integersIn: mark.range)
            case .marker:
                let sign = units[mark.range.lowerBound]
                if sign == backtick {
                    // A fence's own line goes with the break that ends it, the last line's with the break before it.
                    var start = mark.range.lowerBound
                    while start > 0, units[start - 1] != newline { start -= 1 }
                    var end = mark.range.upperBound
                    if end < units.count { end += 1 } else if start > 0 { start -= 1 }
                    gone.insert(integersIn: start..<end)
                } else if sign == u16(">") {
                    var end = mark.range.upperBound
                    if end < units.count, units[end] == u16(" ") { end += 1 }
                    gone.insert(integersIn: mark.range.lowerBound..<end)
                } else if mark.range.count == 1 {
                    gone.insert(mark.range.lowerBound)
                    put[mark.range.lowerBound] = Array("•".utf16)
                }
            case .link(let address):
                if String(decoding: units[mark.range], as: UTF16.self) != address {
                    put[mark.range.upperBound] = Array(" (\(address))".utf16)
                }
            default:
                break
            }
        }
        var kept: [UInt16] = []
        kept.reserveCapacity(units.count)
        for (index, unit) in units.enumerated() {
            if let words = put[index] { kept.append(contentsOf: words) }
            if !gone.contains(index) { kept.append(unit) }
        }
        return String(decoding: kept, as: UTF16.self).trimmingCharacters(in: .newlines)
    }
}
