import Foundation

/// What the page says needs the person while they are elsewhere.
///
/// The page writes it into its title for a browser's tab ("(3) Tilo · Lounge"); the shell
/// reads the same title for the menu-bar item, the Dock and the app's icon.
public struct Attention: Equatable, Sendable {
    /// Notices the person has not seen. The page stops counting at 99.
    public let unread: Int
    /// Whether the page counted past what it shows ("99+").
    public let more: Bool
    /// The title without its count.
    public let line: String

    public init(title: String?) {
        let title = title ?? ""
        if let counted = title.wholeMatch(of: /\((\d{1,4})(\+?)\)\s+(.*)/), let unread = Int(counted.1) {
            self.unread = unread
            more = !counted.2.isEmpty
            line = String(counted.3)
        } else {
            unread = 0
            more = false
            line = title
        }
    }

    /// What a badge shows: the count, or nothing when nothing waits.
    public var badge: String? {
        unread > 0 ? "\(unread)\(more ? "+" : "")" : nil
    }
}
