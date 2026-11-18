import CoreText
import SwiftUI

#if os(macOS)
import AppKit
public typealias PlatformColor = NSColor
public typealias PlatformFont = NSFont
#else
import UIKit
public typealias PlatformColor = UIColor
public typealias PlatformFont = UIFont
#endif

/// A colour of the window's drawing, by the name its style sheet gives it, with its light and
/// its dark value. The values are the drawing's own, turned from OKLCH into sRGB once.
///
/// It is a style: `.foregroundStyle(Token.ink)`, `.background(Token.raised)`. It follows the
/// colour scheme of the place it is drawn in.
public struct Token: ShapeStyle, Sendable, Equatable {
    let light: UInt32
    let dark: UInt32
    var lightOpacity: Double = 1
    var darkOpacity: Double = 1

    public func resolve(in environment: EnvironmentValues) -> Color {
        color(environment.colorScheme)
    }

    public func color(_ scheme: ColorScheme) -> Color {
        let (hex, opacity) = scheme == .dark ? (dark, darkOpacity) : (light, lightOpacity)
        return Color(.sRGB, red: Double((hex >> 16) & 0xff) / 255, green: Double((hex >> 8) & 0xff) / 255,
                     blue: Double(hex & 0xff) / 255, opacity: opacity)
    }

    /// The same colour for a text view of the system, which takes no style.
    public func platform(_ scheme: ColorScheme) -> PlatformColor {
        let (hex, opacity) = scheme == .dark ? (dark, darkOpacity) : (light, lightOpacity)
        return PlatformColor(red: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
                             blue: CGFloat(hex & 0xff) / 255, alpha: opacity)
    }

    // The neutrals: `--content`, `--page`, `--raised`, `--ink`, `--ink-2`, `--ink-3`, `--line`.
    public static let content = Token(light: 0xfcfdff, dark: 0x141619)
    public static let page = Token(light: 0xeef0f4, dark: 0x0b0c0f)
    public static let raised = Token(light: 0xeef0f4, dark: 0x212326)
    public static let ink = Token(light: 0x141619, dark: 0xeceff3)
    public static let ink2 = Token(light: 0x515356, dark: 0xafb1b5)
    public static let ink3 = Token(light: 0x7e8084, dark: 0x7e8084)
    public static let line = Token(light: 0xe1e3e6, dark: 0x2c2e31)
    // The person's own colour: `--you`, `--you-tint`.
    public static let you = Token(light: 0xdb703b, dark: 0xf0995b)
    public static let youTint = Token(light: 0xffdfca, dark: 0x56331f)
    // The assistant's: `--asst`, `--asst-quiet`, `--asst-tint`.
    public static let asst = Token(light: 0x2a3fd1, dark: 0x7f90ff)
    public static let asstQuiet = Token(light: 0x9fa6e0, dark: 0x3b4376)
    public static let asstTint = Token(light: 0xe6eeff, dark: 0x212a44)
    // Going on, and done: `--on`, `--on-tint`.
    public static let on = Token(light: 0x1b9247, dark: 0x45b164)
    public static let onTint = Token(light: 0xd5f5da, dark: 0x1a3520)
    /// The two layers of `--shadow`, and the field's own.
    public static let shadowNear = Token(light: 0x13161b, dark: 0x000000, lightOpacity: 0.06, darkOpacity: 0.3)
    public static let shadowFar = Token(light: 0x13161b, dark: 0x000000, lightOpacity: 0.1, darkOpacity: 0.35)
    /// The drawing's shadow under the message field: seven per cent of its dark ink in both themes.
    /// On a dark ground a shadow darker than the ground is a ring about the field, not a shadow.
    public static let fieldShadow = Token(light: 0x13161b, dark: 0x13161b, lightOpacity: 0.07, darkOpacity: 0.07)
    /// Nothing: a row that is not chosen, a button that is not on.
    public static let clear = Token(light: 0xffffff, dark: 0x000000, lightOpacity: 0, darkOpacity: 0)
    /// White on the person's colour and on a switch.
    public static let onColour = Token(light: 0xffffff, dark: 0xffffff)
}

