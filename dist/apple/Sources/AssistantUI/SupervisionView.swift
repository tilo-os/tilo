import AssistantCore
import SwiftUI

/// The assistant's workspace roster status, model, usage, connections and learned facts.
public struct SupervisionView: View {
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording
    @Environment(\.colorScheme) private var scheme
    @Environment(\.dynamicTypeSize) private var textSize
    @State private var detail: Detail?

    enum Detail: String, Identifiable {
        case logins, importance, learned, places, workspaces

        var id: String { rawValue }
    }

    public init() {}

    public var body: some View {
        let facts = model.supervision
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                if let state = facts.state {
                    HStack(spacing: 6) {
                        Circle().fill(state == .active ? Token.on : Token.ink3).frame(width: 8, height: 8)
                        Text((state == .active ? AssistantWords.assistantActive : AssistantWords.assistantRetired).said)
                            .font(.title15).foregroundStyle(Token.ink)
                    }
                    .padding(.vertical, 4)
                }

                VStack(spacing: 0) {
                    FactLine(key: .model, line: wording.model(facts)) {
                        Menu {
                            if model.models.isEmpty { Text(AssistantWords.noModelNote.said) }
                            ForEach(model.models) { choice in
                                Button("\(choice.name) · \(choice.provider)") { model.choose(choice.id) }
                            }
                            // Under the models, the ways back to the provider step: another key for a
                            // provider that is linked, another provider. Each leads to the main chat,
                            // where the app's one protected field is.
                            let ways = model.keyWays
                            if !ways.isEmpty {
                                Divider()
                                ForEach(ways) { way in
                                    Button(way.words) { model.take(way) }
                                }
                            }
                        } label: {
                            Text(AssistantWords.change.said).font(.tilo(14, .medium)).foregroundStyle(Token.asst)
                        }
                        .menuStyle(.button)
                        .buttonStyle(.plain)
                        .menuIndicator(.hidden)
                        .fixedSize()
                    }
                    usage(facts.usage)
                    FactLine(key: .connections, line: wording.connections(facts)) {
                        // Add opens the chat, where the assistant does the setup.
                        link(.add) { model.begin(AssistantWords.addConnectionDraft.said) }
                    }
                    FactLine(key: .logins, line: wording.logins(facts)) { link(.open) { detail = .logins } }
                    FactLine(key: .important, line: wording.importance(facts)) { link(.open) { detail = .importance } }
                    FactLine(key: .hasLearned, line: wording.learned(facts)) { link(.open) { detail = .learned } }
                    FactLine(key: .whereItWorks, line: wording.places(facts)) { link(.open) { detail = .places } }
                    FactLine(key: .workspaces, line: wording.workspaces(facts), isLast: true) { link(.open) { detail = .workspaces } }
                }
            }
            .padding(16)
            .background(Token.content, in: RoundedRectangle(cornerRadius: Measure.cardRadius))
            .overlay { RoundedRectangle(cornerRadius: Measure.cardRadius).strokeBorder(Token.line, lineWidth: 1) }
            .shadow(color: Token.shadowNear.color(scheme), radius: 1, y: 1)
            .shadow(color: Token.shadowFar.color(scheme), radius: 16, y: 12)
            .frame(maxWidth: 860)
            .padding(.horizontal, 24)
            .padding(.bottom, 32)
            .padding(.top, 4)
            .frame(maxWidth: .infinity)
        }
        .scrollClipDisabled()
        .background(Token.page)
        .onAppear { model.loadModels() }
        .sheet(item: $detail) { DetailSheet(detail: $0) }
    }

    private func link(_ words: AssistantWords, _ action: @escaping () -> Void) -> some View {
        Button(words.said, action: action).buttonStyle(LinkStyle()).font(.tilo(14, .medium))
    }

    /// Messages, tokens and money of the day: `.use`.
    private func usage(_ usage: Supervision.Usage) -> some View {
        let spent = wording.spent(usage)
        // At the largest text sizes each figure has a line of its own: three beside each other break in mid-word.
        let row = textSize.isAccessibilitySize ? AnyLayout(VStackLayout(alignment: .leading, spacing: 8))
            : AnyLayout(HStackLayout(alignment: .lastTextBaseline, spacing: 10))
        return row {
            figure(wording.messages(usage), AssistantWords.messagesToday.said, counted: usage.messagesToday != nil)
            figure(wording.tokens(usage), AssistantWords.tokensToday.said, counted: usage.tokensToday != nil)
            figure(spent.first, spent.second ?? "")
        }
        .padding(.vertical, 10)
        .overlay(alignment: .bottom) { Rectangle().fill(Token.line).frame(height: 1) }
    }

    /// One figure of the day over what it counts. Where the person's server counts none, the
    /// words that say so stand in the figure's place as a sentence stands: nothing there reads
    /// as a number.
    private func figure(_ figure: String, _ name: String, counted: Bool = true) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(figure).font(counted ? .tilo(19, .semibold, relativeTo: .title3) : .body14).monospacedDigit()
                .foregroundStyle(counted ? Token.ink : Token.ink2)
            Text(name).font(.small125).foregroundStyle(Token.ink2)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}

