#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

// How the Mac app is driven with nothing on a screen.
//
// The app's own window is made and never ordered onto a screen: it only answers that it is
// visible and key, so its views lay out, take the keyboard and answer. The process is one that
// may never come to the front. Measured on this system, in such a window:
//
// - key events reach whatever holds the keyboard, so every key is a real key event;
// - a mouse event reaches an AppKit view it is handed to (the message field's text view);
// - no mouse event reaches a SwiftUI control, however it is handed over. A SwiftUI control is
//   therefore pressed by its accessibility press, the system's own way to act on a control
//   without a pointer, and only after the check that the control is what stands at its own
//   centre. What stays unproven is the pointer's own way into SwiftUI: hover, how a press
//   looks, a drag.
// - whether the conversation keeps its end in view while the field under it grows cannot be
//   judged: here the same check passes and fails from run to run, so none is made.

/// A window that is never put on a screen and answers as one that is. Whatever would bring it,
/// or something of it, onto a screen is not done and written down instead.
final class HeldWindow: NSWindow {
    /// What was asked of the window that would have shown something.
    private(set) var asked: [String] = []
    /// The sheets it was asked to show and has not been asked to end, newest last.
    private(set) var heldSheets: [NSWindow] = []
    private var ends: [ObjectIdentifier: (NSApplication.ModalResponse) -> Void] = [:]

    override var isVisible: Bool { true }
    override var occlusionState: NSWindow.OcclusionState { [.visible] }
    override var screen: NSScreen? { NSScreen.screens.first }
    override var isKeyWindow: Bool { true }
    override var isMainWindow: Bool { true }
    override var canBecomeKey: Bool { true }

    override func orderFront(_ sender: Any?) { asked.append("orderFront") }
    override func orderFrontRegardless() { asked.append("orderFrontRegardless") }
    override func makeKeyAndOrderFront(_ sender: Any?) { asked.append("makeKeyAndOrderFront") }
    override func order(_ place: NSWindow.OrderingMode, relativeTo otherWin: Int) { asked.append("order") }
    override func makeKey() { asked.append("makeKey") }
    override func makeMain() { asked.append("makeMain") }
    override func addChildWindow(_ childWin: NSWindow, ordered place: NSWindow.OrderingMode) { asked.append("addChildWindow") }

    override func beginSheet(_ sheetWindow: NSWindow, completionHandler handler: ((NSApplication.ModalResponse) -> Void)? = nil) {
        asked.append("beginSheet")
        heldSheets.append(sheetWindow)
        ends[ObjectIdentifier(sheetWindow)] = handler
    }

    override func endSheet(_ sheetWindow: NSWindow, returnCode: NSApplication.ModalResponse) {
        asked.append("endSheet")
        heldSheets.removeAll { $0 === sheetWindow }
        ends.removeValue(forKey: ObjectIdentifier(sheetWindow))?(returnCode)
    }

    static func make(_ frame: NSRect, _ style: NSWindow.StyleMask) -> HeldWindow {
        let window = HeldWindow(contentRect: frame, styleMask: style, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        return window
    }
}

/// One element of the accessibility tree, as a person who cannot see the window is told it.
struct Node: CustomStringConvertible {
    let object: NSObject
    let depth: Int
    let role: String
    let subrole: String
    let label: String
    /// What an AppKit control is called where it carries its name as a title: a menu's button.
    let title: String
    let value: String
    let help: String
    /// Where it stands, in the screen's measure: the origin at the foot.
    let frame: CGRect
    let isSelected: Bool
    let isEnabled: Bool

    var description: String {
        var said = String(repeating: "  ", count: depth) + role
        if !subrole.isEmpty { said += " (\(subrole))" }
        if !label.isEmpty { said += " “\(label)”" }
        if !title.isEmpty, title != label { said += " titled “\(title)”" }
        if !value.isEmpty { said += " = “\(value.prefix(70))”" }
        if isSelected { said += " [selected]" }
        if !isEnabled { said += " [off]" }
        return said
    }

