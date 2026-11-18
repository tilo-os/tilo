import AssistantCore
import Foundation
import TiloLive

// What the app makes of the person's events (`tilo.events` on their server): the list the
// Tasks panel shows, and the card an event stands as in the main chat, under the answer
// during which it was set up. An event is its person's server's: nothing here keeps one, and
// a card says only what its event says. What the person presses on a card is an action on
// the event at their server; the card changes by what the server answers.

/// The person's events as their server last listed them, and where each stands as a card.
struct EventCards {
    /// One event as the person's server answers it.
    struct Kept: Equatable {
        var automation: Automation
        /// When it was set up, by its server's clock.
        var setUp: Date?
        /// What its person's assistant is told to do when it comes.
        var told = ""
        /// Its note, as the server answers it: every part.
        var note: JSON?
        /// The actions that leave the workspace it may take without asking, each a rule as the
        /// server keeps it.
        var permits: [JSON] = []
    }

    /// The stretch of one answer, as Chat keeps it: from when the reply began to when the
    /// work on it ended. Open while it is still being written.
    struct Stretch: Equatable {
        var from: Date
        var to: Date?
    }

    /// What marks a card's piece: its answer's own name, this, and the event's id.
    static let mark = "#event:"
    /// How far a server's clock for an event and Chat's whole seconds for a message may lie apart.
    static let slack: TimeInterval = 2
    /// The answers of a card, by name.
    static let show = "show", ask = "ask", undo = "undo"

    private(set) var events: [Kept] = []
    private var stretches: [Item.ID: Stretch] = [:]
    /// The events whose person asked on this device to see what they keep.
    private var showing: Set<Automation.ID> = []
    /// The events whose person took back on this device that they may act alone.
    private var asksNow: Set<Automation.ID> = []
    /// The events their person undid on this device since the app started, as they last
    /// stood: each card stays to say so. Their server keeps them no more.
    private var undone: [Kept] = []

    /// What the Tasks panel lists: every event, in the server's order.
    var automations: [Automation] { events.map(\.automation) }

    /// Take the events as the server listed them (`list` of `tilo.events`).
    mutating func take(_ listed: [JSON]) {
        events = listed.compactMap(EventCards.kept)
        let there = Set(events.map(\.automation.id))
        showing.formIntersection(there)
        asksNow.formIntersection(there)
    }

    /// Take one event as an action on it answered it.
    mutating func take(event view: JSON) {
        guard let kept = EventCards.kept(view), let index = events.firstIndex(where: { $0.automation.id == kept.automation.id })
        else { return }
        events[index] = kept
    }

    /// One event of the server's list. Nil for one that names no id.
    static func kept(_ event: JSON) -> Kept? {
        guard let id = event["id"]?.string else { return nil }
        let scheduled = (event["sort"]?.string ?? event["sort"]?["$variant"]?.string) != "event"
        let when = event["when"]?.string ?? ""
        let permits = event["permits"]?.array ?? []
        let automation = Automation(
            id: .init(id), sort: scheduled ? .scheduled : .event, title: event["name"]?.string ?? id,
            summary: scheduled ? when : event["instruction"]?.string ?? when,
            next: event["nextAtMs"]?.number.map { Date(timeIntervalSince1970: $0 / 1000) },
            once: when.lowercased().hasPrefix("once"), filter: event["onlyIf"]?.string,
            threshold: event["every"]?.number.map { Int($0) }, note: EventCards.noteTag(event["note"]),
            actsAlone: !permits.isEmpty, isOn: event["on"]?.bool ?? true)
        return Kept(automation: automation, setUp: event["setUpAtMs"]?.number.map { Date(timeIntervalSince1970: $0 / 1000) },
                    told: event["instruction"]?.string ?? "", note: event["note"], permits: permits)
    }

