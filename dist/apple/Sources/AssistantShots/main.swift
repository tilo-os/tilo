import AssistantCore
import AssistantDrawing
import AssistantUI
import Foundation
import SwiftUI

// Draws every screen of the assistant to a picture, with nothing on screen.
//
//   AssistantShots --words                      the sentences' file, as the bundle carries it
//   AssistantShots --out DIR [--fonts DIR]      (Mac) every screen at the Mac's sizes, light and dark
//
// Built for an iPhone it is a small app of its own, which draws every screen at the phone's
// size into its documents and ends. It holds the drawing's content; the assistant's app does not.

if CommandLine.arguments.contains("--words") {
    print(AssistantWords.catalogue, terminator: "")
    exit(0)
}

func argument(_ name: String) -> String? {
    guard let index = CommandLine.arguments.firstIndex(of: name), index + 1 < CommandLine.arguments.count else { return nil }
    return CommandLine.arguments[index + 1]
}

#if os(macOS)
import AppKit

guard let out = argument("--out") else {
    print("AssistantShots --out DIR [--fonts DIR]")
    exit(2)
}
// No icon in the Dock and no window on any screen.
NSApplication.shared.setActivationPolicy(.prohibited)
if let fonts = argument("--fonts") { Typeface.register(in: URL(fileURLWithPath: fonts)) }
print("typeface: \(Typeface.isKnown ? Typeface.family : "the system's (Tilo Sans is not at hand)")")
if CommandLine.arguments.contains("--metrics") {
    // What one line of each setting measures, to hold against the drawing's 20.3 for its 14.
    let font = PlatformFont.tilo(14)
    print("face: \(font.fontName) ascender \(font.ascender) descender \(font.descender) leading \(font.leading)")
    func height(_ name: String, _ line: AttributedString.LineHeight?) {
        let view = NSHostingView(rootView: Text("Xg").font(.body14).lineHeight(line).fixedSize())
        print("one line, \(name): \(view.fittingSize.height)")
    }
    height("as the face has it", nil)
    height("multiple 1.45", .multiple(factor: 1.45))
    height("the root's", Typeface.line)
    height("exact 20.3", .exact(points: 20.3))
    print("one line, as the app sets it: \(NSHostingView(rootView: Text("Xg").font(.body14).drawingLines().fixedSize()).fittingSize.height)")
    exit(0)
}
let folder = URL(fileURLWithPath: out)
try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
for scheme in [ColorScheme.light, .dark] {
    for screen in await Screens.mac(scheme) {
        let name = "\(screen.name)-\(scheme == .dark ? "dark" : "light").png"
        guard let png = MacPicture.png(of: screen.view, size: screen.size, scheme: scheme) else {
            print("FAILED \(name)")
            continue
        }
        try png.write(to: folder.appendingPathComponent(name))
        print("drew \(name)")
    }
}
exit(0)
#else
import UIKit

UIApplicationMain(CommandLine.argc, CommandLine.unsafeArgv, nil, NSStringFromClass(ShotsDelegate.self))
#endif
