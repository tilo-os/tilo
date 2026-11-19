#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// Where things stand: a task's line on a narrow window, and code's ground in the message field.
@MainActor
final class LayoutTests: XCTestCase {
    private final class Places {
        var held: [String: CGRect] = [:]
    }

    /// A task's line made of plain blocks of known widths, at a width, and where each block lands.
    private func run(title: CGFloat, rest: CGFloat, mark: CGFloat?, within width: CGFloat) async -> [String: CGRect] {
        let places = Places()
        func block(_ name: String, _ wide: CGFloat) -> some View {
            Color.clear.frame(width: wide, height: 20)
                .onGeometryChange(for: CGRect.self) { $0.frame(in: .named("run")) } action: { places.held[name] = $0 }
        }
        let view = TitleRun(spacing: 7, lineSpacing: 2) {
            block("title", title).layoutValue(key: TitleRun.Part.self, value: .title)
            block("rest", rest)
            if let mark { block("mark", mark).layoutValue(key: TitleRun.Part.self, value: .mark) }
        }
        .coordinateSpace(.named("run"))
        .frame(width: width, alignment: .topLeading)
        let hosting = NSHostingView(rootView: view)
        let window = HeldWindow.make(NSRect(x: 0, y: 0, width: width, height: 100), [.borderless])
        window.contentView = hosting
        hosting.layoutSubtreeIfNeeded()
        try? await Task.sleep(for: .milliseconds(150))
        hosting.layoutSubtreeIfNeeded()
        window.contentView = nil
        window.close()
        return places.held
    }

    func testWhileItFitsTheLineStandsAsDrawn() async {
        let places = await run(title: 150, rest: 120, mark: 70, within: 400)
        XCTAssertEqual(places["title"]?.origin, CGPoint(x: 0, y: 0))
        XCTAssertEqual(places["rest"]?.origin, CGPoint(x: 157, y: 0), "how the task stands follows its title")
        XCTAssertEqual(places["mark"]?.origin, CGPoint(x: 284, y: 0), "and the mark comes last")
    }

    func testWhatDoesNotFitGoesUnderTheTitleAndTheMarkStaysWithItsTitle() async {
        let places = await run(title: 150, rest: 200, mark: 70, within: 300)
        XCTAssertEqual(places["title"]?.origin, CGPoint(x: 0, y: 0))
        XCTAssertEqual(places["mark"]?.origin, CGPoint(x: 157, y: 0), "the mark stays on its title's line while it fits there")
        XCTAssertEqual(places["rest"]?.origin, CGPoint(x: 0, y: 22), "the rest begins under the title's own words")
    }

    func testAMarkThatDoesNotFitBesideItsTitleFollowsInTheDrawingsOrder() async {
        let beside = await run(title: 150, rest: 120, mark: 70, within: 200)
        XCTAssertEqual(beside["rest"]?.origin, CGPoint(x: 0, y: 22))
        XCTAssertEqual(beside["mark"]?.origin, CGPoint(x: 127, y: 22))
        let under = await run(title: 150, rest: 120, mark: 70, within: 180)
        XCTAssertEqual(under["rest"]?.origin, CGPoint(x: 0, y: 22))
        XCTAssertEqual(under["mark"]?.origin, CGPoint(x: 0, y: 44))
        let none = await run(title: 150, rest: 200, mark: nil, within: 300)
        XCTAssertEqual(none["rest"]?.origin, CGPoint(x: 0, y: 22))
    }

    /// The real line at an iPhone's width: its second part starts where its title starts, not
    /// at the window's edge under the arrow.
    func testOnANarrowWindowATasksSecondPartAlignsWithItsTitle() async throws {
        let model = AssistantModel(source: Drawing.source(), snapshots: cacheOfTheCheck())
        await model.prime()
        let line = TaskLine(task: "maya", title: "Reply to Maya at Northwind", state: .replies(2))
        let view = AssistantRoot(model: model) {
            TaskLineView(line: line, isImportant: true, before: "maya", chat: .main)
                .frame(width: 300, alignment: .leading)
        }
        let window = HeldWindow.make(NSRect(x: 0, y: 0, width: 300, height: 80), [.borderless])
        defer { window.close() }
        window.contentView = NSHostingView(rootView: view)
        let drive = Drive(window)
        await drive.settle(300)
        let said = drive.tree()
        let title = try XCTUnwrap(said.first { $0.isHeading }, drive.said())
        let replies = try XCTUnwrap(said.first { $0.isButton }, drive.said())
        XCTAssertEqual(title.label, "Reply to Maya at Northwind, Important", "the heading says the mark it carries")
        XCTAssertEqual(replies.label, AssistantWords.counted(2, one: .showReplyOne, many: .showReplyMany))
        XCTAssertEqual(replies.frame.minX, title.frame.minX, accuracy: 0.5, "the second part begins under the title's words")
        XCTAssertLessThan(replies.frame.maxY, title.frame.minY + 1, "and under the title, not beside it")
        // The arrow and the eye stand before the title: 16 and 20 wide with 7 between and after.
        XCTAssertEqual(title.frame.minX - window.frame.minX, 50, accuracy: 0.5)
        // Wide enough, it is one line again.
        window.setContentSize(NSSize(width: 520, height: 80))
        window.contentView = NSHostingView(rootView: AssistantRoot(model: model) {
            TaskLineView(line: line, isImportant: true, before: "maya", chat: .main).frame(width: 520, alignment: .leading)
        })
        await drive.settle(300)
        let wide = drive.tree()
        let wideTitle = try XCTUnwrap(wide.first { $0.isHeading }), wideReplies = try XCTUnwrap(wide.first { $0.isButton })
        XCTAssertGreaterThan(wideReplies.frame.minX, wideTitle.frame.maxX)
        XCTAssertEqual(wideReplies.frame.midY, wideTitle.frame.midY, accuracy: 1)
    }

