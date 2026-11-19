import AssistantCore
import SwiftUI

/// The sidebar: the assistant, the one main chat, the sub-agents with how each stands, and the
/// person at the foot. There is no new chat: the plus starts a sub-agent.
public struct Sidebar: View {
    /// Called when the person chose where to go: a phone puts the sidebar away then.
    private let chose: () -> Void
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording
    @Environment(\.dynamicTypeSize) private var textSize
    /// How much larger the person's text is than the drawing's.
    @ScaledMetric(relativeTo: .body) private var grown: CGFloat = 1
    @State private var searches = false

    public init(chose: @escaping () -> Void = {}) {
        self.chose = chose
    }

    /// What the sub-agents' head says of them beside their name: how many wait for the person,
    /// how many are at work, and how many are done, failed or stopped. Nothing while there are none.
    private var stood: String {
        let agents = model.subAgents
        guard !agents.isEmpty else { return "" }
        let finished: Set<SubAgent.State> = [.done, .failed, .stopped]
        var parts: [String] = []
        for state in [SubAgent.State.needsYou, .working, .done] {
            let count = agents.filter { finished.contains(state) ? finished.contains($0.state) : $0.state == state }.count
            guard count > 0 else { continue }
            let words: String = switch state {
            case .needsYou: AssistantWords.needsYou.said
            case .working: AssistantWords.working.said
            default: AssistantWords.done.said
            }
            parts.append("\(count) \(words)")
        }
        return parts.joined(separator: " · ")
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 8) {
                Button {
                    model.place = .assistantView
                    chose()
                } label: {
                    HStack(spacing: 8) {
                        Mark()
                        Text(AssistantWords.assistant.said).font(.title15).foregroundStyle(Token.ink)
                        Spacer(minLength: 0)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(AssistantWords.assistantView.said)
                .tip(AssistantWords.assistantView.said)
                Button {
                    searches = true
                } label: {
                    Glyph(.search).contentShape(Rectangle().inset(by: -6))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Token.ink3)
                // The window's Find brings up the search of the conversation it shows, with no
                // menu line of its own: the shortcut is said where the button is.
                .keyboardShortcut("f", modifiers: .command)
                .accessibilityLabel(AssistantWords.search.said)
                .tip(AssistantWords.searchShortcut.said(["shortcut": "⌘F"]))
                .popover(isPresented: $searches) { SearchList { chose() } }
            }
            .padding(EdgeInsets(top: 6, leading: 8, bottom: 14, trailing: 8))

            Button {
                model.open(.main)
                chose()
            } label: {
                HStack(spacing: 10) {
                    // The logo is the head's alone. The main chat has a sign of its own, whose dots
                    // move while the assistant writes there.
                    ChatSign(writes: model.isWriting(.main)).foregroundStyle(Token.ink)
                    Text(AssistantWords.mainChat.said)
                        .font(.tilo(14, model.place == .chat(.main) ? .medium : .regular)).foregroundStyle(Token.ink)
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 7)
                .background(model.place == .chat(.main) ? Token.raised : Token.clear,
                            in: RoundedRectangle(cornerRadius: Measure.rowRadius))
                .contentShape(RoundedRectangle(cornerRadius: Measure.rowRadius))
            }
            .buttonStyle(.plain)
            .accessibilityAddTraits(model.place == .chat(.main) ? .isSelected : [])
            // Read out as writing by the word the conversation itself says under a reply in work.
            .accessibilityValue(model.isWriting(.main) ? AssistantWords.answering.said : "")

            HStack {
                // How many there are and which of them wait for the person: the head says it once,
                // above the rows that say it each, so the list is read without opening a row.
                // Where there are none, the list under it already says so: nothing repeated.
                let stood = self.stood
                Text(stood.isEmpty ? AssistantWords.subAgents.said : "\(AssistantWords.subAgents.said) · \(stood)")
                    .font(.small12).foregroundStyle(Token.ink3)
                    .lineLimit(1).minimumScaleFactor(0.8)
                    .accessibilityLabel(AssistantWords.subAgents.said)
                    .accessibilityAddTraits(.isHeader)
                Spacer(minLength: 0)
                Button {
                    model.asksForJob = true
                } label: {
                    Glyph(.plus).contentShape(Rectangle().inset(by: -6))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Token.ink3)
                .accessibilityLabel(AssistantWords.newSubAgent.said)
                .tip(AssistantWords.newSubAgent.said)
            }
            .padding(EdgeInsets(top: 14, leading: 10, bottom: 4, trailing: 10))

            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    if model.subAgents.isEmpty {
                        Text(AssistantWords.noSubAgents.said).font(.small12).foregroundStyle(Token.ink3)
                            .padding(.horizontal, 10).padding(.vertical, 6)
                    }
                    ForEach(model.subAgents) { agent in
                        AgentRow(agent: agent, isSelected: model.place == .chat(.subAgent(agent.id))) {
                            model.open(.subAgent(agent.id))
                            chose()
                        }
                    }
                }
            }

