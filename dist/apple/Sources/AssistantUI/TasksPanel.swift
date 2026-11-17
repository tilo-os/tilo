import AssistantCore
import SwiftUI

/// The Tasks panel: what waits for the person and what is in work, what comes next, what comes
/// at a time and what happens when something else does, and what is resolved. It lists tasks
/// only: a sub-agent is never one.
public struct TasksPanel: View {
    private let close: () -> Void
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording
    @State private var adds = false
    @State private var title = ""
    @FocusState private var writes: Bool

    public init(close: @escaping () -> Void) {
        self.close = close
    }

    public var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                Text(AssistantWords.tasks.said).font(.title15).foregroundStyle(Token.ink)
                    .accessibilityAddTraits(.isHeader)
                Spacer(minLength: 0)
                Button {
                    adds.toggle()
                    writes = adds
                } label: {
                    Glyph(.plus).contentShape(Rectangle().inset(by: -5))
                }
                .accessibilityLabel(AssistantWords.addTask.said)
                .help(AssistantWords.addTask.said)
                Button(action: close) {
                    Glyph(.x).contentShape(Rectangle().inset(by: -5))
                }
                .accessibilityLabel(AssistantWords.close.said)
                .help(AssistantWords.close.said)
            }
            .buttonStyle(.plain)
            .foregroundStyle(Token.ink3)
            .padding(.vertical, 4)

            Flow(spacing: 2, lineSpacing: 2) {
                ForEach(AssistantModel.PanelTab.allCases, id: \.self) { tab in
                    let isOn = model.panelTab == tab
                    Button {
                        model.panelTab = tab
                    } label: {
                        Text(name(tab)).font(.tilo(14, isOn ? .medium : .regular))
                            .foregroundStyle(isOn ? Token.ink : Token.ink2)
                            .padding(.horizontal, 10).padding(.vertical, 4)
                            .background(isOn ? Token.raised : Token.clear, in: RoundedRectangle(cornerRadius: 7))
                            .contentShape(RoundedRectangle(cornerRadius: 7))
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(isOn ? .isSelected : [])
                }
            }

            if adds {
                TextField(AssistantWords.addTaskField.said, text: $title)
                    .textFieldStyle(.plain).font(.body14).focused($writes)
                    .padding(.horizontal, 10).padding(.vertical, 7)
                    .background(Token.raised, in: RoundedRectangle(cornerRadius: Measure.rowRadius))
                    .onSubmit {
                        model.addTask(title)
                        title = ""
                        adds = false
                        model.panelTab = .upNext
                    }
            }

            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    switch model.panelTab {
                    case .active: active
                    case .upNext: plain(.upNext, sign: .clock, empty: .nothingUpNext)
                    case .scheduled: scheduled
                    case .resolved: plain(.resolved, sign: .check, empty: .nothingResolved)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

    private func name(_ tab: AssistantModel.PanelTab) -> String {
        switch tab {
        case .active: AssistantWords.active.said
        case .upNext: AssistantWords.upNext.said
        case .scheduled: AssistantWords.scheduled.said
        case .resolved: AssistantWords.resolved.said
        }
    }

    @ViewBuilder private var active: some View {
        let groups: [(AssistantWords, AssistantTask.Standing, Sign)] = [
            (.readyForReview, .readyForReview, .check), (.needsInput, .needsInput, .ask), (.inProgress, .inProgress, .run),
        ]
        if groups.allSatisfy({ model.tasks($0.1).isEmpty }) { Empty(words: .nothingActive) }
        ForEach(groups, id: \.0) { name, standing, sign in
            let tasks = model.tasks(standing)
            if !tasks.isEmpty {
                SectionHead(name: name.said, count: tasks.count)
                ForEach(tasks) { TaskRow(task: $0, sign: sign) }
            }
        }
    }

    @ViewBuilder
    private func plain(_ standing: AssistantTask.Standing, sign: Sign, empty: AssistantWords) -> some View {
        let tasks = model.tasks(standing)
        if tasks.isEmpty { Empty(words: empty) }
        ForEach(tasks) { TaskRow(task: $0, sign: sign) }
    }

    /// Two kinds, kept apart: something at a time, and something that happens when something else does.
    @ViewBuilder private var scheduled: some View {
        let timed = model.automations.filter { $0.sort == .scheduled }
        let events = model.automations.filter { $0.sort == .event }
        if timed.isEmpty, events.isEmpty { Empty(words: .nothingScheduled) }
        if !timed.isEmpty {
            SectionHead(name: AssistantWords.scheduled.said, count: timed.count)
            ForEach(timed) { AutomationRow(automation: $0, isLast: events.isEmpty && $0.id == timed.last?.id) }
        }
        if !events.isEmpty {
            SectionHead(name: AssistantWords.events.said, count: events.count)
            ForEach(events) { AutomationRow(automation: $0, isLast: $0.id == events.last?.id) }
        }
    }
}

/// A group's name and how many it holds: `.sec`.
private struct SectionHead: View {
    let name: String
    let count: Int

    var body: some View {
        HStack(spacing: 8) {
            Text(name).foregroundStyle(Token.ink2)
            Text(String(count)).foregroundStyle(Token.ink3)
        }
        .font(.small125)
        .padding(.top, 12)
        .padding(.bottom, 2)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
    }
}

private struct Empty: View {
    let words: AssistantWords

    var body: some View {
        Text(words.said).font(.small125).foregroundStyle(Token.ink3).padding(.top, 12).padding(.bottom, 2)
    }
}

/// The small square a row begins with: `.bx`.
private struct SignBox: View {
    let sign: Sign

    var body: some View {
        Glyph(sign).foregroundStyle(Token.ink2)
            .frame(width: 26, height: 26)
            .background(Token.raised, in: RoundedRectangle(cornerRadius: 8))
    }
}

/// A task: `.row`.
private struct TaskRow: View {
    let task: AssistantTask
    let sign: Sign
    @Environment(AssistantModel.self) private var model

    var body: some View {
        Button {
            if let item = task.item { model.show(item) }
        } label: {
            HStack(alignment: .top, spacing: 10) {
                SignBox(sign: sign)
                VStack(alignment: .leading, spacing: 2) {
                    Text(task.title).font(.body14).foregroundStyle(Token.ink).lineLimit(1)
                        .frame(minHeight: 26, alignment: .leading)
                    if task.isImportant { ImportantMark() }
                    if let note = task.note { Text(note).font(.small12).foregroundStyle(Token.ink3) }
                }
                Spacer(minLength: 0)
            }
            .padding(.vertical, 6)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
    }
}

/// Something that comes at a time, or an event: `.ev`, with its switch `.sw`.
private struct AutomationRow: View {
    let automation: Automation
    let isLast: Bool
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            SignBox(sign: automation.sort == .scheduled ? .clock : .bolt)
            // Opening one changes it by telling the assistant: its name is begun in the field.
            Button {
                model.begin("\(automation.title): ")
            } label: {
                VStack(alignment: .leading, spacing: 2) {
                    Text(automation.title).font(.tilo(14, .medium)).foregroundStyle(Token.ink)
                        .frame(minHeight: 26, alignment: .leading)
                    Text(automation.summary).font(.tilo(13, relativeTo: .callout)).foregroundStyle(Token.ink2)
                        .fixedSize(horizontal: false, vertical: true)
                    let tags = wording.tags(automation)
                    if !tags.isEmpty {
                        Flow(spacing: 6, lineSpacing: 6) {
                            ForEach(tags, id: \.self) { Tag(words: $0) }
                        }
                        .padding(.top, 4)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            Toggle(automation.title, isOn: Binding(get: { automation.isOn }, set: { model.setOn($0, for: automation.id) }))
                .toggleStyle(SwitchLook())
                .labelsHidden()
                .padding(.top, 4)
        }
        .padding(.vertical, 9)
        .overlay(alignment: .bottom) { if !isLast { Rectangle().fill(Token.line).frame(height: 1) } }
    }
}

/// The drawing's switch: `.sw`. Off is paused.
struct SwitchLook: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View {
        Button {
            configuration.isOn.toggle()
        } label: {
            Capsule().fill(configuration.isOn ? Token.on : Token.ink3)
                .frame(width: 30, height: 18)
                .overlay(alignment: configuration.isOn ? .trailing : .leading) {
                    Circle().fill(Token.onColour).frame(width: 14, height: 14).padding(2)
                }
                .animation(.easeOut(duration: 0.15), value: configuration.isOn)
                .contentShape(Rectangle().inset(by: -6))
        }
        .buttonStyle(.plain)
        .accessibilityRepresentation { Toggle(isOn: configuration.$isOn) { configuration.label } }
    }
}
