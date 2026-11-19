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

/// Direct deep-link destination for the first waiting decision or item.
public enum WaitingTarget: Equatable, Sendable {
    case item(Item.ID, Chat)
    case subAgent(SubAgent.ID)
}

extension AssistantModel {
    /// Resolve the highest-priority waiting decision or item to jump straight to.
    public func firstWaitingTarget(preferring lines: [SinceLeft.Line] = []) -> WaitingTarget? {
        if let item = lines.first(where: { $0.item != nil })?.item {
            return .item(item, .main)
        }
        if let item = tasks(.needsInput).first(where: { $0.item != nil })?.item {
            return .item(item, .main)
        }
        if let item = tasks(.readyForReview).first(where: { $0.item != nil })?.item {
            return .item(item, .main)
        }
        if let item = sinceLeft?.waits.first(where: { $0.item != nil })?.item {
            return .item(item, .main)
        }
        if let item = conversation(.main).items.first(where: { $0.waits }) {
            return .item(item.id, .main)
        }
        if let agent = subAgents.first(where: { $0.state == .needsYou }) {
            let subChat = Chat.subAgent(agent.id)
            if let item = conversation(subChat).items.first(where: { $0.waits }) {
                return .item(item.id, subChat)
            }
            return .subAgent(agent.id)
        }
        return nil
    }

    /// Jump directly to the first waiting decision or item; falls back to Tasks only when none can be addressed directly.
    @discardableResult
    public func openFirstWaiting(preferring lines: [SinceLeft.Line] = []) -> Bool {
        putAwayStage()
        if let target = firstWaitingTarget(preferring: lines) {
            showsPanel = false
            switch target {
            case .item(let item, let chat):
                show(item, in: chat)
            case .subAgent(let id):
                open(.subAgent(id))
            }
            return true
        }
        showTasks(.active)
        return false
    }
}

/// The line a phone's views stand on: the same edge the conversation and the message field stand
/// on, so that a strip, a card and a field are never a point or two out from one another.
private let phoneEdge: CGFloat = 16

/// The room a phone's sheet keeps under the grabber the system draws at its top: the grabber lies
/// over a sheet's first points, and a head that stands there is a head the person cannot read.
/// The stage sheet and the Tasks sheet begin under it with the same room.
let phoneSheetGrabber: CGFloat = 22

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
/// Tilo cannot be reached, and the one press that asks again. From the app's opening until the
/// main chat stands: the quiet word for waiting, one sentence through every step behind it (the
/// page, the session, the look for the assistant, what answers for it), so that a server that
/// takes its time is not silent and no step is said as if it were something new. A try that is
/// in work is never said to have failed, however long it takes. Where Tilo answers and does not
/// let this device in, that is said in its place, with the way in offered (`WayInOffer`).
public struct ReachLine: View {
    @Environment(AssistantModel.self) private var model

    public init() {}

