import AssistantCore
import SwiftUI
#if os(iOS)
import UIKit
#endif

/// What every view of the assistant stands in: the model, and how lines are worded, which
/// follows the person's language and calendar and the minute it is.
public struct AssistantRoot<Content: View>: View {
    private let model: AssistantModel
    private let engine: AnyView?
    private let content: Content
    @Environment(\.locale) private var locale
    @Environment(\.calendar) private var calendar

    /// `engine` is a view the source needs to stand in the window to do its work, though it
    /// shows nothing: on an iPhone a web view runs its page only while it is in a window. It is
    /// given no room and is out of sight and out of VoiceOver's way.
    public init(model: AssistantModel, engine: AnyView? = nil, @ViewBuilder content: () -> Content) {
        self.model = model
        self.engine = engine
        self.content = content()
    }

    public var body: some View {
        TimelineView(.everyMinute) { moment in
            content.environment(\.wording, Wording(locale: locale, calendar: calendar, now: moment.date))
        }
        .background {
            if let engine {
                engine.frame(width: 0, height: 0).opacity(0).allowsHitTesting(false).accessibilityHidden(true)
            }
        }
        .environment(model)
        .font(.body14)
        .drawingLines()
        .foregroundStyle(Token.ink)
    }
}

/// A button of the window's top bar: `.tb`, and `.tb.on`.
struct BarButton: View {
    let sign: Sign
    var isOn = false
    let label: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Glyph(sign).foregroundStyle(isOn ? Token.ink : Token.ink2)
                .frame(width: Measure.toolbarButton, height: Measure.toolbarButton)
                .background(isOn ? Token.raised : Token.clear, in: RoundedRectangle(cornerRadius: 8))
                .contentShape(RoundedRectangle(cornerRadius: 8))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
        .help(label)
    }
}

