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
        Task {
            if CommandLine.arguments.contains("--tell") {
                await PhoneShots.tell(in: window)
            } else {
                await PhoneShots.draw(in: window)
                exit(0)
            }
        }
    }
}

@MainActor
enum PhoneShots {
    /// Every screen at this phone's size, light and dark, into the app's documents.
    static func draw(in window: UIWindow) async {
        Typeface.registerBundled()
        print("SHOTS typeface: \(Typeface.isKnown ? Typeface.family : "the system's")")
        let folder = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        for scheme in [ColorScheme.light, .dark] {
            window.overrideUserInterfaceStyle = scheme == .dark ? .dark : .light
            for screen in await Screens.phone(scheme) {
                window.rootViewController = UIHostingController(rootView: screen.view)
                window.layoutIfNeeded()
                // Long enough for a sheet to have come up and a list to have settled.
                try? await Task.sleep(for: .milliseconds(1100))
                let picture = UIGraphicsImageRenderer(bounds: window.bounds).image { _ in
                    window.drawHierarchy(in: window.bounds, afterScreenUpdates: true)
                }
                let name = "\(screen.name)-\(scheme == .dark ? "dark" : "light").png"
                try? picture.pngData()?.write(to: folder.appendingPathComponent(name))
                print("SHOTS drew \(name)")
            }
        }
        print("SHOTS done")
    }

    /// What the system is handed outside the window, through the app's own code and with the
    /// drawing's content at this hour: the notifications with their categories and buttons, and
    /// the live activities. Says what the system then holds, and stays, so a picture can be taken.
    static func tell(in window: UIWindow) async {
        Typeface.registerBundled()
        // Said aloud and kept in the app's documents, where whoever started it reads it.
        let kept = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("told.txt")
        var lines: [String] = []
        func say(_ line: String) {
            print("TELL \(line)")
            lines.append("TELL \(line)")
            try? Data((lines.joined(separator: "\n") + "\n").utf8).write(to: kept)
        }
        try? FileManager.default.removeItem(at: kept)
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
#endif