    // MARK: Code's ground in the field

    private func laidOut(_ text: String, caret: Int? = nil) -> (NSTextStorage, FieldLayout, NSTextContainer) {
        let storage = NSTextStorage(string: text)
        let layout = FieldLayout()
        storage.addLayoutManager(layout)
        let container = NSTextContainer(size: CGSize(width: 400, height: 1000))
        container.lineFragmentPadding = 0
        layout.addTextContainer(container)
        FieldStyle.apply(to: storage, selection: caret.map { NSRange(location: $0, length: 0) }, scheme: .light)
        layout.ensureLayout(for: container)
        return (storage, layout, container)
    }

    func testCodeInTheFieldStandsOnAPaddedRoundedGround() throws {
        let text = "check `billing#retry` is live"
        let (storage, layout, container) = laidOut(text)
        func letters(_ words: String) -> CGRect {
            layout.boundingRect(forGlyphRange: layout.glyphRange(forCharacterRange: (text as NSString).range(of: words), actualCharacterRange: nil),
                                in: container)
        }
        let grounds = layout.grounds()
        XCTAssertEqual(grounds.count, 1)
        let ground = try XCTUnwrap(grounds.first).rect
        let code = letters("billing#retry")
        // Five points of room beside the letters, one above and one below, as the drawing pads code.
        XCTAssertEqual(code.minX - ground.minX, 5, accuracy: 0.3)
        XCTAssertEqual(ground.maxX - code.maxX, 5, accuracy: 0.3)
        let face = PlatformFont.tiloMono(13)
        XCTAssertEqual(ground.height, face.ascender - face.descender + 2, accuracy: 0.1)
        XCTAssertGreaterThanOrEqual(ground.minY, 0)
        XCTAssertLessThanOrEqual(ground.maxY, 22.5, "the ground stays inside its line")
        // The words on both sides stand clear of it.
        XCTAssertLessThanOrEqual(letters("check").maxX, ground.minX)
        XCTAssertGreaterThanOrEqual(letters("is live").minX, ground.maxX)
        XCTAssertEqual(storage.string, text, "the room is made without a character added")

        // Drawn: its corners are round, its middle is the drawing's raised ground.
        let picture = try XCTUnwrap(NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: 400, pixelsHigh: 30, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
            isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0))
        let context = try XCTUnwrap(NSGraphicsContext(bitmapImageRep: picture))
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        let down = NSAffineTransform()
        down.translateX(by: 0, yBy: 30)
        down.scaleX(by: 1, yBy: -1)
        down.concat()
        layout.drawBackground(forGlyphRange: layout.glyphRange(for: container), at: .zero)
        NSGraphicsContext.restoreGraphicsState()
        func alpha(_ x: CGFloat, _ y: CGFloat) -> CGFloat { picture.colorAt(x: Int(x), y: Int(y))?.alphaComponent ?? -1 }
        XCTAssertEqual(alpha(ground.midX, ground.midY), 1, accuracy: 0.01)
        XCTAssertEqual(alpha(ground.minX + 0.5, ground.minY + 0.5), 0, accuracy: 0.3, "the corner is cut round")
        XCTAssertEqual(alpha(ground.maxX - 0.5, ground.maxY - 0.5), 0, accuracy: 0.3)
        XCTAssertEqual(alpha(ground.minX + 6, ground.minY + 1.5), 1, accuracy: 0.01, "past the corner the edge is straight")
        let middle = try XCTUnwrap(picture.colorAt(x: Int(ground.midX), y: Int(ground.midY))?.usingColorSpace(.sRGB))
        let raised = try XCTUnwrap(Token.raised.platform(.light).usingColorSpace(.sRGB))
        XCTAssertEqual(middle.redComponent, raised.redComponent, accuracy: 0.02)
        XCTAssertEqual(middle.blueComponent, raised.blueComponent, accuracy: 0.02)
    }

    func testCodeBeingWrittenKeepsItsGroundAndShowsItsSigns() throws {
        let text = "check `billing#retry` is live"
        let caret = (text as NSString).range(of: "retry").location
        let (storage, layout, _) = laidOut(text, caret: caret)
        let sign = (text as NSString).range(of: "`").location
        XCTAssertEqual((storage.attribute(.font, at: sign, effectiveRange: nil) as? NSFont)?.pointSize, 15, "its signs show while it is written")
        XCTAssertNil(storage.attribute(.kern, at: sign, effectiveRange: nil), "and take their own room")
        XCTAssertEqual(layout.grounds().count, 1)
        // Two stretches of code keep a ground each, also on two lines.
        let (twoWords, two, _) = laidOut("`one` and\n`two`")
        var kept: [String] = []
        twoWords.enumerateAttribute(FieldStyle.ground, in: NSRange(location: 0, length: twoWords.length)) { value, range, _ in
            kept.append("\(range) \(value == nil ? "none" : "ground")")
        }
        XCTAssertEqual(two.grounds().count, 2, "\(kept) marks \(Markdown.marks(in: twoWords.string))")
        guard two.grounds().count == 2 else { return }
        XCTAssertNotEqual(two.grounds()[0].rect.minY, two.grounds()[1].rect.minY)
        // A fence of code is no ground of this kind: it keeps the whole lines' flat one.
        let (fenced, block, _) = laidOut("```\nlet x = 1\n```")
        XCTAssertEqual(block.grounds().count, 0)
        XCTAssertNotNil(fenced.attribute(.backgroundColor, at: 5, effectiveRange: nil))
    }
}
#endif
