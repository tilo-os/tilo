#if os(macOS)
import AppKit
import AssistantCore
@testable import AssistantTilo
import ShellCore
import SwiftUI
@testable import TiloLive
import WebKit
import XCTest

/// The place of a live thing that is not shown, drawn and read pixel by pixel: one block of
/// words on its holder's ground, as high as the words are. Against a page that refuses the way
/// Tilo's does; `slow` is a page that never answers. The window the place is drawn in is never
/// put on a screen, nothing is fetched and nothing is kept.
@MainActor
final class UnshownPlaceTests: XCTestCase {
    private static let site = "https://tilo.test"
    private nonisolated static let width: CGFloat = 381
    private nonisolated static let around: CGFloat = 20
    private var engine: TiloEngine!
    private var stage: LiveStage!
    private let view = TiloLive.LiveThing(id: "m_reply_7", title: "Word Processor",
                                          .messageView(message: "m_reply_7", conversation: "c_main"))

    private static let page = #"""
    <!doctype html><html data-arrival="load"><title>Tilo</title><span id="status"></span>
    <script>
      window.enter = how => {
        const item = 'chat.workspace/messages/m_reply_7';
        const own = [{address: {resource: item, action: 'reveal-message'}, view: {id: 'message', open_action: 'reveal-message', primary: true}}];
        if (how === 'early') own.push({address: {resource: item, action: 'show-view'}, view: {id: 'view', open_action: 'show-view'}});
        globalThis.tiloOpen = async () => {
          if (how === 'closed') throw new Error('not_permitted: Editors may not open chat.workspace');
          return {
            semantics: document.createElement('div'), state: () => ({phase: 'presented'}), actions: () => [],
            invokeAction: async (resource, action) => { if (how === 'slow') await new Promise(() => {}); throw new Error(`unknown_action: no action addressed by (${resource}, ${action})`); },
            cleanup: async () => {},
          };
        };
        document.documentElement.dataset.arrival = 'done';
        document.documentElement.dataset.entry = 'open';
        globalThis.tiloMount = {semantics: document.createElement('div'), actions: () => own, invokeAction: async () => ({state: 'completed'})};
      };
    </script></html>
    """#

    private func enter(_ how: String) async throws {
        let engine = TiloEngine(settings: try ShellSettings(site: Self.site), store: .keepsNothing)
        engine.loads = { _ in }
        self.engine = engine
        stage = LiveStage(engine: engine)
        engine.view.loadHTMLString(Self.page, baseURL: URL(string: "\(Self.site)/app/"))
        try await until("the page is there") {
            (try? await engine.view.callAsyncJavaScript("return typeof window.enter", arguments: [:], in: nil, contentWorld: .page)) as? String == "function"
        }
        _ = try await engine.view.callAsyncJavaScript("location.hash = '#/w/ws-1/so.tilo.chat'; enter(how)", arguments: ["how": how],
                                                      in: nil, contentWorld: .page)
        try await until("the engine is in") { if case .open = engine.state { true } else { false } }
    }

    private func until(_ what: String, _ holds: @MainActor () async -> Bool) async throws {
        for _ in 0..<240 {
            if await holds() { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        XCTFail("waited twelve seconds for \(what)")
        throw CancellationError()
    }

    private struct Drawn {
        let pixels: NSBitmapImageRep
        let size: CGSize

        /// A pixel's red, green and blue as numbers from 0 to 255, at a place in points from the top left.
        func colour(_ x: CGFloat, _ y: CGFloat) -> [Int] {
            let scale = CGFloat(pixels.pixelsWide) / size.width
            guard let colour = pixels.colorAt(x: Int(x * scale), y: Int(y * scale)), colour.numberOfComponents >= 3 else { return [-1, -1, -1] }
            var parts = [CGFloat](repeating: 0, count: colour.numberOfComponents)
            colour.getComponents(&parts)
            return parts.prefix(3).map { Int(($0 * 255).rounded()) }
        }

        /// How high the place itself is, without the ground around it.
        var height: CGFloat { size.height - 2 * UnshownPlaceTests.around }
    }

    /// The place as its holder would hold it: at a phone's width, on a ground of the holder's own.
    private func drawn(_ name: String) async throws -> Drawn {
        let place = TiloLiveView(view, on: stage, says: TiloSource.said)
            .frame(width: Self.width)
            .padding(Self.around)
            .background(Color(red: 0.25, green: 0.4, blue: 0.55))
        let hosting = NSHostingView(rootView: place)
        let window = NSWindow(contentRect: CGRect(x: 0, y: 0, width: Self.width + 2 * Self.around, height: 400),
                              styleMask: [.borderless], backing: .buffered, defer: true)
        window.isReleasedWhenClosed = false
        window.contentView = hosting
        defer {
            window.contentView = nil
            window.close()
        }
        // The place asks for its thing as it comes into a window: until that ask is answered as the first was.
        let before = stage.state(of: view)
        try await Task.sleep(for: .milliseconds(300))
        try await until("the place's own ask is answered") { self.stage.state(of: self.view) == before }
        hosting.layoutSubtreeIfNeeded()
        let size = hosting.fittingSize
        window.setContentSize(size)
        hosting.frame = CGRect(origin: .zero, size: size)
        hosting.layoutSubtreeIfNeeded()
        try await Task.sleep(for: .milliseconds(100))
        let pixels = try XCTUnwrap(hosting.bitmapImageRepForCachingDisplay(in: hosting.bounds))
        hosting.cacheDisplay(in: hosting.bounds, to: pixels)
        if let folder = ProcessInfo.processInfo.environment["TILO_SHOTS_DIR"] {
            try? FileManager.default.createDirectory(atPath: folder, withIntermediateDirectories: true)
            try? pixels.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: folder).appendingPathComponent("live-place-\(name).png"))
        }
        return Drawn(pixels: pixels, size: size)
    }

    /// The holder's ground stands at every edge of the place, where no word stands: the place
    /// lays no ground of its own under its words and no box around them.
    ///
    /// Tolerance: 2 of 255 for each of red, green and blue.
    private func assertTheHoldersGroundShows(_ drawn: Drawn, line: UInt = #line) {
        let ground = drawn.colour(4, 4)
        let left = Self.around, right = Self.around + Self.width, top = Self.around, foot = drawn.size.height - Self.around
        let places: [(String, CGFloat, CGFloat)] = [
            ("its upper leading corner", left + 3, top + 3), ("its upper trailing corner", right - 3, top + 3),
            ("its lower leading corner", left + 3, foot - 3), ("its lower trailing corner", right - 3, foot - 3),
            ("its upper edge", (left + right) / 2, top + 2), ("its lower edge", (left + right) / 2, foot - 2),
            ("its trailing edge", right - 3, (top + foot) / 2),
        ]
        for (name, x, y) in places {
            let here = drawn.colour(x, y)
            let apart = zip(here, ground).map { abs($0 - $1) }.max() ?? 255
            XCTAssertLessThanOrEqual(apart, 2, "at \(name) the place lays a tone of its own, \(here), over its holder's \(ground)", line: line)
        }
    }

    /// How far from the place's leading edge the name's first letter begins, in points.
    private func wordsBegin(_ drawn: Drawn) -> CGFloat {
        let ground = drawn.colour(4, 4)
        var first = Self.width
        // Along the name's line, which stands under twelve points of padding.
        for y in stride(from: Self.around + 14, to: Self.around + 30, by: 2) {
            for x in stride(from: Self.around, to: Self.around + Self.width, by: 0.5) {
                let here = drawn.colour(x, y)
                if (zip(here, ground).map { abs($0 - $1) }.max() ?? 0) > 40 {
                    first = min(first, x - Self.around)
                    break
                }
            }
        }
        return first
    }

    func testAViewThisTiloCannotShowIsOneBlockOfWordsAsHighAsTheWords() async throws {
        try await enter("old")
        await stage.bring(view)
        XCTAssertEqual(stage.troubles[view.id], .notOffered)
        let drawn = try await drawn("cannot-show")
        assertTheHoldersGroundShows(drawn)
        XCTAssertLessThan(drawn.height, 100, "a name and one sentence stand \(drawn.height) points high")
        XCTAssertGreaterThan(drawn.height, 40)
        // The words begin at the place's leading edge, fourteen points in, as a card's words do.
        XCTAssertEqual(wordsBegin(drawn), 14, accuracy: 2.5, "the words begin \(wordsBegin(drawn)) points from the place's leading edge")
    }

    func testAViewThatDidNotComeIsOneBlockWithItsButtonUnderTheWords() async throws {
        try await enter("early")
        await stage.bring(view)
        XCTAssertEqual(stage.troubles[view.id], .notShown)
        let drawn = try await drawn("try-again")
        assertTheHoldersGroundShows(drawn)
        XCTAssertNotEqual(drawn.height, 132, accuracy: 0.5, "the place keeps a page's height though no page is shown")
        XCTAssertEqual(wordsBegin(drawn), 14, accuracy: 2.5, "the words begin \(wordsBegin(drawn)) points from the place's leading edge")
    }

    func testWhileAViewComesItsPlaceKeepsThePagesHeightAndLaysNoGround() async throws {
        try await enter("slow")
        let view = view, stage = stage!
        Task { await stage.bring(view) }
        try await until("the view is asked for") { stage.state(of: view) == .loading }
        let drawn = try await drawn("comes")
        XCTAssertEqual(stage.state(of: view), .loading)
        assertTheHoldersGroundShows(drawn)
        XCTAssertEqual(drawn.height, 132, accuracy: 0.5, "while its page comes the place keeps the height a view has until it says its own")
    }
}
#endif
