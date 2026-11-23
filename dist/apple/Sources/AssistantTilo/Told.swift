import AssistantCore
import Foundation
import TiloLive

// What the app makes of a thing the person's server told them: the line that says why it did
// not notify, the buttons under its notice, and the message about what came while they were
// away. Their server decides all of it; this only puts it into the frame's values.
extension TiloSource {
    /// Mark a piece as its person's server told of it.
    static func mark(_ item: inout Item, as told: ToldThing) {
        item.keptQuiet = told.reaches ? nil
            : Quiet.line(about: told.title, code: told.why.code, fact: told.why.fact, value: told.why.value, words: told.why.words)
        // A question that was answered offers nothing more, and neither does what was taken back.
        var over = told.isWithdrawn
        if case .question(let asked) = item.content, !asked.waits { over = true }
        item.offers = told.reaches && !over ? offers(of: told) : []
        item.raised = told.isSettled ? 0 : told.raised
    }

    /// What a thing told offers, as far as this app can do it, as the buttons under its notice.
    static func offers(of told: ToldThing) -> [Offer] {
        told.offers.filter(mayPress).map { one in
            Offer(id: .init(one.id), title: one.label.isEmpty ? worded(one.id) : one.label, isMain: one.isMain,
                  by: told.offeredBy.isEmpty ? nil : told.offeredBy)
        }
    }

    /// Whether a button under a notice may invoke this. The app's page holds Chat, so an action
    /// of Chat's is one it can invoke; any other is done in the app it belongs to. A permission
    /// that is to stay is never given from under a notification: the person gives it in the
    /// conversation, where the whole question stands.
    static func mayPress(_ offer: ToldThing.Offer) -> Bool {
        guard offer.app == "so.tilo.chat" else { return false }
        return !(offer.action == "answer-question" && offer.arguments["answer"]?.string == "always")
    }

    /// An answer that Chat words itself, by its id.
    static func worded(_ answer: String) -> String {
        switch answer {
        case "allow": AssistantWords.allow.said
        case "deny": AssistantWords.deny.said
        default: answer
        }
    }

    /// What the person was told while they were away, for the main chat to say.
    static func since(_ told: ToldSince, shown: [String: Item.ID]) -> SinceLeft {
        func lines(_ things: [ToldThing]) -> [SinceLeft.Line] {
            things.map { thing in
                SinceLeft.Line(id: thing.id, words: Markdown.plain(thing.words.isEmpty ? thing.title : thing.words),
                               item: shown[thing.id], isImportant: thing.isImportant)
            }
        }
        return SinceLeft(left: told.left, waits: lines(told.waits), didAlone: lines(told.didAlone), cameIn: lines(told.cameIn))
    }
}
