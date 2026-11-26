import Foundation

/// Where an address the page wants to show belongs.
public enum Destination: Equatable, Sendable {
    /// Tilo's own site: it opens in the shell.
    case inside
    /// Something else on the web, or a mail or phone address: the system opens it.
    case outside
    /// Nothing the shell opens at all.
    case refused
}

/// The shell shows Tilo's own page and nothing else, so the script that joins the page to
/// the system never meets another site's content.
public struct SitePolicy: Equatable, Sendable {
    public let origin: String
    /// The one app of a build that shows nothing else of Tilo.
    public let only: String?

    public init(settings: ShellSettings) {
        origin = settings.origin
        only = settings.alone ? settings.app : nil
    }

    public func destination(of url: URL) -> Destination {
        switch url.scheme?.lowercased() {
        case "about":
            return .inside
        case "blob":
            // A blob's address is `blob:<origin>/<id>`: the page's own when the origin is.
            let inner = String(url.absoluteString.dropFirst("blob:".count))
            return URL(string: inner).flatMap(SitePolicy.origin(of:)) == origin ? .inside : .refused
        case "http", "https":
            return SitePolicy.origin(of: url) == origin ? .inside : .outside
        case "mailto", "tel", "sms", "facetime", "facetime-audio":
            return .outside
        default:
            return .refused
        }
    }

    /// Whether `url` is what this build is for. A build that shows Tilo as a whole is for
    /// everything in its site. A build that shows one app is for that app's own page: the
    /// link grammar's one-app entry (`app/`) with a route that names the app, in the person's
    /// own workspace (`/me/<app>`) or in a named one (`[/@<host>]/w/<workspace>/<app>`).
    public func isOwn(_ url: URL) -> Bool {
        guard destination(of: url) == .inside else { return false }
        // What is no page of the site (a blank page, a blob of the page's) shows nothing of Tilo.
        guard let only, SitePolicy.origin(of: url) != nil else { return true }
        // The page the server renders names its app in the path: `/w/<workspace>/<app>`.
        // It is the app's own page, as the one-app entry's route is, and the entry hands
        // every admitted browser to it.
        let pathParts = url.path(percentEncoded: true).split(separator: "/", omittingEmptySubsequences: false)
        if (4...5).contains(pathParts.count), pathParts[0].isEmpty, pathParts[1] == "w", !pathParts[2].isEmpty {
            // PageAddress spells a native app by its short name. Older one-app
            // builds also recognized the fully qualified app on its own site.
            let app = pathParts[3]
            let workspace = pathParts[2].utf8
            let plain = workspace.allSatisfy { (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || $0 == 45 }
            let named = !app.isEmpty && (app == only || "so.tilo." + app == only)
            if plain && named && (pathParts.count == 4 || ![".", ".."].contains(pathParts[4])) { return true }
        }
        guard url.path(percentEncoded: true) == "/app/", let fragment = url.fragment(percentEncoded: true),
              let route = fragment.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).first
        else { return false }
        let parts = route.split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count > 1, parts[0].isEmpty else { return false }
        let app: Int
        switch parts[1] {
        case "me": app = 2
        case "w": app = 3
        case let host where host.hasPrefix("@") && parts.count > 2 && parts[2] == "w": app = 4
        default: return false
        }
        return parts.count > app && parts[app] == only
    }

