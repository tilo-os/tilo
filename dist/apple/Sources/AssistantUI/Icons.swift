import SwiftUI

/// The drawing's own signs, as it draws them: sixteen units square, a line one and a half
/// thick with round ends. Each is the drawing's path, read as written.
public enum Sign: String, CaseIterable, Sendable {
    case side, search, panel, new, plus, x, mic, ret, check, ask, eye, clock, bolt, lock, into, run, stop, back
    /// The main chat's own sign: a speech bubble. Its three dots are drawn over it (`ChatSign`).
    case chat

    enum Part {
        case path(String)
        case rect(CGRect, CGFloat)
        case circle(CGPoint, CGFloat)
    }

    var parts: [Part] {
        switch self {
        case .side: [.rect(CGRect(x: 2.5, y: 3.5, width: 11, height: 9), 2), .path("M6 3.5v9")]
        case .search: [.circle(CGPoint(x: 7, y: 7), 4), .path("M10 10l3.2 3.2")]
        case .panel: [.path("M3 4.6l1.2 1.2 2.2-2.2M9 4.7h4M3 10.6l1.2 1.2 2.2-2.2M9 10.7h4")]
        case .new: [.path("M8 3H4.5A1.5 1.5 0 0 0 3 4.5v7A1.5 1.5 0 0 0 4.5 13h7a1.5 1.5 0 0 0 1.5-1.5V8"),
                    .path("M11.8 2.6l1.6 1.6-4.9 4.9-2 .4.4-2z")]
        case .plus: [.path("M8 3.5v9M3.5 8h9")]
        case .x: [.path("M4 4l8 8M12 4l-8 8")]
        case .mic: [.rect(CGRect(x: 6, y: 2.5, width: 4, height: 7), 2), .path("M4 8a4 4 0 0 0 8 0M8 12v2")]
        case .ret: [.path("M12.5 4v4.5h-8M7 6L4.5 8.5 7 11")]
        case .check: [.circle(CGPoint(x: 8, y: 8), 5.5), .path("M5.6 8.2l1.6 1.6 3.2-3.4")]
        case .ask: [.circle(CGPoint(x: 8, y: 8), 5.5), .path("M6.6 6.5a1.5 1.5 0 1 1 2.2 1.3c-.5.3-.8.6-.8 1.1M8 11v.2")]
        case .eye: [.path("M2 8s2.2-4 6-4 6 4 6 4-2.2 4-6 4-6-4-6-4z"), .circle(CGPoint(x: 8, y: 8), 1.6)]
        case .clock: [.circle(CGPoint(x: 8, y: 8), 5.5), .path("M8 5v3.2l2 1.2")]
        case .bolt: [.path("M8.8 2.5L4 9h3.2l-.6 4.5L11.5 7H8.2z")]
        case .lock: [.rect(CGRect(x: 4, y: 7.5, width: 8, height: 6), 1.5), .path("M5.8 7.5V5.6a2.2 2.2 0 0 1 4.4 0v1.9")]
        case .into: [.path("M4 3v4a3 3 0 0 0 3 3h5M9.5 7.5L12 10l-2.5 2.5")]
        case .run: [.circle(CGPoint(x: 8, y: 8), 5.5)]
        // Two the drawing has no use for and an app does, drawn in its hand.
        case .stop: [.rect(CGRect(x: 4.5, y: 4.5, width: 7, height: 7), 1.5)]
        case .back: [.path("M9.5 3.5L5 8l4.5 4.5")]
        // The main chat's, in the same hand: a bubble with its tail at the lower left.
        case .chat: [.path("M4.5 2.75h7a2 2 0 0 1 2 2v4.5a2 2 0 0 1-2 2H7.4l-2.9 2.3v-2.3a2 2 0 0 1-2-2v-4.5a2 2 0 0 1 2-2z")]
        }
    }