    /// What it is called when it is read out: its label, its title, or the words it is.
    var name: String { !label.isEmpty ? label : (!title.isEmpty ? title : value) }

    var isButton: Bool { role == "AXButton" }
    var isHeading: Bool { role == "AXHeading" }
}

/// Reads an element's facts however it offers them: SwiftUI's own elements answer the
/// accessibility questions without saying they keep to the protocol.
private struct Facts {
    let object: NSObject
    private var kept: NSAccessibilityProtocol? { object as? NSAccessibilityProtocol }

    private func asked(_ key: String) -> Any? {
        object.responds(to: Selector(key)) ? object.value(forKey: key) : nil
    }

    /// Words, however they are handed over: plain, or with how they look.
    private func words(_ key: String) -> String? {
        let given = asked(key)
        return given as? String ?? (given as? NSAttributedString)?.string
    }

    var role: String { (kept?.accessibilityRole() ?? (asked("accessibilityRole") as? NSAccessibility.Role))?.rawValue ?? "" }
    var subrole: String { (kept?.accessibilitySubrole() ?? (asked("accessibilitySubrole") as? NSAccessibility.Subrole))?.rawValue ?? "" }
    var label: String { kept?.accessibilityLabel() ?? words("accessibilityLabel") ?? "" }
    var title: String { kept?.accessibilityTitle() ?? words("accessibilityTitle") ?? "" }
    var help: String { kept?.accessibilityHelp() ?? words("accessibilityHelp") ?? "" }
    var value: String { (kept?.accessibilityValue() ?? asked("accessibilityValue")).map { "\($0)" } ?? "" }
    var frame: CGRect { kept?.accessibilityFrame() ?? (asked("accessibilityFrame") as? NSValue)?.rectValue ?? .zero }
    var isSelected: Bool { kept?.isAccessibilitySelected() ?? (asked("isAccessibilitySelected") as? Bool) ?? false }
    var isEnabled: Bool { kept?.isAccessibilityEnabled() ?? (asked("isAccessibilityEnabled") as? Bool) ?? true }
    var children: [NSObject] {
        ((kept?.accessibilityChildren() ?? (asked("accessibilityChildren") as? [Any])) ?? []).compactMap { $0 as? NSObject }
    }
    var parent: NSObject? { (kept?.accessibilityParent() ?? asked("accessibilityParent")) as? NSObject }
}

/// Drives a held window: reads what it says of itself, presses, types.
@MainActor
final class Drive {
    let window: HeldWindow
    /// The menus of the menu bar, for the keys they stand for.
    var menu: NSMenu?
    private var events = 0

    init(_ window: HeldWindow, menu: NSMenu? = nil) {
        self.window = window
        self.menu = menu
        // Never the app in front, no icon in the Dock.
        NSApplication.shared.setActivationPolicy(.prohibited)
        // SwiftUI makes its accessibility tree once something that reads one has said it is there.
        NSApp.accessibilitySetValue(true as NSNumber, forAttribute: NSAccessibility.Attribute(rawValue: "AXEnhancedUserInterface"))
    }

    // MARK: Waiting

    /// Let what was started settle: the model's calls, SwiftUI's next layout.
    func settle(_ milliseconds: Int = 120) async {
        try? await Task.sleep(for: .milliseconds(milliseconds))
        for view in [window.contentView] + window.heldSheets.map(\.contentView) {
            guard let view else { continue }
            view.layoutSubtreeIfNeeded()
            // A window on a screen is drawn again and again, and views settle things as they are
            // drawn (a scroll view its place). Here nothing draws unless asked: one small drawing.
            let corner = NSRect(x: 0, y: 0, width: 2, height: 2)
            if let scrap = view.bitmapImageRepForCachingDisplay(in: corner) { view.cacheDisplay(in: corner, to: scrap) }
        }
    }

