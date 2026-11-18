import AssistantCore
import Foundation
import TiloLive

// What the app makes of the person's events (`tilo.events` on their server): the list the
// Tasks panel shows, and the card an event stands as in the main chat, under the answer
// during which it was set up. An event is its person's server's: nothing here keeps one, and
// a card says only what its event says.

/// The person's events as their server last listed them, and where each stands as a card.
struct EventCards {
    /// One event as the person's server answers it.
    struct Kept: Equatable {
        var automation: Automation
        /// When it was set up, by its server's clock.
        var setUp: Date?
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

    private(set) var events: [Kept] = []
    private var stretches: [Item.ID: Stretch] = [:]

    /// What the Tasks panel lists: every event, in the server's order.
    var automations: [Automation] { events.map(\.automation) }

    /// Take the events as the server listed them (`list` of `tilo.events`).
    mutating func take(_ listed: [JSON]) {
        events = listed.compactMap(EventCards.kept)
    }

    /// One event of the server's list. Nil for one that names no id.
    static func kept(_ event: JSON) -> Kept? {
        guard let id = event["id"]?.string else { return nil }
        let scheduled = (event["sort"]?.string ?? event["sort"]?["$variant"]?.string) != "event"
        let when = event["when"]?.string ?? ""
        let automation = Automation(
            id: .init(id), sort: scheduled ? .scheduled : .event, title: event["name"]?.string ?? id,
            summary: scheduled ? when : event["instruction"]?.string ?? when,
            next: event["nextAtMs"]?.number.map { Date(timeIntervalSince1970: $0 / 1000) },
            once: when.lowercased().hasPrefix("once"), filter: event["onlyIf"]?.string,
            threshold: event["every"]?.number.map { Int($0) }, note: nil,
            actsAlone: !(event["permits"]?.array ?? []).isEmpty, isOn: event["on"]?.bool ?? true)
        return Kept(automation: automation, setUp: event["setUpAtMs"]?.number.map { Date(timeIntervalSince1970: $0 / 1000) })
    }

    /// The question an event's card answers under.
    static func question(of event: Automation.ID) -> Question.ID {
        .init("event:" + event.value)
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
        guard !events.isEmpty else { return plain }
        // Each answer with the stretch of its work. One that was left open ends where the
        // next one began: only the newest answer can still be going.
        let worked = plain.filter { stretches[$0.id] != nil && !EventCards.isPersons($0) }
        var spans: [(piece: Item.ID, from: Date, to: Date)] = []
        for (index, piece) in worked.enumerated() {
            guard let stretch = stretches[piece.id] else { continue }
            let next = index + 1 < worked.count ? stretches[worked[index + 1].id]?.from : nil
            spans.append((piece.id, stretch.from, stretch.to ?? next ?? .distantFuture))
        }
        var under: [Item.ID: [Kept]] = [:]
        for event in events {
            guard let at = event.setUp,
                  let answer = spans.last(where: { at >= $0.from - EventCards.slack && at <= $0.to + EventCards.slack })
            else { continue }
            under[answer.piece, default: []].append(event)
        }
        guard !under.isEmpty else { return plain }
        return plain.flatMap { piece -> [Item] in
            [piece] + (under[piece.id] ?? []).map { event in
                Item(id: .init(piece.id.value + EventCards.mark + event.automation.id.value), at: piece.at,
                     .event(EventCard(text: "", automation: event.automation,
                                      question: EventCards.question(of: event.automation.id), choices: [])),
                     task: piece.task)
            }
        }
    }

    private static func isPersons(_ piece: Item) -> Bool {
        if case .message(let said) = piece.content, said.author == .person { return true }
        return false
    }
}
