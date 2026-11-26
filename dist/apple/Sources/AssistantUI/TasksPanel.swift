import AssistantCore
import SwiftUI

/// The Tasks panel: what waits for the person and what is in work, what comes next, what comes
/// at a time and what happens when something else does, and what is resolved. It lists tasks
/// only: a sub-agent is never one.
public struct TasksPanel: View {
    private let close: () -> Void
    @Environment(AssistantModel.self) private var model
    @State private var adds = false
    @State private var draft = Draft(text: "")
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
                .tip(AssistantWords.addTask.said)
                Button(action: close) {
                    Glyph(.x).contentShape(Rectangle().inset(by: -5))
                }
                .accessibilityLabel(AssistantWords.close.said)
                .tip(AssistantWords.close.said)
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
                TextField(AssistantWords.addTaskField.said, text: $draft.text)
                    .textFieldStyle(.plain).font(.body14).focused($writes)
                    .padding(.horizontal, 10).padding(.vertical, 7)
                    .background(Token.raised, in: RoundedRectangle(cornerRadius: Measure.rowRadius))
                    .onSubmit {
                        let sent = draft
                        Task {
                            guard await model.addTask(sent), draft.delivery == sent.delivery else { return }
                            draft = Draft(text: "")
                            adds = false
                            model.panelTab = .upNext
                        }
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
        // One part of the window, read as one after the conversation.
        .accessibilityElement(children: .contain)
        .accessibilityLabel(AssistantWords.tasks.said)
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
        let groups: [(AssistantWords, AssistantTask.State, Sign)] = [
            (.readyForReview, .readyForReview, .check), (.needsInput, .needsInput, .ask), (.inProgress, .inProgress, .run),
        ]
        ProposalSection().id(model.person)
        if model.proposals?.isEmpty != false, groups.allSatisfy({ model.tasks($0.1).isEmpty }) {
            Empty(words: .nothingActive)
        }
        ForEach(groups, id: \.0) { name, state, sign in
            let tasks = model.tasks(state)
            if !tasks.isEmpty {
                SectionHead(name: name.said, count: tasks.count)
                ForEach(tasks) { TaskRow(task: $0, sign: sign) }
            }
        }
    }

    @ViewBuilder
    private func plain(_ state: AssistantTask.State, sign: Sign, empty: AssistantWords) -> some View {
        let tasks = model.tasks(state)
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

/// Decisions apply to one suggestion in the person's current workspace.
private struct ProposalSection: View {
    @Environment(\.scenePhase) private var phase
    @Environment(AssistantModel.self) private var model
    @State private var submitting = false
    @State private var status: String?
    @State private var readError: String?
    @State private var confirming: ProposedTask?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if readError == nil, let proposals = model.proposals {
                if !proposals.isEmpty {
                    SectionHead(name: AssistantWords.proposals.said, count: proposals.count)
                    ForEach(proposals) { proposal in row(proposal) }
                }
                ForEach(model.proposalPreferences) { preference in
                    if preference.always { preferenceRow(preference, suggestAgain: false) }
                    if preference.quiet { preferenceRow(preference, suggestAgain: true) }
                }
                if model.canLookNow {
                    Button(AssistantWords.lookNow.said) {
                        perform(acknowledged: .lookRequested) { try await model.lookNow() }
                    }
                    .buttonStyle(LinkStyle())
                }
            }
            if let status { Text(status).font(.small125).foregroundStyle(Token.ink2) }
            if let readError { Text(readError).font(.small125).foregroundStyle(Token.ink2) }
            if model.proposals != nil || readError != nil || status != nil {
                Button(AssistantWords.refreshProposals.said) {
                    status = nil
                    Task { await refresh() }
                }
                .buttonStyle(LinkStyle())
            }
        }
        .disabled(submitting)
        .task { if model.person != nil { await refresh() } }
        .onChange(of: phase) { _, now in
            if now == .active { Task { await refresh() } }
        }
        .onChange(of: model.isWriting(.main)) { was, now in
            if was && !now { Task { await refresh() } }
        }
    }