    /// What an event's note keeps, in a few words for its tag: the person's style, and from
    /// how many answers they accepted where it keeps such; else the answers alone, what the
    /// person cares about, or what its last time left for the next. Nil for a note that keeps
    /// nothing. Never a number the server does not keep.
    static func noteTag(_ note: JSON?) -> String? {
        guard let note, !note.isNull else { return nil }
        func has(_ part: String) -> Bool {
            !(note[part]?.string ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
        let accepted = (note["accepted"]?.array ?? []).count
        if has("style") {
            return accepted == 0 ? AssistantWords.noteStyle.said
                : AssistantWords.counted(accepted, one: .noteStyleFromOne, many: .noteStyleFromMany)
        }
        if accepted > 0 { return AssistantWords.counted(accepted, one: .noteAcceptedOne, many: .noteAcceptedMany) }
        if has("guidance") { return AssistantWords.noteGuidance.said }
        if let kept = note["kept"], !kept.isNull { return AssistantWords.noteKept.said }
        return nil
    }

    /// The question an event's card answers under.
    static func question(of event: Automation.ID) -> Question.ID {
        .init("event:" + event.value)
    }

    /// The event a question is the card of. Nil for a question that is none.
    static func event(of question: Question.ID) -> Automation.ID? {
        question.value.hasPrefix("event:") ? .init(String(question.value.dropFirst("event:".count))) : nil
    }

    /// What an event may do without asking, as the server listed it. Nil for an event it lists no more.
    func permits(of event: Automation.ID) -> [JSON]? {
        events.first { $0.automation.id == event }?.permits
    }

    /// The person asked to see what an event keeps. False for an event the server lists no more.
    mutating func show(_ event: Automation.ID) -> Bool {
        guard events.contains(where: { $0.automation.id == event }) else { return false }
        showing.insert(event)
        return true
    }

    /// The person took back that an event may act alone, and their server carried it out.
    mutating func asked(_ event: Automation.ID) {
        asksNow.insert(event)
    }

    /// The person undid an event, and their server removed it: its card stays to say so.
    mutating func undid(_ event: Automation.ID) {
        if let was = events.first(where: { $0.automation.id == event }) { undone.append(was) }
        events.removeAll { $0.automation.id == event }
        showing.remove(event)
        asksNow.remove(event)
    }

    /// A message as Chat keeps it: where an agent worked on it, the stretch of that work.
    mutating func see(_ message: ChatMessage, as piece: Item.ID) {
        see(work: message.fields["work"], began: message.at, stopped: message.fields["stopped"], going: message.isWorkedOn, as: piece)
    }

    /// The work Chat keeps on a reply that began at `began`: its stretch ends when the work
    /// did (`since` of the state it is in, epoch seconds) or when somebody stopped it, and is
    /// open while the work is `going`. A reply nobody worked on has no stretch.
    mutating func see(work: JSON?, began: Date, stopped: JSON? = nil, going: Bool, as piece: Item.ID) {
        guard let work, !work.isNull else { return }
        let ended = work["state"]?["since"]?.number ?? stopped?["at"]?.number
        stretches[piece] = Stretch(from: began, to: going ? nil : Date(timeIntervalSince1970: ended ?? began.timeIntervalSince1970))
    }

    /// The pieces of the main chat with each event's card under the answer it was set up in:
    /// the newest answer whose work went on at the moment the event was set up. A card is a
    /// piece of its own, named by its answer and its event, so the answer stays what it is.
    /// An event that was set up during no answer among the pieces has no card here.
    func carded(_ pieces: [Item]) -> [Item] {
        let plain = pieces.filter { !$0.id.value.contains(EventCards.mark) }
        guard !events.isEmpty || !undone.isEmpty else { return plain }
        // Each answer with the stretch of its work. One that was left open ends where the
        // next one began: only the newest answer can still be going.
        let worked = plain.filter { stretches[$0.id] != nil && !EventCards.isPersons($0) }
        var spans: [(piece: Item.ID, from: Date, to: Date)] = []
        for (index, piece) in worked.enumerated() {
            guard let stretch = stretches[piece.id] else { continue }
            let next = index + 1 < worked.count ? stretches[worked[index + 1].id]?.from : nil
            spans.append((piece.id, stretch.from, stretch.to ?? next ?? .distantFuture))
        }
        var under: [Item.ID: [EventCard]] = [:]
        for (event, gone) in events.map({ ($0, false) }) + undone.map({ ($0, true) }) {
            guard let at = event.setUp,
                  let answer = spans.last(where: { at >= $0.from - EventCards.slack && at <= $0.to + EventCards.slack })
            else { continue }
            under[answer.piece, default: []].append(card(of: event, gone: gone))
        }
        guard !under.isEmpty else { return plain }
        return plain.flatMap { piece -> [Item] in
            [piece] + (under[piece.id] ?? []).map { card in
                Item(id: .init(piece.id.value + EventCards.mark + card.automation.id.value), at: piece.at, .event(card), task: piece.task)
            }
        }
    }

    /// An event's card: what it is, what its person may press, and what they pressed here.
    private func card(of event: Kept, gone: Bool) -> EventCard {
        let id = event.automation.id
        var choices: [Choice] = []
        if !gone {
            if !showing.contains(id) { choices.append(Choice(id: .init(EventCards.show), title: AssistantWords.showAnswer.said)) }
            // Only an event that may act alone can be told to ask each time.
            if event.automation.actsAlone { choices.append(Choice(id: .init(EventCards.ask), title: AssistantWords.askEachTime.said)) }
            choices.append(Choice(id: .init(EventCards.undo), title: AssistantWords.undo.said))
        }
        var card = EventCard(text: "", automation: event.automation, question: EventCards.question(of: id), choices: choices)
        card.shown = !gone && showing.contains(id) ? EventCards.keeps(event) : nil
        card.settled = gone ? .undone : asksNow.contains(id) && !event.automation.actsAlone ? .asksEachTime : nil
        return card
    }

    /// What an event keeps for its answers, as Markdown: what the assistant is told to do, how
    /// it writes, what it keeps in mind, and the newest answer its person accepted. Where it
    /// keeps none of the last three, one sentence says so.
    static func keeps(_ event: Kept) -> String {
        func clean(_ words: String?) -> String { (words ?? "").trimmingCharacters(in: .whitespacesAndNewlines) }
        let kept: [(AssistantWords, String)] = [
            (.keptStyle, clean(event.note?["style"]?.string)),
            (.keptGuidance, clean(event.note?["guidance"]?.string)),
            (.keptAccepted, clean((event.note?["accepted"]?.array ?? []).compactMap(\.string).last)),
        ]
        var said = ([(AssistantWords.keptTold, clean(event.told))] + kept).filter { !$0.1.isEmpty }.map { "**\($0.0.said)**\n\n\($0.1)" }
        if kept.allSatisfy({ $0.1.isEmpty }) { said.append(AssistantWords.keptNoAnswer.said) }
        return said.joined(separator: "\n\n")
    }

    private static func isPersons(_ piece: Item) -> Bool {
        if case .message(let said) = piece.content, said.author == .person { return true }
        return false
    }
}