/// The drawing's measures, by what they are for.
public enum Measure {
    public static let sidebar: CGFloat = 250
    public static let panel: CGFloat = 340
    public static let column: CGFloat = 700
    public static let columnGutter: CGFloat = 24
    /// What stays free under a conversation's last piece: `.col`'s `padding-bottom`.
    public static let columnFoot: CGFloat = 14
    /// How much of what shows of a conversation the opened steps of its turn may take before they
    /// scroll in a part of their own: what stood above the turn's line stays in sight.
    public static let stepsShare: CGFloat = 1 / 3
    /// How deep a conversation's ends fade into the ground where pieces scroll out: as deep as
    /// what stays free under the last piece, so nothing that is at rest stands under a fade.
    public static let edgeFade: CGFloat = columnFoot
    public static let bubbleWidth: CGFloat = 500
    public static let bubbleRadius: CGFloat = 14
    public static let rowRadius: CGFloat = 8
    public static let cardRadius: CGFloat = 12
    public static let fieldHeight: CGFloat = 58
    public static let toolbarButton: CGFloat = 30
    public static let windowRadius: CGFloat = 12
}

/// How things move, in Tilo's own measures: `fast` for what answers a press or changes in
/// place, `base` for a part that opens or closes. Nothing bounces. Where the system asks for
/// reduced motion nothing moves and nothing eases: there is no animation then.
public enum Motion {
    /// 130 ms: a press, a change in place.
    public static let fast: TimeInterval = 0.13
    /// 210 ms: a part that opens or closes, a place brought into view.
    public static let base: TimeInterval = 0.21
    /// 1.2 s: one wave of a sign that says something is being written. It moves only while
    /// that is so, and never where the system asks for reduced motion.
    public static let wave: TimeInterval = 1.2
}

/// The drawing's type: its face, its sizes and its weights.
///
/// The face is Tilo Sans, Tilo's own, which is the drawing's Instrument Sans with more letters.
/// A bundle carries it and `Typeface.register` makes it known; where it is missing the system's
/// face stands in. Sizes grow with the person's text size on an iPhone.
public enum Typeface {
    public static let family = "Tilo Sans"

    /// Whether the family is known to the system now.
    public static var isKnown: Bool {
        #if os(macOS)
        NSFontManager.shared.availableMembers(ofFontFamily: family) != nil
        #else
        !UIFont.fontNames(forFamilyName: family).isEmpty
        #endif
    }

    /// Make the faces in a folder known to this program, and to nothing else on the device.
    @discardableResult
    public static func register(in folder: URL?) -> Bool {
        guard let folder, let files = try? FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: nil)
        else { return false }
        var known = false
        for file in files where ["ttf", "otf"].contains(file.pathExtension.lowercased()) {
            known = CTFontManagerRegisterFontsForURL(file as CFURL, .process, nil) || known
        }
        return known
    }

    /// The faces the app's bundle carries.
    public static func registerBundled() {
        register(in: Bundle.main.resourceURL?.appendingPathComponent("Fonts"))
    }

    /// How far apart lines stand: 1.45 times the letters' size, the drawing's `line-height`.
    public static let line = AttributedString.LineHeight.multiple(factor: 1.45)
}

extension View {
    /// Text set as the drawing sets it: lines 1.45 apart, the letters in the middle of their line.
    public func drawingLines() -> some View {
        lineHeight(Typeface.line).textRenderer(MiddleOfLine())
    }
}

/// Draws text a little lower than the system lays it out. The system gives a taller line its
/// extra room under the letters; a page shares it out above and under them, and so does the
/// drawing. Nothing is measured differently: only where the letters are drawn in their line.
struct MiddleOfLine: TextRenderer {
    /// The share of a line that is extra room, halved: (1.45 - 1.22) / 1.45 / 2, where 1.22 is
    /// the face's own line.
    private static let share: CGFloat = 0.079

    func draw(layout: Text.Layout, in context: inout GraphicsContext) {
        for line in layout {
            var lower = context
            lower.translateBy(x: 0, y: line.typographicBounds.rect.height * MiddleOfLine.share)
            lower.draw(line)
        }
    }
}

