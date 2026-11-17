#if os(macOS)
import AppKit
import AssistantCore
import AssistantUI
import SwiftUI

/// Tilo Assistant on the Mac: one window as drawn, its menus and keys, and an item in the menu
/// bar that keeps it running when the window is closed, so it can still tell the person.
struct AssistantMacApp: App {
    @NSApplicationDelegateAdaptor(MacDelegate.self) private var delegate
    @State private var model: AssistantModel
    @State private var teller: Teller?

    init() {
        Typeface.registerBundled()
        let model = AssistantModel(source: Source.make())
        model.start()
        _model = State(initialValue: model)
        // The system's notifications belong to an app bundle; a bare program has none.
        _teller = State(initialValue: Bundle.main.bundleIdentifier == nil ? nil : Teller(model: model))
    }

    var body: some Scene {
        Window(AssistantWords.appName, id: "main") {
            AssistantRoot(model: model, engine: Source.engine) { MacWindow() }
                .environment(\.liveThingView, { thing in Source.live(thing) })
                .frame(minWidth: 880, minHeight: 560)
                .task(id: model.person?.id) {
                    guard model.person != nil, let teller else { return }
                    teller.looksAt = { chat in NSApp.isActive && NSApp.keyWindow != nil && model.place == .chat(chat) }
                    await teller.ask()
                }
                .onChange(of: model.waiting) { teller?.setBadge(model.waiting) }
                .onOpenURL { LiveLink.open($0, in: model) }
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1180, height: 820)
        .commands { AssistantCommands(model: model) }

        MenuBarExtra {
            MenuBarList(model: model)
        } label: {
            Image(nsImage: MenuBarMark.image)
            if model.waiting > 0 { Text(String(model.waiting)) }
        }
    }
}

final class MacDelegate: NSObject, NSApplicationDelegate {
    /// Closing the window leaves the app in the menu bar.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }
}

/// The menus' own lines, beside the system's.
struct AssistantCommands: Commands {
    let model: AssistantModel

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            Button(AssistantWords.newSubAgent.said) { model.asksForJob = true }
                .keyboardShortcut("n")
                .disabled(model.person == nil)
        }
        CommandGroup(replacing: .appSettings) {
            Button("\(AssistantWords.assistantView.said)…") { model.place = .assistantView }
                .keyboardShortcut(",")
                .disabled(model.person == nil)
        }
        CommandGroup(before: .toolbar) {
            Button((model.showsSidebar ? AssistantWords.hideSidebar : AssistantWords.showSidebar).said) {
                model.showsSidebar.toggle()
            }
            .keyboardShortcut("s", modifiers: [.control, .command])
            Button((model.showsPanel ? AssistantWords.hideTasks : AssistantWords.showTasks).said) {
                model.showsPanel.toggle()
            }
            .keyboardShortcut("t", modifiers: [.option, .command])
            Divider()
            Button(AssistantWords.mainChat.said) { model.open(.main) }.keyboardShortcut("1")
            ForEach(Array(model.subAgents.prefix(8).enumerated()), id: \.element.id) { index, agent in
                Button(agent.title) { model.open(.subAgent(agent.id)) }
                    .keyboardShortcut(KeyEquivalent(Character(String(index + 2))))
            }
            Divider()
        }
    }
}

/// What the menu-bar item offers: the window, what waits for the person, a pause, and Quit.
struct MenuBarList: View {
    let model: AssistantModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Button(AssistantWords.openApp.said) { front() }
        Divider()
        let waiting = model.tasks(.needsInput) + model.tasks(.readyForReview)
        Text(model.waiting == 0 ? AssistantWords.waitsNone.said
            : AssistantWords.counted(model.waiting, one: .waitsOne, many: .waitsMany))
        ForEach(waiting.prefix(8)) { task in
            Button(task.title) {
                front()
                if let item = task.item { model.show(item) } else { model.showTasks(.active) }
            }
        }
        if model.person != nil {
            Divider()
            Button((model.supervision.isOn ? AssistantWords.pauseAssistant : AssistantWords.resumeAssistant).said) {
                model.setAssistantOn(!model.supervision.isOn)
            }
        }
        Divider()
        Button(AssistantWords.quit.said) { NSApp.terminate(nil) }.keyboardShortcut("q")
    }

    private func front() {
        openWindow(id: "main")
        NSApp.activate(ignoringOtherApps: true)
    }
}

/// Tilo's mark for the menu bar, in the bar's own colour.
enum MenuBarMark {
    @MainActor static let image: NSImage = {
        let image = NSImage(size: NSSize(width: 18, height: 18), flipped: true) { _ in
            NSColor.black.setFill()
            NSBezierPath(roundedRect: NSRect(x: 5.2, y: 2.9, width: 7.6, height: 12.2), xRadius: 1.1, yRadius: 1.1).fill()
            NSColor.black.withAlphaComponent(0.55).setFill()
            NSBezierPath(roundedRect: NSRect(x: 2, y: 4.4, width: 2.5, height: 9.2), xRadius: 0.9, yRadius: 0.9).fill()
            NSBezierPath(roundedRect: NSRect(x: 13.5, y: 4.4, width: 2.5, height: 9.2), xRadius: 0.9, yRadius: 0.9).fill()
            return true
        }
        image.isTemplate = true
        return image
    }()
}
/// Where the program begins: asked with `--check` it says what it can tell of itself and
/// ends, with nothing on screen; asked with `--drive` it is the app with no window, for a
/// check to drive (`AssistantDrive`); otherwise it is the app.
@main
enum Entry {
    @MainActor
    static func main() {
        if AssistantCheck.isAsked {
            AssistantCheck.run(source: Source.name)
        }
        if AssistantDrive.isAsked {
            AssistantDrive.run()
        }
        AssistantMacApp.main()
    }
}
#else
@main
enum NotThisPlatform {
    static func main() {
        print("AssistantMac is the Mac app. The iPhone app is AssistantPhone.")
    }
}
#endif
