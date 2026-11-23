// swift-tools-version: 6.0
import PackageDescription

// Tilo Shell and Tilo Assistant for the Mac and the iPhone: a native shell around Tilo's own
// page, as a whole or held to one app. Both apps are these same programs; build.sh gives a
// bundle its name, its identifier and what it shows. README.md says what each part is for.
let package = Package(
    name: "TiloShell",
    // The page draws with WebGPU and reaches its host over WebTransport. The system web view
    // has the first from OS 26 and the second from OS 26.4, so nothing older can show Tilo.
    platforms: [.macOS("26.4"), .iOS("26.4")],
    targets: [
        // What the shell decides without a web view: which site it opens and what of it it
        // shows, which addresses stay inside, what the page's title says, what a share hands over.
        .target(name: "ShellCore"),
        // The web view that holds the page, the script that joins the page to the system,
        // notifications and dictation. Shared by the Mac and the iPhone app.
        .target(name: "ShellWeb", dependencies: ["ShellCore"]),
        // The Mac app: one window, the menu-bar item, settings.
        .executableTarget(name: "TiloMac", dependencies: ["ShellCore", "ShellWeb"]),
        // The iPhone app.
        .executableTarget(name: "TiloPhone", dependencies: ["ShellCore", "ShellWeb"]),
        // The share extension of both: an app extension starts at Foundation's entry point.
        .executableTarget(
            name: "TiloShare",
            dependencies: ["ShellCore"],
            linkerSettings: [.unsafeFlags(["-Xlinker", "-e", "-Xlinker", "_NSExtensionMain"])]
        ),
        // Tilo Assistant, the SwiftUI app of the assistant's window. Its views read and call one
        // interface, AssistantSource, and never Tilo itself.
        // The values the window shows and the interface they come through. No view.
        .target(name: "AssistantCore"),
        // The views, shared by the Mac and the iPhone app. Dictation is the shell's.
        .target(name: "AssistantUI", dependencies: ["AssistantCore", "AssistantClosed", "ShellWeb"]),
        // Being told while the app is closed: the device's own key, what it opens and names,
        // where it is kept, and what a push says. No view, and nothing of Tilo's page.
        .target(name: "AssistantClosed", dependencies: ["AssistantCore"]),
        .executableTarget(name: "AssistantMac", dependencies: ["AssistantCore", "AssistantClosed", "AssistantUI", "AssistantTilo", "TiloLive", "ShellCore"]),
        .executableTarget(name: "AssistantPhone", dependencies: ["AssistantCore", "AssistantClosed", "AssistantUI", "AssistantTilo", "ShellCore"]),
        // The iPhone's live activity: a widget extension, which starts as the share extension does.
        .executableTarget(
            name: "AssistantWidgets",
            dependencies: ["AssistantCore", "AssistantUI"],
            linkerSettings: [.unsafeFlags(["-Xlinker", "-e", "-Xlinker", "_NSExtensionMain"])]
        ),
        // The notice extension: it opens what a push says for this device alone. Only a build
        // that is told while it is closed carries it (build.sh, TILO_APPLE_PUSH).
        .executableTarget(
            name: "AssistantNotice",
            dependencies: ["AssistantClosed"],
            linkerSettings: [.unsafeFlags(["-Xlinker", "-e", "-Xlinker", "_NSExtensionMain"])]
        ),
        // The content of the window's drawing, for tests and pictures. No app links it.
        .target(name: "AssistantDrawing", dependencies: ["AssistantCore"]),
        // Draws every screen to a picture with nothing on screen.
        .executableTarget(name: "AssistantShots", dependencies: ["AssistantCore", "AssistantUI", "AssistantDrawing"]),
        .testTarget(name: "ShellCoreTests", dependencies: ["ShellCore"]),
        .testTarget(name: "ShellWebTests", dependencies: ["ShellCore", "ShellWeb"]),
        .testTarget(name: "AssistantCoreTests", dependencies: ["AssistantCore", "AssistantDrawing"]),
        .testTarget(name: "AssistantUITests", dependencies: ["AssistantCore", "AssistantUI", "AssistantDrawing"]),
        // Tilo inside a native app: Tilo's own page as the client behind a Swift interface
        // (pairing, live reads, typed actions), and the view that shows one live Tilo thing.
        .target(name: "TiloLive", dependencies: ["ShellCore"]),
        // The assistant's data interface on Tilo itself: the app as its person's device,
        // through the engine. The one thing the apps name as their source.
        .target(name: "AssistantTilo", dependencies: ["AssistantCore", "AssistantClosed", "ShellCore", "TiloLive"]),
        .testTarget(name: "TiloLiveTests", dependencies: ["ShellCore", "TiloLive", "AssistantCore", "AssistantClosed", "AssistantTilo"]),
        .testTarget(name: "AssistantClosedTests", dependencies: ["AssistantClosed", "AssistantCore"]),
    ],
    swiftLanguageModes: [.v6]
)
