import Foundation

/// A file a message carries, as Chat keeps it among what the message carries beside its words
/// (`attachments`): a file, a picture or a recording, by the name it was posted under and by
/// what the workspace keeps its content under. Chat measured it where it was posted: nothing
/// here measures, opens or fetches anything.
public struct ChatFile: Sendable, Equatable {
    public enum Kind: String, Sendable, CaseIterable {
        case file, image, video
    }

    public let kind: Kind
    /// Where it stands among what its message carries, in Chat's own order. With its message's
    /// item this names it for as long as the message is there.
    public let index: Int
    /// The name it was posted under, as Chat gives it.
    public let name: String
    /// What the workspace keeps its content under.
    public let content: String
    /// Its length in bytes, where Chat says one. It says none for a picture.
    public let bytes: Int?

    public init(kind: Kind, index: Int, name: String, content: String, bytes: Int? = nil) {
        self.kind = kind
        self.index = index
        self.name = name
        self.content = content
        self.bytes = bytes
    }

    /// One of the things a message carries, when it is a file. Chat's own document and its
    /// actions spell the kind as a field (`{"kind": "file", …}`); a read may spell an enum's
    /// case as `{"$variant": "file", …}`, or as the one name its fields stand under
    /// (`{"file": {…}}`). A thing with no name, or with nothing its content is kept under, is
    /// none Chat would have posted, and is no file here.
    init?(_ carried: JSON, at index: Int) {
        var kind: Kind?
        var fields = carried
        if let named = (carried["kind"] ?? carried["$variant"])?.string {
            kind = Kind(rawValue: named)
        } else if let only = carried.object, only.count == 1, let (named, inner) = only.first {
            kind = Kind(rawValue: named)
            fields = inner
        }
        guard let kind, let name = fields["name"]?.string, !name.isEmpty,
              let content = fields["blob"]?.string, !content.isEmpty
        else { return nil }
        // A length is a whole number of bytes and not below nothing. Anything else is no length.
        let size = fields["size"]?.number.flatMap { $0 >= 0 && $0 <= 9_007_199_254_740_992 && $0.rounded() == $0 ? Int($0) : nil }
        self.init(kind: kind, index: index, name: name, content: content, bytes: size)
    }
}

extension ChatMessage {
    /// The files this message carries, in Chat's order. What it carries otherwise (something a
    /// document keeps, words quoted from one) is no file.
    public var files: [ChatFile] {
        (fields["attachments"]?.array ?? []).enumerated().compactMap { ChatFile($0.element, at: $0.offset) }
    }
}
