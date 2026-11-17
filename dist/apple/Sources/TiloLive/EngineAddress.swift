import Foundation
import ShellCore

/// The addresses the engine opens: always the page that shows one app alone, which has no
/// desktop and writes nothing into the workspace's layout.
public enum EngineAddress {
    /// The app whose page holds a person's conversations.
    public static let chat = "so.tilo.chat"

    /// The part of a route that names a workspace, as the link grammar writes it: the person's
    /// own (`/me`), one of this site's host (`/w/<workspace>`) or one of a named host
    /// (`/@<host>/w/<workspace>`). Nil for anything else.
    public static func workspace(of route: String) -> String? {
        let parts = route.split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        guard parts.count > 1, parts[0].isEmpty else { return nil }
        if parts[1] == "me" { return "/me" }
        if parts[1] == "w", parts.count > 2, !parts[2].isEmpty { return "/w/\(parts[2])" }
        if parts[1].hasPrefix("@"), parts[1].count > 1, parts.count > 3, parts[2] == "w", !parts[3].isEmpty {
            return "/\(parts[1])/w/\(parts[3])"
        }
        return nil
    }

    /// The workspace the page at `address` stands in, read from what follows its `#`.
    public static func workspace(ofPage address: URL) -> String? {
        guard let fragment = address.fragment(percentEncoded: true) else { return nil }
        return workspace(of: String(fragment.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false)[0]))
    }

    /// Whether the page at `route` stands in another workspace than `asked` names. The person's
    /// own (`/me`) is wherever the site opens it, and a workspace is the same under whichever
    /// host's name it is written.
    public static func isElsewhere(_ route: String, than asked: String?) -> Bool {
        guard let asked, asked != "/me" else { return false }
        guard let here = workspace(of: route) else { return true }
        return here.split(separator: "/").last != asked.split(separator: "/").last
    }

    /// The page that shows `app` alone in the workspace `route` names, on a record of its own
    /// where one is named.
    public static func page(of app: String, record: String? = nil, in route: String, of settings: ShellSettings) -> URL? {
        guard let workspace = workspace(of: route),
              app.wholeMatch(of: /[a-z0-9]+(\.[a-z0-9][a-z0-9-]*)+/) != nil
        else { return nil }
        let named = record.flatMap { $0.isEmpty ? nil : $0.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "/?#"))) }
        return URL(string: "\(settings.origin)/app/#\(workspace)/\(app)\(named.map { "/\($0)" } ?? "")")
    }

    /// A pairing link as the engine opens it: the same site, the same workspace and the same
    /// secret, on the page that shows Chat alone. Nil for a link into another site, or one that
    /// carries nothing to pair with.
    public static func pairing(_ link: URL, of settings: ShellSettings) -> URL? {
        guard SitePolicy.origin(of: link) == settings.origin,
              ["", "/", "/app/"].contains(link.path(percentEncoded: true)),
              let fragment = link.fragment(percentEncoded: true)
        else { return nil }
        let pieces = fragment.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).map(String.init)
        guard pieces.count == 2, let workspace = workspace(of: pieces[0]),
              pieces[1].split(separator: "&").contains(where: { $0.hasPrefix("pair=") && $0.count > "pair=".count })
        else { return nil }
        return URL(string: "\(settings.origin)/app/#\(workspace)/\(chat)?\(pieces[1])")
    }
}