    private func row(_ proposal: ProposedTask) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(proposal.title).font(.body14).foregroundStyle(Token.ink)
            if !proposal.why.isEmpty {
                Text(proposal.why).font(.small12).foregroundStyle(Token.ink2)
            }
            if let when = proposal.eventWhen { detail(.proposedWhen, when) }
            if let condition = proposal.eventCondition { detail(.proposedCondition, condition) }
            if let instruction = proposal.eventInstruction { detail(.proposedInstruction, instruction) }
            if confirming == proposal, canAllow(proposal) {
                Text(AssistantWords.alwaysAllowDetail.said(["kind": proposal.kind]))
                    .font(.small125).foregroundStyle(Token.ink)
                Flow(spacing: 12, lineSpacing: 6) {
                    Button(AssistantWords.confirmAlwaysAllow.said) {
                        confirming = nil
                        perform { try await model.alwaysAllow(proposal) }
                    }
                    Button(AssistantWords.cancel.said) { confirming = nil }
                }
                .buttonStyle(LinkStyle())
            } else {
                Flow(spacing: 12, lineSpacing: 6) {
                    Button((proposal.isEvent ? AssistantWords.setUpEvent : .startTask).said) {
                        perform { try await model.review(proposal, accept: true) }
                    }
                    Button(AssistantWords.declineProposal.said) {
                        perform { try await model.review(proposal, accept: false) }
                    }
                    if canAllow(proposal) {
                        Button(AssistantWords.alwaysAllow.said(["kind": proposal.kind])) { confirming = proposal }
                    }
                }
                .buttonStyle(LinkStyle())
            }
        }
        .accessibilityElement(children: .contain)
    }

    private func canAllow(_ proposal: ProposedTask) -> Bool {
        model.canAlwaysAllow && !proposal.isEvent
    }

    private func preferenceRow(_ preference: ProposalPreference, suggestAgain: Bool) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text((suggestAgain ? AssistantWords.kindQuiet : .kindAlwaysAllowed).said(["kind": preference.kind]))
                .font(.small125).foregroundStyle(Token.ink2)
            Button((suggestAgain ? AssistantWords.suggestAgain : .askMeFirst).said) {
                perform { try await model.changePreference(preference, suggestAgain: suggestAgain) }
            }
            .buttonStyle(LinkStyle())
        }
        .accessibilityElement(children: .contain)
    }

    private func detail(_ label: AssistantWords, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label.said).font(.small12).foregroundStyle(Token.ink3)
            Text(value).font(.body14).foregroundStyle(Token.ink)
        }
        .accessibilityElement(children: .combine)
    }

    private func refresh() async {
        do {
            try await model.refreshProposals()
            readError = nil
        } catch SourceProblem.refused(let why) {
            readError = why
        } catch SourceProblem.signedOut {
            readError = AssistantWords.suggestionsSignedOut.said
        } catch {
            readError = AssistantWords.notReachable.said
        }
    }

    private func perform(acknowledged: AssistantWords? = nil, _ action: @escaping @MainActor () async throws -> Void) {
        guard !submitting else { return }
        submitting = true
        status = nil
        Task { @MainActor in
            defer { submitting = false }
            do {
                try await action()
                status = acknowledged?.said
            } catch SourceProblem.refused(let why) {
                status = why
            } catch SourceProblem.signedOut {
                status = AssistantWords.suggestionsSignedOut.said
                return
            } catch {
                status = AssistantWords.proposalUnconfirmed.said
            }
            await refresh()
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
    /// How much larger the person's text is than the drawing's.
    @ScaledMetric(relativeTo: .body) private var grown: CGFloat = 1

    var body: some View {
        let side = 26 * min(max(grown, 1), 2)
        Glyph(sign).foregroundStyle(Token.ink2)
            .frame(width: side, height: side)
            .background(Token.raised, in: RoundedRectangle(cornerRadius: 8))
    }
}

/// A task: `.row`.
private struct TaskRow: View {
    let task: AssistantTask
    let sign: Sign
    @Environment(AssistantModel.self) private var model
    @Environment(\.dynamicTypeSize) private var textSize

    var body: some View {
        Button {
            if let item = task.item { model.show(item) }
        } label: {
            HStack(alignment: .top, spacing: 10) {
                SignBox(sign: sign)
                VStack(alignment: .leading, spacing: 2) {
                    Text(task.title).font(.body14).foregroundStyle(Token.ink).lineLimit(textSize.isAccessibilitySize ? 4 : 1)
                        .multilineTextAlignment(.leading)
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
            .accessibilityHint(AssistantWords.changeByTelling.said)
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