    /// Wait until something holds, a few seconds at most.
    func until(_ what: String, _ holds: () -> Bool, file: StaticString = #filePath, line: UInt = #line) async {
        for _ in 0..<150 {
            if holds() { return }
            await settle(40)
        }
        XCTFail("it never came: \(what)", file: file, line: line)
    }

    // MARK: Reading

    /// Everything a window says of itself, in the order it is read out.
    func tree(of root: NSView? = nil) -> [Node] {
        guard let root = root ?? window.contentView else { return [] }
        root.layoutSubtreeIfNeeded()
        var nodes: [Node] = []
        func walk(_ object: NSObject, _ depth: Int) {
            guard depth < 60 else { return }
            let facts = Facts(object: object)
            nodes.append(Node(object: object, depth: depth, role: facts.role, subrole: facts.subrole, label: facts.label,
                              title: facts.title, value: facts.value, help: facts.help, frame: facts.frame, isSelected: facts.isSelected,
                              isEnabled: facts.isEnabled))
            for child in facts.children { walk(child, depth + 1) }
        }
        walk(root, 0)
        return nodes
    }

    func said(of root: NSView? = nil) -> String {
        tree(of: root).map(\.description).joined(separator: "\n")
    }

    /// What the window says of itself, kept as a file when TILO_SAID_DIR names a folder: for a
    /// person to read what someone who cannot see the window is told.
    func keep(_ name: String, of root: NSView? = nil) {
        guard let folder = ProcessInfo.processInfo.environment["TILO_SAID_DIR"] else { return }
        try? FileManager.default.createDirectory(atPath: folder, withIntermediateDirectories: true)
        try? Data((said(of: root) + "\n").utf8).write(to: URL(fileURLWithPath: folder).appendingPathComponent("\(name).txt"))
    }

    func nodes(_ label: String, role: String? = nil, in root: NSView? = nil) -> [Node] {
        tree(of: root).filter { $0.label == label && (role == nil || $0.role == role) }
    }

    func node(_ label: String, role: String? = nil, in root: NSView? = nil, file: StaticString = #filePath, line: UInt = #line) -> Node? {
        let found = nodes(label, role: role, in: root)
        if found.isEmpty { XCTFail("nothing is called “\(label)”\(role.map { " as \($0)" } ?? "") in:\n\(said(of: root))", file: file, line: line) }
        return found.first
    }

    func has(_ label: String, role: String? = nil, in root: NSView? = nil) -> Bool {
        !nodes(label, role: role, in: root).isEmpty
    }

    /// Whether some element's label or value holds these words.
    func shows(_ words: String, in root: NSView? = nil) -> Bool {
        tree(of: root).contains { $0.label.contains(words) || $0.value.contains(words) }
    }

    // MARK: Pressing

    /// Controls whose press puts a window of the system's own on the screen: a menu, a
    /// popover, the file picker, the question about the microphone. They are read, never pressed.
    static let opensOnScreen = [AssistantWords.search.said, AssistantWords.attach.said, AssistantWords.dictate.said]

    /// Press a SwiftUI control: the system's press for something that cannot be clicked, after
    /// the check that the control is what stands at its own centre.
    func press(_ node: Node?, in root: NSView? = nil, file: StaticString = #filePath, line: UInt = #line) {
        guard let node else { return }
        guard ["AXButton", "AXCheckBox", "AXSwitch", "AXRadioButton", "AXToggle"].contains(node.role) || node.subrole == "AXSwitch" else {
            return XCTFail("“\(node.label)” is \(node.role), nothing to press without showing a menu", file: file, line: line)
        }
        guard !Drive.opensOnScreen.contains(node.label) else {
            return XCTFail("“\(node.label)” opens something on the screen", file: file, line: line)
        }
        XCTAssertTrue(node.isEnabled, "“\(node.label)” is off", file: file, line: line)
        XCTAssertFalse(node.frame.isEmpty, "“\(node.label)” takes no room", file: file, line: line)
        if let root = root ?? window.contentView {
            let centre = CGPoint(x: node.frame.midX, y: node.frame.midY)
            let hit = root.accessibilityHitTest(centre) as? NSObject
            XCTAssertTrue(stands(hit, for: node), "at the centre of “\(node.label)” stands \(hit.map { Facts(object: $0).label } ?? "nothing")",
                          file: file, line: line)
        }
        XCTAssertTrue(node.object.responds(to: Selector(("accessibilityPerformPress"))), file: file, line: line)
        _ = node.object.perform(Selector(("accessibilityPerformPress")))
    }