/// Asks for a new sub-agent's job. Just the job: the main agent picks its role and its model.
struct JobSheet: View {
    @Environment(AssistantModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var job = ""
    @FocusState private var writes: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(AssistantWords.newSubAgent.said).font(.title15).foregroundStyle(Token.ink)
                .accessibilityAddTraits(.isHeader)
            TextField(AssistantWords.subAgentJob.said, text: $job, axis: .vertical)
                .lineLimit(3...8)
                .textFieldStyle(.plain)
                .font(.field15)
                .focused($writes)
                .padding(12)
                .background(Token.content, in: RoundedRectangle(cornerRadius: 10))
                .overlay { RoundedRectangle(cornerRadius: 10).strokeBorder(Token.line, lineWidth: 1) }
            Fine(words: AssistantWords.subAgentJobBody.said)
            HStack(spacing: 8) {
                Spacer(minLength: 0)
                Button(AssistantWords.cancel.said) { dismiss() }
                    .buttonStyle(BoxButtonStyle()).keyboardShortcut(.cancelAction)
                Button(AssistantWords.start.said) {
                    model.startSubAgent(job: job)
                    dismiss()
                }
                .buttonStyle(BoxButtonStyle(isMain: true))
                .keyboardShortcut(.defaultAction)
                .disabled(job.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(20)
        .frame(minWidth: 320, idealWidth: 460)
        .background(Token.raised)
        .onAppear { writes = true }
    }
}

/// The line that says Tilo is away, when it is.
struct ReachLine: View {
    @Environment(AssistantModel.self) private var model

    var body: some View {
        if model.reach == .unreachable {
            Text(AssistantWords.notReachable.said).font(.small12).foregroundStyle(Token.ink2)
                .padding(.horizontal, 10).padding(.vertical, 3)
                .background(Token.raised, in: Capsule())
                .padding(.bottom, 6)
        }
    }
}

#if os(macOS)
/// The Mac's window: the sidebar, the conversation with its field, and the Tasks panel, as drawn.
public struct MacWindow: View {
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording

    public init() {}

    public var body: some View {
        @Bindable var model = model
        Group {
            switch model.session {
            case .none:
                Rectangle().fill(Token.content)
            case .signedOut, .pairing:
                WayIn()
            case .signedIn:
                frame
            }
        }
        .background(Token.content)
        .ignoresSafeArea()
        .sheet(isPresented: $model.asksForJob) { JobSheet() }
    }

    private var frame: some View {
        HStack(spacing: 0) {
            if model.showsSidebar {
                Sidebar()
                    // Room for the window's three buttons, where the drawing has its three dots.
                    .padding(.top, 27)
                    .frame(width: Measure.sidebar)
                Rectangle().fill(Token.line).frame(width: 1)
            }
            VStack(spacing: 0) {
                topBar
                switch model.place {
                case .chat(let chat):
                    ConversationView(chat: chat)
                    ReachLine()
                    Composer(chat: chat)
                case .assistantView:
                    SupervisionView()
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            // The assistant view is a card on the page's ground, as the drawing has it.
            .background(model.place == .assistantView ? Token.page : Token.content)
            if model.showsPanel, model.chat != nil {
                Rectangle().fill(Token.line).frame(width: 1)
                TasksPanel { model.showsPanel = false }
                    .frame(width: Measure.panel)
            }
        }
    }

    /// The bar over the conversation: `.main-top`, with a sub-agent's head when one is open.
    private var topBar: some View {
        HStack(spacing: 4) {
            if !model.showsSidebar {
                BarButton(sign: .side, label: AssistantWords.showSidebar.said) { model.showsSidebar = true }
            }
            if case .chat(.subAgent(let id)) = model.place, let agent = model.subAgent(id) {
                VStack(alignment: .leading, spacing: 1) {
                    Text(agent.title).font(.title15).foregroundStyle(Token.ink).lineLimit(1)
                    Text(wording.header(agent)).font(.small12).foregroundStyle(Token.ink3).lineLimit(2)
                }
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(.isHeader)
            }
            Spacer(minLength: 8)
            if model.chat != nil {
                BarButton(sign: .panel, isOn: model.showsPanel,
                          label: (model.showsPanel ? AssistantWords.hideTasks : AssistantWords.showTasks).said) {
                    model.showsPanel.toggle()
                }
            }
            BarButton(sign: .new, label: AssistantWords.newSubAgent.said) { model.asksForJob = true }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: 48)
        // With the sidebar away, the window's three buttons stand here.
        .padding(.leading, model.showsSidebar ? 0 : 64)
    }
}
#endif

#if os(iOS)
/// The iPhone's arrangement of the same views: the conversation first, the sidebar one step
/// back, the Tasks panel as a sheet over the conversation.
public struct PhoneRoot: View {
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording
    @State private var path: [AssistantModel.Place]
    /// Whether the app is in front. When it leaves the front the system keeps a picture of its
    /// window, and what stands in a protected field is put out of sight before that.
    @State private var inFront = true

    /// `path` is what stands on the list of conversations when the window opens: the main chat,
    /// unless something else is asked for. Empty is the list itself.
    public init(path: [AssistantModel.Place] = [.chat(.main)]) {
        _path = State(initialValue: path)
    }

    public var body: some View {
        @Bindable var model = model
        Group {
            switch model.session {
            case .none:
                Rectangle().fill(Token.content).ignoresSafeArea()
            case .signedOut, .pairing:
                WayIn()
            case .signedIn:
                NavigationStack(path: $path) {
                    Sidebar { path = [model.place] }
                        .background(Token.content)
                        .toolbar(.hidden, for: .navigationBar)
                        .navigationDestination(for: AssistantModel.Place.self) { screen($0) }
                }
                .tint(Token.ink)
                .onChange(of: model.place) { if path.last != model.place { path = [model.place] } }
                .sheet(isPresented: $model.showsPanel) {
                    TasksPanel { model.showsPanel = false }
                        .padding(.top, 8)
                        .background(Token.content)
                        .presentationDetents([.medium, .large])
                        .presentationDragIndicator(.visible)
                }
            }
        }
        .sheet(isPresented: $model.asksForJob) {
            JobSheet().presentationDetents([.medium])
        }
        .environment(\.coversSecrets, !inFront)
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.willResignActiveNotification)) { _ in inFront = false }
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.didBecomeActiveNotification)) { _ in inFront = true }
    }

    @ViewBuilder
    private func screen(_ place: AssistantModel.Place) -> some View {
        VStack(spacing: 0) {
            switch place {
            case .chat(let chat):
                if case .subAgent(let id) = chat, let agent = model.subAgent(id) {
                    Text(wording.header(agent)).font(.small12).foregroundStyle(Token.ink3)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 16).padding(.vertical, 6)
                }
                ConversationView(chat: chat)
                ReachLine()
                Composer(chat: chat)
            case .assistantView:
                SupervisionView()
            }
        }
        .background(Token.content)
        .navigationTitle(title(place))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                if case .chat = place {
                    Button {
                        model.showsPanel = true
                    } label: {
                        Glyph(.panel, side: 20)
                    }
                    .accessibilityLabel(AssistantWords.showTasks.said)
                }
                Button {
                    model.asksForJob = true
                } label: {
                    Glyph(.new, side: 20)
                }
                .accessibilityLabel(AssistantWords.newSubAgent.said)
            }
        }
    }

    private func title(_ place: AssistantModel.Place) -> String {
        switch place {
        case .chat(.main): AssistantWords.mainChat.said
        case .chat(.subAgent(let id)): model.subAgent(id)?.title ?? AssistantWords.subAgents.said
        case .assistantView: AssistantWords.assistantView.said
        }
    }
}
#endif
