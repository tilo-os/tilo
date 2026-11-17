#if os(iOS)
import ShellCore
import UIKit

/// What a long press on the app's icon offers.
enum QuickAction: String {
    case dictate = "tilo.dictate"
    case openLink = "tilo.open-link"
}

@MainActor
final class PhoneAppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        let action = { (action: QuickAction, words: Words, symbol: String) in
            UIApplicationShortcutItem(type: action.rawValue, localizedTitle: words.said, localizedSubtitle: nil,
                                      icon: UIApplicationShortcutIcon(systemImageName: symbol), userInfo: nil)
        }
        application.shortcutItems = [action(.dictate, .dictate, "mic"), action(.openLink, .openLink, "link")]
        return true
    }

    func application(_ application: UIApplication, configurationForConnecting session: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let configuration = UISceneConfiguration(name: "Tilo", sessionRole: session.role)
        configuration.delegateClass = SceneDelegate.self
        return configuration
    }
}

@MainActor
final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    private let root = RootViewController()

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        let window = UIWindow(windowScene: scene)
        window.rootViewController = root
        window.makeKeyAndVisible()
        self.window = window
        root.perform(options.shortcutItem.flatMap { QuickAction(rawValue: $0.type) })
    }

    func windowScene(_ windowScene: UIWindowScene, performActionFor shortcutItem: UIApplicationShortcutItem) async -> Bool {
        root.perform(QuickAction(rawValue: shortcutItem.type))
        return true
    }

    func sceneDidBecomeActive(_ scene: UIScene) {
        root.cameForward()
    }
}
#endif
