import AssistantCore
import SwiftUI

/// The one line under the newest message while a turn is at work: what the assistant does now,
/// how long the turn has gone on, and how many steps it has taken. Each step takes the place
/// of the one before; the line never grows into a list. A press opens the steps so far over
/// it, each one line, the newest next to it, and a press closes them: the line keeps its place
/// at the foot, under the hand that pressed it. Opened, the steps stand no taller than `tallest`:
/// past it they scroll in a part of their own, so what stood above the line stays in sight.
/// While the reply's own words come the line says nothing: the reply says it. Where nothing
/// says what the turn does, the line says that an answer is being written, as it always did.
struct TurnLine: View {
    let turn: Message.Turn?
    @Binding var open: Bool
    /// How tall the opened steps may stand before they scroll in a part of their own: no bound
    /// where what shows of the conversation is not known yet.
    let tallest: CGFloat
    @Environment(\.accessibilityReduceMotion) private var reduced

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let turn, !turn.steps.isEmpty {
                if open { TurnSteps(steps: turn.steps, tallest: tallest) }
                Button {
                    withAnimation(reduced ? nil : .easeOut(duration: Motion.base)) { open.toggle() }
                } label: {
                    line.contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .allowsHitTesting(!givesWay)
                .accessibilityHidden(givesWay)
                .accessibilityHint(open ? AssistantWords.hideSteps.said : AssistantWords.showSteps.said)
            } else {
                line
            }
        }
        // The steps' part and the line are each told as their own: the line keeps its own place
        // for whoever reads the window without seeing it, also while its steps stand open over it.
        .accessibilityElement(children: .contain)
        .font(.small12)
        .foregroundStyle(Token.ink3)
    }

    /// The reply's own words come: they say what the turn does. The line keeps its place, so
    /// nothing moves when it speaks again, and stays while its steps are open, to close them.
    private var givesWay: Bool {
        turn?.now == .writing && !open
    }

    private var line: some View {
        HStack(spacing: 5) {
            Text("\(doing)…").lineLimit(1).truncationMode(thinks ? .head : .tail)
            if let turn {
                Text("·").accessibilityHidden(true)
                // Only these digits are drawn anew each second.
                TimelineView(.periodic(from: turn.began, by: 1)) { clock in
                    Text(TurnLine.said(clock.date.timeIntervalSince(turn.began))).monospacedDigit().fixedSize()
                }
                if !turn.steps.isEmpty {
                    Text("·").accessibilityHidden(true)
                    Text(AssistantWords.counted(turn.steps.count, one: .stepOne, many: .stepMany)).fixedSize()
                    // The sign that the line opens: it points to where the steps stand once they do.
                    Text("›").rotationEffect(.degrees(open ? -90 : 0)).accessibilityHidden(true)
                }
            }
        }
        .accessibilityElement(children: .combine)
        .opacity(givesWay ? 0 : 1)
        .accessibilityHidden(givesWay)
    }

    /// How long a turn has gone on, as a clock says it: minutes and seconds, and hours once there
    /// are any, in the language the line's own words are in.
    static func said(_ gone: TimeInterval, in language: Locale = AssistantWords.language) -> String {
        let seconds = Duration.seconds(Int(max(0, gone)))
        return seconds.formatted(.time(pattern: gone < 3600 ? .minuteSecond : .hourMinuteSecond).locale(language))
    }

    /// Whether the line shows what a model thinks: then the end of it is what shows.
    private var thinks: Bool {
        if case .thinking(let words)? = turn?.now { !words.isEmpty } else { false }
    }

    private var doing: String {
        switch turn?.now {
        case nil, .writing?: AssistantWords.answering.said
        case .thinking(let words)?: words.isEmpty ? AssistantWords.thinking.said : words
        case .step(let words)?: words ?? AssistantWords.atWork.said
        }
    }
}

/// The opened steps in a part of their own. A few steps take only the height of their lines; past
/// `tallest` the part scrolls and rests on its newest step, next to the line. No step is left out.
struct TurnSteps: View {
    let steps: [Message.Turn.Step]
    let tallest: CGFloat

    var body: some View {
        ScrollView {
            TurnStepList(steps: steps).frame(maxWidth: .infinity, alignment: .leading)
        }
        .defaultScrollAnchor(.bottom)
        // As tall as its steps and no taller than `tallest`: by itself a scroll view takes all it is offered.
        .frame(maxHeight: tallest)
        .fixedSize(horizontal: false, vertical: true)
    }
}

/// The steps a turn has taken so far, each one line: its number, the step in the system's own
/// words, how long it took where that is known, and that it failed where it did.
struct TurnStepList: View {
    let steps: [Message.Turn.Step]

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(Array(steps.enumerated()), id: \.element.id) { index, step in
                HStack(spacing: 5) {
                    Text("\(index + 1)").monospacedDigit().frame(minWidth: 18, alignment: .trailing)
                    Text(step.words).lineLimit(1).truncationMode(.tail)
                    if let took = step.took {
                        Text("·").accessibilityHidden(true)
                        Text(TurnStepList.said(took)).monospacedDigit().fixedSize()
                    }
                    if step.outcome == .failed {
                        Text("·").accessibilityHidden(true)
                        Text(AssistantWords.stepFailed.said).foregroundStyle(Token.ink2).fixedSize()
                    }
                }
                .accessibilityElement(children: .combine)
            }
        }
    }

    /// How long a step took, as the language the step's own words are in says a short time.
    static func said(_ took: TimeInterval, in language: Locale = AssistantWords.language) -> String {
        Duration.seconds(max(0, took)).formatted(.units(allowed: [.minutes, .seconds], width: .narrow,
                                                         fractionalPart: took < 10 ? .show(length: 1) : .hide).locale(language))
    }
}
