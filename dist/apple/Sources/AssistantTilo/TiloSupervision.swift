import AssistantCore
import Foundation
import TiloLive

/// The assistant's day as the assistant view says it, read from the assistant's own item among
/// the workspace's agents (Assistant's `tilo.workspace.agent`).
///
/// Every figure is one the person's server counted. Nothing here counts anything: what their
/// server keeps no count of is said as not counted, never as nought.
///
/// A server counts a day under its date (`spent.day`, `YYYY-MM-DD`) and starts again at the
/// first charge of a later day; until then the item still holds the day before. So what an
/// item holds is today's only while its date is not behind this device's, which is how the
/// server reads it too: a day it has not charged yet starts at nought. A date ahead of this
/// device's is a server whose own day turned earlier, and is its today.
@MainActor
final class AssistantDay {
    /// The time it is on this device.
    var now: @MainActor () -> Date = { Date() }
    /// The calendar a day is dated by: the one Tilo's servers date theirs by, in this device's zone.
    var calendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = .autoupdatingCurrent
        return calendar
    }()
    /// The one wait for this device's day to turn, and the moment it waits for.
    private var turn: Task<Void, Never>?
    private var turnsAt: Date?

    /// Today as the workspace's agents date a day: `2026-11-17`.
    var today: String {
        let day = calendar.dateComponents([.year, .month, .day], from: now())
        return String(format: "%04d-%02d-%02d", day.year ?? 0, day.month ?? 0, day.day ?? 0)
    }

    /// What the assistant used today and the most it may spend, as its own item says them.
    /// What the view held `before` keeps what no item says: the currency.
    func usage(of item: JSON, before: Supervision.Usage) -> Supervision.Usage {
        var usage = before
        let spent = item["spent"]
        let isTodays = (spent?["day"]?.string).map { $0 >= today } ?? false
        usage.spentTodayMicros = isTodays ? AssistantDay.count(spent?["cost_micros"]) ?? 0 : 0
        // A count of messages or of tokens is shown only where the item holds one. Where it does,
        // a day the server has not charged yet is a day it counted none on.
        usage.messagesToday = AssistantDay.count(spent?["messages"]).map { isTodays ? $0 : 0 }
        usage.tokensToday = AssistantDay.count(spent?["tokens"]).map { isTodays ? $0 : 0 }
        usage.dailyLimitMicros = AssistantDay.count(item["spend_limit_micros"])
        return usage
    }

    /// Have `read` done once this device's day has turned: a day turns with nothing written
    /// anywhere, so nothing else would say that what the item holds is the day before's now.
    /// Asked for again before then, it waits for the same moment once.
    func atNextDay(_ read: @escaping @MainActor () async -> Void) {
        let time = now()
        guard let next = calendar.date(byAdding: .day, value: 1, to: calendar.startOfDay(for: time)), next != turnsAt else { return }
        turnsAt = next
        turn?.cancel()
        let wait = max(0, next.timeIntervalSince(time))
        turn = Task { [weak self] in
            try? await Task.sleep(for: .seconds(wait) + .milliseconds(50))
            guard !Task.isCancelled, let self else { return }
            self.turnsAt = nil
            await read()
        }
    }

    /// A count as an item says one: a whole number, not below nought. Nil where it says none.
    private static func count(_ said: JSON?) -> Int? {
        guard let number = said?.number, number >= 0 else { return nil }
        return Int(exactly: number.rounded())
    }
}

/// Where the assistant and its sub-agents work, as the assistant view says it, read from the
/// workspace's agents as Assistant keeps them. A place is named only where an agent's own item puts
/// that agent: nothing is assumed of a place no item says.
///
/// An agent's item says whether the workspace's server works it, around the clock and with
/// every device of its person off, or its keeper's own devices do. A sub-agent is worked by
/// the server's own loop, unless its item holds what a coding agent's holds (`sub.coding`):
/// then a coding harness works it in the workspace's machine, and the item says the harness's
/// name and the folder it was started in.
@MainActor
enum AssistantPlaces {
    /// The places, the assistant's own first. A sub-agent counts while it is at work: one that
    /// is done, failed or stopped works nowhere.
    static func places(among all: [JSON], assistant: String?, keptBy person: String) -> [Supervision.Place] {
        // An agent that is no longer active works nowhere, and neither does what it started.
        guard let assistant, let own = all.first(where: { $0["id"]?.string == assistant }), variant(own["state"]) == "active"
        else { return [] }
        var onServer = 0
        var coders: [String] = []
        for agent in all {
            guard let sub = agent["sub"], !sub.isNull, ChatMember(agent["keeper"])?.id == person, variant(agent["state"]) == "active",
                  ["working", "needs_person"].contains(variant(sub["stands"]) ?? "working")
            else { continue }
            if let coding = sub["coding"], !coding.isNull {
                coders.append(coder(coding))
            } else {
                onServer += 1
            }
        }
        typealias W = AssistantWords
        var places: [Supervision.Place] = []
        if own[TiloSource.answeredByServer]?.bool == true {
            let detail = onServer == 0 ? W.placeAlways.said : W.counted(onServer, one: .placeAlwaysWithOne, many: .placeAlwaysWithMany)
            places.append(.init(name: server, detail: detail))
        } else {
            places.append(.init(name: W.placeOwnDevices.said, detail: W.placeWhileOpen.said))
            if onServer > 0 {
                places.append(.init(name: server, detail: W.counted(onServer, one: .placeSubAgentOne, many: .placeSubAgentMany)))
            }
        }
        if !coders.isEmpty {
            var said = Array(coders.prefix(2))
            if coders.count > 2 { said.append(W.moreCount.said(["count": String(coders.count - 2)])) }
            places.append(.init(name: machine, detail: said.joined(separator: " · ")))
        }
        return places
    }

    /// The two places by name, as a line begins with them.
    private static var server: String { AssistantWords.placeServer.said }
    private static var machine: String { AssistantWords.placeMachine.said }

    /// A coding agent as a person reads one: its harness by the name its item says, and the
    /// folder it was started in.
    private static func coder(_ coding: JSON) -> String {
        let name = coding["harness_name"]?.string.flatMap { $0.isEmpty ? nil : $0 } ?? AssistantWords.codingAgent.said
        let folder = [coding["repository"]?.string, coding["directory"]?.string].compactMap { $0 }.first { !$0.isEmpty }
        return folder.map { AssistantWords.coderIn.said(["name": name, "folder": $0]) } ?? name
    }

    /// An enum's case, however the read spells it: the bare word, or `{"$variant": word}`.
    private static func variant(_ value: JSON?) -> String? {
        value?.string ?? value?["$variant"]?.string
    }
}