    /// Whether what a hit test found is the element, or a part of it.
    private func stands(_ hit: NSObject?, for node: Node) -> Bool {
        var seen = hit
        for _ in 0..<12 {
            guard let object = seen else { return false }
            if object === node.object { return true }
            let facts = Facts(object: object)
            if facts.label == node.label, facts.frame.intersects(node.frame), facts.role == node.role { return true }
            seen = facts.parent
        }
        return false
    }

    func press(_ label: String, role: String? = nil, in root: NSView? = nil, file: StaticString = #filePath, line: UInt = #line) {
        press(node(label, role: role, in: root, file: file, line: line), in: root, file: file, line: line)
    }

    // MARK: The pointer, for AppKit's own views

    /// A click on an AppKit view at a place of its own, as its window hands one over: what was
    /// clicked takes the keyboard, then gets the press, and finds the release waiting.
    func click(_ view: NSView, at place: CGPoint? = nil) {
        let inView = place ?? CGPoint(x: view.bounds.midX, y: view.bounds.midY)
        let point = view.convert(inView, to: nil)
        func event(_ type: NSEvent.EventType) -> NSEvent? {
            events += 1
            return NSEvent.mouseEvent(with: type, location: point, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                                      windowNumber: view.window?.windowNumber ?? 0, context: nil, eventNumber: events, clickCount: 1,
                                      pressure: 1)
        }
        guard let down = event(.leftMouseDown), let up = event(.leftMouseUp) else { return }
        if view.acceptsFirstResponder { view.window?.makeFirstResponder(view) }
        // A text view follows the pointer until the button is let go, and reads that from the app's queue.
        NSApp.postEvent(up, atStart: false)
        view.mouseDown(with: down)
        if let left = NSApp.nextEvent(matching: [.leftMouseUp], until: nil, inMode: .default, dequeue: true) {
            view.mouseUp(with: left)
        }
    }

    // MARK: Keys

    /// The keys that are no letter, by the number the keyboard gives them.
    enum Key {
        static let returnKey: (String, UInt16) = ("\r", 36)
        static let escape: (String, UInt16) = ("\u{1b}", 53)
        static let delete: (String, UInt16) = ("\u{7f}", 51)
        static let tab: (String, UInt16) = ("\t", 48)
    }

    private static let codes: [Character: UInt16] = [
        "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9, "b": 11, "q": 12, "w": 13, "e": 14,
        "r": 15, "y": 16, "t": 17, "1": 18, "2": 19, "3": 20, "4": 21, "6": 22, "5": 23, "9": 25, "7": 26, "8": 28, "0": 29,
        "o": 31, "u": 32, "i": 34, "p": 35, "l": 37, "j": 38, "k": 40, "n": 45, "m": 46, ",": 43, ".": 47, " ": 49,
    ]

