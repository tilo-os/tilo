import AssistantCore
import Foundation
import TiloLive

/// The assistant's day as the assistant view says it, read from the assistant's own item among
/// the workspace's agents (Chat's `chat.agent`).
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
        usage.isUnknownPrice = isTodays && AssistantDay.unknownPrice(in: spent)
        usage.causes = isTodays ? AssistantDay.causes(in: spent) : []
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

    /// Whether a spent object reports calls whose model pricing is unknown.
    private static func unknownPrice(in spent: JSON?) -> Bool {
        if spent?["is_unknown_price"]?.bool == true || spent?["unknown_price"]?.bool == true {
            return true
        }
        return (count(spent?["unknown_calls"]) ?? 0) > 0
    }

    /// Per-cause breakdown of today's usage, from either an array (`causes`) or a map (`by_cause`).
    private static func causes(in spent: JSON?) -> [Supervision.Usage.CauseSpend] {
        guard let raw = spent?["causes"] ?? spent?["by_cause"] else { return [] }
        if let list = raw.array {
            return list.compactMap { entry in
                guard let kind = entry["kind"]?.string?.trimmingCharacters(in: .whitespacesAndNewlines),
                      !kind.isEmpty
                else { return nil }
                return causeSpend(kind: kind, entry: entry)
            }
        }
        if let fields = raw.object {
            let order = ["turn", "event", "look", "reflection", "shadow"]
            let sortedKeys = fields.keys.sorted { lhs, rhs in
                let leftRank = order.firstIndex(of: lhs.lowercased()) ?? order.count
                let rightRank = order.firstIndex(of: rhs.lowercased()) ?? order.count
                if leftRank != rightRank { return leftRank < rightRank }
                return lhs < rhs
            }
            return sortedKeys.compactMap { key in
                let trimmed = key.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !trimmed.isEmpty, let entry = fields[key] else { return nil }
                return causeSpend(kind: trimmed, entry: entry)
            }
        }
        return []
    }

    private static func causeSpend(kind: String, entry: JSON) -> Supervision.Usage.CauseSpend {
        let tokens: Int
        if let direct = count(entry["tokens"]) {
            tokens = direct
        } else {
            let input = count(entry["input_tokens"]) ?? 0
            let output = count(entry["output_tokens"]) ?? 0
            tokens = input + output
        }
        let costCents: Int
        if let cents = count(entry["cost_cents"]) {
            costCents = cents
        } else if let micros = count(entry["cost_micros"]) {
            costCents = (micros + 5_000) / 10_000
        } else {
            costCents = 0
        }
        return Supervision.Usage.CauseSpend(
            kind: kind,
            tokens: tokens,
            costCents: costCents,
            isUnknownPrice: unknownPrice(in: entry)
        )
    }
}

/// Transparency details read from server JSON for the assistant view: dated learned facts with
/// confirmation authority, muted activity offers, and workspace data-destination policies.
enum AssistantTransparency {
    /// Parse a learned fact item, including optional date window and confirmation authority.
    static func learned(from fact: JSON, idPrefix: String = "") -> Supervision.Learned? {
        guard let id = fact["id"]?.string, let text = fact["text"]?.string else { return nil }
        let confirmed = fact["confirmed"]?.bool == true
            || fact["authority"]?.string?.lowercased() == "person"
        let window = factWindow(from: fact)
        return Supervision.Learned(id: .init(idPrefix + id), text: text, window: window, confirmed: confirmed)
    }

    /// Parse muted offer kinds from a server list of strings or offer objects.
    static func mutedOffers(from json: JSON?) -> [String] {
        guard let items = json?.array else { return [] }
        return items.compactMap { item -> String? in
            if let text = item.string?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty {
                return readableOffer(text)
            }
            for key in ["label", "prompt", "title", "kind_id", "kindId", "kind"] {
                if let text = item[key]?.string?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty {
                    return readableOffer(text)
                }
            }
            return nil
        }
    }

    /// Parse a workspace data-destination policy note from either a plain string or a serialized
    /// `DestinationPolicy` JSON value. Returns nil when unrestricted (`Any` or absent).
    static func destinationNote(from json: JSON?) -> String? {
        guard let json, !json.isNull else { return nil }
        typealias W = AssistantWords
        if let raw = json.string?.trimmingCharacters(in: .whitespacesAndNewlines), !raw.isEmpty {
            switch raw {
            case "Any", "any": return nil
            case "NoneOutsideServer", "none_outside_server": return W.destinationServerOnly.said
            default: return raw
            }
        }
        if let note = (json["note"]?.string ?? json["summary"]?.string)?
            .trimmingCharacters(in: .whitespacesAndNewlines), !note.isEmpty {
            return note
        }
        if let allowed = (json["Only"] ?? json["only"])?.array {
            let names = allowed.compactMap(describeDestination)
            guard !names.isEmpty else { return W.destinationNoneAllowed.said }
            return W.destinationOnly.said(["places": names.joined(separator: ", ")])
        }
        return nil
    }

