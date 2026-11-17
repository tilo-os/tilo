#if os(macOS)
import AppKit
import Security
import ShellCore
import ShellWeb

/// `--check`: what this build can tell about itself and this Mac with nothing on screen, as
/// lines on standard output. It ends with status 0 when nothing failed.
@MainActor
enum CheckRun {
    static func run(live: Bool) {
        let app = NSApplication.shared
        // Not in the Dock, never in front, no menu-bar item: nothing of this run is seen.
        app.setActivationPolicy(.prohibited)
        Task { @MainActor in
            var lines = [signature(), sandbox()]
            switch ShellSettings.load() {
            case .success(let settings):
                lines += await PageCheck(settings: settings, live: live).run()
            case .failure(let why):
                lines.append(PageCheck.Line(name: "site", value: "its address cannot be used (\(why))", passed: false))
            }
            for line in lines {
                let mark = line.passed == true ? "ok  " : line.passed == false ? "FAIL" : "    "
                print("\(mark) \(line.name): \(line.value)")
            }
            exit(lines.contains { $0.passed == false } ? 1 : 0)
        }
        app.run()
    }

    /// WebKit wraps the page's keys with a key the keychain hands only to this app, and the
    /// keychain knows the app by who signed it.
    private static func signature() -> PageCheck.Line {
        var running: SecCode?
        var onDisk: SecStaticCode?
        var facts: CFDictionary?
        guard SecCodeCopySelf([], &running) == errSecSuccess, let running,
              SecCodeCopyStaticCode(running, [], &onDisk) == errSecSuccess, let onDisk,
              SecCodeCopySigningInformation(onDisk, SecCSFlags(rawValue: kSecCSSigningInformation), &facts) == errSecSuccess,
              let facts = facts as? [String: Any]
        else { return PageCheck.Line(name: "signature", value: "cannot be read", passed: false) }
        let name = facts[kSecCodeInfoIdentifier as String] as? String ?? "unnamed"
        let team = facts[kSecCodeInfoTeamIdentifier as String] as? String
        return team == nil
            ? PageCheck.Line(name: "signature", value: "\(name), signed by nobody: the next build cannot read the keys this one keeps", passed: nil)
            : PageCheck.Line(name: "signature", value: "\(name), signed by a team: its keys last across builds", passed: true)
    }

    private static func sandbox() -> PageCheck.Line {
        let inside = ProcessInfo.processInfo.environment["APP_SANDBOX_CONTAINER_ID"] != nil
        return PageCheck.Line(name: "App Sandbox", value: inside ? "on" : "off", passed: inside ? true : nil)
    }
}
#endif