    /// One key, pressed and let go, as the app hands a key on: first to whatever in the window
    /// takes it as a shortcut, then to the menus, then to whatever holds the keyboard.
    /// Says who took it.
    @discardableResult
    func key(_ characters: String, code: UInt16? = nil, _ flags: NSEvent.ModifierFlags = [], in target: NSWindow? = nil) -> String {
        let target = target ?? window
        let plain = characters.lowercased()
        let code = code ?? plain.first.flatMap { Drive.codes[$0] } ?? 0
        func event(_ type: NSEvent.EventType) -> NSEvent? {
            NSEvent.keyEvent(with: type, location: .zero, modifierFlags: flags, timestamp: ProcessInfo.processInfo.systemUptime,
                             windowNumber: target.windowNumber, context: nil, characters: characters,
                             charactersIgnoringModifiers: plain, isARepeat: false, keyCode: code)
        }
        guard let down = event(.keyDown), let up = event(.keyUp) else { return "no event" }
        var taker = "the keyboard's holder"
        if target.performKeyEquivalent(with: down) {
            taker = "the window"
        } else if let menu, let item = line(for: down, in: menu) {
            taker = "the menu’s “\(item.title)”"
            if item.target == nil, let action = item.action {
                // A line with no target of its own goes to whatever in the key window answers it, from
                // the keyboard's holder upwards; and stands only if that one says so.
                var answers = target.firstResponder
                while let asked = answers, !asked.responds(to: action) { answers = asked.nextResponder }
                if let answers {
                    let stands = (answers as? NSMenuItemValidation)?.validateMenuItem(item)
                        ?? (answers as? NSUserInterfaceValidations)?.validateUserInterfaceItem(item) ?? true
                    if stands { _ = answers.perform(action, with: item) } else { taker += ", which is off" }
                } else {
                    taker += ", which nothing took"
                }
            } else {
                // The menu itself decides whether its line stands, by asking whoever the line goes to; a
                // line that is off takes its key and does nothing.
                if !((item.target as? NSMenuItemValidation)?.validateMenuItem(item) ?? true) { taker += ", which is off" }
                _ = menu.performKeyEquivalent(with: down)
            }
        } else {
            target.sendEvent(down)
        }
        target.sendEvent(up)
        return taker
    }

    /// The menu's line a key stands for, as the menu finds it: its menus brought up to date first.
    func line(for event: NSEvent, in menu: NSMenu) -> NSMenuItem? {
        menu.delegate?.menuNeedsUpdate?(menu)
        let wanted = event.modifierFlags.intersection([.command, .option, .control, .shift])
        for item in menu.items {
            if let submenu = item.submenu, let found = line(for: event, in: submenu) { return found }
            if !item.keyEquivalent.isEmpty, item.keyEquivalent == event.charactersIgnoringModifiers,
               item.keyEquivalentModifierMask.intersection([.command, .option, .control, .shift]) == wanted {
                return item
            }
        }
        return nil
    }

    /// Words typed letter by letter. A line break in them is Shift Return.
    func type(_ words: String, in target: NSWindow? = nil) {
        for character in words {
            if character == "\n" {
                key(Key.returnKey.0, code: Key.returnKey.1, [.shift], in: target)
            } else {
                key(String(character), character.isUppercase ? [.shift] : [], in: target)
            }
        }
    }

    // MARK: Pictures

    /// The window as it draws itself, kept as a picture when TILO_SHOTS_DIR names a folder.
    /// Nothing is taken from a screen.
    func picture(_ name: String, of view: NSView? = nil) {
        guard let folder = ProcessInfo.processInfo.environment["TILO_SHOTS_DIR"],
              let view = view ?? window.contentView?.superview ?? window.contentView else { return }
        view.layoutSubtreeIfNeeded()
        guard let picture = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return }
        view.cacheDisplay(in: view.bounds, to: picture)
        try? FileManager.default.createDirectory(atPath: folder, withIntermediateDirectories: true)
        try? picture.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: folder).appendingPathComponent("\(name).png"))
    }

    // MARK: Nothing on a screen

    /// The windows of this process the system has on a screen. A run must leave this empty.
    static func onScreen() -> [String] {
        let listed = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
        let own = Int(ProcessInfo.processInfo.processIdentifier)
        return listed.filter { ($0[kCGWindowOwnerPID as String] as? Int) == own }.map { "\($0[kCGWindowBounds as String] ?? "")" }
    }
}

/// A source that writes down every call it is given, with everything the call carried, before
/// handing it to the in-memory source: what the frame called, and with what.
final class Recording: AssistantSource, @unchecked Sendable {
    let inner: MemorySource
    private let lock = NSLock()
    private var written: [String] = []
    private var secrets: [String] = []

