import Foundation

/// A value as Tilo's page hands it over: what a resource reads as, what an action takes and
/// what it answers.
public enum JSON: Sendable, Equatable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSON])
    case object([String: JSON])

    /// From what `JSONSerialization` reads. Nil for anything that is no such value.
    public init?(any value: Any) {
        switch value {
        case is NSNull:
            self = .null
        case let number as NSNumber:
            // A Boolean is a number to Foundation: its own type tells the two apart.
            self = CFGetTypeID(number) == CFBooleanGetTypeID() ? .bool(number.boolValue) : .number(number.doubleValue)
        case let text as String:
            self = .string(text)
        case let list as [Any]:
            var items: [JSON] = []
            for item in list {
                guard let read = JSON(any: item) else { return nil }
                items.append(read)
            }
            self = .array(items)
        case let fields as [String: Any]:
            var read: [String: JSON] = [:]
            for (name, field) in fields {
                guard let value = JSON(any: field) else { return nil }
                read[name] = value
            }
            self = .object(read)
        default:
            return nil
        }
    }

    /// From JSON text. Nil when it is none.
    public init?(text: String) {
        guard let data = text.data(using: .utf8),
              let value = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed])
        else { return nil }
        self.init(any: value)
    }

    /// As `JSONSerialization` and a web view's arguments take it.
    public var any: Any {
        switch self {
        case .null: NSNull()
        case .bool(let value): value
        case .number(let value): value
        case .string(let value): value
        case .array(let items): items.map(\.any)
        case .object(let fields): fields.mapValues(\.any)
        }
    }

    /// As JSON text, with its fields in one order every time.
    public var text: String {
        let data = try? JSONSerialization.data(withJSONObject: any, options: [.fragmentsAllowed, .sortedKeys, .withoutEscapingSlashes])
        return data.flatMap { String(data: $0, encoding: .utf8) } ?? "null"
    }

    public subscript(name: String) -> JSON? {
        if case .object(let fields) = self { fields[name] } else { nil }
    }

    public subscript(index: Int) -> JSON? {
        if case .array(let items) = self, items.indices.contains(index) { items[index] } else { nil }
    }

    public var string: String? { if case .string(let value) = self { value } else { nil } }
    public var number: Double? { if case .number(let value) = self { value } else { nil } }
    public var bool: Bool? { if case .bool(let value) = self { value } else { nil } }
    public var array: [JSON]? { if case .array(let items) = self { items } else { nil } }
    public var object: [String: JSON]? { if case .object(let fields) = self { fields } else { nil } }
    public var isNull: Bool { self == .null }
}

extension JSON: ExpressibleByStringLiteral, ExpressibleByBooleanLiteral, ExpressibleByIntegerLiteral,
    ExpressibleByFloatLiteral, ExpressibleByArrayLiteral, ExpressibleByDictionaryLiteral, ExpressibleByNilLiteral {
    public init(stringLiteral value: String) { self = .string(value) }
    public init(booleanLiteral value: Bool) { self = .bool(value) }
    public init(integerLiteral value: Int) { self = .number(Double(value)) }
    public init(floatLiteral value: Double) { self = .number(value) }
    public init(arrayLiteral elements: JSON...) { self = .array(elements) }
    public init(dictionaryLiteral elements: (String, JSON)...) {
        self = .object(Dictionary(elements, uniquingKeysWith: { _, last in last }))
    }
    public init(nilLiteral: ()) { self = .null }
}
