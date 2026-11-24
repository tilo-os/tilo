// swift-tools-version: 6.0
import PackageDescription

// Tilo Shell for the Mac and the iPhone: a native shell around Tilo's own page, as a whole or
// held to one app. README.md says what each part is for, and what of the retired Swift assistant
// is still here and why.
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
        // The retired Swift assistant's remaining parts, kept until what they alone still do has
        // a place in Tilo: the values its window showed and the interface they came through
        // (AssistantCore), and being told while no app runs (AssistantClosed, AssistantNotice).
        .target(name: "AssistantCore"),
        .target(name: "AssistantClosed", dependencies: ["AssistantCore"]),
        .executableTarget(
            name: "AssistantNotice",
            dependencies: ["AssistantClosed"],
            linkerSettings: [.unsafeFlags(["-Xlinker", "-e", "-Xlinker", "_NSExtensionMain"])]
        ),
        // The content of the retired window's drawing, for its tests only.
        .target(name: "AssistantDrawing", dependencies: ["AssistantCore"]),
        .testTarget(name: "ShellCoreTests", dependencies: ["ShellCore"]),
        .testTarget(name: "ShellWebTests", dependencies: ["ShellCore", "ShellWeb"]),
        .testTarget(name: "AssistantCoreTests", dependencies: ["AssistantCore", "AssistantDrawing"]),
        .testTarget(name: "AssistantClosedTests", dependencies: ["AssistantClosed", "AssistantCore"]),
    ],
    swiftLanguageModes: [.v6]
)
