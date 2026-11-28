#if os(iOS)
import ActivityKit
import ShellCore
import SwiftUI
import WidgetKit
struct TiloLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: TiloLiveTurn.self) { context in
            LiveCard(state: context.state).widgetURL(LiveActivityLink.of(context.state))
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    LiveRing(state: context.state).padding(.leading, 4)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    LivePhaseLabel(state: context.state).font(.caption).padding(.trailing, 4)
                }
                DynamicIslandExpandedRegion(.center) {
                    Text(LiveActivityWords.opened(context.state)?.asked ?? LiveWords.workingWords.said)
                        .font(.headline).lineLimit(1)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    VStack(alignment: .leading, spacing: 4) {
                        if let said = LiveActivityWords.opened(context.state)?.words, !said.isEmpty {
                            Text(said).font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
                        }
                        if let progress = context.state.progress, !context.state.phase.isOver {
                            ProgressView(value: progress).tint(.accentColor)
                        }
                    }.frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 4)
                }
            } compactLeading: {
                LiveRing(state: context.state)
            } compactTrailing: {
                if let progress = context.state.progress, !context.state.phase.isOver {
                    Text("\(context.state.step)/\(context.state.steps)")
                        .monospacedDigit().foregroundStyle(.secondary).font(.caption2)
                } else {
                    LivePhaseLabel(state: context.state).font(.caption2)
                }
            } minimal: {
                LiveRing(state: context.state)
            }
            .widgetURL(LiveActivityLink.of(context.state))
        }
    }
}
private struct LiveCard: View {
    let state: LiveTurnState
    var body: some View {
        let words = LiveActivityWords.opened(state)
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                LiveRing(state: state)
                WidgetHeading(words: words?.from ?? LiveWords.assistant.said)
                Spacer(minLength: 8)
                LivePhaseLabel(state: state).font(.caption.weight(.medium))
            }
            Text(words?.asked ?? LiveWords.workingWords.said).font(.headline).lineLimit(2)
            if let said = words?.words, !said.isEmpty {
                Text(said).font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
            }
            if let progress = state.progress, !state.phase.isOver {
                ProgressView(value: progress).tint(.accentColor)
            }
        }
        .padding(.horizontal, 16).padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}
private struct LiveRing: View {
    let state: LiveTurnState
    var body: some View {
        let tint: Color = switch state.phase {
        case .asking: .orange
        case .answered: .green
        case .failed: .red
        case .working, .stopped, .cancelled: .accentColor
        }
        Circle().strokeBorder(Color.secondary.opacity(0.25), lineWidth: 1.5)
            .overlay {
                if let progress = state.progress {
                    Circle().trim(from: 0, to: progress)
                        .stroke(tint, style: StrokeStyle(lineWidth: 1.5, lineCap: .round)).rotationEffect(.degrees(-90))
                } else if !state.phase.isOver {
                    Circle().fill(tint).frame(width: 5, height: 5)
                }
            }
            .frame(width: 18, height: 18).accessibilityHidden(true)
    }
}
private struct LivePhaseLabel: View {
    let state: LiveTurnState
    var body: some View {
        switch state.phase {
        case .working: Text(LiveWords.working.said).foregroundStyle(.secondary)
        case .asking: Text(LiveWords.asking.said).foregroundStyle(.orange)
        case .answered: Text(LiveWords.answered.said).foregroundStyle(.green)
        case .stopped: Text(LiveWords.stopped.said).foregroundStyle(.secondary)
        case .failed: Text(LiveWords.failed.said).foregroundStyle(.red)
        case .cancelled: Text(LiveWords.cancelled.said).foregroundStyle(.secondary)
        }
    }
}
enum LiveWords: String {
    case assistant, working, asking, answered, stopped, failed, cancelled
    case workingWords = "working-words"

    var said: String { NSLocalizedString(rawValue, value: english, comment: "") }
    private var english: String {
        let words = ["assistant": "Assistant", "working": "Working", "asking": "Needs you", "answered": "Answered",
                     "stopped": "Stopped", "failed": "Could not finish", "cancelled": "Stopped by you",
                     "working-words": "Your assistant is working."]
        return words[rawValue] ?? rawValue
    }
}
private enum LiveActivityWords {
    static func opened(_ state: LiveTurnState) -> LiveTurnWords? {
        guard let enc = state.enc, let sealed = Data(base64Encoded: enc),
              let keys = KeychainClosedKeys(info: Bundle.main.infoDictionary),
              let key = keys.read(), let plain = key.open(sealed) else { return nil }
        return try? JSONDecoder().decode(LiveTurnWords.self, from: plain)
    }
}
private enum LiveActivityLink {
    static func of(_ state: LiveTurnState) -> URL? {
        guard let opens = LiveActivityWords.opened(state)?.opens else { return nil }
        return URL(string: opens)
    }
}
#endif
