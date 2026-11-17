import AssistantCore
import SwiftUI

/// What a live activity shows on the lock screen: the assistant's sign, how long is left or how
/// it stands, the Important mark, what it is, and a brief's points. Drawn like the drawing's
/// notifications, so the lock screen reads as one hand.
public struct LiveFactsView: View {
    private let facts: LiveFacts

    public init(facts: LiveFacts) {
        self.facts = facts
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Mark(side: 14)
                Text(AssistantWords.assistant.said).font(.small12).foregroundStyle(Token.ink3)
                Spacer(minLength: 8)
                LiveState(facts: facts).font(.small12)
            }
            if facts.isImportant { ImportantMark() }
            Text(facts.title).font(.tilo(15, .semibold)).foregroundStyle(Token.ink).lineLimit(2)
            if let detail = facts.detail {
                Text(detail).font(.line135).foregroundStyle(Token.ink2).lineLimit(2)
            }
            ForEach(Array(facts.points.enumerated()), id: \.offset) { _, point in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Circle().fill(Token.ink).frame(width: 4.5, height: 4.5)
                        .alignmentGuide(.firstTextBaseline) { $0[.bottom] + 3.5 }
                        .accessibilityHidden(true)
                    Text(point).lineLimit(2)
                }
                .font(.line135)
                .foregroundStyle(Token.ink)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// How an activity stands, in a few letters: the time left while it counts down, else whether
/// it works, needs the person, or is done.
public struct LiveState: View {
    private let facts: LiveFacts

    public init(facts: LiveFacts) {
        self.facts = facts
    }

    public var body: some View {
        if let until = facts.until, facts.state == .running, until > Date() {
            Text(timerInterval: Date()...until, countsDown: true)
                .monospacedDigit().multilineTextAlignment(.trailing).foregroundStyle(Token.ink2)
                .frame(maxWidth: 64, alignment: .trailing)
        } else {
            switch facts.state {
            case .running: Text(AssistantWords.working.said).foregroundStyle(Token.asst)
            case .needsYou: Text(AssistantWords.needsYou.said).foregroundStyle(Token.you)
            case .done: Text(AssistantWords.done.said).foregroundStyle(Token.on)
            }
        }
    }
}

/// Where a press on a live activity leads in the app.
public enum LiveLink {
    public static let scheme = "tilo-assistant"

    public static func url(for name: String) -> URL? {
        var parts = URLComponents()
        parts.scheme = scheme
        parts.host = "live"
        parts.path = "/" + name
        return parts.url
    }

    /// Open what an activity was about: its task in the panel, its sub-agent, its brief, its reminder.
    @MainActor
    public static func open(_ url: URL, in model: AssistantModel) {
        guard url.scheme == scheme, url.host == "live" else { return }
        let name = String(url.path.dropFirst())
        if name.hasPrefix("sub-agent-") {
            model.open(.subAgent(SubAgent.ID(String(name.dropFirst("sub-agent-".count)))))
        } else if name.hasPrefix("brief-") {
            model.show(Item.ID(String(name.dropFirst("brief-".count))))
        } else if name.hasPrefix("reminder-") {
            model.open(.main)
            model.showTasks(.scheduled)
        } else if name.hasPrefix("task-") {
            model.open(.main)
            model.showTasks(.active)
        }
    }
}
