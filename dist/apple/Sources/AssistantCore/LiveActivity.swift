import Foundation

/// What a live activity shows at one moment. It is also what Apple's service carries to the
/// phone when the host tells an activity its news, so it stays small and plain.
public struct LiveFacts: Codable, Hashable, Sendable {
    public var kind: Kind
    public var state: State
    public var title: String
    /// One line under the title.
    public var detail: String?
    /// A brief's points, three at most.
    public var points: [String]
    /// When it comes or begins: the activity counts down to it.
    public var until: Date?
    public var isImportant: Bool

    public init(kind: Kind, state: State = .running, title: String, detail: String? = nil,
                points: [String] = [], until: Date? = nil, isImportant: Bool = false) {
        self.kind = kind
        self.state = state
        self.title = title
        self.detail = detail
        self.points = points
        self.until = until
        self.isImportant = isImportant
    }

    public enum Kind: String, Codable, Sendable {
        /// Work that goes on and will come back to the person.
        case work
        /// A reminder that is about to come.
        case reminder
        /// A brief before something begins.
        case brief
    }

    public enum State: String, Codable, Sendable {
        case running
        case needsYou
        case done
    }
}

/// Which things earn a live activity.
///
/// A live activity is a promise: something is under way, it is worth a glance, and it has an
/// end. So three things earn one, and nothing that merely exists does:
///
/// - **A reminder about to come**: one that comes once, within the hour. The person asked for
///   it a moment ago and is waiting for it; it ends when it comes.
/// - **A brief before something begins**: its points stay in view while the time runs down, and
///   it ends when the thing begins.
/// - **Work the person is waiting on**: a task marked important, or a sub-agent the person
///   started themselves, from the moment it works until it is resolved. When it needs the
///   person it says so. The assistant's other work, which never stops, earns none: an activity
///   that is always there says nothing.
public enum LiveActivityPlan {
    /// How far ahead a reminder or a brief is shown counting down.
    public static let horizon: TimeInterval = 60 * 60

    /// The activities that should exist now, each under a name that stays the same while it lasts.
    public static func activities(tasks: [AssistantTask], subAgents: [SubAgent], automations: [Automation],
                                  items: [Item], now: Date) -> [String: LiveFacts] {
        var found: [String: LiveFacts] = [:]
        let soon = now...now.addingTimeInterval(horizon)
        for reminder in automations where reminder.sort == .scheduled && reminder.once && reminder.isOn {
            guard let next = reminder.next, next > now, soon.contains(next) else { continue }
            found["reminder-\(reminder.id)"] = LiveFacts(kind: .reminder, title: reminder.title, until: next)
        }
        for item in items {
            guard case .brief(let brief) = item.content, let begins = brief.begins, begins > now, soon.contains(begins)
            else { continue }
            found["brief-\(item.id)"] = LiveFacts(kind: .brief, title: brief.title, points: points(of: brief.text),
                                                  until: begins, isImportant: item.isImportant)
        }
        for task in tasks where task.isImportant && task.state != .resolved && task.state != .upNext {
            found["task-\(task.id)"] = LiveFacts(kind: .work, state: task.state == .inProgress ? .running : .needsYou,
                                                 title: task.title, detail: task.note, isImportant: true)
        }
        for agent in subAgents where agent.startedBy == .person {
            guard agent.state == .working || agent.state == .needsYou else { continue }
            found["sub-agent-\(agent.id)"] = LiveFacts(kind: .work, state: agent.state == .working ? .running : .needsYou,
                                                       title: agent.title, detail: agent.role.isEmpty ? nil : agent.role)
        }
        return found
    }

    /// Whether an activity may begin in this state. Work begins while it runs: work that is
    /// born needing the person is told by a notification, which has the answer on it.
    public static func mayBegin(_ facts: LiveFacts) -> Bool {
        facts.kind != .work || facts.state == .running
    }

    /// A brief's points: its list's entries, or its lines, three at most.
    static func points(of text: String) -> [String] {
        var entries: [String] = []
        var lines: [String] = []
        for block in Markdown.blocks(in: text) {
            switch block {
            case .bullets(let list), .numbered(_, let list): entries += list
            case .paragraph(let words), .quote(let words), .heading(_, let words): lines.append(words)
            case .code: break
            }
        }
        return (entries.isEmpty ? lines : entries).prefix(3).map(Markdown.plain)
    }
}

#if os(iOS)
import ActivityKit

/// The live activity's fixed part: the name `LiveActivityPlan` gave it.
public struct AssistantActivity: ActivityAttributes {
    public typealias ContentState = LiveFacts
    public var name: String

    public init(name: String) {
        self.name = name
    }
}
#endif