    /// The sign's lines in a square of this side.
    func path(side: CGFloat) -> Path {
        var path = Path()
        for part in parts {
            switch part {
            case .path(let written): path.addPath(SVGPath.path(written))
            case .rect(let rect, let radius): path.addRoundedRect(in: rect, cornerSize: CGSize(width: radius, height: radius))
            case .circle(let centre, let radius):
                path.addEllipse(in: CGRect(x: centre.x - radius, y: centre.y - radius, width: radius * 2, height: radius * 2))
            }
        }
        return path.applying(CGAffineTransform(scaleX: side / 16, y: side / 16))
    }
}

/// One of the drawing's signs, in the colour of the words around it.
public struct Glyph: View {
    private let sign: Sign
    private let side: CGFloat
    private let grows: Bool
    /// How much larger the person's text is than the drawing's.
    @ScaledMetric(relativeTo: .body) private var grown: CGFloat = 1

    /// `grows`: whether the sign grows with the person's text size, up to twice. In a bar of the
    /// system's it does not: the bar keeps its own height.
    public init(_ sign: Sign, side: CGFloat = 16, grows: Bool = true) {
        self.sign = sign
        self.side = side
        self.grows = grows
    }

    public var body: some View {
        let side = side * (grows ? min(max(grown, 1), 2) : 1)
        let scale = side / 16
        sign.path(side: side)
            .stroke(style: StrokeStyle(lineWidth: 1.5 * scale, lineCap: .round, lineJoin: .round,
                                       dash: sign == .run ? [3 * scale, 3 * scale] : []))
            .frame(width: side, height: side)
            .accessibilityHidden(true)
    }
}

/// Tilo's mark in the assistant's colours: the middle bar strong, its two sides quiet.
public struct Mark: View {
    private let side: CGFloat

    public init(side: CGFloat = 18) {
        self.side = side
    }

    public var body: some View {
        let scale = CGAffineTransform(scaleX: side / 100, y: side / 100)
        ZStack {
            SVGPath.path("M10.98 31.85Q10.98 27.97 14.4 26.12L20.26 22.97Q24.84 20.51 24.84 25.71L24.84 74.29Q24.84 79.49 20.26 77.03L14.4 73.88Q10.98 72.03 10.98 68.15Z")
                .applying(scale).fill(Token.asstQuiet)
            SVGPath.path("M89.02 31.85Q89.02 27.97 85.6 26.12L79.74 22.97Q75.16 20.51 75.16 25.71L75.16 74.29Q75.16 79.49 79.74 77.03L85.6 73.88Q89.02 72.03 89.02 68.15Z")
                .applying(scale).fill(Token.asstQuiet)
            Path(roundedRect: CGRect(x: 29, y: 16, width: 42, height: 68), cornerRadius: 6)
                .applying(scale).fill(Token.asst)
        }
        .frame(width: side, height: side)
        .accessibilityHidden(true)
    }
}

/// How the three dots of a sign that says "writing" move: one after the other rises and falls,
/// then all three are at rest, and after `Motion.wave` it begins again.
public enum Wave {
    /// How long after the dot before it a dot begins to rise, as a share of the wave.
    static let step = 0.16
    /// How long one dot takes to rise and fall, as a share of the wave.
    static let rise = 0.44

    /// How high each of the three dots is at a moment: 0 on its line, 1 at its highest.
    public static func lifts(at moment: TimeInterval) -> [Double] {
        let turn = moment / Motion.wave
        let part = turn - turn.rounded(.down)
        return (0..<3).map { dot in
            let begun = part - Double(dot) * step
            guard begun > 0, begun < rise else { return 0 }
            return sin(.pi * begun / rise)
        }
    }
}

/// Three dots in a line. While someone `writes` they are in the assistant's colour and rise and
/// fall one after the other; where the system asks for reduced motion they are in that colour
/// and do not move. Otherwise they are at rest, in the colour of the words around them.
struct WritingDots: View {
    let writes: Bool
    /// A dot's width, how far apart the middles of two dots are, and how high a dot rises.
    let dot: CGFloat
    let pitch: CGFloat
    let lift: CGFloat
    @Environment(\.accessibilityReduceMotion) private var reduced

    var body: some View {
        if writes, !reduced {
            TimelineView(.animation) { moment in
                dots(Wave.lifts(at: moment.date.timeIntervalSinceReferenceDate)).foregroundStyle(Token.asst)
            }
        } else if writes {
            dots([0, 0, 0]).foregroundStyle(Token.asst)
        } else {
            dots([0, 0, 0])
        }
    }

