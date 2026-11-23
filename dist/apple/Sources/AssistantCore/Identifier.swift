import Foundation

/// The name a thing has in the source it came from. Only that source reads what is inside:
/// the views pass it back and compare it, nothing more.
public struct Identifier<Of>: Hashable, Sendable, Codable, ExpressibleByStringLiteral, CustomStringConvertible {
    public let value: String

    public init(_ value: String) {
        self.value = value
    }

    public init(stringLiteral value: String) {
        self.value = value
    }

    public init(from decoder: any Decoder) throws {
        value = try decoder.singleValueContainer().decode(String.self)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(value)
    }

    public var description: String { value }
}
