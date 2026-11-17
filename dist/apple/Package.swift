// swift-tools-version: 6.0
import PackageDescription

// The Mac and iPhone app: a native shell around Tilo's own page. README.md says what each
// part is for; build.sh turns the products into signed app bundles.
let package = Package(
    name: "TiloShell",
    // The page draws with WebGPU and reaches its host over WebTransport. The system web view
    // has the first from OS 26 and the second from OS 26.4, so nothing older can show Tilo.
    platforms: [.macOS("26.4"), .iOS("26.4")],
    targets: [
        // What the shell decides without a web view: which site and app it opens, which
        // addresses stay inside, what the page's title says, what a share hands over.
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
        .testTarget(name: "ShellCoreTests", dependencies: ["ShellCore"]),
        .testTarget(name: "ShellWebTests", dependencies: ["ShellCore", "ShellWeb"]),
    ],
    swiftLanguageModes: [.v6]
)