    init(_ inner: MemorySource) {
        self.inner = inner
    }

    /// Every call so far, oldest first: its name and what it carried.
    var calls: [String] { lock.withLock { written } }
    /// What `keep` was handed, in the order it came.
    var kept: [String] { lock.withLock { secrets } }

    func calls(_ name: String) -> [String] { calls.filter { $0.hasPrefix(name + "(") } }

    private func note(_ call: String) { lock.withLock { written.append(call) } }

    func session() -> AsyncStream<Session> { inner.session() }
    func reach() -> AsyncStream<Reach> { inner.reach() }
    func pair(with link: PairingLink) async throws { note("pair(\(link.url))"); try await inner.pair(with: link) }
    func signOut() async { note("signOut()"); await inner.signOut() }
    func answerPairing(goOn: Bool) async { note("answerPairing(\(goOn))"); await inner.answerPairing(goOn: goOn) }
    func setup() -> AsyncStream<Setup> { inner.setup() }
    func makeAssistant() async throws { note("makeAssistant()"); try await inner.makeAssistant() }
    func link(_ provider: ProviderChoice.ID, key: Secret) async throws -> [ModelChoice] {
        // Written down as anything that writes calls down would see it: the key says nothing of itself.
        note("link(\(provider), \(String(reflecting: key)))")
        return try await inner.link(provider, key: key)
    }
    func conversation(_ chat: Chat) -> AsyncStream<ConversationChange> { inner.conversation(chat) }
    func earlier(in chat: Chat, before item: Item.ID) async throws -> [Item] {
        note("earlier(\(chat), \(item))"); return try await inner.earlier(in: chat, before: item)
    }
    func replies(of task: AssistantTask.ID, in chat: Chat) async throws -> [Item] {
        note("replies(\(task), \(chat))"); return try await inner.replies(of: task, in: chat)
    }
    func send(_ draft: Draft, to chat: Chat) async throws {
        note("send(\(String(reflecting: draft.text)), files: \(draft.files.map(\.name)), \(chat))"); try await inner.send(draft, to: chat)
    }
    func answer(_ question: Question.ID, with choice: Choice.ID) async throws {
        note("answer(\(question), \(choice))"); try await inner.answer(question, with: choice)
    }
    func seen(_ item: Item.ID, in chat: Chat) async {
        note("seen(\(item), \(chat))")
        await inner.seen(item, in: chat)
    }
    func stopSharing(_ thing: LiveThing.ID) async throws { note("stopSharing(\(thing))"); try await inner.stopSharing(thing) }
    func keep(_ secret: Secret, for request: SecretRequest.ID) async throws {
        // The call is written down as anything that writes calls down would see it; what was
        // handed over is kept apart, as the vault keeps it.
        note("keep(\(String(reflecting: secret)), \(request))")
        lock.withLock { secrets.append(secret.handOver()) }
        try await inner.keep(secret, for: request)
    }
    func fetchSecret(for request: SecretRequest.ID) async throws { note("fetchSecret(\(request))"); try await inner.fetchSecret(for: request) }
    func decline(_ request: SecretRequest.ID) async throws { note("declineSecret(\(request))"); try await inner.decline(request) }
    func authenticate(_ offer: ConnectionOffer.ID) async throws -> URL? { note("authenticate(\(offer))"); return try await inner.authenticate(offer) }
    func decline(_ offer: ConnectionOffer.ID) async throws { note("declineOffer(\(offer))"); try await inner.decline(offer) }
    func file(of attachment: Attachment.ID) async throws -> URL { note("file(\(attachment))"); return try await inner.file(of: attachment) }
    func subAgents() -> AsyncStream<[SubAgent]> { inner.subAgents() }
    func startSubAgent(job: String) async throws { note("startSubAgent(\(String(reflecting: job)))"); try await inner.startSubAgent(job: job) }
    func stop(_ subAgent: SubAgent.ID) async throws { note("stop(\(subAgent))"); try await inner.stop(subAgent) }
    func tasks() -> AsyncStream<[AssistantTask]> { inner.tasks() }
    func joinStops() -> AsyncStream<Bool> { inner.joinStops() }
    func pairingEnded() -> AsyncStream<String?> { inner.pairingEnded() }
    func pairingEndedDetails() -> AsyncStream<String?> { inner.pairingEndedDetails() }
    func joinApproved() -> AsyncStream<Date?> { inner.joinApproved() }
    func pageLook() -> AsyncStream<Look?> { inner.pageLook() }
    func goOn() async { note("goOn()"); await inner.goOn() }
    func addTask(_ title: String, delivery: UUID) async throws { note("addTask(\(String(reflecting: title)))"); try await inner.addTask(title, delivery: delivery) }
    func automations() -> AsyncStream<[Automation]> { inner.automations() }
    func setOn(_ on: Bool, for automation: Automation.ID) async throws { note("setOn(\(on), \(automation))"); try await inner.setOn(on, for: automation) }
    func supervision() -> AsyncStream<Supervision> { inner.supervision() }
    func models() async throws -> [ModelChoice] { note("models()"); return try await inner.models() }
    func providers() async throws -> [ProviderChoice] { note("providers()"); return try await inner.providers() }
    func choose(_ model: ModelChoice.ID, for work: [WorkKind]) async throws {
        note("choose(\(model), \(work.map(\.rawValue).joined(separator: "+")))")
        try await inner.choose(model, for: work)
    }
    func forget(_ learned: Supervision.Learned.ID) async throws { note("forget(\(learned))"); try await inner.forget(learned) }
    func register(_ address: PushAddress) async { note("register()"); await inner.register(address) }
}

