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
    /// Where the steps were opened and where they were left. The conversation draws the line anew
    /// for every piece that comes, and a part that is drawn anew is asked for its end before it is
    /// laid out: that ask, and only that ask, is not the person resting the steps.
    @State private var place = TurnSteps.Place()

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let turn, !turn.steps.isEmpty {
                if open { TurnSteps(steps: turn.steps, tallest: tallest, place: $place) }
                Button {
                    place = TurnSteps.Place()
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
        // A turn of its own begins: the steps the person scrolled of the last one are not these.
        .onChange(of: turn?.steps.first?.id) { place = TurnSteps.Place() }
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
        case .step(let words)?: words.map(TurnLine.humanize) ?? AssistantWords.atWork.said
        }
    }

    /// Known internal tool names mapped to calm, human-readable activity verbs.
    private static let toolVerbs: [String: String] = [
        "tilo_ls": "Looking through workspace",
        "tilo_read": "Reading",
        "tilo_do": "Taking action",
        "tilo_find": "Searching",
        "answer_with_view": "Building overview",
    ]

    /// Turns raw internal tool identifiers into calm human-readable activity text while
    /// preserving real step labels untouched.
    static func humanize(_ raw: String) -> String {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return AssistantWords.atWork.said }

        let head: String
        let detail: String?
        if let colon = trimmed.firstIndex(of: ":") {
            head = String(trimmed[..<colon]).trimmingCharacters(in: .whitespacesAndNewlines)
            let after = String(trimmed[trimmed.index(after: colon)...]).trimmingCharacters(in: .whitespacesAndNewlines)
            detail = cleanDetail(after)
        } else if let blank = trimmed.firstIndex(where: \.isWhitespace) {
            let first = String(trimmed[..<blank])
            if let mapped = mappedToolVerb(for: first) {
                let rest = String(trimmed[blank...]).trimmingCharacters(in: .whitespacesAndNewlines)
                if let info = cleanDetail(rest) { return "\(mapped): \(info)" }
                return mapped
            }
            return trimmed
        } else {
            head = trimmed
            detail = nil
        }

        if let mapped = mappedToolVerb(for: head) {
            if let detail { return "\(mapped): \(detail)" }
            return mapped
        }

        if !looksLikeInternalTool(head) {
            return trimmed
        }

        if let detail { return "\(AssistantWords.atWork.said): \(detail)" }
        return AssistantWords.atWork.said
    }

    private static func mappedToolVerb(for token: String) -> String? {
        let lowered = token.lowercased()
        if let direct = toolVerbs[lowered] { return direct }
        if !lowered.contains(where: \.isWhitespace) {
            for (name, verb) in toolVerbs {
                if lowered.hasSuffix(".\(name)") || lowered.hasSuffix("/\(name)") || lowered.hasSuffix("__\(name)") {
                    return verb
                }
            }
        }
        return nil
    }

    private static func looksLikeInternalTool(_ token: String) -> Bool {
        guard !token.isEmpty, !token.contains(where: \.isWhitespace) else { return false }
        return token.contains("_") || token.contains(".") || token.first?.isUppercase != true
    }

    private static func cleanDetail(_ raw: String) -> String? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        if trimmed.hasPrefix("{") || trimmed.hasPrefix("[") { return nil }
        if mappedToolVerb(for: trimmed) != nil { return nil }
        return trimmed
    }
}

/// The opened steps in a part of their own. A few steps take only the height of their lines; past
/// `tallest` the part scrolls and rests on its newest step, next to the line. No step is left out.
/// Once the person scrolled the steps away from their newest, the part stays where they left it
/// while the turn goes on: a step that comes does not pull the list out of their hands.
struct TurnSteps: View {
    let steps: [Message.Turn.Step]
    let tallest: CGFloat
    @Binding var place: Place
    @Environment(\.accessibilityReduceMotion) private var reduced

    /// Where the part was opened or left, as the turn goes on.
    struct Place: Equatable {
        /// The newest step the part rested on, or took as it came.
        var on: Message.Turn.Step.ID?
        /// Whether the person scrolled the part: then it is theirs, and no step that comes moves it.
        var moved = false
    }

    var body: some View {
        ScrollViewReader { reader in
            ScrollView {
                TurnStepList(steps: steps).frame(maxWidth: .infinity, alignment: .leading)
            }
            .defaultScrollAnchor(.bottom)
            // As tall as its steps and no taller than `tallest`: by itself a scroll view takes all it is offered.
            .frame(maxHeight: tallest)
            .fixedSize(horizontal: false, vertical: true)
            .onScrollGeometryChange(for: Resting.self) { place in
                Resting(laidOut: place.containerSize.height > 0,
                        rests: place.contentSize.height - place.visibleRect.maxY <= 2)
            } action: { was, now in
                // A part asked where it rests before it is laid out has scrolled nowhere yet.
                guard was.laidOut, now.laidOut, was.rests != now.rests else { return }
                place.moved = !now.rests
            }
            .onAppear { place.on = steps.last?.id }
            .onChange(of: steps.last?.id) { _, newest in
                // The person reads older steps: nothing moves under their hand.
                guard !place.moved, place.on != newest else { return }
                place.on = newest
                withAnimation(reduced ? nil : .easeOut(duration: Motion.base)) { rest(newest, in: reader) }
            }
        }
    }

    /// What the part says of itself: whether it is laid out, and whether it rests on its newest step.
    private struct Resting: Equatable {
        var laidOut: Bool
        var rests: Bool
    }

    /// Put the part's end back in sight, where the steps' own words say which step is which.
    private func rest(_ step: Message.Turn.Step.ID?, in reader: ScrollViewProxy) {
        guard let step else { return }
        reader.scrollTo(step, anchor: .bottom)
    }
}

/// The steps a turn has taken so far, each one line: its number, the step in the system's own
/// words, how long it took where that is known, and that it failed where it did.
struct TurnStepList: View {
    let steps: [Message.Turn.Step]

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(Array(steps.enumerated()), id: \.element.id) { index, step in
                line(index: index, step: step).id(step.id)
            }
        }
    }

    private func line(index: Int, step: Message.Turn.Step) -> some View {
        HStack(spacing: 5) {
            Text("\(index + 1)").monospacedDigit().foregroundStyle(Token.ink3.opacity(0.75)).frame(minWidth: 18, alignment: .trailing)
            Text(TurnLine.humanize(step.words))
                .foregroundStyle(step.outcome == .going ? Token.ink2 : Token.ink3)
                .lineLimit(1)
                .truncationMode(.tail)
            if let took = step.took {
                Text("·").accessibilityHidden(true)
                Text(TurnStepList.said(took)).monospacedDigit().fixedSize()
            }
            if step.outcome == .failed {
                Text("·").accessibilityHidden(true)
                Text(AssistantWords.stepFailed.said).foregroundStyle(Token.you).fixedSize()
            }
        }
        .accessibilityElement(children: .combine)
    }

    /// How long a step took, as the language the step's own words are in says a short time.
    static func said(_ took: TimeInterval, in language: Locale = AssistantWords.language) -> String {
        Duration.seconds(max(0, took)).formatted(.units(allowed: [.minutes, .seconds], width: .narrow,
                                                         fractionalPart: took < 10 ? .show(length: 1) : .hide).locale(language))
    }
}
