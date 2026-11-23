import AssistantCore
import Foundation
import TiloLive

/// What this device saw of the turns that write replies: what each does now, since when, and
/// how long each of its steps took.
///
/// Chat keeps with a reply what its turn does (`work`): the state it is in, in the system's
/// own words where it takes a step (`did`, and what it is `about`); a row for each step it
/// took (`trace`: the step's label, its resource, how it ended); and what its model gave of
/// its thinking (`stream.reasoning`). All of a step's words are read from there, none is
/// written here. A step's time is what Chat keeps with its row (`took_ms`: from the call
/// handed to its tool to its answer). Where a row keeps none, the time is this device's own:
/// from when it first saw the step going to when it saw it ended, and a step it never saw
/// going has none.
struct TurnWatch {
    private struct Seen {
        var began: Date
        var now: Message.Turn.Now
        /// How much of the reply's words and of its thinking was there at the last look, and the state it was in.
        var words = 0
        var thought = 0
        var state = ""
        var steps: [String: Times] = [:]
    }

    private struct Times {
        /// When the step was first seen going. Nil for one first seen ended.
        var from: Date?
        var took: TimeInterval?
    }

    private struct Row {
        var id: String
        var sequence: Double
        var words: String
        var outcome: Message.Turn.Step.Outcome
        /// How long the step took, where Chat keeps it with the row.
        var took: TimeInterval?
    }

    private var seen: [String: Seen] = [:]

    /// The turn at work on the reply `piece`, as Chat says it at `now`: `work` is what Chat
    /// keeps with the reply, `words` the reply's own words so far, `began` when the reply was
    /// begun, `going` whether its work goes on. Nil for a reply no turn works on, and what was
    /// seen of its turn is forgotten.
    mutating func read(work: JSON?, words: String, of piece: String, began: Date, going: Bool, now: Date) -> Message.Turn? {
        guard going, let work, !work.isNull else {
            seen[piece] = nil
            return nil
        }
        let state = work["state"]?["state"]
        let kind = state?.string ?? state?["$variant"]?.string ?? ""
        let did = state?["did"]?.string ?? "", about = state?["about"]?.string ?? ""
        let rows = TurnWatch.rows(work["trace"])
        let thought = work["stream"]?["reasoning"]?.string ?? ""
        let key = "\(kind)|\(work["state"]?["since"]?.number ?? 0)|\(did)|\(about)|\(rows.count)"
        let first = seen[piece] == nil
        // A device whose clock is behind its person's Tilo counts from when it first saw the turn.
        var known = seen[piece] ?? Seen(began: min(began, now), now: .thinking(""))
        let step = Message.Turn.Now.step(TurnWatch.stepWords(did: did, about: about))
        if first {
            known.now = kind == "doing" ? step : words.isEmpty ? .thinking(TurnWatch.newest(of: thought)) : .writing
        } else if words.utf8.count > known.words {
            known.now = .writing
        } else if thought.utf8.count > known.thought {
            known.now = .thinking(TurnWatch.newest(of: thought))
        } else if key != known.state, kind == "doing" {
            known.now = step
        }
        known.words = words.utf8.count
        known.thought = thought.utf8.count
        known.state = key
        var steps: [Message.Turn.Step] = []
        for row in rows {
            let ended = row.outcome != .going
            var times = known.steps[row.id] ?? Times(from: ended ? nil : now)
            if ended, times.took == nil, let from = times.from { times.took = now.timeIntervalSince(from) }
            known.steps[row.id] = times
            steps.append(.init(id: row.id, words: row.words, took: row.took ?? times.took, outcome: row.outcome))
        }
        seen[piece] = known
        return Message.Turn(now: known.now, began: known.began, steps: steps)
    }

    /// The rows Chat keeps of a turn's steps, in the order they were taken. Chat keys them by
    /// their ids: they are read as a list, or as what stands under each id.
    private static func rows(_ trace: JSON?) -> [Row] {
        guard let trace, !trace.isNull else { return [] }
        let kept = trace.array ?? trace.object.map { Array($0.values) } ?? []
        return kept.compactMap { row -> Row? in
            guard let id = row["id"]?.string, !id.isEmpty else { return nil }
            let outcome = row["outcome"]
            let ended = outcome?.string ?? outcome?["$variant"]?.string ?? outcome?["outcome"]?.string
            let how: Message.Turn.Step.Outcome = switch ended {
            case "applied", "no_change": .done
            case "refused": .failed
            default: .going
            }
            return Row(id: id, sequence: row["sequence"]?.number ?? 0,
                       words: said(row["label"]?.string ?? "", about: row["resource"]?.string ?? ""), outcome: how,
                       took: how == .going ? nil : row["took_ms"]?.number.map { max(0, $0) / 1000 })
        }
        .sorted { ($0.sequence, $0.id) < ($1.sequence, $1.id) }
    }

    /// A step and what it is about, as one line says both.
    private static func said(_ did: String, about: String) -> String {
        let did = did.trimmingCharacters(in: .whitespacesAndNewlines), about = about.trimmingCharacters(in: .whitespacesAndNewlines)
        if did.isEmpty { return about }
        return about.isEmpty ? did : "\(did): \(about)"
    }

    /// A step's own words, where Chat says the step in words a person reads ("Add an account",
    /// and what it is about). A tool's bare name (`tilo_do`, `tasks.add`, `find`) is no such
    /// words: nil.
    static func stepWords(did: String, about: String) -> String? {
        let name = did.trimmingCharacters(in: .whitespacesAndNewlines)
        // Words have a blank between them, or begin as a label does. A tool's name has neither.
        let isName = !name.contains(" ") && (name.contains("_") || name.contains(".") || name.first?.isUppercase != true)
        guard !name.isEmpty, !isName else { return nil }
        return said(name, about: about)
    }

    /// The newest words of what a model gave of its thinking: its last line, plain. The line
    /// that shows them shows their end.
    static func newest(of thought: String) -> String {
        let end = thought.suffix(600)
        var line = end.split(whereSeparator: \.isNewline).last { !$0.allSatisfy(\.isWhitespace) }.map(String.init) ?? ""
        // What was cut in the middle of a word begins at its next word.
        if end.utf8.count < thought.utf8.count, line.utf8.count == end.utf8.count, let blank = line.firstIndex(of: " ") {
            line = String(line[blank...])
        }
        return Markdown.plain(line).trimmingCharacters(in: .whitespaces)
    }
}
