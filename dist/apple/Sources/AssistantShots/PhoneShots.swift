#if os(iOS)
import AssistantCore
import AssistantDrawing
import AssistantUI
import SwiftUI
import UIKit
import UserNotifications

final class ShotsDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication, configurationForConnecting session: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let configuration = UISceneConfiguration(name: "Shots", sessionRole: session.role)
        configuration.delegateClass = ShotsScene.self
        return configuration
    }
}

final class ShotsScene: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        let window = UIWindow(windowScene: scene)
        window.rootViewController = UIViewController()
        window.makeKeyAndVisible()
        self.window = window
        let asked = CommandLine.arguments
        Task {
            if asked.contains("--tell") {
                await PhoneShots.tell(in: window)
            } else if asked.contains("--secret") {
                await PhoneShots.secret(in: window)
            } else if asked.contains("--typing") {
                await PhoneShots.typing(in: window)
            } else if asked.contains("--said") {
                await PhoneShots.say(in: window)
                exit(0)
            } else {
                await PhoneShots.draw(in: window)
                exit(0)
            }
        }
    }
}

/// Lines said aloud and kept in the app's documents, where whoever started the app reads them.
@MainActor
final class Kept {
    private let file: URL
    private let word: String
    private var lines: [String] = []

    init(_ name: String, word: String) {
        file = PhoneShots.documents.appendingPathComponent(name)
        self.word = word
        try? FileManager.default.removeItem(at: file)
    }

    func say(_ line: String) {
        print("\(word) \(line)")
        lines.append("\(word) \(line)")
        try? Data((lines.joined(separator: "\n") + "\n").utf8).write(to: file)
    }
}

