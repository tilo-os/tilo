import AssistantCore
import SwiftUI

/// What the person was told while no device of theirs was looking: one message at the top of
/// the new stretch of the main chat. What waits for them stands first, then what was done
/// without asking, then the rest. A line leads to the piece it is about.
struct SinceLeftCard: View {
    let since: SinceLeft
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording

    /// How many lines of one kind stand there before the rest is counted.
    static let most = 3

    var body: some View {
        CardBox(name: AssistantWords.sinceLeft.said, note: wording.mark(since.left)) {
            lines(AssistantWords.sinceWaits.said, since.waits, isWaiting: true)
            lines(AssistantWords.sinceDidAlone.said, since.didAlone, isWaiting: false)
            lines(AssistantWords.sinceCameIn.said, since.cameIn, isWaiting: false)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(AssistantWords.sinceLeft.said)
    }

    @ViewBuilder
    private func lines(_ name: String, _ all: [SinceLeft.Line], isWaiting: Bool) -> some View {
        if !all.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Fine(words: name).accessibilityAddTraits(.isHeader)
                    if isWaiting {
                        Spacer(minLength: 0)
                        Button {
                            model.openFirstWaiting(preferring: all)
                        } label: {
                            HStack(spacing: 5) {
                                Circle().fill(Token.you).frame(width: 6, height: 6)
                                Text(AssistantWords.waitingCount.said(["count": String(all.count)]))
                                    .font(.tilo(11.5, .semibold)).foregroundStyle(Token.ink)
                            }
                            .padding(.horizontal, 9).padding(.vertical, 3)
                            .background(Token.youTint, in: Capsule())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(AssistantWords.waitingCount.said(["count": String(all.count)]))
                        .accessibilityHint(AssistantWords.waitsForAnswer.said)
                    }
                }
                ForEach(all.prefix(SinceLeftCard.most)) { line in
                    if let item = line.item {
                        Button { model.show(item, in: .main) } label: { said(line) }.buttonStyle(.plain)
                    } else if isWaiting {
                        Button { model.openFirstWaiting(preferring: all) } label: { said(line) }.buttonStyle(.plain)
                    } else {
                        said(line)
                    }
                }
                if all.count > SinceLeftCard.most {
                    Fine(words: AssistantWords.sinceMore.said(["count": String(all.count - SinceLeftCard.most)]))
                }
            }
        }
    }

    private func said(_ line: SinceLeft.Line) -> some View {
        Flow(spacing: 7, lineSpacing: 2) {
            Text(line.words).font(.body14).foregroundStyle(Token.ink).lineLimit(2).multilineTextAlignment(.leading)
            if line.isImportant { ImportantMark() }
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }
}