/// The app as its program makes it, over a source that writes its calls down, in a window that
/// is never on a screen.
@MainActor
struct Started {
    let app: MacApp
    let window: HeldWindow
    let source: Recording
    let drive: Drive

    var model: AssistantModel { app.model }

    /// Start the app with this content, at the drawing's window size, and wait until it stands.
    static func app(_ content: MemorySource.Content = Drawing.content(), size: CGSize = CGSize(width: 1280, height: 820),
                    file: StaticString = #filePath, line: UInt = #line) async throws -> Started {
        NSApplication.shared.setActivationPolicy(.prohibited)
        // For pictures: the app's own face, where a folder holds it. A bundle carries it; a test has none.
        if let faces = ProcessInfo.processInfo.environment["TILO_FONTS_DIR"] { Typeface.register(in: URL(fileURLWithPath: faces)) }
        let source = Recording(MemorySource(content, now: { Drawing.now }))
        let app = MacApp(source: source, window: { HeldWindow.make($0, $1) })
        app.build()
        let window = try XCTUnwrap(app.window as? HeldWindow, file: file, line: line)
        window.setContentSize(size)
        let drive = Drive(window, menu: app.mainMenu)
        let signedIn = { if case .signedIn = content.session { true } else { false } }()
        await drive.until("the source's first word", { app.model.session != nil && (!signedIn || app.model.conversation(.main).isLoaded) },
                          file: file, line: line)
        await drive.settle(300)
        return Started(app: app, window: window, source: source, drive: drive)
    }

    /// The message field's own text view.
    func field(file: StaticString = #filePath, line: UInt = #line) throws -> NSTextView {
        func find(_ view: NSView) -> NSTextView? {
            if let found = view as? FieldTextView { return found }
            for sub in view.subviews { if let found = find(sub) { return found } }
            return nil
        }
        return try XCTUnwrap(window.contentView.flatMap(find), "the window has no message field", file: file, line: line)
    }

    /// End the run: the window is closed, and nothing of this process may stand on a screen.
    func end(file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(Drive.onScreen(), [], "a window of the run stood on a screen", file: file, line: line)
        window.contentView = nil
        window.close()
    }
}
#endif
