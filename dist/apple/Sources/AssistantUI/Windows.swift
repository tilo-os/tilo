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
    @Environment(\.colorScheme) private var system
    @Environment(\.accessibilityReduceMotion) private var reduced

    /// `engine` is a view the source needs to stand in the window to do its work, though it
    /// shows nothing: on an iPhone a web view runs its page only while it is in a window. It is
    /// given no room and is out of sight and out of VoiceOver's way.
    public init(model: AssistantModel, engine: AnyView? = nil, @ViewBuilder content: () -> Content) {
        self.model = model
        self.engine = engine
        self.content = content()
    }

    /// Light or dark, as every part of the frame is drawn: the look the person's Tilo has, so
    /// that frame and Tilo never differ, and the system's until their Tilo has said one.
    private var look: ColorScheme {
        switch model.look {
        case .dark?: .dark
        case .light?: .light
        case nil: system
        }
    }

    public var body: some View {
        TimelineView(.everyMinute) { moment in
            content.environment(\.wording, Wording(locale: locale, calendar: calendar, now: moment.date))
        }
        // The frame's own parts take the look of the person's Tilo, and follow it as it changes.
        // The engine's view below does not: Tilo's page reads the system's look from the view
        // it stands in, and a theme that follows the system must never read the frame's back.
        .environment(\.colorScheme, look)
        .animation(reduced ? nil : .easeOut(duration: Motion.fast), value: look)
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
        .tip(label)
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

/// What is said over the message field of whether Tilo answers. After a try that failed: that
/// Tilo cannot be reached. From the app's opening until the main chat stands: the quiet word
/// for waiting, one sentence through every step behind it (the page, the session, the look for
/// the assistant, what answers for it), so that a server that takes its time is not silent and
/// no step is said as if it were something new. A try that is in work is never said to have
/// failed, however long it takes. Where Tilo answers and does not let this device in, that is
/// said in its place, with the way in offered (`WayInOffer`).
public struct ReachLine: View {
    @Environment(AssistantModel.self) private var model

    public init() {}

    public var body: some View {
        if model.joinStops {
            WayInOffer()
        } else if model.reach == .unreachable {
            Text(AssistantWords.notReachable.said).font(.small12).foregroundStyle(Token.ink2)
                .padding(.horizontal, 10).padding(.vertical, 3)
                .background(Token.raised, in: Capsule())
                .padding(.bottom, 6)
        } else if model.isStarting {
            Fine(words: AssistantWords.pairing.said).padding(.bottom, 6)
        }
    }
}

/// Where the person's Tilo answers and does not let this device in: one sentence that says so
/// and what its button does, and the button, which does what "Sign out of this device" does
/// and so shows the way in. Nothing happens until the person presses.
struct WayInOffer: View {
    @Environment(AssistantModel.self) private var model

    var body: some View {
        VStack(spacing: 8) {
            Text(AssistantWords.joinStops.said).font(.small12).foregroundStyle(Token.ink2)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Button(AssistantWords.signOutToPair.said) { model.signOut() }.buttonStyle(BoxButtonStyle(isMain: true))
        }
        .padding(.horizontal, 14).padding(.vertical, 10)
        .background(Token.raised, in: RoundedRectangle(cornerRadius: 12))
        .padding(.horizontal, 16).padding(.bottom, 6)
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
            case .signedOut, .reset, .asked, .pairing, .confirming:
                WayIn()
            case .signedIn:
                frame
            }
        }
        .background(Token.content)
        .ignoresSafeArea()
        .sheet(isPresented: $model.asksForJob) { JobSheet() }
        .sheet(item: $model.selecting) { SelectTextSheet(words: $0) }
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
                    Composer(chat: chat).layoutPriority(1)
                case .assistantView:
                    SupervisionView()
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            // The assistant view is a card on the page's ground, as the drawing has it.
            .background(model.place == .assistantView ? Token.page : Token.content)
            // The middle of the window is read as one part, between the sidebar and the Tasks panel.
            .accessibilityElement(children: .contain)
            .accessibilityLabel((model.place == .assistantView ? AssistantWords.assistantView : AssistantWords.conversation).said)
            if let staged = model.stagedLive, case .chat(let chat) = model.place {
                Rectangle().fill(Token.line).frame(width: 1)
                SplitStudioStage(staged: staged, artifacts: model.liveThings(in: chat))
                    .frame(minWidth: 480, maxWidth: .infinity, maxHeight: .infinity)
            } else if model.showsPanel, model.chat != nil {
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
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("\(agent.title), \(wording.header(agent).spoken)")
                .accessibilityAddTraits(.isHeader)
            }
            Spacer(minLength: 8)
            if let active = model.activeSubAgents.first {
                Button {
                    model.open(.subAgent(active.id))
                } label: {
                    HStack(spacing: 6) {
                        Circle().fill(Token.asst).frame(width: 6, height: 6)
                        Text(model.activeSubAgents.count == 1 ? active.title : "\(model.activeSubAgents.count) sub-agents")
                            .font(.small12).foregroundStyle(Token.ink).lineLimit(1)
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .background(Token.raised, in: Capsule())
                    .overlay { Capsule().strokeBorder(Token.line, lineWidth: 1) }
                }
                .buttonStyle(.plain)
            }
            if model.chat != nil {
                BarButton(sign: .panel, isOn: model.showsPanel,
                          label: (model.showsPanel ? AssistantWords.hideTasks : AssistantWords.showTasks).said) {
                    model.stagedLive = nil
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

/// Variant A (Split Studio): full-height live Tilo stage right beside the conversation.
private struct SplitStudioStage: View {
    let staged: LiveThing
    let artifacts: [LiveThing]
    @Environment(AssistantModel.self) private var model
    @Environment(\.liveStageView) private var drawStage
    @Environment(\.liveThingView) private var drawInline

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 6) {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        let list = artifacts.contains(where: { $0.id == staged.id }) ? artifacts : [staged] + artifacts
                        ForEach(list) { item in
                            let selected = item.id == staged.id
                            Button {
                                model.stagedLive = item
                            } label: {
                                Text(item.title)
                                    .font(.tilo(12, selected ? .semibold : .medium))
                                    .foregroundStyle(selected ? Token.ink : Token.ink2)
                                    .lineLimit(1)
                                    .padding(.horizontal, 10)
                                    .padding(.vertical, 5)
                                    .background(selected ? Token.content : Token.clear, in: RoundedRectangle(cornerRadius: 7))
                                    .overlay {
                                        RoundedRectangle(cornerRadius: 7)
                                            .strokeBorder(selected ? Token.asst : Token.line, lineWidth: 1)
                                    }
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
                Button {
                    model.stagedLive = nil
                } label: {
                    Text("×").font(.tilo(16, .medium)).foregroundStyle(Token.ink2)
                        .frame(width: 28, height: 28)
                        .background(Token.content, in: RoundedRectangle(cornerRadius: 7))
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Close stage")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(Token.raised)
            Rectangle().fill(Token.line).frame(height: 1)
            Group {
                if let draw = drawStage ?? drawInline {
                    draw(staged)
                } else {
                    VStack(spacing: 8) {
                        Mark(side: 28)
                        Text(staged.title).font(.title15).foregroundStyle(Token.ink)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .background(Token.content)
    }
}
#endif

#if os(iOS)
private extension View {
    /// The ground a whole screen of the iPhone stands on. It reaches past every safe area, the
    /// keyboard's too: the keyboard's upper corners are round, and what they leave in sight is
    /// the screen's own ground, where the system's view under it is black in the dark.
    func ground(_ token: Token) -> some View {
        background { Rectangle().fill(token).ignoresSafeArea() }
    }
}

/// The iPhone's order of the same views: the conversation first, the sidebar one step
/// back, the Tasks panel as a sheet over the conversation.
public struct PhoneRoot: View {
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording
    @State private var path: [AssistantModel.Place]
    /// Whether the app is in front. When it leaves the front the system keeps a picture of its
    /// window, and what stands in a protected field is put out of sight before that. An app that
    /// is brought up while it is not in front begins covered.
    @State private var inFront = UIApplication.shared.applicationState == .active
    @Environment(\.dynamicTypeSize) private var textSize

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
            case .signedOut, .reset, .asked, .pairing, .confirming:
                WayIn()
            case .signedIn:
                NavigationStack(path: $path) {
                    Sidebar { path = [model.place] }
                        .ground(Token.content)
                        .toolbar(.hidden, for: .navigationBar)
                        .navigationDestination(for: AssistantModel.Place.self) { screen($0) }
                }
                .tint(Token.ink)
                .onChange(of: model.place) { if path.last != model.place { path = [model.place] } }
                .onChange(of: model.shownItem, initial: true) {
                    if model.shownItem != nil, path.last != model.place { path = [model.place] }
                }
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
        .sheet(item: $model.selecting) {
            SelectTextSheet(words: $0).presentationDetents([.medium, .large]).presentationDragIndicator(.visible)
        }
        .sheet(item: $model.stagedLive) { thing in
            PhoneLiveStageSheet(thing: thing)
                .presentationDetents([.large])
                .presentationDragIndicator(.visible)
        }
        .environment(\.coversSecrets, !inFront)
        // An iPhone's Settings show the version alone, so the app says its build itself: at
        // its way in and in its list, in the words of the Mac's About window.
        .environment(\.buildSaid, AssistantBuild.said(info: Bundle.main.infoDictionary))
        // Every word the system has for leaving the front covers, whichever comes first; only being
        // in front again uncovers.
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.willResignActiveNotification)) { _ in inFront = false }
        .onReceive(NotificationCenter.default.publisher(for: UIScene.willDeactivateNotification)) { _ in inFront = false }
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.didEnterBackgroundNotification)) { _ in inFront = false }
        .onReceive(NotificationCenter.default.publisher(for: UIScene.didEnterBackgroundNotification)) { _ in inFront = false }
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.didBecomeActiveNotification)) { _ in inFront = true }
    }

    @ViewBuilder
    private func screen(_ place: AssistantModel.Place) -> some View {
        VStack(spacing: 0) {
            switch place {
            case .chat(let chat):
                // Who a sub-agent is and where it runs stays in sight over its conversation; at the
                // largest text sizes it would take the conversation's room, so it scrolls with it.
                let head: String? = if case .subAgent(let id) = chat, let agent = model.subAgent(id) { wording.header(agent) } else { nil }
                if let head, !textSize.isAccessibilitySize {
                    Text(head).font(.small12).foregroundStyle(Token.ink3)
                        .accessibilityLabel(head.spoken)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 16).padding(.vertical, 6)
                }
                PhoneNowStrip(chat: chat)
                ConversationView(chat: chat, head: textSize.isAccessibilitySize ? head : nil)
                    // A touch on the conversation puts the keyboard away. What is touched still takes the touch.
                    .simultaneousGesture(TapGesture().onEnded { Self.putKeyboardAway() })
                ReachLine()
                Composer(chat: chat).layoutPriority(1)
            case .assistantView:
                SupervisionView()
            }
        }
        .ground(place == .assistantView ? Token.page : Token.content)
        // A drag down the conversation takes the keyboard down with the finger, as the system's own apps do.
        .scrollDismissesKeyboard(.interactively)
        .navigationTitle(title(place))
        .navigationBarTitleDisplayMode(.inline)
        // The bar stands on the window's own ground: the conversation does not show through its title.
        .toolbarBackground(place == .assistantView ? Token.page : Token.content, for: .navigationBar)
        .toolbarBackgroundVisibility(.visible, for: .navigationBar)
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                if case .chat = place {
                    Button {
                        model.showsPanel = true
                    } label: {
                        Glyph(.panel, side: 20, grows: false)
                    }
                    .accessibilityLabel(AssistantWords.showTasks.said)
                }
                Button {
                    model.asksForJob = true
                } label: {
                    Glyph(.new, side: 20, grows: false)
                }
                .accessibilityLabel(AssistantWords.newSubAgent.said)
            }
        }
    }

    /// Whoever holds the keyboard lets it go. The field keeps its words, and sending does not call
    /// this: after a message the field is empty and takes the next one.
    private static func putKeyboardAway() {
        UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
    }

    private func title(_ place: AssistantModel.Place) -> String {
        switch place {
        case .chat(.main): AssistantWords.mainChat.said
        case .chat(.subAgent(let id)): model.subAgent(id)?.title ?? AssistantWords.subAgents.said
        case .assistantView: AssistantWords.assistantView.said
        }
    }
}

