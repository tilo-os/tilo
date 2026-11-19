#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// The frame draws in the look the person's Tilo has, light or dark, and follows it as it
/// changes, whatever the system's look is: drawn, and read pixel by pixel. Nothing is taken
/// from a screen.
@MainActor
final class FrameLookTests: XCTestCase {
    private static let size = CGSize(width: 429, height: 320)
    /// A place beside the bubbles: the ground.
    private static let beside: CGFloat = 6
    /// A place inside a bubble's left padding: its fill, and never a letter.
    private static let inBubble = Measure.columnGutter + 7

    /// The look that was given to what stands where an app puts its engine's view.
    private final class Seen {
        var scheme: ColorScheme?
    }

    /// What stands where an app puts its engine's view, and writes down the look it is given.
    private struct EnginePlace: View {
        let seen: Seen
        @Environment(\.colorScheme) private var scheme

        var body: some View {
            let _ = (seen.scheme = scheme)
            Color.clear
        }
    }

    /// A window that holds the root as an app makes it, around a conversation with one long reply.
    private struct Held {
        let window: HeldWindow
        let hosting: NSHostingView<AnyView>
        let source: MemorySource
        let seen: Seen
    }

    /// The ground beside the bubbles and a bubble's own fill, each as red, green and blue from 0 to 255.
    private struct Colours {
        let ground: [Int]
        let bubble: [Int]
    }

    private func far(_ one: [Int], _ other: [Int]) -> Int {
        zip(one, other).map { abs($0 - $1) }.max() ?? 255
    }

    /// The root in a window whose system draws in `system`, over a source whose Tilo says `look`.
    private func stand(system: ColorScheme, look: Look?) async -> Held {
        let size = FrameLookTests.size
        var content = Drawing.content()
        let lines = (1...40).map { "Line \($0) of a reply that is far longer than the window is high." }
        content.main = [Item(id: "long", at: Drawing.now, .message(Message(.assistant, lines.joined(separator: "\n\n"))))]
        content.tasks = []
        content.look = look
        let source = MemorySource(content, now: { Drawing.now })
        let model = AssistantModel(source: source, snapshots: cacheOfTheCheck())
        await model.prime()
        model.start()
        let seen = Seen()
        let view = AssistantRoot(model: model, engine: AnyView(EnginePlace(seen: seen))) {
            ConversationView(chat: .main).background(Token.content)
        }
        .environment(\.colorScheme, system)
        .frame(width: size.width, height: size.height)
        let hosting = NSHostingView(rootView: AnyView(view))
        hosting.frame = CGRect(origin: .zero, size: size)
        let window = HeldWindow.make(hosting.frame, [.borderless])
        let appearance = NSAppearance(named: system == .dark ? .darkAqua : .aqua)
        window.appearance = appearance
        hosting.appearance = appearance
        window.contentView = hosting
        return Held(window: window, hosting: hosting, source: source, seen: seen)
    }

