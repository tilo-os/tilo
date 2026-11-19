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
        CardBox(name: AssistantWords.sinceLeft.said,
                note: wording.mark(since.left),
                emphasis: since.waits.isEmpty ? .quiet : .waiting) {
            VStack(alignment: .leading, spacing: 12) {
                waitingSection(since.waits)
                quietSection(AssistantWords.sinceDidAlone.said, since.didAlone, handledQuietly: true)
                quietSection(AssistantWords.sinceCameIn.said, since.cameIn, handledQuietly: false)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(AssistantWords.sinceLeft.said)
    }

    @ViewBuilder
    private func waitingSection(_ all: [SinceLeft.Line]) -> some View {
        if !all.isEmpty {
            VStack(alignment: .leading, spacing: 7) {
                HStack(spacing: 8) {
                    HStack(spacing: 6) {
                        Circle().fill(Token.you).frame(width: 6, height: 6).accessibilityHidden(true)
                        Text(AssistantWords.sinceWaits.said)
                            .font(.tilo(12, .semibold, relativeTo: .caption))
                            .foregroundStyle(Token.ink)
                            .accessibilityAddTraits(.isHeader)
                    }
                    Spacer(minLength: 0)
                    Button {
                        model.openFirstWaiting(preferring: all)
                    } label: {
                        HStack(spacing: 5) {
                            Circle().fill(Token.you).frame(width: 6, height: 6).accessibilityHidden(true)
                            Text(AssistantWords.waitingCount.said(["count": String(all.count)]))
                                .font(.tilo(11.5, .semibold))
                                .foregroundStyle(Token.ink)
                        }
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                        .background(Token.youTint, in: Capsule())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(AssistantWords.waitingCount.said(["count": String(all.count)]))
                    .accessibilityHint(AssistantWords.waitsForAnswer.said)
                }

                VStack(alignment: .leading, spacing: 6) {
                    ForEach(all.prefix(SinceLeftCard.most)) { line in
                        waitingRow(line, among: all)
                    }
                }

                if all.count > SinceLeftCard.most {
                    let more = AssistantWords.sinceMore.said(["count": String(all.count - SinceLeftCard.most)])
                    Button {
                        model.openFirstWaiting(preferring: all)
                    } label: {
                        Fine(words: more)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(more)
                    .accessibilityHint(AssistantWords.waitsForAnswer.said)
                }
            }
        }
    }

    private func waitingRow(_ line: SinceLeft.Line, among all: [SinceLeft.Line]) -> some View {
        Button {
            if let item = line.item {
                model.show(item, in: .main)
            } else {
                model.openFirstWaiting(preferring: all)
            }
        } label: {
            HStack(alignment: .center, spacing: 10) {
                Flow(spacing: 7, lineSpacing: 2) {
                    Text(line.words)
                        .font(.tilo(13.5, .medium))
                        .foregroundStyle(Token.ink)
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                    if line.isImportant { ImportantMark() }
                }
                Spacer(minLength: 4)
                Text(AssistantWords.open.said)
                    .font(.tilo(11.5, .semibold, relativeTo: .caption))
                    .foregroundStyle(Token.you)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .background(Token.youTint.opacity(0.65), in: Capsule())
                    .accessibilityHidden(true)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Token.content, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            .contentShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(spokenLabel(for: line))
        .accessibilityHint(AssistantWords.waitsForAnswer.said)
    }

    @ViewBuilder
    private func quietSection(_ name: String, _ all: [SinceLeft.Line], handledQuietly: Bool) -> some View {
        if !all.isEmpty {
            VStack(alignment: .leading, spacing: 5) {
                Fine(words: name)
                    .accessibilityAddTraits(.isHeader)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(all.prefix(SinceLeftCard.most)) { line in
                        let row = QuietOutcomeLine(words: line.words, handledQuietly: handledQuietly, isImportant: line.isImportant)
                        if let item = line.item {
                            Button { model.show(item, in: .main) } label: { row }
                                .buttonStyle(.plain)
                                .accessibilityLabel(spokenLabel(for: line))
                        } else {
                            row
                        }
                    }
                }
                if all.count > SinceLeftCard.most {
                    Fine(words: AssistantWords.sinceMore.said(["count": String(all.count - SinceLeftCard.most)]))
                }
            }
        }
    }

    private func spokenLabel(for line: SinceLeft.Line) -> String {
        line.isImportant ? "\(line.words), \(AssistantWords.important.said)" : line.words
    }
}