/// Phone 3 (Focus Feed + Now Strip): pinned strip for running sub-agents, waiting approvals, and latest live artifact.
private struct PhoneNowStrip: View {
    let chat: Chat
    @Environment(AssistantModel.self) private var model

    var body: some View {
        let active = model.activeSubAgents
        let latestLive = model.liveThings(in: chat).first
        if !active.isEmpty || model.waiting > 0 || latestLive != nil {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    if model.waiting > 0 {
                        Button {
                            model.showsPanel = true
                        } label: {
                            HStack(spacing: 6) {
                                Circle().fill(Token.you).frame(width: 6, height: 6)
                                Text("\(model.waiting) waiting").font(.tilo(12, .semibold)).foregroundStyle(Token.ink)
                            }
                            .padding(.horizontal, 11).padding(.vertical, 6)
                            .background(Token.youTint, in: Capsule())
                        }
                        .buttonStyle(.plain)
                    }
                    ForEach(active) { agent in
                        Button {
                            model.open(.subAgent(agent.id))
                        } label: {
                            HStack(spacing: 6) {
                                Circle().fill(Token.asst).frame(width: 6, height: 6)
                                Text(agent.title).font(.tilo(12, .medium)).foregroundStyle(Token.ink).lineLimit(1)
                            }
                            .padding(.horizontal, 11).padding(.vertical, 6)
                            .background(Token.asstTint, in: Capsule())
                        }
                        .buttonStyle(.plain)
                    }
                    if let latestLive {
                        Button {
                            model.stagedLive = latestLive
                        } label: {
                            HStack(spacing: 6) {
                                Circle().fill(Token.on).frame(width: 6, height: 6)
                                Text("\(latestLive.title) ⤢").font(.tilo(12, .medium)).foregroundStyle(Token.ink).lineLimit(1)
                            }
                            .padding(.horizontal, 11).padding(.vertical, 6)
                            .background(Token.raised, in: Capsule())
                            .overlay { Capsule().strokeBorder(Token.line, lineWidth: 1) }
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 7)
            }
            .background(Token.content)
            Rectangle().fill(Token.line).frame(height: 1)
        }
    }
}

/// Full-screen interactive live Tilo sheet on iPhone.
private struct PhoneLiveStageSheet: View {
    let thing: LiveThing
    @Environment(AssistantModel.self) private var model
    @Environment(\.liveStageView) private var drawStage
    @Environment(\.liveThingView) private var drawInline

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(thing.title).font(.title15).foregroundStyle(Token.ink).lineLimit(1)
                Spacer()
                Button("Done") { model.stagedLive = nil }
                    .font(.tilo(14, .semibold))
                    .foregroundStyle(Token.asst)
                    .buttonStyle(.plain)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .background(Token.raised)
            Rectangle().fill(Token.line).frame(height: 1)
            if let draw = drawStage ?? drawInline {
                draw(thing).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Token.content)
    }
}
#endif