    /// What the window draws now. The picture is kept where pictures are asked for.
    private func drawn(_ held: Held, kept name: String) async -> Colours {
        // Twice: the first drawing begins what eases, the second finds it at rest.
        let drive = Drive(held.window)
        await drive.settle(400)
        await drive.settle(400)
        let view = held.hosting
        guard let pixels = view.bitmapImageRepForCachingDisplay(in: view.bounds) else {
            XCTFail("the window gave no picture of itself")
            return Colours(ground: [-1, -1, -1], bubble: [-1, -1, -1])
        }
        view.cacheDisplay(in: view.bounds, to: pixels)
        if let folder = ProcessInfo.processInfo.environment["TILO_SHOTS_DIR"] {
            try? FileManager.default.createDirectory(atPath: folder, withIntermediateDirectories: true)
            try? pixels.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: folder).appendingPathComponent("\(name).png"))
        }
        let scale = CGFloat(pixels.pixelsWide) / FrameLookTests.size.width
        func colour(_ x: CGFloat, _ y: CGFloat) -> [Int] {
            guard let colour = pixels.colorAt(x: Int(x * scale), y: Int(y * scale)), colour.numberOfComponents >= 3 else { return [-1, -1, -1] }
            var parts = [CGFloat](repeating: 0, count: colour.numberOfComponents)
            colour.getComponents(&parts)
            return parts.prefix(3).map { Int(($0 * 255).rounded()) }
        }
        let middle = FrameLookTests.size.height / 2
        return Colours(ground: colour(FrameLookTests.beside, middle), bubble: colour(FrameLookTests.inBubble, middle))
    }

    private func close(_ held: Held) {
        held.window.contentView = nil
        held.window.close()
    }

    /// The frame as the system's look alone draws it, where the person's Tilo has said none.
    private func alone(_ system: ColorScheme) async -> Colours {
        let held = await stand(system: system, look: nil)
        let colours = await drawn(held, kept: "frame-look-system-\(system == .dark ? "dark" : "light")")
        XCTAssertEqual(held.seen.scheme, system, "the engine's view stands in the system's look")
        close(held)
        return colours
    }

    /// A frame whose person's Tilo says `look` on a system of the other look is drawn as `wanted`.
    private func check(system: ColorScheme, look: Look, wanted: Colours, _ name: String) async {
        let held = await stand(system: system, look: look)
        let got = await drawn(held, kept: "frame-look-\(name)")
        XCTAssertLessThanOrEqual(far(got.ground, wanted.ground), 1, "\(name): the ground is \(got.ground), and in that look it is \(wanted.ground)")
        XCTAssertLessThanOrEqual(far(got.bubble, wanted.bubble), 1, "\(name): a piece is \(got.bubble), and in that look it is \(wanted.bubble)")
        // What draws Tilo's own page stands in the system's look: a theme that follows the
        // system must never read the frame's look back.
        XCTAssertEqual(held.seen.scheme, system, "\(name): the engine's view was given the frame's look")
        close(held)
    }

    /// A person whose Tilo is dark on a system that is light sees a dark frame, and the other
    /// way round: the frame's ground and its pieces are what a system of that look alone gives
    /// them. Where their Tilo has said no look, the system's stands in.
    ///
    /// Tolerance: 1 of 255 for each of red, green and blue against the same frame drawn by the
    /// system's look alone. A light ground and a dark one differ by more than 200.
    func testTheFrameDrawsInTheLookItsPersonsTiloHasWhateverTheSystemsIs() async {
        let light = await alone(.light)
        let dark = await alone(.dark)
        XCTAssertGreaterThan(far(light.ground, dark.ground), 200, "a light ground \(light.ground) and a dark one \(dark.ground) are told apart")
        await check(system: .light, look: .dark, wanted: dark, "tilo-dark-system-light")
        await check(system: .dark, look: .light, wanted: light, "tilo-light-system-dark")
    }

    /// The frame follows the look as it changes, with nothing started again: a person who tells
    /// their assistant to make Tilo dark sees the frame go dark with it, and light again when
    /// they say so. When their Tilo says no look any more, the system's stands in again.
    ///
    /// Tolerance: as above.
    func testTheFrameFollowsTheLookAsItChanges() async {
        let dark = await alone(.dark)
        let held = await stand(system: .light, look: nil)
        defer { close(held) }
        let light = await drawn(held, kept: "frame-look-follows-1-nothing-said")
        XCTAssertGreaterThan(far(light.ground, dark.ground), 200, "a light ground \(light.ground) and a dark one \(dark.ground) are told apart")
        held.source.set(look: .dark)
        let went = await drawn(held, kept: "frame-look-follows-2-tilo-went-dark")
        XCTAssertLessThanOrEqual(far(went.ground, dark.ground), 1, "Tilo went dark, and the ground is \(went.ground); dark is \(dark.ground)")
        XCTAssertLessThanOrEqual(far(went.bubble, dark.bubble), 1, "Tilo went dark, and a piece is \(went.bubble); dark is \(dark.bubble)")
        held.source.set(look: .light)
        let back = await drawn(held, kept: "frame-look-follows-3-tilo-went-light")
        XCTAssertLessThanOrEqual(far(back.ground, light.ground), 1, "Tilo went light, and the ground is \(back.ground); light is \(light.ground)")
        XCTAssertLessThanOrEqual(far(back.bubble, light.bubble), 1, "Tilo went light, and a piece is \(back.bubble); light is \(light.bubble)")
        held.source.set(look: .dark)
        let again = await drawn(held, kept: "frame-look-follows-4-tilo-went-dark-again")
        XCTAssertLessThanOrEqual(far(again.ground, dark.ground), 1, "Tilo went dark again, and the ground is \(again.ground); dark is \(dark.ground)")
        held.source.set(look: nil)
        let none = await drawn(held, kept: "frame-look-follows-5-nothing-said-again")
        XCTAssertLessThanOrEqual(far(none.ground, light.ground), 1, "with no look said the system's stands in, and the ground is \(none.ground); light is \(light.ground)")
        XCTAssertEqual(held.seen.scheme, .light, "through every change the engine's view stood in the system's look")
    }
}
#endif
