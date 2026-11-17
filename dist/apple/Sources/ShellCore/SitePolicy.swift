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

    public init(settings: ShellSettings) {
        origin = settings.origin
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
