#if os(iOS)
import AssistantCore
import AssistantUI
import SwiftUI
import UIKit

/// Tilo Assistant on the iPhone: the same views arranged for a phone, the system's
/// notifications with their answers, and live activities.
struct AssistantPhoneApp: App {
    @UIApplicationDelegateAdaptor(PhoneDelegate.self) private var delegate
    @Environment(\.scenePhase) private var phase
    @State private var model: AssistantModel
    @State private var teller: Teller
    @State private var activities: LiveActivities

    init() {
        Typeface.registerBundled()
        let model = AssistantModel(source: Source.make())
        // On a phone the Tasks panel is a sheet: it comes when asked for.
        model.showsPanelInMain = false
        model.start()
        _model = State(initialValue: model)
        _teller = State(initialValue: Teller(model: model))
        _activities = State(initialValue: LiveActivities(model: model))
    }

    var body: some Scene {
        WindowGroup {
            AssistantRoot(model: model, engine: Source.engine) { PhoneRoot() }
                .environment(\.liveThingView, { thing in Source.live(thing) })
                .task(id: model.person?.id) {
                    delegate.source = model.source
                    teller.looksAt = { chat in
                        UIApplication.shared.applicationState == .active && model.place == .chat(chat)
                    }
                    guard model.person != nil else { return }
                    if await teller.ask() { UIApplication.shared.registerForRemoteNotifications() }
                }
                .onChange(of: model.waiting) { teller.setBadge(model.waiting) }
                .onChange(of: model.tasks) { activities.bring() }
                .onChange(of: model.subAgents) { activities.bring() }
                .onChange(of: model.automations) { activities.bring() }
                .onChange(of: model.conversation(.main).items.count) { activities.bring() }
                .onChange(of: phase) { if phase == .active { activities.bring() } }
                .onOpenURL { LiveLink.open($0, in: model) }
        }
    }
}

final class PhoneDelegate: NSObject, UIApplicationDelegate {
    var source: (any AssistantSource)?

    /// Apple's address for this phone goes to the person's host, which tells them while the app is closed.
    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        guard let source else { return }
        Task { await source.register(PushAddress(use: .notices, token: deviceToken)) }
    }
}
/// Where the program begins: asked with `--check` it says what it can tell of itself and
/// ends, with nothing on screen; otherwise it is the app.
@main
enum Entry {
    @MainActor
    static func main() {
        if AssistantCheck.isAsked {
            AssistantCheck.run(source: Source.name)
        }
        AssistantPhoneApp.main()
    }
}
#else
@main
enum NotThisPlatform {
    static func main() {
        print("AssistantPhone is the iPhone app. The Mac app is AssistantMac.")
    }
}
#endif