    /// The first link in `text` that leads into this site: what a person pasted, scanned or
    /// shared to get in (an invitation, a pairing link) or to open a document. It is taken out
    /// of whatever is written around it, a sentence or brackets.
    ///
    /// A link leads into this site when its origin is the site's, and when the computer that
    /// serves the site wrote it under a name only that computer can use (`asOwn`). What is
    /// returned is always an address in this site's origin; a link of any other site is none.
    public func link(in text: String) -> URL? {
        for word in text.split(whereSeparator: \.isWhitespace) {
            guard let start = word.firstMatch(of: /https?:\/\//)?.range.lowerBound else { continue }
            var written = String(word[start...])
            // What closes a sentence or a bracket around a link is not part of it. A dash or
            // an underline may end an invitation's key, and stays.
            while let last = written.last, ".,;:!?)]}>\"'”’»".contains(last) { written.removeLast() }
            guard let url = URL(string: written) else { continue }
            if let inside = inSite(url) { return inside }
        }
        return nil
    }

    /// `url` as an address in this site: itself when its origin is the site's, read as the
    /// site's when the computer that serves the site wrote it for itself (`asOwn`), and `nil`
    /// for anything else. The address is judged whole, by where it leads: what it carries in
    /// its path, in its query or after its `#` adds no name.
    public func inSite(_ url: URL) -> URL? {
        if SitePolicy.origin(of: url) == origin { return url }
        return asOwn(url)
    }

    /// `url` as a link of this site, when it is written the way the computer that serves the
    /// site writes it for itself: its host means "this computer" (`localhost`, a name under
    /// it, a `127.x` address, `[::1]`) and its port is this site's. `nil` for anything else.
    ///
    /// A page opened as `http://tilo.localhost:24120` on the computer that runs the site makes
    /// its pairing links and invitations under that name, and names the workspace's host in
    /// the route the same way (`#/@127.0.0.1:24100/w/…`). No other device reaches either name:
    /// on a phone both mean the phone. A link spelled so was made for another device than the
    /// one it was made on, and the one site it can be meant for here is the site this app
    /// opens. It is read as that: this site's origin with the link's own path, query and
    /// fragment, its secret untouched. Where this site goes by a name other devices use, a
    /// host in the route that means "this computer" is written with the site's name and the
    /// host's own port: the computer the link calls itself is the one this app reaches under
    /// that name, and a site served beyond its computer serves its host on the same port there.
    ///
    /// What that gives away: nothing. The address opened is in this site's origin, so the
    /// secret reaches this site's page and no other, and the script that joins page and system
    /// runs nowhere else. A link written with this site's name could always carry any path and
    /// name any host; one spelled as "this computer" can now say as much and no more. A link of
    /// another site stays refused, and so does one on another port, which is some other
    /// program on someone's computer and not this site.
    private func asOwn(_ url: URL) -> URL? {
        guard let site = URLComponents(string: origin), let name = site.host?.lowercased(),
              let parts = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let scheme = parts.scheme?.lowercased(), scheme == "https" || scheme == "http",
              let host = parts.host?.lowercased(), ShellSettings.isThisComputer(host),
              // A name before the host is a sign-in, which no link of Tilo's carries.
              parts.user == nil, parts.password == nil,
              SitePolicy.port(of: parts) == SitePolicy.port(of: site)
        else { return nil }
        var written = origin + (parts.percentEncodedPath.isEmpty ? "/" : parts.percentEncodedPath)
        if let query = parts.percentEncodedQuery { written += "?\(query)" }
        if let route = parts.percentEncodedFragment {
            written += "#" + (ShellSettings.isThisComputer(name) ? route : SitePolicy.route(route, hostAs: name))
        }
        return URL(string: written)
    }

    /// `route` with the host it starts by (`/@<host>[:port]/…`, the link grammar's way to say
    /// whose workspace follows) written as `name`, when that host means "this computer". The
    /// port stays: it is the host's own under whatever name its computer goes by. The mark the
    /// grammar gives this computer over TLS (`https:<host>`) goes, because any other name is
    /// reached over https anyway, and so does the port https has by itself.
    private static func route(_ route: String, hostAs name: String) -> String {
        // A name with a colon in it is an address the grammar has no way to write.
        guard route.hasPrefix("/@"), !name.contains(":") else { return route }
        let rest = route.dropFirst(2)
        let end = rest.firstIndex { $0 == "/" || $0 == "?" } ?? rest.endIndex
        guard var host = String(rest[..<end]).removingPercentEncoding?.lowercased() else { return route }
        if host.hasPrefix("https:") { host.removeFirst("https:".count) }
        var port: String?
        // `127.0.0.1:24100` and `[::1]:24100`: the port follows the last colon outside brackets.
        if let colon = host.lastIndex(of: ":"), !host[colon...].contains("]") {
            port = String(host[host.index(after: colon)...])
            host = String(host[..<colon])
        }
        guard ShellSettings.isThisComputer(host),
              port.map({ !$0.isEmpty && $0.allSatisfy { ("0"..."9").contains($0) } }) ?? true
        else { return route }
        let kept = port.flatMap { $0 == "443" ? nil : ":\($0)" } ?? ""
        return "/@\(name)\(kept)\(rest[end...])"
    }

    /// The port an address is reached on: the one it names, or its scheme's own.
    private static func port(of parts: URLComponents) -> Int {
        parts.port ?? (parts.scheme?.lowercased() == "https" ? 443 : 80)
    }

    /// `scheme://host[:port]` of an http address, with the standard port left out.
    public static func origin(of url: URL) -> String? {
        guard let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http",
              let host = url.host(percentEncoded: false)?.lowercased(), !host.isEmpty
        else { return nil }
        let standard = scheme == "https" ? 443 : 80
        let name = host.contains(":") ? "[\(host)]" : host
        guard let port = url.port, port != standard else { return "\(scheme)://\(name)" }
        return "\(scheme)://\(name):\(port)"
    }
}
