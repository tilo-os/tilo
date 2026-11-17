import AssistantCore
import AssistantDrawing
import AssistantUI
import SwiftUI

/// One screen to draw.
@MainActor
struct Screen {
    let name: String
    let size: CGSize
    let view: AnyView
}

/// Every screen of the drawing, built from the app's own views and the drawing's own content.
@MainActor
enum Screens {
    /// A model that holds what a source holds now.
    static func model(_ content: MemorySource.Content, _ arrange: (AssistantModel) -> Void = { _ in }) async -> AssistantModel {
        let model = AssistantModel(source: MemorySource(content, now: { Drawing.now }))
        await model.prime()
        arrange(model)
        return model
    }

    /// A view as the app roots it, at the drawing's moment and in its language.
    static func rooted(_ model: AssistantModel, _ scheme: ColorScheme, @ViewBuilder _ view: () -> some View) -> AnyView {
        AnyView(view()
            .environment(model)
            .environment(\.wording, Drawing.wording)
            .environment(\.locale, Drawing.locale)
            .environment(\.calendar, Drawing.calendar)
            .environment(\.colorScheme, scheme)
            .font(.body14)
            .drawingLines()
            .foregroundStyle(Token.ink))
    }

    /// The drawing's content with one piece alone in the main chat, and nothing in the panel.
    static func piece(_ items: [Item]) -> MemorySource.Content {
        var content = Drawing.content()
        content.main = items
        content.tasks = []
        return content
    }

    static var empty: MemorySource.Content {
        var content = MemorySource.Content()
        content.session = .signedIn(Drawing.person)
        return content
    }

    static let pieces: [(String, [Item], CGFloat)] = [
        ("piece-secret", Drawing.secretPiece, 330),
        ("piece-connection", Drawing.connectionPiece, 250),
        ("piece-event", Drawing.eventPiece, 360),
        ("piece-brief", Drawing.briefPiece, 300),
    ]

    #if os(iOS)
    /// Every screen on an iPhone, at the size of the phone it is drawn on.
    static func phone(_ scheme: ColorScheme) async -> [Screen] {
        var screens: [Screen] = []
        func add(_ name: String, _ content: MemorySource.Content, path: [AssistantModel.Place] = [.chat(.main)],
                 secret: Bool = false, _ arrange: @escaping (AssistantModel) -> Void = { _ in }) async {
            let model = await model(content) {
                $0.showsPanelInMain = false
                arrange($0)
            }
            screens.append(Screen(name: name, size: .zero, view: rooted(model, scheme) { PhoneRoot(path: path) }))
        }
        await add("phone-main-chat", Drawing.content())
        await add("phone-list", Drawing.content(), path: [])
        await add("phone-tasks", Drawing.content()) { $0.showsPanelInMain = true }
        await add("phone-scheduled", Drawing.content()) {
            $0.panelTab = .scheduled
            $0.showsPanelInMain = true
        }
        await add("phone-sub-agent", Drawing.content(), path: [.chat(.subAgent("usage-drop"))]) { $0.open(.subAgent("usage-drop")) }
        await add("phone-replies", Drawing.content()) { $0.toggleReplies(of: "maya", under: "maya", in: .main) }
        var comparing = MemorySource.Content()
        comparing.session = .confirming(code: "481 207")
        await add("phone-compare-code", comparing)
        await add("phone-assistant-view", Drawing.content(), path: [.assistantView]) { $0.place = .assistantView }
        for (name, items, _) in pieces {
            await add("phone-\(name)", piece(items))
        }
        await add("phone-piece-field", piece([])) { $0.drafts[.main] = Draft(text: Drawing.typed) }
        var out = MemorySource.Content()
        out.session = .signedOut
        await add("phone-way-in", out)
        await add("phone-empty", empty)
        let live = await model(empty)
        screens.append(Screen(name: "phone-live-activities", size: .zero, view: rooted(live, scheme) { LiveBoard() }))
        return screens
    }
    #endif

    #if os(macOS)
    static func mac(_ scheme: ColorScheme) async -> [Screen] {
        let window = CGSize(width: 1280, height: 820)
        var screens: [Screen] = []
        screens.append(Screen(name: "main-chat", size: window, view: rooted(await model(Drawing.content()), scheme) { MacWindow() }))
        let agent = await model(Drawing.content()) { $0.open(.subAgent("usage-drop")) }
        screens.append(Screen(name: "sub-agent", size: CGSize(width: 1280, height: 700), view: rooted(agent, scheme) { MacWindow() }))
        let replies = await model(Drawing.content()) { $0.toggleReplies(of: "maya", under: "maya", in: .main) }
        screens.append(Screen(name: "replies", size: window, view: rooted(replies, scheme) { MacWindow() }))
        let scheduled = await model(Drawing.content()) { $0.panelTab = .scheduled }
        screens.append(Screen(name: "scheduled-events", size: window, view: rooted(scheduled, scheme) { MacWindow() }))
        // The panel alone at the width the drawing gives it on its own board.
        screens.append(Screen(name: "scheduled-events-wide", size: CGSize(width: 850, height: 720), view: rooted(scheduled, scheme) {
            TasksPanel {}.background(Token.content)
        }))
        let view = await model(Drawing.content()) { $0.place = .assistantView }
        screens.append(Screen(name: "assistant-view", size: window, view: rooted(view, scheme) { MacWindow() }))
        for (name, items, height) in pieces {
            let one = await model(piece(items))
            screens.append(Screen(name: name, size: CGSize(width: 429, height: height), view: rooted(one, scheme) {
                ConversationView(chat: .main, picturedSecret: name == "piece-secret" ? "••••••••••••" : "")
                    .background(Token.content)
            }))
        }
        let typing = await model(piece([])) { $0.drafts[.main] = Draft(text: Drawing.typed) }
        screens.append(Screen(name: "piece-field", size: CGSize(width: 429, height: 170), view: rooted(typing, scheme) {
            VStack { Spacer(minLength: 0); Composer(chat: .main) }.background(Token.content)
        }))
        var out = MemorySource.Content()
        out.session = .signedOut
        screens.append(Screen(name: "way-in", size: window, view: rooted(await model(out), scheme) { MacWindow() }))
        screens.append(Screen(name: "empty", size: window, view: rooted(await model(empty), scheme) { MacWindow() }))
        screens.append(Screen(name: "lock-screen", size: CGSize(width: 300, height: 470), view: rooted(await model(empty), scheme) { LockScreenBoard() }))
        screens.append(Screen(name: "live-activities", size: CGSize(width: 393, height: 560), view: rooted(await model(empty), scheme) { LiveBoard() }))
        return screens
    }
    #endif
}