/// One line of the assistant view: what it is about, what there is to say, and where it leads: `.ln`.
private struct FactLine<Action: View>: View {
    let key: AssistantWords
    let line: Wording.Line
    var isLast = false
    @ViewBuilder var action: Action
    @Environment(\.horizontalSizeClass) private var width

    var body: some View {
        Group {
            #if os(iOS)
            // A phone is too narrow for three columns: what the line is about stands over it.
            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline) {
                    Text(key.said).font(.small125).foregroundStyle(Token.ink2)
                    Spacer(minLength: 12)
                    action
                }
                said
            }
            #else
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text(key.said).font(.body14).foregroundStyle(Token.ink2).frame(width: 119, alignment: .leading)
                said.frame(maxWidth: .infinity, alignment: .leading)
                action
            }
            #endif
        }
        .padding(.vertical, 10)
        .overlay(alignment: .bottom) { if !isLast { Rectangle().fill(Token.line).frame(height: 1) } }
    }

    private var said: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(line.first).font(.body14).foregroundStyle(Token.ink).accessibilityLabel(line.first.spoken)
            if let second = line.second {
                Text(second).font(.small125).foregroundStyle(Token.ink2).accessibilityLabel(second.spoken)
            }
        }
        .fixedSize(horizontal: false, vertical: true)
    }
}

/// Everything behind one line of the assistant view. What it has learned can be taken back
/// here; the rest is changed by telling the assistant.
private struct DetailSheet: View {
    let detail: SupervisionView.Detail
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let facts = model.supervision
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text(title).font(.title15).foregroundStyle(Token.ink).accessibilityAddTraits(.isHeader)
                Spacer(minLength: 0)
                Button {
                    dismiss()
                } label: {
                    Glyph(.x).contentShape(Rectangle().inset(by: -6))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Token.ink3)
                .keyboardShortcut(.cancelAction)
                .accessibilityLabel(AssistantWords.close.said)
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    switch detail {
                    case .logins:
                        ForEach(facts.logins) { row(wording.said($0)) }
                        if let manager = facts.manager { row(wording.said(manager)) }
                    case .importance:
                        ForEach(facts.importance, id: \.self) { row($0) }
                        ForEach(facts.situations, id: \.name) { row("\($0.name): \($0.value)") }
                    case .learned:
                        ForEach(facts.learned) { fact in
                            row(fact.text) {
                                Button(AssistantWords.forget.said) { model.forget(fact.id) }
                                    .buttonStyle(LinkStyle()).font(.tilo(13, .medium, relativeTo: .callout))
                            }
                        }
                        if facts.learnedCount > facts.learned.count {
                            Fine(words: AssistantWords.moreCount.said(["count": String(facts.learnedCount - facts.learned.count)]))
                                .padding(.top, 10)
                        }
                    case .places:
                        ForEach(facts.places, id: \.name) { row("\($0.name), \($0.detail)") }
                    case .workspaces:
                        ForEach(facts.workspaces, id: \.self) { row($0) }
                        if facts.workspaces.count == 1 { Fine(words: AssistantWords.worksInThis.said).padding(.top, 10) }
                    }
                    if isEmpty(facts) { Fine(words: AssistantWords.none.said) }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding(16)
        .frame(minWidth: 320, idealWidth: 460, minHeight: 260, idealHeight: 380)
        .background(Token.content)
    }

    private var title: String {
        switch detail {
        case .logins: AssistantWords.logins.said
        case .importance: AssistantWords.important.said
        case .learned: AssistantWords.hasLearned.said
        case .places: AssistantWords.whereItWorks.said
        case .workspaces: AssistantWords.workspaces.said
        }
    }

    private func isEmpty(_ facts: Supervision) -> Bool {
        switch detail {
        case .logins: facts.logins.isEmpty && facts.manager == nil
        case .importance: facts.importance.isEmpty && facts.situations.isEmpty
        case .learned: facts.learned.isEmpty
        case .places: facts.places.isEmpty
        case .workspaces: facts.workspaces.isEmpty
        }
    }

    private func row(_ words: String) -> some View {
        row(words) { EmptyView() }
    }

    private func row<Action: View>(_ words: String, @ViewBuilder action: () -> Action) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text(words).font(.body14).foregroundStyle(Token.ink).fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
            action()
        }
        .padding(.vertical, 10)
        .overlay(alignment: .bottom) { Rectangle().fill(Token.line).frame(height: 1) }
    }
}
