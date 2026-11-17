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

    /// The link in `text` when it is one into this site: what a person pasted or shared to
    /// get in (an invitation, a pairing link) or to open a document.
    public func link(in text: String) -> URL? {
        let written = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !written.contains(where: \.isWhitespace), let url = URL(string: written),
              url.scheme == "https" || url.scheme == "http",
              SitePolicy.origin(of: url) == origin
        else { return nil }
        return url
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
