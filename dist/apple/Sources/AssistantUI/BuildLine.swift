import AssistantCore
import SwiftUI

/// Which build this app is, as a person reads it: the release's name with the build number
/// beside it, as the Mac's About window shows the two ("Version 0.1.13 (49855)"). Both are the
/// bundle's own (`CFBundleShortVersionString`, `CFBundleVersion`), which `build.sh` writes from
/// the commit it builds, so the Mac's About window and this line say the same of one build.
public enum AssistantBuild {
    /// "Version 0.1.13 (49855)", or the version alone for a bundle that carries no build
    /// number. Nil where the bundle names no version: a program in no bundle says nothing.
    public static func said(info: [String: Any]?) -> String? {
        guard let version = info?["CFBundleShortVersionString"] as? String, !version.isEmpty else { return nil }
        guard let number = info?["CFBundleVersion"] as? String, !number.isEmpty else {
            return AssistantWords.versionAlone.said(["version": version])
        }
        return AssistantWords.versionAndBuild.said(["version": version, "build": number])
    }
}

extension EnvironmentValues {
    /// Which build the app is, where the app says so itself. An iPhone's Settings show the
    /// version alone, so two builds of one release read alike there: a phone's root hands this
    /// down (`AssistantBuild.said`), and the way in and the list say it. A Mac's About window
    /// says it already, so a Mac hands none and shows no such line.
    @Entry public var buildSaid: String?
}

/// That line, small and dimmed, wherever it was handed down; nothing where it was not. In the
/// middle at the foot of the way in; under the person's name in the list, where it begins as
/// the name does.
public struct BuildLine: View {
    @Environment(\.buildSaid) private var said
    private let inList: Bool

    public init(inList: Bool = false) {
        self.inList = inList
    }

    public var body: some View {
        if let said {
            Text(said).font(.small12).foregroundStyle(Token.ink3)
                .multilineTextAlignment(inList ? .leading : .center)
                .frame(maxWidth: inList ? .infinity : nil, alignment: inList ? .leading : .center)
                .padding(.horizontal, inList ? 8 : 0)
        }
    }
}
