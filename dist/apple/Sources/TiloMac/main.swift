#if os(macOS)
import AppKit

// One program is the app. Asked with `--check`, it says what it can tell with no window.
if CommandLine.arguments.contains("--check") {
    CheckRun.run(live: CommandLine.arguments.contains("--live"))
} else {
    let delegate = AppDelegate()
    NSApplication.shared.delegate = delegate
    NSApplication.shared.run()
}
#else
print("TiloMac is the Mac app. The iPhone app is TiloPhone.")
#endif
