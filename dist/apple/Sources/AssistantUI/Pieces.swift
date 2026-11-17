import AssistantCore
import SwiftUI

// The small things a conversation is made of, each as the drawing draws it.

/// Children one after the other, on as many lines as it takes: the drawing's wrapping rows.
struct Flow: Layout {
    var spacing: CGFloat = 8
    var lineSpacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let lines = lines(of: subviews, within: proposal.width ?? .infinity)
        let width = lines.map(\.width).max() ?? 0
        let height = lines.map(\.height).reduce(0, +) + lineSpacing * CGFloat(max(0, lines.count - 1))
        return CGSize(width: width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for line in lines(of: subviews, within: bounds.width) {
            var x = bounds.minX
            for index in line.members {
                let size = subviews[index].sizeThatFits(.unspecified)
                let fitted = CGSize(width: min(size.width, bounds.width), height: size.height)
                subviews[index].place(at: CGPoint(x: x, y: y + (line.height - fitted.height) / 2), anchor: .topLeading,
                                      proposal: ProposedViewSize(fitted))
                x += fitted.width + spacing
            }
            y += line.height + lineSpacing
        }
    }

    private struct Line {
        var members: [Int] = []
        var width: CGFloat = 0
        var height: CGFloat = 0
    }

    private func lines(of subviews: Subviews, within width: CGFloat) -> [Line] {
        var lines: [Line] = []
        var line = Line()
        for index in subviews.indices {
            let ideal = subviews[index].sizeThatFits(.unspecified)
            let size = ideal.width > width ? subviews[index].sizeThatFits(ProposedViewSize(width: width, height: nil)) : ideal
            if !line.members.isEmpty, line.width + spacing + size.width > width {
                lines.append(line)
                line = Line()
            }
            line.width += (line.members.isEmpty ? 0 : spacing) + size.width
            line.height = max(line.height, size.height)
            line.members.append(index)
        }
        if !line.members.isEmpty { lines.append(line) }
        return lines
    }
}

/// As wide as what it holds while that fits on one line, and as wide as it may be once it does
/// not: how a page sizes a box around words, and so how the drawing's bubbles are sized.
struct HugOrFill: Layout {
    let most: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard let held = subviews.first else { return .zero }
        let room = min(most, proposal.width ?? most)
        let own = held.sizeThatFits(.unspecified)
        guard own.width > room else { return own }
        return CGSize(width: room, height: held.sizeThatFits(ProposedViewSize(width: room, height: nil)).height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        subviews.first?.place(at: bounds.origin, anchor: .topLeading, proposal: ProposedViewSize(bounds.size))
    }
}

/// Whose bubble: the assistant's, the person's, the main agent's.
enum BubbleTone {
    case plain, me, lead
}

/// A bubble: `.b`, `.b.me`, `.b.lead`.
struct Bubble<Content: View>: View {
    var tone = BubbleTone.plain
    @ViewBuilder var content: Content

    var body: some View {
        HugOrFill(most: Measure.bubbleWidth) {
            content
                .font(.body14)
                .foregroundStyle(Token.ink)
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
        }
        .background(fill, in: RoundedRectangle(cornerRadius: Measure.bubbleRadius))
        .frame(maxWidth: .infinity, alignment: tone == .me ? .trailing : .leading)
    }

    private var fill: Token {
        switch tone {
        case .plain: .raised
        case .me: .youTint
        case .lead: .asstTint
        }
    }
}

/// A message's Markdown as a bubble shows it: paragraphs, lists, headings, code.
struct MarkdownText: View {
    let text: String

    var body: some View {
        let blocks = Markdown.blocks(in: text)
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                switch block {
                case .paragraph(let words):
                    inline(words)
                case .heading(let level, let words):
                    inline(words).font(.tilo(level == 1 ? 17 : 15, .semibold))
                case .bullets(let entries):
                    list(entries.map { ("•", $0) }, width: 15.4)
                case .numbered(let first, let entries):
                    list(entries.enumerated().map { ("\(first + $0.offset).", $0.element) }, width: 22)
                case .code(let code):
                    Text(code).font(.tiloMono(12.5)).padding(8)
                        .background(Token.content, in: RoundedRectangle(cornerRadius: 6))
                case .quote(let words):
                    inline(words).foregroundStyle(Token.ink2).padding(.leading, 10)
                        .overlay(alignment: .leading) { Rectangle().fill(Token.ink3).frame(width: 2) }
                }
            }
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    private func list(_ entries: [(String, String)], width: CGFloat) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(Array(entries.enumerated()), id: \.offset) { _, entry in
                HStack(alignment: .firstTextBaseline, spacing: 0) {
                    if entry.0 == "•" {
                        // The drawing's round sign, at the height of the small letters' middle.
                        Circle().fill(Token.ink).frame(width: 5, height: 5)
                            .alignmentGuide(.firstTextBaseline) { $0[.bottom] + 0.9 }
                            .padding(.leading, 3)
                            .frame(width: width, alignment: .leading)
                            .accessibilityHidden(true)
                    } else {
                        Text(entry.0).frame(width: width, alignment: .leading)
                    }
                    inline(entry.1)
                }
            }
        }
    }

    private func inline(_ words: String) -> Text {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        guard let read = try? AttributedString(markdown: words, options: options) else { return Text(words) }
        return Text(read)
    }
}

/// A day and time between pieces: `.time`.
struct TimeMark: View {
    let words: String

    var body: some View {
        Text(words).font(.small12).foregroundStyle(Token.ink3)
            .padding(.top, 10).padding(.bottom, 2).frame(maxWidth: .infinity)
            .accessibilityAddTraits(.isHeader)
    }
}

