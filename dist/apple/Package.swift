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
        // The notification service extension of both: what the person's host pushed while no
        // page ran, opened on the device with the key the app keeps.
        .executableTarget(
            name: "TiloNotice",
            dependencies: ["ShellCore"],
            linkerSettings: [.unsafeFlags(["-Xlinker", "-e", "-Xlinker", "_NSExtensionMain"])]
        ),
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
        // The home screen's card of what waits for the person: a widget extension, which starts
        // as the share extension does, on the Mac and the iPhone.
        .executableTarget(
            name: "TiloWidgets",
            dependencies: ["ShellCore"],
            linkerSettings: [.unsafeFlags(["-Xlinker", "-e", "-Xlinker", "_NSExtensionMain"])]
        ),
        // The fixture is read from the source tree by `#filePath`, not copied into a bundle.
        .testTarget(name: "ShellCoreTests", dependencies: ["ShellCore"], exclude: ["Fixtures"]),
        .testTarget(name: "ShellWebTests", dependencies: ["ShellCore", "ShellWeb"]),
    ],
    swiftLanguageModes: [.v6]
)
