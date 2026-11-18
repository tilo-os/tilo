#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// The message field's ground and its shadow, read off the window's own drawing dot by dot,
/// light and dark. Nothing is taken from a screen.
@MainActor
final class FieldGroundTests: XCTestCase {
    /// A dot of the picture: its red, green and blue as the picture holds them, each of 255.
    private func dot(_ picture: NSBitmapImageRep, _ x: CGFloat, _ y: CGFloat, _ dots: CGFloat) throws -> [Int] {
        let across = Int((x * dots).rounded(.down)), down = Int((y * dots).rounded(.down))
        let held = across >= 0 && down >= 0 && across < picture.pixelsWide && down < picture.pixelsHigh
        XCTAssertTrue(held, "no dot at \(x), \(y)")
        XCTAssertEqual(picture.bitsPerSample, 8)
        var parts = [Int](repeating: 0, count: max(picture.samplesPerPixel, 4))
        guard held else { return [0, 0, 0] }
        picture.getPixel(&parts, atX: across, y: down)
        return picture.bitmapFormat.contains(.alphaFirst) ? Array(parts[1...3]) : Array(parts[0...2])
    }

    func testTheFieldsGroundIsFlatUpToItsLineAndItsShadowIsNoRingOnADarkGround() async throws {
        for dark in [false, true] {
            let theme = dark ? "dark" : "light"
            let started = try await Started.app()
            defer { started.end() }
            started.window.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
            let drive = started.drive, field = try started.field()
            await drive.settle(500)
            let content = try XCTUnwrap(started.window.contentView)
            XCTAssertTrue(content.isFlipped, "the window's content is measured from its top")
            let scroll = try XCTUnwrap(field.enclosingScrollView)
            let seen = scroll.convert(scroll.bounds, to: content)
            XCTAssertEqual(seen.height, 22, accuracy: 1.5, "an empty field is one line tall")
            // The field's box: sixteen points about its one line on every side; the line is drawn a little higher than it lies.
            let top = seen.minY + FieldStyle.rise - 16, foot = top + 58, middle = seen.midX
            let picture = try XCTUnwrap(content.bitmapImageRepForCachingDisplay(in: content.bounds))
            content.cacheDisplay(in: content.bounds, to: picture)
            let dots = CGFloat(picture.pixelsWide) / content.bounds.width
            // The field's own colour at its middle, and the colour of what it lies on, well under its foot.
            let ground = try dot(picture, middle, top + 29, dots), under = try dot(picture, middle, foot + 14, dots)
            func apart(_ one: [Int], _ other: [Int]) -> Int { zip(one, other).map { abs($0 - $1) }.max() ?? 0 }
            func darker(_ one: [Int], than other: [Int]) -> Int { zip(one, other).map { $1 - $0 }.max() ?? 0 }

            // Inside, the ground is flat from under its top line to over its foot's line: no line casts a shadow onto it.
            var band = 0, where_ = CGFloat(0)
            for y in Array(stride(from: top + 2, through: top + 12, by: 0.5)) + Array(stride(from: foot - 12, through: foot - 1.5, by: 0.5)) {
                let off = apart(try dot(picture, middle, y, dots), ground)
                if off > band { (band, where_) = (off, y - top) }
            }
            XCTAssertLessThanOrEqual(band, 1, "\(theme): a band lies on the field's ground inside its line, \(where_) points under the field's top, \(band) of 255 off the ground's colour")

            // Under the foot: the drawing's soft shadow where the ground is light; on a dark ground nothing darker than the ground.
            var shade = 0
            for y in stride(from: foot + 1, through: foot + 6, by: 0.5) {
                shade = max(shade, darker(try dot(picture, middle, y, dots), than: under))
            }
            if dark {
                XCTAssertLessThanOrEqual(shade, 2, "dark: the field's shadow is \(shade) of 255 darker than the ground it lies on: a ring, not a shadow")
            } else {
                // On a light ground the ground's own shadow is soft: in the window's own drawing it measures 1 of 255,
                // too little to hold it to a least value here. It is never a dark edge.
                XCTAssertLessThanOrEqual(shade, 16, "light: the shadow under the field's foot is soft, the drawing's seven per cent")
            }
        }
    }
}
#endif