@MainActor
enum PhoneShots {
    static var documents: URL { FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0] }

    /// A window as it draws itself.
    static func picture(of window: UIWindow) -> UIImage {
        let format = UIGraphicsImageRendererFormat()
        format.preferredRange = .standard
        return UIGraphicsImageRenderer(bounds: window.bounds, format: format).image { _ in
            window.drawHierarchy(in: window.bounds, afterScreenUpdates: true)
        }
    }

    /// A window drawn from what it holds, where it can no longer be asked to draw: behind another app.
    static func held(of window: UIWindow) -> UIImage {
        let format = UIGraphicsImageRendererFormat()
        format.preferredRange = .standard
        return UIGraphicsImageRenderer(bounds: window.bounds, format: format).image { context in
            window.layer.render(in: context.cgContext)
        }
    }

    static func save(_ picture: UIImage, _ name: String) {
        try? picture.pngData()?.write(to: documents.appendingPathComponent(name))
    }

    /// How many points of a stretch of a picture are not the colour of its ground.
    static func marks(in picture: UIImage, _ place: CGRect) -> Int {
        guard let image = picture.cgImage, let data = image.dataProvider?.data, let bytes = CFDataGetBytePtr(data),
              image.bitsPerPixel == 32
        else { return -1 }
        let scale = CGFloat(image.width) / picture.size.width
        let area = place.insetBy(dx: 2, dy: 2)
        func colour(_ x: Int, _ y: Int) -> (Int, Int, Int) {
            let at = y * image.bytesPerRow + x * 4
            return (Int(bytes[at]), Int(bytes[at + 1]), Int(bytes[at + 2]))
        }
        let across = Int(area.minX * scale)..<min(image.width, Int(area.maxX * scale))
        let down = Int(area.minY * scale)..<min(image.height, Int(area.maxY * scale))
        guard !across.isEmpty, !down.isEmpty else { return -1 }
        let ground = colour(across.upperBound - 1, (down.lowerBound + down.upperBound) / 2)
        var count = 0
        for y in down {
            for x in across {
                let here = colour(x, y)
                if abs(here.0 - ground.0) + abs(here.1 - ground.1) + abs(here.2 - ground.2) > 60 { count += 1 }
            }
        }
        return count
    }

    /// Every screen at this phone's size, light and dark, into the app's documents.
    static func draw(in window: UIWindow) async {
        Typeface.registerBundled()
        print("SHOTS typeface: \(Typeface.isKnown ? Typeface.family : "the system's")")
        print("SHOTS screen \(Int(window.bounds.width)) by \(Int(window.bounds.height)), text size \(UIApplication.shared.preferredContentSizeCategory.rawValue)")
        for scheme in [ColorScheme.light, .dark] {
            window.overrideUserInterfaceStyle = scheme == .dark ? .dark : .light
            for screen in await Screens.phone(scheme) {
                window.rootViewController = UIHostingController(rootView: screen.view)
                window.layoutIfNeeded()
                // Long enough for a sheet to have come up and a list to have settled.
                try? await Task.sleep(for: .milliseconds(1100))
                let name = "\(screen.name)-\(scheme == .dark ? "dark" : "light").png"
                save(picture(of: window), name)
                print("SHOTS drew \(name)")
            }
        }
        print("SHOTS done")
    }

    /// What every screen says of itself to someone who cannot see it, in the order it is read.
    static func say(in window: UIWindow) async {
        Typeface.registerBundled()
        Spoken.wake()
        for screen in await Screens.phone(.light) {
            window.rootViewController = UIHostingController(rootView: screen.view)
            window.layoutIfNeeded()
            try? await Task.sleep(for: .milliseconds(1300))
            var lines: [String] = []
            Spoken.read(window, into: &lines)
            try? Data((lines.joined(separator: "\n") + "\n").utf8).write(to: documents.appendingPathComponent("said-\(screen.name).txt"))
            print("SAID \(screen.name): \(lines.count) things")
        }
        print("SAID done")
    }

    /// The protected field with something typed into it, and then the app leaves the front:
    /// what its window shows at that moment is what the system keeps as its picture.
    static func secret(in window: UIWindow) async {
        Typeface.registerBundled()
        let kept = Kept("secret.txt", word: "SECRET")
        var content = Drawing.content()
        content.main = Drawing.secretPiece
        content.tasks = []
        let model = AssistantModel(source: MemorySource(content, now: { Drawing.now }))
        await model.prime()
        model.showsPanelInMain = false
        window.rootViewController = UIHostingController(rootView: Screens.rooted(model, .light) { PhoneRoot() })
        window.layoutIfNeeded()
        try? await Task.sleep(for: .milliseconds(1500))
        func protected(_ view: UIView) -> UITextField? {
            if let field = view as? UITextField, field.isSecureTextEntry { return field }
            for sub in view.subviews { if let found = protected(sub) { return found } }
            return nil
        }
        guard let field = protected(window) else {
            kept.say("the window has no protected field")
            kept.say("typed")
            kept.say("done")
            return
        }
        // Typed the way the keyboard types: letter by letter into whatever holds the keyboard.
        let marker = "Zq7-marker-9f3KxW"
        field.becomeFirstResponder()
        for letter in marker { field.insertText(String(letter)) }
        try? await Task.sleep(for: .milliseconds(900))
        kept.say("the field holds \(field.text?.count ?? 0) of \(marker.count) letters typed")
        let place = field.convert(field.bounds, to: window)
        let front = picture(of: window)
        save(front, "secret-in-front.png")
        kept.say("in front: \(marks(in: front, place)) marks where the letters stand")
        Spoken.wake()
        var lines: [String] = []
        Spoken.read(window, into: &lines)
        kept.say("read out in front: \(lines.contains { $0.contains(marker) } ? "THE SECRET" : "no letter of it"), \(lines.filter { $0.contains("protected field") }.count) protected field")
        // In front, the window drawn from what it holds shows that something stands in the field:
        // the measure the picture behind another app is held against.
        kept.say("in front, drawn from what the window holds: \(marks(in: held(of: window), place)) marks where the letters stand")
        kept.say("the app is \(UIApplication.shared.applicationState == .active ? "in front" : "NOT in front") as its observers begin")
        let center = NotificationCenter.default
        _ = center.addObserver(forName: UIApplication.willResignActiveNotification, object: nil, queue: .main) { _ in
            MainActor.assumeIsolated { kept.say("leaving the front") }
        }
        _ = center.addObserver(forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main) { _ in
            MainActor.assumeIsolated {
                // The system keeps its picture of the window once the app has been told it is behind.
                let behind = held(of: window)
                save(behind, "secret-behind.png")
                kept.say("behind another app: \(marks(in: behind, place)) marks where the letters stand")
                var lines: [String] = []
                Spoken.read(window, into: &lines)
                kept.say("read out behind another app: \(lines.contains { $0.contains(marker) } ? "THE SECRET" : "no letter of it"), \(lines.filter { $0.contains("protected field") }.count) protected field")
                kept.say("done")
            }
        }
        kept.say("typed")
    }

    /// The main chat with the keyboard up and words in the field, for a picture of the whole screen.
    static func typing(in window: UIWindow) async {
        Typeface.registerBundled()
        let model = AssistantModel(source: MemorySource(Drawing.content(), now: { Drawing.now }))
        await model.prime()
        model.showsPanelInMain = false
        window.rootViewController = UIHostingController(rootView: Screens.rooted(model, .light) { PhoneRoot() })
        window.layoutIfNeeded()
        try? await Task.sleep(for: .milliseconds(1200))
        // The field takes the keyboard with words begun in it, as when something is changed by telling the assistant.
        model.begin(Drawing.typed)
        print("TYPING ready")
    }

    /// What the system is handed outside the window, through the app's own code and with the
    /// drawing's content at this hour: the notifications with their categories and buttons, and
    /// the live activities. Says what the system then holds, and stays, so a picture can be taken.
    static func tell(in window: UIWindow) async {
        Typeface.registerBundled()
        let kept = Kept("told.txt", word: "TELL")
        func say(_ line: String) { kept.say(line) }
        // The drawing's day, moved to now: a reminder in half an hour, a brief for something a quarter of an hour off.
        let now = Date()
        var content = Drawing.content()
        content.automations[0].next = now.addingTimeInterval(30 * 60)
        var brief = Drawing.briefPiece[0]
        if case .brief(var held) = brief.content {
            held.begins = now.addingTimeInterval(15 * 60)
            brief.content = .brief(held)
        }
        content.main = Drawing.mainChat + [brief]
        content.subAgents[0].startedBy = .person
        let model = AssistantModel(source: MemorySource(content, now: { now }))
        await model.prime()
        model.showsPanelInMain = false
        window.rootViewController = UIHostingController(rootView: Screens.rooted(model, .light) { PhoneRoot() })

        let center = UNUserNotificationCenter.current()
        // Asked for quietly, so no question stands on the screen: the system then lists what it is handed.
        let allowed = (try? await center.requestAuthorization(options: [.alert, .sound, .badge, .provisional])) ?? false
        say("notifications allowed: \(allowed)")
        let teller = Teller(model: model)
        for item in [Drawing.mainChat[3], Drawing.reminder, Drawing.mainChat[2]] { teller.tell(item, in: .main) }
        try? await Task.sleep(for: .seconds(2))
        for category in await center.notificationCategories().sorted(by: { $0.identifier < $1.identifier }) {
            say("category \(category.identifier): \(category.actions.map { "\($0.title) [\($0.identifier)]" }.joined(separator: ", "))")
        }
        for delivered in await center.deliveredNotifications().sorted(by: { $0.request.identifier < $1.request.identifier }) {
            let said = delivered.request.content
            say("delivered \(delivered.request.identifier): title \(said.title) | subtitle \(said.subtitle) | body \(said.body) | category \(said.categoryIdentifier) | level \(said.interruptionLevel.rawValue)")
        }

        let activities = LiveActivities(model: model)
        activities.bring(now: now)
        try? await Task.sleep(for: .seconds(2))
        for line in activities.running() { say("live activity \(line)") }
        say("done")
    }
}

