import Foundation

// A message that is still being written, as a bubble draws it meanwhile.
//
// A reply grows while its model writes it, and its words arrive in pieces that end anywhere:
// after the two stars that begin strong words and before the two that end them, after a list's
// dash and before its first word. Read as a finished message, such a piece shows the signs as
// typed, and a moment later the same words stand there in another form. So what is still
// being written is read as it will stand: words whose signs are open are drawn in their form,
// and a sign that nothing follows yet waits for what follows. What was drawn is not taken
// away again, and it does not change its form.
extension Markdown {
    /// A message that is still being written, cut into what a bubble draws one under the other.
    public static func blocks(in text: String, whileWritten: Bool) -> [Block] {
        blocks(in: whileWritten ? Markdown.whileWritten(text) : text)
    }

    /// The text of a message that is still being written, as `blocks` should read it: with
    /// what stands open at its end closed, and without a sign at its end that says nothing
    /// before its words are there. Only the end is read differently: the last line, or the
    /// last lines where they are one paragraph. Inside a fence of code nothing is, but for
    /// the fence's own closing sign while it is half written.
    public static func whileWritten(_ text: String) -> String {
        var lines = text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        guard let last = lines.popLast() else { return text }
        let bare = last.trimmingCharacters(in: .whitespaces)
        let fenced = lines.filter { $0.trimmingCharacters(in: .whitespaces).hasPrefix("```") }.count % 2 == 1
        if fenced || bare.hasPrefix("```") {
            // Inside a fence a line that is empty yet, or begins the closing sign, waits.
            if fenced, bare.isEmpty || (isBareSign(bare) && bare.first == "`") { return lines.joined(separator: "\n") }
            return text
        }
        let sign = blockSign(of: last)
        if sign > 0 {
            // A list's entry, a heading or a quote: its own words are the line's.
            let words = closed(String(last.dropFirst(sign)))
            if words.trimmingCharacters(in: .whitespaces).isEmpty { return lines.joined(separator: "\n") }
            return (lines + [String(last.prefix(sign)) + words]).joined(separator: "\n")
        }
        // A paragraph: the lines since the last empty line, entry, heading, quote or fence.
        var paragraph = [last]
        while let before = lines.last, !before.trimmingCharacters(in: .whitespaces).isEmpty, blockSign(of: before) == 0,
              !before.trimmingCharacters(in: .whitespaces).hasPrefix("```") {
            paragraph.insert(lines.removeLast(), at: 0)
        }
        var read = closed(paragraph.joined(separator: "\n")).split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        // A line that holds only a sign may still become a list's entry or a heading: it waits.
        if let end = read.last, isBareSign(end.trimmingCharacters(in: .whitespaces)) { read.removeLast() }
        return (lines + read).joined(separator: "\n")
    }

    /// How many characters at a line's beginning make it a list's entry, a heading or a
    /// quote: the blanks before the sign, the sign and the blank after it. None: a plain line.
    private static func blockSign(of line: String) -> Int {
        let blanks = line.prefix { $0 == " " || $0 == "\t" }.count
        let rest = Array(line.dropFirst(blanks))
        guard let first = rest.first else { return 0 }
        func blank(_ at: Int) -> Bool { at < rest.count && rest[at] == " " }
        if first == ">" { return blanks + (blank(1) ? 2 : 1) }
        if "-*+".contains(first) { return blank(1) ? blanks + 2 : 0 }
        if first == "#" {
            let hashes = rest.prefix { $0 == "#" }.count
            return hashes <= 6 && blank(hashes) ? blanks + hashes + 1 : 0
        }
        let digits = rest.prefix { $0.isASCII && $0.isNumber }.count
        guard (1...9).contains(digits), digits < rest.count, rest[digits] == "." || rest[digits] == ")", blank(digits + 1) else { return 0 }
        return blanks + digits + 2
    }

    /// Whether a line holds a sign and nothing else: a list's dash or number, a heading's
    /// hashes, a quote's sign, the beginning of a fence. With words after it, it would be
    /// something else than it is alone.
    private static func isBareSign(_ line: String) -> Bool {
        if ["-", "*", "+", ">", "`", "``"].contains(line) { return true }
        if !line.isEmpty, line.count <= 6, line.allSatisfy({ $0 == "#" }) { return true }
        let digits = line.prefix { $0.isASCII && $0.isNumber }
        guard (1...9).contains(digits.count) else { return false }
        let rest = line.dropFirst(digits.count)
        return rest.isEmpty || rest == "." || rest == ")"
    }

