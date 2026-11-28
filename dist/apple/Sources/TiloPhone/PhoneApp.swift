#if os(iOS)
import ShellCore
import UIKit

/// What a long press on the app's icon offers.
enum QuickAction: String {
    case dictate = "tilo.dictate"
    case openLink = "tilo.open-link"
    case scanCode = "tilo.scan-code"
}

@MainActor
final class PhoneAppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        let action = { (action: QuickAction, words: Words, symbol: String) in
            UIApplicationShortcutItem(type: action.rawValue, localizedTitle: words.said, localizedSubtitle: nil,
                                      icon: UIApplicationShortcutIcon(systemImageName: symbol), userInfo: nil)
        }
        var items = [action(.dictate, .dictate, "mic"), action(.openLink, .openLink, "link")]
        // The way in is in the site's entry page. These two reach it from anywhere, also where a
        // page shows no entry page the app knows.
        if RootViewController.readsCodes { items.insert(action(.scanCode, .scanCode, "qrcode.viewfinder"), at: 1) }
        application.shortcutItems = items
        return true
    }

    /// Apple's address for this iPhone goes to the page, which hands it to the person's host.
    func application(_ application: UIApplication,
                     didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        for scene in application.connectedScenes {
            guard let window = (scene as? UIWindowScene)?.windows.first,
                  let root = window.rootViewController as? RootViewController else { continue }
            root.registered(deviceToken)
        }
    }

    /// A push that shows nothing: the thing it names waits no more, and its notice is taken away.
    func application(_ application: UIApplication, didReceiveRemoteNotification userInfo: [AnyHashable: Any],
                     fetchCompletionHandler completionHandler: @escaping (UIBackgroundFetchResult) -> Void) {
        guard let push = ClosedPush(userInfo), push.isGone else { return completionHandler(.noData) }
        ClosedPush.withdraw(push.name)
        completionHandler(.newData)
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