extension Font {
    /// A size of the drawing in its face.
    public static func tilo(_ size: CGFloat, _ weight: Font.Weight = .regular, relativeTo style: Font.TextStyle = .body) -> Font {
        if Typeface.isKnown {
            return .custom(Typeface.family, size: size, relativeTo: style).weight(weight)
        }
        #if os(iOS)
        return .system(size: UIFontMetrics(forTextStyle: .body).scaledValue(for: size), weight: weight)
        #else
        return .system(size: size, weight: weight)
        #endif
    }

    /// The drawing's `--mono`.
    public static func tiloMono(_ size: CGFloat) -> Font {
        #if os(iOS)
        // The fixed face grows with the person's text size as the words around it do.
        return .system(size: UIFontMetrics(forTextStyle: .body).scaledValue(for: size), design: .monospaced)
        #else
        return .system(size: size, design: .monospaced)
        #endif
    }

    // The sizes the drawing uses, by what they are for.
    public static var body14: Font { .tilo(14) }
    public static var title15: Font { .tilo(15, .semibold) }
    public static var small12: Font { .tilo(12, relativeTo: .caption) }
    public static var small125: Font { .tilo(12.5, relativeTo: .caption) }
    public static var line135: Font { .tilo(13.5, relativeTo: .callout) }
    public static var button13: Font { .tilo(13, .medium, relativeTo: .callout) }
    public static var tag115: Font { .tilo(11.5, relativeTo: .caption2) }
    public static var field15: Font { .tilo(15) }
}

extension PlatformFont {
    /// A size of the drawing in its face, for a text view of the system.
    public static func tilo(_ size: CGFloat, bold: Bool = false, italic: Bool = false) -> PlatformFont {
        #if os(macOS)
        var font = NSFont(name: bold ? "TiloSans-Bold" : "TiloSans-Regular", size: size)
            ?? NSFontManager.shared.font(withFamily: Typeface.family, traits: bold ? .boldFontMask : [], weight: bold ? 9 : 5, size: size)
            ?? NSFont.systemFont(ofSize: size, weight: bold ? .semibold : .regular)
        if italic { font = NSFontManager.shared.convert(font, toHaveTrait: .italicFontMask) }
        return font
        #else
        let scaled = UIFontMetrics(forTextStyle: .body).scaledValue(for: size)
        let base = UIFont(name: bold ? "TiloSans-Bold" : "TiloSans-Regular", size: scaled)
            ?? UIFont.systemFont(ofSize: scaled, weight: bold ? .semibold : .regular)
        guard italic, let slanted = base.fontDescriptor.withSymbolicTraits(base.fontDescriptor.symbolicTraits.union(.traitItalic))
        else { return base }
        return UIFont(descriptor: slanted, size: scaled)
        #endif
    }

    /// The same face with every figure as wide as the next: how a page sets a list's numbers.
    public static func tiloFigures(_ size: CGFloat) -> PlatformFont {
        let base = tilo(size)
        #if os(macOS)
        let features = [[NSFontDescriptor.FeatureKey.typeIdentifier: kNumberSpacingType,
                         NSFontDescriptor.FeatureKey.selectorIdentifier: kMonospacedNumbersSelector]]
        return NSFont(descriptor: base.fontDescriptor.addingAttributes([.featureSettings: features]), size: base.pointSize) ?? base
        #else
        let features = [[UIFontDescriptor.FeatureKey.type: kNumberSpacingType,
                         UIFontDescriptor.FeatureKey.selector: kMonospacedNumbersSelector]]
        return UIFont(descriptor: base.fontDescriptor.addingAttributes([.featureSettings: features]), size: base.pointSize)
        #endif
    }

    public static func tiloMono(_ size: CGFloat) -> PlatformFont {
        #if os(macOS)
        NSFont.monospacedSystemFont(ofSize: size, weight: .regular)
        #else
        UIFont.monospacedSystemFont(ofSize: UIFontMetrics(forTextStyle: .body).scaledValue(for: size), weight: .regular)
        #endif
    }
}
