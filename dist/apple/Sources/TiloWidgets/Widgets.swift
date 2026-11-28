#if canImport(WidgetKit)
import ShellCore
import SwiftUI
import WidgetKit

/// What the widget shows at one moment. The shell writes the snapshot whenever the page's notices
/// change and asks the widget to draw again, so nothing waits between those changes.
struct ToldEntry: TimelineEntry {
    let date: Date
    let snapshot: WidgetSnapshot
}

struct ToldProvider: TimelineProvider {
    func placeholder(in context: Context) -> ToldEntry {
        ToldEntry(date: .now,
                  snapshot: WidgetSnapshot(items: [.init(title: "Scout needs you")]))
    }

    func getSnapshot(in context: Context, completion: @escaping (ToldEntry) -> Void) {
        completion(current())
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<ToldEntry>) -> Void) {
        completion(Timeline(entries: [current()], policy: .never))
    }

    private func current() -> ToldEntry {
        ToldEntry(date: .now, snapshot: WidgetSnapshot.load(in: WidgetSnapshot.group()))
    }
}

/// The home screen's card of what waits for the person: a small one and a medium one. A tap opens
/// where the page would have gone.
struct ToldWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "so.tilo.told", provider: ToldProvider()) { entry in
            ToldCard(entry: entry)
        }
        .configurationDisplayName("Told")
        .description("What waits for you.")
        .supportedFamilies([.systemSmall, .systemMedium])
    }
}

/// A heading and a line for each thing that waits. The words are the host's (`words_outside`), so
/// a phone on a table shows no more than its lock screen. A tap opens where the page would go.
struct ToldCard: View {
    let entry: ToldEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            WidgetHeading(words: "Waiting for you")
            if entry.snapshot.items.isEmpty {
                Text("Nothing waits for you").font(.caption).foregroundStyle(.secondary)
            }
            ForEach(Array(entry.snapshot.items.enumerated()), id: \.offset) { _, item in
                if let url = URL(string: item.link), WidgetLink.activation(in: url) != nil {
                    Link(destination: url) { Line(item: item) }
                } else {
                    Line(item: item)
                }
            }
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .containerBackground(for: .widget) { Rectangle().fill(.background) }
    }

}

/// A small uppercase heading, shared by the cards.
struct WidgetHeading: View {
    let words: String

    var body: some View {
        Text(words)
            .font(.caption2.weight(.semibold))
            .tracking(1.2)
            .textCase(.uppercase)
            .foregroundStyle(.secondary)
    }
}

/// One thing and what it is about, in the card's own type.
private struct Line: View {
    let item: WidgetSnapshot.Item

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(item.title).font(.footnote.weight(.semibold)).lineLimit(1)
            if !item.body.isEmpty {
                Text(item.body).font(.caption2).foregroundStyle(.secondary).lineLimit(2)
            }
        }
    }
}

@main
struct TiloWidgets: WidgetBundle {
    var body: some Widget {
        ToldWidget()
        #if os(iOS)
        TiloLiveActivity()
        #endif
    }
}
#else
@main
enum NotThisPlatform {
    static func main() {}
}
#endif
