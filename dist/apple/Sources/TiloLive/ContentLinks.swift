import Foundation

/// A link to a file's content.
///
/// Tilo's page hands a native app that holds it as its person's device a link to content the
/// workspace keeps: an address on the page's own site that serves the content for a short
/// time, minted by the workspace's server where this person may read it. The ask goes through
/// the session the page holds, and the engine's script takes it only with the pass this
/// engine made. The link is handed to whoever asked and to nobody else: the engine keeps
/// none, says none to a log and opens none itself.
extension TiloEngine {
    /// Whether this page hands the app a link to a file's content. The page of a site whose
    /// code is older hands none, and neither does a page that hands a native app nothing at
    /// all. Nobody's server is asked.
    public func linkIsOffered() async throws -> Bool {
        do {
            _ = try await ask("return globalThis.__tiloLive.link(given, null)", ["given": pass])
            return true
        } catch Problem.refused(code: "unknown_action", reason: _) {
            return false
        } catch Problem.refused(code: "unavailable", reason: _) {
            return false
        }
    }

    /// A link to `content`, what the workspace keeps a file under, to fetch under `name`: an
    /// address on this engine's own site, good for a short time.
    ///
    /// - Throws: `Problem.refused` with the server's own code and sentence where it mints
    ///   none, and with `outside` for an answer that is no path on this engine's site: such a
    ///   link is handed to nobody.
    public func contentLink(to content: String, named name: String) async throws -> URL {
        let address = content.hasPrefix("tilo.content:") ? content : "tilo.content:\(content)"
        let answer = try await ask("return globalThis.__tiloLive.link(given, of)",
                                   ["given": pass, "of": ["content": address, "name": name]])
        guard let path = answer["path"]?.string, let link = TiloEngine.link(on: settings.origin, at: path) else {
            throw Problem.refused(code: "outside", reason: "The link to this file does not lead into this Tilo site")
        }
        return link
    }

    /// The address `path` names on the site at `origin`. Nil where it is no path of that
    /// site: one that names another site or a scheme, or nothing at all.
    static func link(on origin: String, at path: String) -> URL? {
        guard path.hasPrefix("/"), !path.hasPrefix("//"), !path.contains("\\"),
              !path.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }),
              let site = URL(string: origin), let link = URL(string: origin + path),
              // The site's own up to the path: its scheme, its name and its port as they are
              // written, and nobody to sign in as.
              link.absoluteString.hasPrefix(origin + "/"), link.scheme == site.scheme, link.port == site.port,
              link.user == nil, link.password == nil
        else { return nil }
        return link
    }
}
