import AssistantCore
import Foundation

#if os(iOS)
import ActivityKit
#endif

/// What the app can tell of itself with nothing on screen, one line per fact, for a build to read.
public enum AssistantCheck {
    /// Whether the app was started to say what it can tell.
    public static var isAsked: Bool {
        CommandLine.arguments.contains("--check")
    }

    /// Say the facts and end. `source` names what the app's views get everything from.
    @MainActor
    public static func run(source: String) -> Never {
        var failed = false
        func say(_ holds: Bool, _ fact: String) {
            print("CHECK \(holds ? "ok  " : "FAIL") \(fact)")
            if !holds { failed = true }
        }
        func note(_ fact: String) {
            print("CHECK note \(fact)")
        }
        let info = Bundle.main.infoDictionary ?? [:]
        let version = info["CFBundleShortVersionString"] as? String ?? "?"
        say(Bundle.main.bundleIdentifier != nil,
            "the app is \(AssistantWords.appName), \(Bundle.main.bundleIdentifier ?? "in no bundle"), version \(version)")
        let team = info["TiloTeam"] as? String ?? ""
        note(team.isEmpty ? "signed by nobody" : "signed by team \(team)")
        #if os(macOS)
        say(ProcessInfo.processInfo.environment["APP_SANDBOX_CONTAINER_ID"] != nil, "it runs in the App Sandbox")
        #endif
        Typeface.registerBundled()
        say(Typeface.isKnown, "its typeface is \(Typeface.family), from its own bundle")
        let missing = "\u{1}"
        let carried = AssistantWords.allCases.filter {
            Bundle.main.localizedString(forKey: $0.rawValue, value: missing, table: nil) != missing
        }
        say(carried.count == AssistantWords.allCases.count,
            "its bundle carries \(carried.count) of its \(AssistantWords.allCases.count) sentences")
        #if os(iOS)
        say(info["NSSupportsLiveActivities"] as? Bool == true, "its bundle asks for live activities")
        let extensionAt = Bundle.main.builtInPlugInsURL?.appendingPathComponent("AssistantWidgets.appex").path ?? ""
        say(FileManager.default.fileExists(atPath: extensionAt), "it carries its widget extension")
        note("this phone \(ActivityAuthorizationInfo().areActivitiesEnabled ? "lets" : "does not let") it show live activities")
        #endif
        note("its views get everything from \(source)")
        print("CHECK end")
        exit(failed ? 1 : 0)
    }
}