/// What a window says of itself to someone who cannot see it.
@MainActor
enum Spoken {
    /// A simulator makes a view's accessibility elements only once something that reads them is there.
    static func wake() {
        guard let library = dlopen("/usr/lib/libAccessibility.dylib", RTLD_NOW) else { return }
        typealias Setter = @convention(c) (Int32) -> Void
        for name in ["_AXSSetAutomationEnabled", "_AXSApplicationAccessibilitySetEnabled"] {
            if let symbol = dlsym(library, name) { unsafeBitCast(symbol, to: Setter.self)(1) }
        }
    }

    /// Everything under an object that is read out, in the order it is read.
    static func read(_ object: NSObject, into lines: inout [String]) {
        if (object as? UIView)?.isHidden == true || object.accessibilityElementsHidden { return }
        if object.isAccessibilityElement {
            lines.append(line(object))
            return
        }
        if let listed = object.accessibilityElements {
            for case let child as NSObject in listed { read(child, into: &lines) }
            return
        }
        let count = object.accessibilityElementCount()
        if count != NSNotFound, count > 0 {
            for index in 0..<count {
                if let child = object.accessibilityElement(at: index) as? NSObject { read(child, into: &lines) }
            }
            return
        }
        guard let view = object as? UIView else { return }
        // What stands over everything else, a sheet, is all that is read.
        let over = view.subviews.filter(\.accessibilityViewIsModal)
        let before = lines.count
        for sub in over { read(sub, into: &lines) }
        if lines.count == before { for sub in view.subviews where !over.contains(sub) { read(sub, into: &lines) } }
    }

    static func line(_ object: NSObject) -> String {
        let traits = object.accessibilityTraits
        let named: [(UIAccessibilityTraits, String)] = [
            (.header, "heading"), (.button, "button"), (.link, "link"), (.toggleButton, "switch"), (.selected, "selected"),
            (.notEnabled, "off"), (.adjustable, "adjustable"), (.searchField, "search field"), (.image, "image"),
            (.staticText, "text"), (.updatesFrequently, "changing"),
        ]
        var kinds = named.filter { traits.contains($0.0) }.map(\.1)
        var isProtected = false
        if let field = object as? UITextField {
            isProtected = field.isSecureTextEntry
            kinds.append(isProtected ? "protected field" : "field")
        }
        if object is UITextView { kinds.append("text field") }
        let frame = object.accessibilityFrame
        var said = "[\(kinds.joined(separator: ", "))] “\(object.accessibilityLabel ?? "")”"
        if !isProtected, let value = object.accessibilityValue, !value.isEmpty { said += " = “\(value)”" }
        if let hint = object.accessibilityHint, !hint.isEmpty { said += " (\(hint))" }
        return said + " at \(Int(frame.minX)),\(Int(frame.minY)) \(Int(frame.width)) by \(Int(frame.height))"
    }
}
#endif
