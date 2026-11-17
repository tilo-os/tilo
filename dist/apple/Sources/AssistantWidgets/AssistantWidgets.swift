#if os(iOS)
import ActivityKit
import AssistantCore
import AssistantUI
import SwiftUI
import WidgetKit

/// The iPhone's widget extension. It holds one thing: the assistant's live activity.
@main
struct AssistantWidgets: WidgetBundle {
    init() {
        Typeface.registerBundled()
    }

    var body: some Widget {
        AssistantLiveActivity()
    }
}

/// The live activity on the lock screen and in the island: a reminder counting down, a brief
/// before something begins, or work the person waits on. `LiveActivityPlan` says which earn one.
struct AssistantLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: AssistantActivity.self) { context in
            LockScreen(facts: context.state)
                .widgetURL(LiveLink.url(for: context.attributes.name))
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Mark(side: 24).padding(.leading, 4)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    LiveStanding(facts: context.state).font(.small12).padding(.trailing, 4)
                }
                DynamicIslandExpandedRegion(.center) {
                    Text(context.state.title).font(.tilo(15, .semibold)).lineLimit(1)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    VStack(alignment: .leading, spacing: 3) {
                        if let detail = context.state.detail {
                            Text(detail).font(.line135).foregroundStyle(.secondary).lineLimit(2)
                        }
                        ForEach(Array(context.state.points.prefix(2).enumerated()), id: \.offset) { _, point in
                            Text("• \(point)").font(.line135).lineLimit(1)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 4)
                }
            } compactLeading: {
                Mark(side: 16)
            } compactTrailing: {
                LiveStanding(facts: context.state).font(.small12)
            } minimal: {
                Mark(side: 16)
            }
            .widgetURL(LiveLink.url(for: context.attributes.name))
        }
    }
}

/// The lock screen's card, on the drawing's own ground in light and dark.
private struct LockScreen: View {
    let facts: LiveFacts
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        LiveFactsView(facts: facts)
            .lineHeight(Typeface.line)
            .activityBackgroundTint(Token.content.color(scheme))
            .activitySystemActionForegroundColor(Token.ink.color(scheme))
    }
}
#else
@main
enum NotThisPlatform {
    static func main() {}
}
#endif