            if let person = model.person {
                Menu {
                    Button(AssistantWords.assistantView.said) {
                        model.place = .assistantView
                        chose()
                    }
                    Button(AssistantWords.signOut.said, role: .destructive) { model.signOut() }
                } label: {
                    HStack(spacing: 10) {
                        Text(person.initials).font(.tilo(11, .semibold, relativeTo: .caption2)).foregroundStyle(Token.onColour)
                            .lineLimit(1).minimumScaleFactor(0.5)
                            .frame(width: 24 * min(max(grown, 1), 2), height: 24 * min(max(grown, 1), 2)).background(Token.you, in: Circle())
                            .accessibilityHidden(true)
                        // At the largest text sizes the workspace stands under the name, both read whole.
                        let large = textSize.isAccessibilitySize
                        let names = large ? AnyLayout(VStackLayout(alignment: .leading, spacing: 0)) : AnyLayout(HStackLayout(spacing: 10))
                        names {
                            Text(person.name).font(.body14).foregroundStyle(Token.ink).lineLimit(large ? 3 : 1)
                            if !large { Spacer(minLength: 0) }
                            Text(person.workspace).font(.small12).foregroundStyle(Token.ink3).lineLimit(large ? 3 : 1)
                        }
                        .multilineTextAlignment(.leading)
                        if large { Spacer(minLength: 0) }
                    }
                    .contentShape(Rectangle())
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel("\(person.name), \(person.workspace)")
                }
                .menuStyle(.button)
                .buttonStyle(.plain)
                .menuIndicator(.hidden)
                .accessibilityLabel("\(person.name), \(person.workspace)")
                .padding(EdgeInsets(top: 10, leading: 8, bottom: 4, trailing: 8))
                .overlay(alignment: .top) { Rectangle().fill(Token.line).frame(height: 1) }
                // On a phone the app says which build it is, under its person: its Settings show the version alone.
                BuildLine(inList: true)
            }
        }
        .padding(10)
        // One part of the window, read as one: the assistant, the main chat, the sub-agents, the person.
        .accessibilityElement(children: .contain)
        .accessibilityLabel(AssistantWords.sidebar.said)
    }
}

/// A sub-agent in the sidebar: `.agent`, with its dot `.st`.
private struct AgentRow: View {
    let agent: SubAgent
    let isSelected: Bool
    let open: () -> Void
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording
    @Environment(\.dynamicTypeSize) private var textSize

    var body: some View {
        // At the largest text sizes a job's name is read whole, on as many lines as it takes.
        let lines = textSize.isAccessibilitySize ? 4 : 1
        let writes = model.isWriting(.subAgent(agent.id))
        Button(action: open) {
            HStack(alignment: .top, spacing: 10) {
                // While this sub-agent writes, its dot gives way to the three dots that say so, in
                // the same place: nothing beside them moves.
                Group {
                    if writes {
                        WritingDots(writes: true, dot: 2.4, pitch: 3.6, lift: 2.4)
                    } else {
                        Circle().fill(dot)
                    }
                }
                .frame(width: 8, height: 8).padding(.top, 6)
                VStack(alignment: .leading, spacing: 0) {
                    Text(agent.title).font(.tilo(14, .medium)).foregroundStyle(Token.ink).lineLimit(lines)
                        .multilineTextAlignment(.leading)
                    Text(wording.sidebarLine(agent)).font(.small12).foregroundStyle(Token.ink3).lineLimit(lines)
                        .multilineTextAlignment(.leading)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(isSelected ? Token.raised : Token.clear, in: RoundedRectangle(cornerRadius: Measure.rowRadius))
            .contentShape(RoundedRectangle(cornerRadius: Measure.rowRadius))
        }
        .buttonStyle(.plain)
        .accessibilityLabel([agent.title, agent.role, wording.state(agent)].filter { !$0.isEmpty }.joined(separator: ", "))
        .accessibilityAddTraits(isSelected ? .isSelected : [])
        .accessibilityValue(writes ? AssistantWords.answering.said : "")
        .contextMenu {
            if agent.state == .working || agent.state == .needsYou {
                Button(AssistantWords.stop.said, role: .destructive) { model.stop(agent.id) }
            }
        }
    }

    private var dot: Token {
        switch agent.state {
        case .working: .asst
        case .needsYou: .you
        case .done: .on
        case .failed, .stopped: .ink3
        }
    }
}

/// Search in the conversation that is open: its pieces that hold the words.
struct SearchList: View {
    let chose: () -> Void
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording
    @Environment(\.dismiss) private var dismiss
    @State private var words = ""

    var body: some View {
        let chat = model.chat ?? .main
        let found = words.count < 2 ? [] : model.conversation(chat).items.filter { $0.said.localizedCaseInsensitiveContains(words) }
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                TextField(AssistantWords.searchField.said, text: $words)
                    .textFieldStyle(.plain).font(.body14)
                // The field's words come off with one press: the next search begins at nothing.
                if !words.isEmpty {
                    Button {
                        words = ""
                    } label: {
                        Glyph(.x, side: 11).contentShape(Rectangle().inset(by: -6))
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(Token.ink3)
                    .accessibilityLabel(AssistantWords.clearSearch.said)
                    .tip(AssistantWords.clearSearch.said)
                }
            }
            .padding(.horizontal, 10).padding(.vertical, 7)
            .background(Token.raised, in: RoundedRectangle(cornerRadius: Measure.rowRadius))
            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    if words.count >= 2, found.isEmpty {
                        Fine(words: AssistantWords.nothingFound.said).padding(8)
                    }
                    ForEach(found.reversed()) { item in
                        Button {
                            model.show(item.id, in: chat)
                            dismiss()
                            chose()
                        } label: {
                            VStack(alignment: .leading, spacing: 1) {
                                Text(item.said).font(.body14).foregroundStyle(Token.ink).lineLimit(2)
                                Text(wording.mark(item.at)).font(.small12).foregroundStyle(Token.ink3)
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(8)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
        }
        .padding(12)
        .frame(minWidth: 300, idealWidth: 340, minHeight: 240, idealHeight: 320)
        .background(Token.content)
        // A popover on an iPhone is a sheet, whose own drag puts it away; there it is said as such.
        #if os(iOS)
        .presentationBackground(Token.content)
        #endif
    }
}