    private static func factWindow(from fact: JSON) -> String? {
        if let explicit = fact["window"]?.string?.trimmingCharacters(in: .whitespacesAndNewlines), !explicit.isEmpty {
            return explicit
        }
        let relative = (fact["relative"]?.string ?? fact["relativeWindow"]?.string ?? fact["relative_window"]?.string)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let startsOn = (fact["startsOn"]?.string ?? fact["starts_on"]?.string)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let endsOn = (fact["endsOn"]?.string ?? fact["ends_on"]?.string)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let range = formatDateRange(startsOn: startsOn, endsOn: endsOn)
        let parts = [relative, range].compactMap { value -> String? in
            guard let value, !value.isEmpty else { return nil }
            return value
        }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    private static func formatDateRange(startsOn: String?, endsOn: String?) -> String? {
        let start = startsOn.flatMap(formatIsoDay)
        let end = endsOn.flatMap(formatIsoDay)
        switch (start, end) {
        case let (first?, last?): return "\(first) – \(last)"
        case let (first?, nil): return first
        case let (nil, last?): return last
        case (nil, nil): return nil
        }
    }

    private static func formatIsoDay(_ iso: String) -> String? {
        let parts = iso.split(separator: "-")
        guard parts.count == 3,
              let year = Int(parts[0]),
              let month = Int(parts[1]), (1...12).contains(month),
              let day = Int(parts[2]), (1...31).contains(day)
        else { return iso.isEmpty ? nil : iso }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC") ?? .current
        var components = DateComponents()
        components.year = year
        components.month = month
        components.day = day
        guard let date = calendar.date(from: components) else { return iso }
        let style = Date.FormatStyle(locale: AssistantWords.language, calendar: calendar, timeZone: calendar.timeZone)
            .day(.defaultDigits).month(.abbreviated)
        return date.formatted(style)
    }

    private static func readableOffer(_ value: String) -> String {
        let parts = value.split(separator: ":", omittingEmptySubsequences: false)
        if value.contains("::"), parts.count >= 5 {
            let pieces = value.components(separatedBy: "::")
            if pieces.count == 3 {
                let app = pieces[0].trimmingCharacters(in: .whitespacesAndNewlines)
                let verb = pieces[1].trimmingCharacters(in: .whitespacesAndNewlines)
                let target = pieces[2].trimmingCharacters(in: .whitespacesAndNewlines)
                if !app.isEmpty, !verb.isEmpty, !target.isEmpty {
                    let cleanVerb = verb.replacingOccurrences(of: "_", with: " ")
                    let cleanTarget = target.replacingOccurrences(of: "_", with: " ")
                    return "\(cleanVerb.prefix(1).uppercased() + cleanVerb.dropFirst()) \(cleanTarget) in \(app)"
                }
            }
        }
        return value
    }

    private static func describeDestination(_ entry: JSON) -> String? {
        typealias W = AssistantWords
        if let raw = entry.string?.trimmingCharacters(in: .whitespacesAndNewlines), !raw.isEmpty {
            switch raw {
            case "ThisServer", "this_server": return W.destinationServerOnly.said
            case "PrivateNetwork", "private_network": return W.destinationPrivateNetwork.said
            default: return raw
            }
        }
        for key in ["Provider", "provider", "Endpoint", "endpoint"] {
            if let name = entry[key]?.string?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty {
                return name
            }
        }
        return nil
    }
}

/// Where the assistant and its sub-agents work, as the assistant view says it, read from the
/// workspace's agents as Chat keeps them. A place is named only where an agent's own item puts
/// that agent: nothing is assumed of a place no item says.
///
/// An agent's item says whether the workspace's server works it, around the clock and with
/// every device of its person off, or its keeper's own devices do. Where it says neither — an
/// item older than the field, or one a server that keeps no such word listed — neither is said
/// for certain, and the line says so rather than taking an unstated place for either. A sub-agent
/// is worked by the server's own loop, unless its item holds what a coding agent's holds
/// (`sub.coding`): then a coding harness works it in the workspace's machine, and the item says
/// the harness's name and the folder it was started in.
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
        // What the item says of where its agent works is read as one of three things and never as
        // two: it says the server does, it says its keeper's own devices do, or it says nothing,
        // which is no word for either.
        let serverFlag = own[TiloSource.answeredByServer]
        let workedOnServer = serverFlag?.bool == true
        let workedOnOwnDevices = serverFlag?.bool == false
        if workedOnServer {
            let detail = onServer == 0 ? W.placeAlways.said : W.counted(onServer, one: .placeAlwaysWithOne, many: .placeAlwaysWithMany)
            places.append(.init(name: server, detail: detail))
        } else if workedOnOwnDevices {
            places.append(.init(name: W.placeOwnDevices.said, detail: W.placeWhileOpen.said))
            if onServer > 0 {
                places.append(.init(name: server, detail: W.counted(onServer, one: .placeSubAgentOne, many: .placeSubAgentMany)))
            }
        } else {
            // Where the item says nothing of where it works, the line promises no place at all: it
            // does not take an unstated word for its keeper's own devices, which would say the
            // assistant stops when every one of them closes. What sub-agents are at work is still
            // said, as every sub-agent that says nothing of a coding harness is worked by the
            // server's own loop.
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
