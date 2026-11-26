#if os(macOS)
import AppKit
import AssistantClosed
import AssistantCore
import Observation
import SwiftUI
import UserNotifications

/// Tilo Assistant on the Mac: one window as drawn, its menus and keys, and an item in the menu
/// bar that keeps it running when the window is closed, so it can still tell the person.
///
/// The window holds the assistant's SwiftUI views. The window itself, the menus and the
/// menu-bar item's menu are AppKit's, made here: so made, each is a thing a check can hold and
/// drive with nothing on a screen (`build()`). The app is the same things put on a screen (`run()`).
@MainActor
public final class MacApp: NSObject, NSApplicationDelegate, NSWindowDelegate, NSMenuDelegate, NSMenuItemValidation {
    /// The smallest the window gets: the sidebar, a conversation one can read, and the Tasks panel.
    public static let smallest = NSSize(width: 880, height: 560)

    public let model: AssistantModel
    /// The one window, once `build()` made it.
    public private(set) var window: NSWindow?
    /// The menus of the menu bar.
    public private(set) var mainMenu = NSMenu()
    /// What the item in the menu bar offers.
    public private(set) var statusMenu = NSMenu()

    private let engine: AnyView?
    private let live: (@MainActor (LiveThing) -> AnyView)?
    private let liveStage: (@MainActor (LiveThing) -> AnyView)?
    private let makeWindow: @MainActor (NSRect, NSWindow.StyleMask) -> NSWindow
    private var viewMenu = NSMenu()
    private var windowMenu = NSMenu()
    private var statusItem: NSStatusItem?
    private var teller: Teller?
    private var askedFor: Person.ID?
    /// What the build says of being told while the app is closed; nil in a build without the push right.
    private let closed: ClosedBuild?

    /// `engine` and `live` are the source's own: a view it needs to stand in the window to do its
    /// work, and the view that draws a live Tilo thing. `window` makes the window; a check hands
    /// in one that is never put on a screen. `closed` is what the build says of being told while
    /// the app is closed: the bundle's own word, and nothing in a build without the push right.
    public init(source: any AssistantSource, engine: AnyView? = nil, live: (@MainActor (LiveThing) -> AnyView)? = nil,
                liveStage: (@MainActor (LiveThing) -> AnyView)? = nil,
                closed: ClosedBuild? = ClosedBuild(info: Bundle.main.infoDictionary),
                window: @escaping @MainActor (NSRect, NSWindow.StyleMask) -> NSWindow = { frame, style in
                    NSWindow(contentRect: frame, styleMask: style, backing: .buffered, defer: true)
                }) {
        model = AssistantModel(source: source)
        self.engine = engine
        self.live = live
        self.liveStage = liveStage
        self.closed = closed
        makeWindow = window
    }

    // MARK: What the app is made of

    /// Make the window, the menus and the menu-bar item's menu. Nothing comes onto a screen and
    /// nothing is asked of the person. Calling it again changes nothing.
    public func build() {
        guard window == nil else { return }
        model.start()
        // The drawing's window: no title bar of its own, the three buttons over the sidebar.
        let made = makeWindow(NSRect(x: 0, y: 0, width: 1180, height: 820),
                              [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView])
        made.title = AssistantWords.appName
        made.titleVisibility = .hidden
        made.titlebarAppearsTransparent = true
        made.isReleasedWhenClosed = false
        // The system keeps nothing of the window for its next start: what it shows comes from the
        // person's Tilo, and what stands in a field (a secret, too) is nobody's to write down.
        made.isRestorable = false
        let root = AssistantRoot(model: model, engine: engine) { MacWindow() }
            .environment(\.liveThingView, live)
            .environment(\.liveStageView, liveStage ?? live)
        let hosting = NSHostingView(rootView: root)
        // The window's sizes are the window's: its smallest is said above.
        hosting.sizingOptions = []
        made.contentView = hosting
        made.contentMinSize = MacApp.smallest
        made.delegate = self
        window = made
        mainMenu = menus()
        statusMenu = NSMenu()
        statusMenu.delegate = self
        fill(status: statusMenu)
    }

    /// Be the app: the window and the menu-bar item on screen, until the person quits.
    public func run() -> Never {
        Typeface.registerBundled()
        let app = NSApplication.shared
        app.delegate = self
        app.setActivationPolicy(.regular)
        app.run()
        exit(0)
    }

    // MARK: The app's life

    public func applicationDidFinishLaunching(_ notification: Notification) {
        build()
        NSApp.mainMenu = mainMenu
        NSApp.windowsMenu = windowMenu
        // The system's notifications belong to an app bundle; a bare program has none.
        if Bundle.main.bundleIdentifier != nil {
            let teller = Teller(model: model)
            teller.looksAt = { [model] chat in NSApp.isActive && NSApp.keyWindow != nil && model.place == .chat(chat) }
            self.teller = teller
        }
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.image = MenuBarMark.image
        item.button?.imagePosition = .imageLeading
        item.button?.setAccessibilityLabel(AssistantWords.appName)
        item.menu = statusMenu
        statusItem = item
        window?.center()
        window?.setFrameAutosaveName("main")
        showWindow(nil)
        follow()
    }

