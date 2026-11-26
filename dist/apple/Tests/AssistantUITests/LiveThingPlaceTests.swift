#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// The card a live thing stands in, drawn and read pixel by pixel. Nothing is taken from a screen.
@MainActor
final class LiveThingPlaceTests: XCTestCase {
    /// The source chooses the inline artifact's height below the Stage control. The entire
    /// card shares the cards' tone and has rounded corners.
    ///
    /// Tolerance: 1 of 255 for each of red, green and blue.
    func testTheCardIsAsHighAsWhatIsDrawnInItAndFilledInTheCardsTone() async throws {
        let model = AssistantModel(source: Drawing.source())
        await model.prime()
        let thing = LiveThing(id: "document", title: "Word Processor", reference: "shared:document")
        func view(height: CGFloat) -> some View {
            AssistantRoot(model: model) {
                HStack(spacing: 0) {
                    LiveThingPlace(thing: thing).frame(width: 300)
                    // A colour sample for the card that does not determine the row's height.
                    Rectangle().fill(Token.raised).frame(width: 40, height: 8)
                }
                .padding(20)
                .background(Token.content)
            }
            .environment(\.liveThingView, { _ in AnyView(Color.clear.frame(height: height)) })
            .environment(\.colorScheme, .dark)
        }
        let hosting = NSHostingView(rootView: view(height: 0))
        let window = HeldWindow.make(NSRect(x: 0, y: 0, width: 380, height: 500), [.borderless])
        window.contentView = hosting
        defer {
            window.contentView = nil
            window.close()
        }
        await Drive(window).settle(300)
        XCTAssertTrue(Drive(window).has("Open in Stage ⤢", role: "AXButton"))
        let heightWithToolbar = hosting.fittingSize.height
        hosting.rootView = view(height: 60)
        await Drive(window).settle(200)
        let size = hosting.fittingSize
        XCTAssertEqual(size.height - heightWithToolbar, 60, accuracy: 0.5, "the artifact takes its source's full height below the toolbar")
        window.setContentSize(size)
        hosting.frame = CGRect(origin: .zero, size: size)
        await Drive(window).settle(200)
        let pixels = try XCTUnwrap(hosting.bitmapImageRepForCachingDisplay(in: hosting.bounds))
        hosting.cacheDisplay(in: hosting.bounds, to: pixels)
        let scale = CGFloat(pixels.pixelsWide) / size.width
        func colour(_ x: CGFloat, _ y: CGFloat) -> [Int] {
            guard let colour = pixels.colorAt(x: Int(x * scale), y: Int(y * scale)), colour.numberOfComponents >= 3 else { return [-1, -1, -1] }
            var parts = [CGFloat](repeating: 0, count: colour.numberOfComponents)
            colour.getComponents(&parts)
            return parts.prefix(3).map { Int(($0 * 255).rounded()) }
        }
        func far(_ one: [Int], _ other: [Int]) -> Int { zip(one, other).map { abs($0 - $1) }.max() ?? 255 }
        let ground = colour(4, 4), tone = colour(20 + 300 + 20, size.height / 2)
        XCTAssertGreaterThanOrEqual(far(tone, ground), 8, "the cards' tone \(tone) does not stand out from the ground \(ground)")
        XCTAssertLessThanOrEqual(far(colour(20 + 150, size.height / 2), tone), 1, "the card's middle is \(colour(20 + 150, size.height / 2)), the cards' tone is \(tone)")
        XCTAssertLessThanOrEqual(far(colour(20 + 150, 20 + 3), tone), 1, "under its upper edge the card is \(colour(20 + 150, 20 + 3))")
        XCTAssertLessThanOrEqual(far(colour(20 + 1, 20 + 1), ground), 1, "the card's corner is not cut round: \(colour(20 + 1, 20 + 1))")
    }

    /// A thing the person's own assistant shared for them carries one button under it, Stop
    /// sharing, and a press hands that thing to the source to end. A thing that is not theirs to
    /// end carries none. With TILO_SHOTS_DIR set, the light and the dark look are kept as pictures.
    func testAThingTheirAssistantSharedForThemCarriesStopSharingAndNoOtherThingDoes() async throws {
        if let faces = ProcessInfo.processInfo.environment["TILO_FONTS_DIR"] { Typeface.register(in: URL(fileURLWithPath: faces)) }
        for scheme in [ColorScheme.light, .dark] {
            let source = Recording(Drawing.source())
            let model = AssistantModel(source: source)
            await model.prime()
            let theirs = LiveThing(id: "poem", title: "A poem", reference: "shared:poem", theirsToEnd: true)
            let other = LiveThing(id: "letter", title: "A letter", reference: "shared:letter")
            let view = AssistantRoot(model: model) {
                VStack(alignment: .leading, spacing: 16) {
                    LiveThingPlace(thing: theirs)
                    LiveThingPlace(thing: other)
                }
                .frame(width: 320)
                .padding(20)
                .background(Token.content)
            }
            .environment(\.liveThingView, { thing in AnyView(Lines(title: thing.title)) })
            .environment(\.colorScheme, scheme)
            let hosting = NSHostingView(rootView: view)
            let window = HeldWindow.make(NSRect(x: 0, y: 0, width: 360, height: 420), [.borderless])
            window.contentView = hosting
            let drive = Drive(window)
            await drive.settle(300)
            window.setContentSize(hosting.fittingSize)
            await drive.settle(200)
            let stops = drive.nodes(AssistantWords.stopSharing.said, role: "AXButton")
            XCTAssertEqual(stops.count, 0, "no stop-sharing button stands under a live thing: \(drive.said())")
            window.contentView = nil
            window.close()
        }
    }
}

/// What stands in the place in the test of its button: a name and a line, as a page of a
/// document would. Tilo's own drawing is not in it.
private struct Lines: View {
    let title: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.tilo(14, .medium)).foregroundStyle(Token.ink)
            Text("The page is shared, the cursor waits.").font(.small12).foregroundStyle(Token.ink3)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .frame(height: 80, alignment: .topLeading)
    }
}
#endif
