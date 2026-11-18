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
    ///
    /// A link the site's own computer made under a name only it can use is read as this site's,
    /// as the shell reads it (`SitePolicy.inSite`): the code and the link a person's page shows
    /// on that computer are the ones a phone scans or is handed. The link is judged whole, by
    /// where it leads: an address written inside it adds no name.
    public static func pairing(_ link: URL, of settings: ShellSettings) -> URL? {
        guard let link = SitePolicy(settings: settings).inSite(link),
              ["", "/", "/app/"].contains(link.path(percentEncoded: true)),
              let fragment = link.fragment(percentEncoded: true)
        else { return nil }
        let pieces = fragment.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).map(String.init)
        guard pieces.count == 2, let workspace = workspace(of: pieces[0]),
              pieces[1].split(separator: "&").contains(where: { $0.hasPrefix("pair=") && $0.count > "pair=".count })
        else { return nil }
        return URL(string: "\(settings.origin)/app/#\(workspace)/\(chat)?\(pieces[1])")
    }

    /// The site a pairing link is from, where that is another Tilo site than this app's: the
    /// name and port it leads to, and nothing else of the link. Nil for a link this app pairs
    /// with, and for one that would pair nowhere.
    ///
    /// A link is another site's pairing link when it would pair here were it this site's: its
    /// path, its query and what follows its `#` are judged by `pairing` under this site's
    /// origin, so what a pairing link looks like is said in one place. What is written so is
    /// only judged and never opened: a link of another site stays refused. It is for telling a
    /// person where the code they scanned is from. Where the two sites differ only in http and
    /// https, the name carries which, so that the two do not read the same.
    public static func pairingElsewhere(_ link: URL, of settings: ShellSettings) -> String? {
        guard pairing(link, of: settings) == nil, link.user(percentEncoded: true) == nil,
              let from = SitePolicy.origin(of: link), from != settings.origin
        else { return nil }
        var written = settings.origin + link.path(percentEncoded: true)
        if let query = link.query(percentEncoded: true) { written += "?\(query)" }
        if let route = link.fragment(percentEncoded: true) { written += "#\(route)" }
        guard let here = URL(string: written), pairing(here, of: settings) != nil else { return nil }
        // An origin is `scheme://name[:port]`: the name and port are what follows its two slashes.
        func name(_ origin: String) -> String { String(origin.drop { $0 != "/" }.dropFirst(2)) }
        return name(from) == name(settings.origin) ? from : name(from)
    }
}
