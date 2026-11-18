import Foundation

/// A file's content on its way into the person's workspace.
///
/// Tilo's page hands a native app that holds it as its person's device a way to put one file
/// into the content store of the workspace the page is in: `begin` with the file's size,
/// `append` a piece at the offset the piece before it ended, `finish` for what the workspace
/// keeps the file under. Every step goes through the session the page holds, as the person on
/// this device, and the engine's script takes a step only with the pass this engine made.
/// A file is never whole in the page or in this app: whoever sends one reads it a piece at a
/// time. Nothing of a piece is kept here or said anywhere.
extension TiloEngine {
    /// The most bytes one piece carries: what one frame of the page's session takes.
    public static let mostPieceBytes = 384 * 1024

    /// How long one step may go unanswered: a piece crosses to the person's server.
    private static let putPatience = Duration.seconds(60)

    private func put(_ step: String, _ of: [String: Any] = [:]) async throws -> JSON {
        try await ask("return globalThis.__tiloLive.put(given, step, of)", patience: TiloEngine.putPatience,
                      ["given": pass, "step": step, "of": of])
    }

    /// Whether this page hands the app a way to put a file into its workspace. The page of a
    /// site whose code is older hands none, and neither does a page that hands a native app
    /// nothing at all. Nobody's server is asked.
    public func putIsOffered() async throws -> Bool {
        do {
            _ = try await put("offered")
            return true
        } catch Problem.refused(code: "unknown_action", reason: _) {
            return false
        } catch Problem.refused(code: "unavailable", reason: _) {
            return false
        }
    }

    /// Begin one file of `size` bytes and answer what its pieces are appended to. A file
    /// larger than a message may carry is refused here, by its size alone, with the page's
    /// own sentence (`invalid_request`): none of it crossed.
    public func putBegin(size: Int) async throws -> Int {
        guard let upload = try await put("begin", ["size": size])["upload"]?.number else { throw Problem.noAnswer }
        return Int(upload)
    }

    /// Append one piece, `mostPieceBytes` at most, at the offset the piece before it ended.
    public func putAppend(_ piece: Data, to upload: Int, at offset: Int) async throws {
        let answer = try await put("append", ["upload": upload, "offset": offset, "piece": piece.base64EncodedString()])
        guard answer["written"]?.number != nil else { throw Problem.noAnswer }
    }

    /// End the file and answer what the workspace keeps it under: the name alone, as a
    /// message names a file it carries (the page says it as `tilo.content:<name>`).
    public func putFinish(_ upload: Int) async throws -> String {
        guard let content = try await put("finish", ["upload": upload])["content"]?.string else { throw Problem.noAnswer }
        let address = "tilo.content:"
        let kept = content.hasPrefix(address) ? String(content.dropFirst(address.count)) : content
        guard !kept.isEmpty else { throw Problem.noAnswer }
        return kept
    }
}