    private func dots(_ lifts: [Double]) -> some View {
        HStack(spacing: pitch - dot) {
            ForEach(0..<3, id: \.self) { index in
                Circle().frame(width: dot, height: dot).offset(y: -lift * lifts[index])
            }
        }
        .accessibilityHidden(true)
    }
}

/// The main chat's sign: a speech bubble with three dots, in the colour of the words around
/// it. While the assistant `writes` there, its dots say so (`WritingDots`).
public struct ChatSign: View {
    private let writes: Bool
    private let side: CGFloat

    public init(writes: Bool = false, side: CGFloat = 18) {
        self.writes = writes
        self.side = side
    }

    public var body: some View {
        let scale = side / 16
        ZStack {
            Sign.chat.path(side: side)
                .stroke(style: StrokeStyle(lineWidth: 1.5 * scale, lineCap: .round, lineJoin: .round))
            // The dots' line is the middle of the bubble, one unit above the sign's own middle.
            WritingDots(writes: writes, dot: 1.6 * scale, pitch: 2.6 * scale, lift: 1.5 * scale)
                .offset(y: -scale)
        }
        .frame(width: side, height: side)
        .accessibilityHidden(true)
    }
}

/// A path as a drawing writes it: the letters M L H V C S Q A Z, large and small.
enum SVGPath {
    static func path(_ written: String) -> Path {
        var path = Path()
        var reader = Reader(written)
        var command: Character = " "
        var current = CGPoint.zero
        var start = CGPoint.zero
        var lastControl: CGPoint?
        while true {
            if let next = reader.command() {
                command = next
            } else if !reader.hasNumber {
                break
            }
            let relative = command.isLowercase
            func point() -> CGPoint {
                let x = reader.number(), y = reader.number()
                return relative ? CGPoint(x: current.x + x, y: current.y + y) : CGPoint(x: x, y: y)
            }
            var control: CGPoint?
            switch command {
            case "M", "m":
                current = point()
                start = current
                path.move(to: current)
                command = relative ? "l" : "L"
            case "L", "l":
                current = point()
                path.addLine(to: current)
            case "H", "h":
                let x = reader.number()
                current.x = relative ? current.x + x : x
                path.addLine(to: current)
            case "V", "v":
                let y = reader.number()
                current.y = relative ? current.y + y : y
                path.addLine(to: current)
            case "C", "c":
                let first = point(), second = point(), end = point()
                path.addCurve(to: end, control1: first, control2: second)
                control = second
                current = end
            case "S", "s":
                let first = lastControl.map { CGPoint(x: 2 * current.x - $0.x, y: 2 * current.y - $0.y) } ?? current
                let second = point(), end = point()
                path.addCurve(to: end, control1: first, control2: second)
                control = second
                current = end
            case "Q", "q":
                let bend = point(), end = point()
                path.addQuadCurve(to: end, control: bend)
                current = end
            case "A", "a":
                let rx = reader.number(), ry = reader.number(), turn = reader.number()
                let large = reader.flag(), sweep = reader.flag()
                let end = point()
                arc(&path, from: current, to: end, rx: rx, ry: ry, turn: turn, large: large, sweep: sweep)
                current = end
            case "Z", "z":
                path.closeSubpath()
                current = start
                command = " "
            default:
                return path
            }
            lastControl = control
        }
        return path
    }