    public var body: some View {
        if model.joinStops {
            WayInOffer()
        } else if model.reach == .unreachable {
            // What says it carries the way on with it: the press is where the sentence is, not
            // hunted for in the menu bar or at the way in.
            HStack(spacing: 10) {
                Text(AssistantWords.notReachable.said).font(.small12).foregroundStyle(Token.ink2)
                    .fixedSize(horizontal: false, vertical: true)
                Button(AssistantWords.tryAgain.said) { model.tryAgain(whereItStopped: model.joinApproved != nil) }
                    .buttonStyle(PillStyle(isMain: true))
            }
            .padding(.leading, 12).padding(.trailing, 5).padding(.vertical, 5)
            .background(Token.raised, in: Capsule())
            .padding(.horizontal, 16).padding(.bottom, 6)
            .accessibilityElement(children: .contain)
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
                    SupervisionView(standsInTheWindow: true)
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
                SplitStudioStage(staged: staged, chat: chat)
                    .frame(minWidth: 480, maxWidth: .infinity, maxHeight: .infinity)
            } else if model.showsPanel, model.chat != nil {
                Rectangle().fill(Token.line).frame(width: 1)
                TasksPanel {
                    if model.cancelFieldStepping?() == true { return }
                    model.showsPanel = false
                }
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
                        // The one at work is named; where more than one is, the count is said in the
                        // person's language, as every other count of the window is.
                        Text(model.activeSubAgents.count == 1
                            ? active.title
                            : AssistantWords.subAgentsCount.said(["count": String(model.activeSubAgents.count)]))
                            .font(.small12).foregroundStyle(Token.ink).lineLimit(1)
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .background(Token.raised, in: Capsule())
                    .overlay { Capsule().strokeBorder(Token.line, lineWidth: 1) }
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(model.activeSubAgents.count == 1
                        ? "\(active.title), \(AssistantWords.showSubAgent.said)"
                        : AssistantWords.subAgentsCount.said(["count": String(model.activeSubAgents.count)]))
                }
                .buttonStyle(.plain)
            }
            if model.chat != nil {
                BarButton(sign: .panel, isOn: model.showsPanel,
                          label: (model.showsPanel ? AssistantWords.hideTasks : AssistantWords.showTasks).said) {
                    // The Tasks panel takes the stage's place, and the stage is put away with its
                    // tabs kept: they stand in the bar again when it is opened.
                    model.putAwayStage()
                    model.showsPanel.toggle()
                }
            }
            // What the conversation shared is opened here while nothing of it stands: one press on
            // a view opens the stage on it, and it stays where it stood as the person goes on reading.
            if model.stagedLive == nil, !model.showsPanel, let chat = model.chat,
               !model.openLiveTabs(in: chat).isEmpty || !model.liveThings(in: chat).isEmpty {
                BarButton(sign: .plus, label: AssistantWords.openLiveView.said) {
                    model.showNextLive(in: chat)
                }
                // The key for it, said where the button is; the menu says it too.
                .keyboardShortcut("j", modifiers: [.command, .option])
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
    /// The conversation the stage stands beside: the tabs are its views, and so is the +.
    let chat: Chat
    @Environment(AssistantModel.self) private var model
    @Environment(\.liveStageView) private var drawStage
    @Environment(\.liveThingView) private var drawInline

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 6) {
                // Every view opened in this conversation, and every one it shared, once each: a tab
                // put away opens again by one press, and one whose tab was closed reopens here.
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(tabs) { item in
                            let selected = item.id == staged.id
                            Button {
                                model.show(item)
                            } label: {
                                Text(item.called)
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
                            .accessibilityAddTraits(selected ? .isSelected : [])
                            .contextMenu {
                                // A tab of the conversation's own is let go of here; one pressed on
                                // again after that opens from the +.
                                Button(AssistantWords.closeStage.said) { model.closeLiveTab(item.id) }
                            }
                        }
                    }
                }
                // A view the conversation shared stands in no bar: the + opens any of them, and the
                // one after the stage's: nothing of it is asked of the assistant.
                Menu {
                    let shared = model.liveThings(in: chat)
                    if shared.isEmpty {
                        Text(AssistantWords.noLiveViews.said)
                    }
                    ForEach(shared) { item in
                        Button(item.called) { model.show(item) }
                    }
                } label: {
                    Glyph(.plus, side: 13, grows: false).foregroundStyle(Token.ink2)
                        .frame(width: 28, height: 28)
                        .background(Token.content, in: RoundedRectangle(cornerRadius: 7))
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .accessibilityLabel(AssistantWords.openLiveView.said)
                .tip(AssistantWords.openLiveView.said)
                Button {
                    if model.cancelFieldStepping?() == true { return }
                    model.putAwayStage()
                } label: {
                    Text("×").font(.tilo(16, .medium)).foregroundStyle(Token.ink2)
                        .frame(width: 28, height: 28)
                        .background(Token.content, in: RoundedRectangle(cornerRadius: 7))
                }
                .buttonStyle(.plain)
                // The stage is put away and its tabs stand: one press opens the view again, and
                // nothing is asked of the assistant meanwhile. Escape puts it away as well.
                .keyboardShortcut(.cancelAction)
                .accessibilityLabel(AssistantWords.closeStage.said)
                .tip(AssistantWords.closeStage.said)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(Token.raised)
            Rectangle().fill(Token.line).frame(height: 1)
            Group {
                if let draw = drawStage ?? drawInline {
                    draw(staged)
                        .id(staged.id)
                } else {
                    VStack(spacing: 8) {
                        Mark(side: 28)
                        Text(staged.called).font(.title15).foregroundStyle(Token.ink)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .background(Token.content)
    }

    /// What the bar's tabs hold, each view once: every view opened in this conversation, the one
    /// the stage shows first so that it never stands out of sight, then the ones the conversation
    /// shared that were not opened, so that a view whose tab was closed comes back from the +.
    private var tabs: [LiveThing] {
        var list: [LiveThing] = [staged]
        for thing in model.openLiveTabs(in: chat) + model.liveThings(in: chat)
        where !list.contains(where: { $0.id == thing.id }) {
            list.append(thing)
        }
        return list
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
                // While a live view fills the screen, its sheet is what the back gesture puts away:
                // the chat under it stays the one the person was in.
                .onChange(of: model.place) {
                    if model.stagedLive == nil, path.last != model.place { path = [model.place] }
                }
                .sheet(isPresented: $model.showsPanel) {
                    TasksPanel { model.showsPanel = false }
                        // Its own head stands under the grabber, as the stage's does.
                        .padding(.top, phoneSheetGrabber)
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
                        .padding(.horizontal, phoneEdge).padding(.vertical, 6)
                }
                // The strip and the conversation are one stretch: the strip's own hairline closes it,
                // and the conversation's end fades over nothing but the pieces that scroll under it.
                PhoneNowStrip(chat: chat)
                let nothingUnder = model.liveThings(in: chat).isEmpty && model.activeSubAgents.isEmpty && model.waiting == 0
                ConversationView(chat: chat, head: textSize.isAccessibilitySize ? head : nil, footFade: nothingUnder)
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
        let live = model.liveThings(in: chat)
        // Every live view open in this conversation stands in the strip, the one in the stage or
        // the sheet first, so that one put away opens again by one press: nothing is asked of
        // the assistant, and no view is read again.
        let staged = model.stagedLive.flatMap { $0.chat == chat ? $0 : nil }
        let lives = PhoneNowStrip.tabs(staged: staged, opened: model.openLiveTabs(in: chat), live: live)
        if !active.isEmpty || model.waiting > 0 || !lives.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    if model.waiting > 0 {
                        Button {
                            model.openFirstWaiting()
                        } label: {
                            HStack(spacing: 6) {
                                Circle().fill(Token.you).frame(width: 6, height: 6)
                                Text(AssistantWords.waitingCount.said(["count": String(model.waiting)]))
                                    .font(.tilo(12, .semibold)).foregroundStyle(Token.ink)
                            }
                            .padding(.horizontal, 11).padding(.vertical, 8)
                            .background(Token.youTint, in: Capsule())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(AssistantWords.waitingCount.said(["count": String(model.waiting)]))
                        .accessibilityHint(model.firstWaitingTarget() != nil
                            ? AssistantWords.waitsForAnswer.said
                            : AssistantWords.showTasks.said)
                    }
                    ForEach(active) { agent in
                        Button {
                            model.open(.subAgent(agent.id))
                        } label: {
                            HStack(spacing: 6) {
                                Circle().fill(Token.asst).frame(width: 6, height: 6)
                                Text(agent.title).font(.tilo(12, .medium)).foregroundStyle(Token.ink).lineLimit(1)
                            }
                            .padding(.horizontal, 11).padding(.vertical, 8)
                            .background(Token.asstTint, in: Capsule())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("\(agent.title), \(AssistantWords.showSubAgent.said)")
                    }
                    ForEach(lives) { thing in
                        let shown = thing.id == staged?.id
                        Button {
                            model.stagedLive = thing
                        } label: {
                            HStack(spacing: 6) {
                                // The stage's view is the assistant's colour, as the card's own dot is;
                                // one put away is a quiet mark, not a green one, which says a thing is on.
                                Circle().fill(shown ? Token.asst : Token.ink3).frame(width: 6, height: 6)
                                Text("\(thing.called) ⤢").font(.tilo(12, .medium)).foregroundStyle(Token.ink).lineLimit(1)
                            }
                            .padding(.horizontal, 11).padding(.vertical, 8)
                            .background(Token.raised, in: Capsule())
                            .overlay { Capsule().strokeBorder(shown ? Token.asst : Token.line, lineWidth: 1) }
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(AssistantWords.openFullScreenNamed.said(["name": thing.called]))
                        .accessibilityAddTraits(shown ? .isSelected : [])
                    }
                }
                .padding(.horizontal, phoneEdge)
                .padding(.vertical, 8)
            }
            .background(Token.content)
            Rectangle().fill(Token.line).frame(height: 1)
        }
    }

    /// What the strip shows, newest first, each view once: the one in the sheet, then the views
    /// opened before it, then the ones the conversation holds. One put away stands here still.
    static func tabs(staged: LiveThing?, opened: [LiveThing], live: [LiveThing]) -> [LiveThing] {
        var tabs: [LiveThing] = []
        for thing in [staged].compactMap({ $0 }) + opened.reversed() + live
        where !tabs.contains(where: { $0.id == thing.id }) {
            tabs.append(thing)
        }
        return tabs
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
            // The way back, its hairline, and the name of what fills the sheet: the hairline stretches
            // the whole way across, and what stands above it stands on the sheet's own ground.
            HStack(spacing: 10) {
                Button {
                    model.stagedLive = nil
                } label: {
                    HStack(spacing: 3) {
                        Glyph(.back, side: 15, grows: false)
                        Text(AssistantWords.back.said).font(.tilo(15, .medium))
                    }
                    .foregroundStyle(Token.asst)
                    .frame(minHeight: 34)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                // The way back takes the Escape key on a Mac, as every view that stands over
                // another is put away by it.
                .keyboardShortcut(.cancelAction)
                .accessibilityLabel(AssistantWords.back.said)
                Text(thing.called)
                    .font(.title15)
                    .foregroundStyle(Token.ink)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(.horizontal, phoneEdge)
            .padding(.top, phoneSheetGrabber)
            .padding(.bottom, 10)
            .background(Token.content)
            Rectangle().fill(Token.line).frame(height: 1)
            if let draw = drawStage ?? drawInline {
                draw(thing).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Token.content)
        .presentationBackground(Token.content)
    }
}
#endif