    /// A paragraph's or a line's words with what stands open at their end closed, innermost
    /// first: code, struck, strong and emphasised words. Signs at the very end that nothing
    /// follows yet are left out, but where they end what stands open. A link whose address is
    /// still being written shows its words.
    private static func closed(_ words: String) -> String {
        var chars = Array(words)
        var blanks: [Character] = []
        while let end = chars.last, end.isWhitespace { blanks.insert(chars.removeLast(), at: 0) }
        // The signs at the very end, where no blank follows them: what they are is not said yet.
        var waiting: [Character] = []
        if blanks.isEmpty, let end = chars.last, "*_~`".contains(end) {
            while chars.last == end { waiting.append(chars.removeLast()) }
        }
        var out: [Character] = []
        var open: [String] = []
        var ticks: Int?
        func isWord(_ character: Character?) -> Bool { character.map { $0.isLetter || $0.isNumber } ?? false }
        /// Close with `count` signs in a row what was opened with the same sign. Answers how many are left over.
        func close(_ sign: Character, _ count: Int) -> Int {
            var left = count
            while left > 0, let top = open.last, top.first == sign, top.count <= left {
                open.removeLast()
                left -= top.count
            }
            return left
        }
        var index = 0
        while index < chars.count {
            let sign = chars[index]
            var length = 1
            while index + length < chars.count, chars[index + length] == sign { length += 1 }
            if sign == "\\" {
                out += chars[index..<min(index + 2, chars.count)]
                index += 2
            } else if sign == "`" {
                if ticks == nil { ticks = length } else if ticks == length { ticks = nil }
                out += chars[index..<(index + length)]
                index += length
            } else if ticks != nil {
                out.append(sign)
                index += 1
            } else if sign == "[" {
                // A link is assumed: its words stand as words while its address is written.
                let label = chars[(index + 1)...].firstIndex(of: "]")
                if let label, label + 1 < chars.count, chars[label + 1] == "(" {
                    if let end = chars[(label + 2)...].firstIndex(of: ")") {
                        out += chars[index...end]
                        index = end + 1
                    } else {
                        chars = Array(chars[(index + 1)..<label])
                        index = 0
                    }
                } else if label == nil || label == chars.count - 1 {
                    if let label { chars.remove(at: label) }
                    index += 1
                } else {
                    out.append(sign)
                    index += 1
                }
            } else if sign == "*" || sign == "_" || sign == "~" {
                let before: Character? = index > 0 ? chars[index - 1] : nil
                // After the last of these words come the signs that wait, or the blanks set aside.
                let after: Character? = index + length < chars.count ? chars[index + length] : (waiting.first ?? blanks.first)
                let opens = !(after?.isWhitespace ?? false) && !isWord(before)
                let closes = !(before?.isWhitespace ?? true) && !(sign == "_" && isWord(after))
                var left = sign == "~" && length != 2 ? 0 : length
                if closes { left = close(sign, left) }
                if opens, (1...3).contains(left) {
                    if left == 3 { open += [String(sign), String([sign, sign])] } else { open.append(String(repeating: sign, count: left)) }
                }
                out += chars[index..<(index + length)]
                index += length
            } else {
                out.append(sign)
                index += 1
            }
        }
        // The signs that waited: they stay where they end what stands open, and wait where
        // they would begin something.
        if let sign = waiting.first {
            if let count = ticks {
                if sign != "`" { out += waiting } else if waiting.count >= count { out += waiting; ticks = nil }
            } else if sign != "`", !(out.last?.isWhitespace ?? true) {
                let usable = sign == "~" && waiting.count != 2 ? 0 : waiting.count
                out += waiting.prefix(usable - close(sign, usable))
            }
        }
        while let end = out.last, end.isWhitespace { blanks.insert(out.removeLast(), at: 0) }
        let closers = (ticks.map { String(repeating: "`", count: $0) } ?? "") + open.reversed().joined()
        return String(out) + closers + String(blanks)
    }
}