    /// An arc of an ellipse between two points, as cubic curves of a quarter turn at most.
    private static func arc(_ path: inout Path, from: CGPoint, to: CGPoint, rx: CGFloat, ry: CGFloat, turn: CGFloat,
                            large: Bool, sweep: Bool) {
        var rx = abs(rx), ry = abs(ry)
        guard rx > 0, ry > 0, from != to else {
            path.addLine(to: to)
            return
        }
        let phi = turn * .pi / 180
        let cosPhi = cos(phi), sinPhi = sin(phi)
        let dx = (from.x - to.x) / 2, dy = (from.y - to.y) / 2
        let x1 = cosPhi * dx + sinPhi * dy, y1 = -sinPhi * dx + cosPhi * dy
        let reach = x1 * x1 / (rx * rx) + y1 * y1 / (ry * ry)
        if reach > 1 {
            rx *= sqrt(reach)
            ry *= sqrt(reach)
        }
        let top = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1
        let bottom = rx * rx * y1 * y1 + ry * ry * x1 * x1
        let factor = (large != sweep ? 1 : -1) * sqrt(max(0, top / bottom))
        let cx1 = factor * rx * y1 / ry, cy1 = -factor * ry * x1 / rx
        let cx = cosPhi * cx1 - sinPhi * cy1 + (from.x + to.x) / 2
        let cy = sinPhi * cx1 + cosPhi * cy1 + (from.y + to.y) / 2
        func angle(_ ux: CGFloat, _ uy: CGFloat, _ vx: CGFloat, _ vy: CGFloat) -> CGFloat {
            atan2(ux * vy - uy * vx, ux * vx + uy * vy)
        }
        let ux = (x1 - cx1) / rx, uy = (y1 - cy1) / ry
        let first = angle(1, 0, ux, uy)
        var sweepAngle = angle(ux, uy, (-x1 - cx1) / rx, (-y1 - cy1) / ry)
        if !sweep, sweepAngle > 0 { sweepAngle -= 2 * .pi }
        if sweep, sweepAngle < 0 { sweepAngle += 2 * .pi }
        let pieces = max(1, Int(ceil(abs(sweepAngle) / (.pi / 2) - 0.0001)))
        let step = sweepAngle / CGFloat(pieces)
        let handle = 4 / 3 * tan(step / 4)
        func at(_ a: CGFloat) -> CGPoint {
            CGPoint(x: cx + rx * cos(a) * cosPhi - ry * sin(a) * sinPhi, y: cy + rx * cos(a) * sinPhi + ry * sin(a) * cosPhi)
        }
        func slope(_ a: CGFloat) -> CGPoint {
            CGPoint(x: -rx * sin(a) * cosPhi - ry * cos(a) * sinPhi, y: -rx * sin(a) * sinPhi + ry * cos(a) * cosPhi)
        }
        for piece in 0..<pieces {
            let a = first + CGFloat(piece) * step, b = a + step
            let p1 = at(a), p2 = at(b), d1 = slope(a), d2 = slope(b)
            path.addCurve(to: p2, control1: CGPoint(x: p1.x + handle * d1.x, y: p1.y + handle * d1.y),
                          control2: CGPoint(x: p2.x - handle * d2.x, y: p2.y - handle * d2.y))
        }
    }

    private struct Reader {
        private let letters: [Character]
        private var index = 0

        init(_ written: String) {
            letters = Array(written)
        }

        private mutating func skip() {
            while index < letters.count, letters[index] == " " || letters[index] == "," || letters[index].isNewline { index += 1 }
        }

        mutating func command() -> Character? {
            skip()
            guard index < letters.count, letters[index].isLetter else { return nil }
            defer { index += 1 }
            return letters[index]
        }

        var hasNumber: Bool {
            mutating get {
                skip()
                guard index < letters.count else { return false }
                let letter = letters[index]
                return letter.isNumber || letter == "-" || letter == "+" || letter == "."
            }
        }

        mutating func number() -> CGFloat {
            skip()
            var digits = ""
            if index < letters.count, letters[index] == "-" || letters[index] == "+" {
                digits.append(letters[index])
                index += 1
            }
            var dotted = false
            while index < letters.count {
                let letter = letters[index]
                if letter.isNumber {
                    digits.append(letter)
                } else if letter == ".", !dotted {
                    dotted = true
                    digits.append(letter)
                } else {
                    break
                }
                index += 1
            }
            return CGFloat(Double(digits) ?? 0)
        }

        /// An arc's flag: one digit, which may stand right against the next number.
        mutating func flag() -> Bool {
            skip()
            guard index < letters.count else { return false }
            defer { index += 1 }
            return letters[index] == "1"
        }
    }
}
