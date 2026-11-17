import AssistantCore
import SwiftUI

/// The sidebar: the assistant, the one main chat, the sub-agents with how each stands, and the
/// person at the foot. There is no new chat: the plus starts a sub-agent.
public struct Sidebar: View {
    /// Called when the person chose where to go: a phone puts the sidebar away then.
    private let chose: () -> Void
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording
    @State private var searches = false

    public init(chose: @escaping () -> Void = {}) {
        self.chose = chose
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
                .accessibilityHint(AssistantWords.assistantView.said)
                .help(AssistantWords.assistantView.said)
                Button {
                    searches = true
                } label: {
                    Glyph(.search).contentShape(Rectangle().inset(by: -6))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Token.ink3)
                .accessibilityLabel(AssistantWords.search.said)
                .help(AssistantWords.search.said)
                .popover(isPresented: $searches) { SearchList { chose() } }
            }
            .padding(EdgeInsets(top: 6, leading: 8, bottom: 14, trailing: 8))

            Button {
                model.open(.main)
                chose()
            } label: {
                HStack(spacing: 10) {
                    Mark()
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

            HStack {
                Text(AssistantWords.subAgents.said).font(.small12).foregroundStyle(Token.ink3)
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
                .help(AssistantWords.newSubAgent.said)
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
                            .frame(width: 24, height: 24).background(Token.you, in: Circle())
                            .accessibilityHidden(true)
                        Text(person.name).font(.body14).foregroundStyle(Token.ink).lineLimit(1)
                        Spacer(minLength: 0)
                        Text(person.workspace).font(.small12).foregroundStyle(Token.ink3).lineLimit(1)
                    }
                    .contentShape(Rectangle())
                }
                .menuStyle(.button)
                .buttonStyle(.plain)
                .menuIndicator(.hidden)
                .padding(EdgeInsets(top: 10, leading: 8, bottom: 4, trailing: 8))
                .overlay(alignment: .top) { Rectangle().fill(Token.line).frame(height: 1) }
            }
        }
        .padding(10)
    }
}

/// A sub-agent in the sidebar: `.agent`, with its dot `.st`.
private struct AgentRow: View {
    let agent: SubAgent
    let isSelected: Bool
    let open: () -> Void
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording

    var body: some View {
        Button(action: open) {
            HStack(alignment: .top, spacing: 10) {
                Circle().fill(dot).frame(width: 8, height: 8).padding(.top, 6)
                VStack(alignment: .leading, spacing: 0) {
                    Text(agent.title).font(.tilo(14, .medium)).foregroundStyle(Token.ink).lineLimit(1)
                    Text(wording.sidebarLine(agent)).font(.small12).foregroundStyle(Token.ink3).lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(isSelected ? Token.raised : Token.clear, in: RoundedRectangle(cornerRadius: Measure.rowRadius))
            .contentShape(RoundedRectangle(cornerRadius: Measure.rowRadius))
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
        .contextMenu {
            if agent.standing == .working || agent.standing == .needsYou {
                Button(AssistantWords.stop.said, role: .destructive) { model.stop(agent.id) }
            }
        }
    }

    private var dot: Token {
        switch agent.standing {
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
            TextField(AssistantWords.searchField.said, text: $words)
                .textFieldStyle(.plain).font(.body14)
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
    }
}