/// The Important mark: `.imp`.
struct ImportantMark: View {
    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(Token.you).frame(width: 7, height: 7)
            Text(AssistantWords.important.said).font(.tilo(12, .semibold, relativeTo: .caption)).foregroundStyle(Token.you)
        }
        .accessibilityElement(children: .combine)
    }
}

/// The small green sign of something the assistant watches: `.eye`.
struct EyeChip: View {
    var body: some View {
        Glyph(.eye, side: 13).foregroundStyle(Token.on)
            .frame(width: 20, height: 20)
            .background(Token.onTint, in: RoundedRectangle(cornerRadius: 6))
    }
}

/// The line that names a task above its pieces: `.task`.
struct TaskLineView: View {
    let line: TaskLine
    let isImportant: Bool
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording

    var body: some View {
        Flow(spacing: 7, lineSpacing: 4) {
            Glyph(.into).foregroundStyle(Token.ink3)
            EyeChip()
            Text(line.title).foregroundStyle(Token.ink)
            if let said = wording.line(line.standing) {
                if case .replies = line.standing {
                    Button("· \(said)") { model.showTasks(.active) }.buttonStyle(.plain).foregroundStyle(Token.asst)
                } else {
                    Text("· \(said)").foregroundStyle(Token.ink3)
                }
            }
            if isImportant { ImportantMark() }
        }
        .font(.line135)
        .padding(.top, 6)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
    }
}

/// A quick answer: `.q`, and `.q.go` for the one the assistant expects first.
struct PillStyle: ButtonStyle {
    var isMain = false
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.tilo(13.5, isMain ? .medium : .regular, relativeTo: .callout))
            .foregroundStyle(isMain ? Token.content : Token.ink)
            .padding(.horizontal, 12)
            .padding(.vertical, 5)
            .background {
                // The drawing's one-point line, as a shape with the ground a point inside it.
                Capsule(style: .circular).fill(isMain ? Token.ink : Token.line)
                if !isMain { Capsule(style: .circular).fill(Token.content).padding(1) }
            }
            .opacity(configuration.isPressed ? 0.7 : (isEnabled ? 1 : 0.45))
            .contentShape(Capsule(style: .circular))
    }
}

/// A button in a card: `.btn`, and `.btn.go`.
struct BoxButtonStyle: ButtonStyle {
    var isMain = false
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.button13)
            .foregroundStyle(isMain ? Token.content : Token.ink)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(isMain ? Token.ink : Token.content, in: RoundedRectangle(cornerRadius: 8))
            .opacity(configuration.isPressed ? 0.7 : (isEnabled ? 1 : 0.45))
            .contentShape(RoundedRectangle(cornerRadius: 8))
    }
}

/// A word that leads somewhere, in the assistant's colour: `.lnk`, `.a`.
struct LinkStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.foregroundStyle(Token.asst).opacity(configuration.isPressed ? 0.6 : 1)
            .contentShape(Rectangle())
    }
}

/// The answers under a question: `.qa`.
struct QuickAnswers: View {
    let choices: [Choice]
    let chosen: Choice.ID?
    let answer: (Choice) -> Void

    var body: some View {
        if let chosen, let choice = choices.first(where: { $0.id == chosen }) {
            Text(AssistantWords.youChose.said(["answer": choice.title])).font(.small12).foregroundStyle(Token.ink3)
                .frame(maxWidth: .infinity).padding(.vertical, 2)
        } else {
            Flow(spacing: 8, lineSpacing: 8) {
                ForEach(choices) { choice in
                    Button(choice.title) { answer(choice) }.buttonStyle(PillStyle(isMain: choice.isMain))
                }
            }
            .padding(.top, 2)
        }
    }
}

/// What waits for review, with the way to all tasks: `.wide`.
struct ReviewLine: View {
    let ready: Int
    @Environment(AssistantModel.self) private var model

    var body: some View {
        HStack(spacing: 12) {
            HStack(spacing: 8) {
                Glyph(.check)
                Text(AssistantWords.counted(ready, one: .readyOne, many: .readyMany))
            }
            Spacer(minLength: 0)
            Button("\(AssistantWords.seeAllTasks.said) ›") { model.showTasks(.active) }.buttonStyle(LinkStyle())
        }
        .font(.body14)
        .foregroundStyle(Token.ink2)
        .padding(.horizontal, 14)
        .padding(.vertical, 11)
        .background(Token.raised, in: RoundedRectangle(cornerRadius: Measure.cardRadius))
        .frame(maxWidth: 440, alignment: .leading)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A small quiet line: `.fine`.
struct Fine: View {
    let words: String

    var body: some View {
        Text(words).font(.small12).foregroundStyle(Token.ink3).fixedSize(horizontal: false, vertical: true)
    }
}

/// The place of a live Tilo thing. The source's own view draws it; without one, its name stands there.
struct LiveThingPlace: View {
    let thing: LiveThing
    @Environment(\.liveThingView) private var draw

    var body: some View {
        Group {
            if let draw {
                draw(thing)
            } else {
                VStack(spacing: 6) {
                    Mark(side: 22)
                    Text(thing.title).font(.tilo(14, .medium)).foregroundStyle(Token.ink)
                    Fine(words: AssistantWords.liveThing.said)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Token.raised)
            }
        }
        .frame(maxWidth: .infinity)
        .frame(height: 360)
        .clipShape(RoundedRectangle(cornerRadius: Measure.cardRadius))
        .overlay { RoundedRectangle(cornerRadius: Measure.cardRadius).strokeBorder(Token.line, lineWidth: 1) }
        .accessibilityLabel(thing.title)
    }
}
