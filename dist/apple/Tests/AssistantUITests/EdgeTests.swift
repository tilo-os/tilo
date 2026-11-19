#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// Where a conversation ends against the bar above it and the field below: drawn, and read
/// pixel by pixel. Nothing is taken from a screen.
@MainActor
final class EdgeTests: XCTestCase {
    private struct Picture {
        let pixels: NSBitmapImageRep
        let scale: CGFloat

        /// A pixel's red, green and blue as numbers from 0 to 255, at a place in points from the top left.
        func colour(_ x: CGFloat, _ y: CGFloat) -> [Int] {
            guard let colour = pixels.colorAt(x: Int(x * scale), y: Int(y * scale)), colour.numberOfComponents >= 3 else { return [-1, -1, -1] }
            var parts = [CGFloat](repeating: 0, count: colour.numberOfComponents)
            colour.getComponents(&parts)
            return parts.prefix(3).map { Int(($0 * 255).rounded()) }
        }
    }

    private static let size = CGSize(width: 429, height: 320)
    /// A place inside a bubble's left padding: its fill, and never a letter.
    private static let inBubble = Measure.columnGutter + 7
    /// A place beside the bubbles: the ground.
    private static let beside: CGFloat = 6

    private func far(_ one: [Int], _ other: [Int]) -> Int {
        zip(one, other).map { abs($0 - $1) }.max() ?? 255
    }

    /// A main chat whose one piece is a reply far taller than the window, as the app draws it:
    /// the reply's bubble crosses the conversation's upper end, and its foot stands at rest.
    private func drawn(_ scheme: ColorScheme) async -> Picture {
        let size = EdgeTests.size
        var content = Drawing.content()
        let lines = (1...40).map { "Line \($0) of a reply that is far longer than the window is high." }
        content.main = [Item(id: "long", at: Drawing.now, .message(Message(.assistant, lines.joined(separator: "\n\n"))))]
        content.tasks = []
        let model = AssistantModel(source: MemorySource(content, now: { Drawing.now }), snapshots: cacheOfTheCheck())
        await model.prime()
        let view = AssistantRoot(model: model) {
            ConversationView(chat: .main).background(Token.content)
        }
        .environment(\.colorScheme, scheme)
        .frame(width: size.width, height: size.height)
        let hosting = NSHostingView(rootView: view)
        hosting.frame = CGRect(origin: .zero, size: size)
        let window = HeldWindow.make(hosting.frame, [.borderless])
        let look = NSAppearance(named: scheme == .dark ? .darkAqua : .aqua)
        window.appearance = look
        hosting.appearance = look
        window.contentView = hosting
        defer {
            window.contentView = nil
            window.close()
        }
        await Drive(window).settle(400)
        guard let pixels = hosting.bitmapImageRepForCachingDisplay(in: hosting.bounds) else {
            XCTFail("the window gave no picture of itself")
            return Picture(pixels: NSBitmapImageRep(), scale: 1)
        }
        hosting.cacheDisplay(in: hosting.bounds, to: pixels)
        if let folder = ProcessInfo.processInfo.environment["TILO_SHOTS_DIR"] {
            try? FileManager.default.createDirectory(atPath: folder, withIntermediateDirectories: true)
            let name = "conversation-ends-\(scheme == .dark ? "dark" : "light").png"
            try? pixels.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: folder).appendingPathComponent(name))
        }
        return Picture(pixels: pixels, scale: CGFloat(pixels.pixelsWide) / size.width)
    }

    /// A piece that scrolls out under the bar is not cut flat, its round corners made square: at
    /// the conversation's upper end stands the ground alone, and the bubble comes out of it by
    /// degrees. Colours are read from the picture itself: the ground beside the bubble, the
    /// bubble's own in its middle.
    ///
    /// Tolerances, in steps of 255 for each of red, green and blue: at the very end the bubble's
    /// place may differ from the ground by 3 (a bubble differs from the ground by 13 in the dark
    /// theme and by 14 in the light); under the fade it is the bubble's own colour to 1.
    func testAPieceThatScrollsOutUnderTheBarIsNotCutFlat() async {
        for scheme in [ColorScheme.dark, .light] {
            let picture = await drawn(scheme)
            let x = EdgeTests.inBubble, middle = EdgeTests.size.height / 2
            let ground = picture.colour(EdgeTests.beside, middle), bubble = picture.colour(x, middle)
            let whole = far(bubble, ground)
            XCTAssertGreaterThanOrEqual(whole, 8, "\(scheme): the bubble \(bubble) does not stand out from the ground \(ground)")
            XCTAssertLessThanOrEqual(far(picture.colour(x, Measure.edgeFade + 2), bubble), 1,
                                     "\(scheme): the bubble crosses the upper end, and under the fade it is whole")
            let end = picture.colour(x, 0.25)
            XCTAssertLessThanOrEqual(far(end, ground), 3, "\(scheme): at the upper end the bubble is cut flat: \(end), the ground is \(ground)")
            // Halfway up it is half there: strictly between its own colour and the ground's.
            let half = picture.colour(x, Measure.edgeFade / 2)
            XCTAssertGreaterThan(far(half, ground), whole / 5, "\(scheme): halfway up the fade the bubble is gone already: \(half)")
            XCTAssertGreaterThan(far(half, bubble), whole / 5, "\(scheme): halfway up the fade the bubble is whole still: \(half)")
        }
    }

    /// At rest nothing stands under the lower fade: what stays free under the last piece is as
    /// deep as the fade, so the last piece keeps its own colour down to its foot.
    func testAtRestTheLastPieceKeepsItsColourToItsFoot() async {
        XCTAssertEqual(Measure.edgeFade, Measure.columnFoot)
        for scheme in [ColorScheme.dark, .light] {
            let picture = await drawn(scheme)
            let middle = EdgeTests.size.height / 2
            let ground = picture.colour(EdgeTests.beside, middle), bubble = picture.colour(EdgeTests.inBubble, middle)
            let foot = EdgeTests.size.height - Measure.columnFoot
            XCTAssertLessThanOrEqual(far(picture.colour(120, foot - 1.5), bubble), 1, "\(scheme): the last piece's foot is \(picture.colour(120, foot - 1.5))")
            XCTAssertLessThanOrEqual(far(picture.colour(120, foot + 2), ground), 1, "\(scheme): under the last piece stands \(picture.colour(120, foot + 2))")
        }
    }
}
#endif