/// The three things that earn a live activity, each as its lock-screen card is drawn: the same
/// view the widget extension hands the system.
struct LiveBoard: View {
    var body: some View {
        let soon = Date().addingTimeInterval(29 * 60 + 30)
        let cards = [
            LiveFacts(kind: .reminder, title: "Remind: call Jonas", until: soon),
            LiveFacts(kind: .brief, title: "Northwind call in 15 minutes",
                      points: ["Northwind led your seed with 1.2 million.", "Maya asked for the October numbers on Monday.",
                               "Last call was 3 October. You promised the hiring plan."],
                      until: Date().addingTimeInterval(14 * 60 + 30), isImportant: true),
            LiveFacts(kind: .work, state: .running, title: "Usage drop: investigate", detail: "Coder"),
            LiveFacts(kind: .work, state: .needsYou, title: "Find the Q3 board deck", detail: "Two versions exist. Which one?"),
        ]
        VStack(spacing: 10) {
            ForEach(Array(cards.enumerated()), id: \.offset) { _, facts in
                LiveFactsView(facts: facts).background(Token.content, in: RoundedRectangle(cornerRadius: 20))
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .padding(.top, 60)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Token.page)
    }
}

/// The drawing's lock screen, laid out from the very notices the app hands the system. The
/// system draws the real ones its own way; this board says what they hold, in the drawing's
/// arrangement, so the two can be held side by side.
struct LockScreenBoard: View {
    var body: some View {
        let told = [Drawing.mainChat[3], Drawing.reminder].compactMap { item in
            NoticePlan.notice(for: item, in: .main).map { (item, $0) }
        }
        let quiet = NoticePlan.notice(for: Drawing.mainChat[2], in: .main)
        VStack(spacing: 10) {
            Text("9:14").font(.tilo(44, .semibold)).monospacedDigit().padding(.top, 18).padding(.bottom, 8)
            ForEach(told, id: \.0.id) { item, notice in
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Mark(side: 14)
                        Text(notice.title).font(.small12).foregroundStyle(Token.ink3)
                        Spacer(minLength: 0)
                        Text(item.id == Drawing.reminder.id ? Drawing.wording.time(item.at) : "now")
                            .font(.small12).foregroundStyle(Token.ink3)
                    }
                    if let subtitle = notice.subtitle {
                        HStack(spacing: 5) {
                            Circle().fill(Token.you).frame(width: 7, height: 7)
                            Text(subtitle).font(.tilo(12, .semibold)).foregroundStyle(Token.you)
                        }
                    }
                    Text(notice.body).font(.body14).foregroundStyle(Token.ink)
                    if let answer = notice.answer {
                        HStack(spacing: 8) {
                            Text(answer.title).font(.button13).foregroundStyle(Token.content)
                                .padding(.horizontal, 12).padding(.vertical, 6)
                                .background(Token.ink, in: RoundedRectangle(cornerRadius: 8))
                            Text(AssistantWords.open.said).font(.button13).foregroundStyle(Token.ink)
                                .padding(.horizontal, 12).padding(.vertical, 6)
                        }
                        .padding(.top, 4)
                    }
                }
                .padding(.horizontal, 12).padding(.vertical, 10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Token.content, in: RoundedRectangle(cornerRadius: 16))
            }
            if let quiet {
                Text(quiet.body).font(.small12).foregroundStyle(Token.ink3).multilineTextAlignment(.center)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .padding(.top, 16)
        .padding(.bottom, 20)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Token.page)
    }
}

#if os(macOS)
import AppKit

/// A view drawn into a picture by the system's own drawing of it, in a window that is never
/// put on a screen.
@MainActor
enum MacPicture {
    static func png(of view: AnyView, size: CGSize, scheme: ColorScheme, scale: CGFloat = 2) -> Data? {
        let hosting = NSHostingView(rootView: view.frame(width: size.width, height: size.height))
        hosting.frame = CGRect(origin: .zero, size: size)
        let look = NSAppearance(named: scheme == .dark ? .darkAqua : .aqua)
        hosting.appearance = look
        let window = NSWindow(contentRect: hosting.frame, styleMask: [.borderless], backing: .buffered, defer: true)
        window.isReleasedWhenClosed = false
        window.appearance = look
        window.contentView = hosting
        hosting.layoutSubtreeIfNeeded()
        // A turn of the run loop, so what settles after the first layout has settled.
        RunLoop.main.run(until: Date().addingTimeInterval(0.5))
        hosting.layoutSubtreeIfNeeded()
        guard let picture = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: Int(size.width * scale), pixelsHigh: Int(size.height * scale),
            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
            bytesPerRow: 0, bitsPerPixel: 0)
        else { return nil }
        picture.size = size
        hosting.cacheDisplay(in: hosting.bounds, to: picture)
        window.contentView = nil
        return picture.representation(using: .png, properties: [:])
    }
}
#endif
