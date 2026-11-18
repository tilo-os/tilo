import Foundation

extension Attachment {
    /// What a file's tag says: the name it came under and, where its source says how large it
    /// is, its size as the system writes a file's size. Both are the source's own: nothing here
    /// measures a file.
    public func words(locale: Locale = .autoupdatingCurrent) -> String {
        guard let bytes, bytes >= 0 else { return name }
        let size = Int64(bytes).formatted(
            .byteCount(style: .file, allowedUnits: .all, spellsOutZero: false, includesActualByteCount: false).locale(locale))
        return "\(name) · \(size)"
    }
}
