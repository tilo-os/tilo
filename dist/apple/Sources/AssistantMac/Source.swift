import AssistantClosed
import AssistantCore
import AssistantTilo
import Foundation
import ShellCore
import SwiftUI

/// Where the app's views get everything from. This is the one place that names an
/// implementation of `AssistantSource`: Tilo itself, with the app as a device of its person
/// (`TiloSource`).
///
/// The site is the build's (`TiloSite` in its `Info.plist`), or what `-TiloSite <origin>` named
/// once on the command line. Until a device is paired the app stands at the way in.
@MainActor
enum Source {
    /// The key of the store the device's key is kept in: named once, the same at every launch.
    static let storeKey = "TiloLiveStore"
    /// The key of one conversation to hold as the main chat, for a site whose Chat keeps no
    /// agents yet. Without it the main chat is the person's conversation with their assistant.
    static let mainChatKey = "TiloMainChat"

    /// What this device is called where its person lets it in and lists their devices.
    #if os(macOS)
    static var deviceName: String { "Tilo Assistant on \(Host.current().localizedName ?? "a Mac")" }
    #else
    // A Mac's program: only build.sh's build of the whole package compiles this file for an iPhone.
    static var deviceName: String { "Tilo Assistant" }
    #endif

    /// What this build says of being told while the app is closed. Nil in a build without the
    /// push right, which asks Apple's service for nothing and keeps no key.
    static let closed = ClosedBuild(info: Bundle.main.infoDictionary)

    static let tilo: TiloSource = {
        ShellSettings.keepArguments()
        let defaults = UserDefaults.standard
        let settings = (try? ShellSettings.load().get()) ?? (try! ShellSettings(site: ShellSettings.productionSite))
        let store = defaults.string(forKey: storeKey).flatMap(UUID.init(uuidString:)) ?? {
            let made = UUID()
            defaults.set(made.uuidString, forKey: storeKey)
            return made
        }()
        let chat = defaults.string(forKey: mainChatKey).flatMap { $0.isEmpty ? nil : $0 }
        // Without one the app finds its person's assistant by itself, and makes it where they keep none.
        let mainChat: TiloSource.MainChat = chat.map { .conversation(id: $0, name: $0) } ?? .assistant
        return TiloSource(settings: settings, store: store, mainChat: mainChat, deviceName: deviceName, closedKeys: closed?.keys)
    }()

    static func make() -> any AssistantSource { tilo }

    /// What the source is called, for the app's own check: said without making one.
    static let name = String(describing: TiloSource.self)

    /// What stands in the window for the source to do its work, though it shows nothing.
    static var engine: AnyView { tilo.engineRoom }

    /// The view of a live Tilo thing in a message.
    static func live(_ thing: LiveThing) -> AnyView { tilo.liveView(thing) }
}