    /// The window is never dragged smaller than its smallest, whatever its content would allow.
    public func windowWillResize(_ sender: NSWindow, to frameSize: NSSize) -> NSSize {
        let least = sender.frameRect(forContentRect: NSRect(origin: .zero, size: MacApp.smallest)).size
        return NSSize(width: max(frameSize.width, least.width), height: max(frameSize.height, least.height))
    }

    /// Closing the window leaves the app in the menu bar.
    public func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    public func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        showWindow(nil)
        return false
    }

    /// An address of the app's own: where a live activity or a notification leads, or a
    /// pairing link handed over by Tilo's page.
    public func application(_ application: NSApplication, open urls: [URL]) {
        for url in urls { AppAddress.open(url, in: model) }
    }

    /// Keep the menu-bar item's number, the icon's and the question about notifications in line
    /// with what the model holds.
    private func follow() {
        withObservationTracking {
            took(waiting: model.waiting, person: model.person)
        } onChange: { [weak self] in
            Task { @MainActor in self?.follow() }
        }
    }

    private func took(waiting: Int, person: Person?) {
        statusItem?.button?.title = waiting > 0 ? String(waiting) : ""
        teller?.setBadge(waiting)
        if let person, askedFor != person.id, let teller {
            askedFor = person.id
            let hasPush = closed != nil
            Task {
                // Apple's service is asked for an address only in a build that has the push right.
                if await teller.ask(), hasPush { NSApp.registerForRemoteNotifications() }
            }
        }
    }

    /// Apple's address for this Mac goes to the person's Tilo, which tells them while the app is closed.
    public func application(_ application: NSApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        guard let closed else { return }
        let source = model.source
        let address = PushAddress(use: .notices, token: deviceToken, service: closed.service, app: Bundle.main.bundleIdentifier ?? "")
        Task { await source.register(address) }
    }

    /// A push that shows nothing: a thing the person was told of waits no more, and its notice is taken away.
    public func application(_ application: NSApplication, didReceiveRemoteNotification userInfo: [String: Any]) {
        guard let push = ClosedPush(userInfo), push.isGone else { return }
        UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: [push.name])
    }

    // MARK: The menus

    @discardableResult
    private func add(_ title: String, _ action: Selector?, key: String = "", _ modifiers: NSEvent.ModifierFlags = .command,
                     to menu: NSMenu, target: AnyObject? = nil) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
        item.keyEquivalentModifierMask = modifiers
        item.target = target
        menu.addItem(item)
        return item
    }

    private func menus() -> NSMenu {
        let main = NSMenu()
        let submenu = { (title: String) -> NSMenu in
            let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
            let menu = NSMenu(title: title)
            item.submenu = menu
            main.addItem(item)
            return menu
        }
        typealias W = AssistantWords

        let own = submenu(W.appName)
        add(W.about.said, #selector(NSApplication.orderFrontStandardAboutPanel(_:)), to: own, target: NSApp)
        own.addItem(.separator())
        add("\(W.assistantView.said)…", #selector(showAssistantView(_:)), key: ",", to: own, target: self)
        add(W.signOut.said, #selector(signOut(_:)), to: own, target: self)
        own.addItem(.separator())
        add(W.hideApp.said, #selector(NSApplication.hide(_:)), key: "h", to: own, target: NSApp)
        add(W.hideOthers.said, #selector(NSApplication.hideOtherApplications(_:)), key: "h", [.command, .option], to: own, target: NSApp)
        add(W.showAll.said, #selector(NSApplication.unhideAllApplications(_:)), to: own, target: NSApp)
        own.addItem(.separator())
        add(W.quit.said, #selector(NSApplication.terminate(_:)), key: "q", to: own, target: NSApp)

        let file = submenu(W.fileMenu.said)
        add(W.newSubAgent.said, #selector(newSubAgent(_:)), key: "n", to: file, target: self)
        file.addItem(.separator())
        add(W.closeWindow.said, #selector(NSWindow.performClose(_:)), key: "w", to: file)

        // To whatever has the keyboard: the message field, a card's field.
        let edit = submenu(W.editMenu.said)
        add(W.undo.said, Selector(("undo:")), key: "z", to: edit)
        add(W.redo.said, Selector(("redo:")), key: "z", [.command, .shift], to: edit)
        edit.addItem(.separator())
        add(W.cut.said, #selector(NSText.cut(_:)), key: "x", to: edit)
        add(W.copy.said, #selector(NSText.copy(_:)), key: "c", to: edit)
        add(W.paste.said, #selector(NSText.paste(_:)), key: "v", to: edit)
        add(W.selectAll.said, #selector(NSText.selectAll(_:)), key: "a", to: edit)

        // Its lines follow what the window shows and which sub-agents there are.
        viewMenu = submenu(W.viewMenu.said)
        viewMenu.delegate = self
        fill(view: viewMenu)

        windowMenu = submenu(W.windowMenu.said)
        add(W.minimize.said, #selector(NSWindow.performMiniaturize(_:)), key: "m", to: windowMenu)
        add(W.zoom.said, #selector(NSWindow.performZoom(_:)), to: windowMenu)
        windowMenu.addItem(.separator())
        add(W.appName, #selector(showWindow(_:)), key: "0", to: windowMenu, target: self)
        return main
    }

    private func fill(view menu: NSMenu) {
        typealias W = AssistantWords
        menu.removeAllItems()
        add((model.showsSidebar ? W.hideSidebar : W.showSidebar).said, #selector(toggleSidebar(_:)), key: "s",
            [.control, .command], to: menu, target: self)
        add((model.showsPanel ? W.hideTasks : W.showTasks).said, #selector(toggleTasks(_:)), key: "t",
            [.option, .command], to: menu, target: self)
        menu.addItem(.separator())
        add(W.mainChat.said, #selector(openMainChat(_:)), key: "1", to: menu, target: self)
        for (index, agent) in model.subAgents.prefix(8).enumerated() {
            add(agent.title, #selector(openSubAgent(_:)), key: String(index + 2), to: menu, target: self)
                .representedObject = agent.id.value
        }
    }

    /// The menu-bar item opens the window and waiting tasks, signs out and quits.
    private func fill(status menu: NSMenu) {
        typealias W = AssistantWords
        menu.removeAllItems()
        add(W.openApp.said, #selector(showWindow(_:)), to: menu, target: self)
        menu.addItem(.separator())
        add(model.waiting == 0 ? W.waitsNone.said : W.counted(model.waiting, one: .waitsOne, many: .waitsMany), nil, to: menu)
        for task in (model.tasks(.needsInput) + model.tasks(.readyForReview)).prefix(8) {
            add(task.title, #selector(openWaiting(_:)), to: menu, target: self).representedObject = task.item?.value
        }
        if model.session != .signedOut {
            menu.addItem(.separator())
            add(W.signOut.said, #selector(signOut(_:)), to: menu, target: self)
        }
        menu.addItem(.separator())
        add(W.quit.said, #selector(NSApplication.terminate(_:)), key: "q", to: menu, target: NSApp)
    }

    public func menuNeedsUpdate(_ menu: NSMenu) {
        if menu === viewMenu { fill(view: menu) }
        if menu === statusMenu { fill(status: menu) }
    }

    /// The window's own lines stand only for someone who is signed in, and the Tasks panel only
    /// beside a conversation.
    public func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        switch menuItem.action {
        case #selector(newSubAgent(_:)), #selector(showAssistantView(_:)), #selector(toggleSidebar(_:)),
             #selector(openMainChat(_:)), #selector(openSubAgent(_:)):
            model.person != nil
        case #selector(toggleTasks(_:)):
            model.person != nil && model.chat != nil
        case #selector(signOut(_:)):
            model.session != .signedOut
        default:
            true
        }
    }

    // MARK: What the menus do

    @objc public func newSubAgent(_ sender: Any?) {
        model.asksForJob = true
    }

    @objc public func showAssistantView(_ sender: Any?) {
        model.place = .assistantView
    }

    @objc public func toggleSidebar(_ sender: Any?) {
        model.showsSidebar.toggle()
    }

    @objc public func toggleTasks(_ sender: Any?) {
        model.showsPanel.toggle()
    }

    @objc public func openMainChat(_ sender: Any?) {
        model.open(.main)
    }

    @objc public func openSubAgent(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        model.open(.subAgent(SubAgent.ID(id)))
    }

    /// Bring the window to the front, and the app with it.
    @objc public func showWindow(_ sender: Any?) {
        window?.makeKeyAndOrderFront(nil)
        // Only an app that stands in the Dock comes forward; a check never does.
        if NSApp.activationPolicy() == .regular { NSApp.activate(ignoringOtherApps: true) }
    }

    @objc public func signOut(_ sender: Any?) {
        model.signOut()
    }

    /// Open the window at something that waits for the person: its piece in the main chat, or
    /// the Tasks panel when it stands in no conversation.
    @objc public func openWaiting(_ sender: NSMenuItem) {
        showWindow(nil)
        if let item = sender.representedObject as? String {
            model.show(Item.ID(item))
        } else {
            model.open(.main)
            model.showTasks(.active)
        }
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
#endif
