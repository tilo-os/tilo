#if os(macOS)
import AppKit
import AssistantCore
import AssistantDrawing
@testable import AssistantUI
import SwiftUI
import XCTest

/// The sidebar's signs. The logo is the head's alone, the main chat has a sign of its own, and
/// a row says that its conversation is being written in. Drawn, and read dot by dot: nothing
/// is taken from a screen.
@MainActor
final class SidebarSignTests: XCTestCase {
    private func until(_ what: String, _ holds: () -> Bool, file: StaticString = #filePath, line: UInt = #line) async {
        for _ in 0..<400 {
            if holds() { return }
            try? await Task.sleep(for: .milliseconds(5))
        }
        XCTFail("it never came: \(what)", file: file, line: line)
    }

    // MARK: What the model says

    func testARowIsWritingExactlyWhileItsConversationHasAReplyInWork() async {
        let source = Drawing.source()
        let model = AssistantModel(source: source)
        model.start()
        let agent = Chat.subAgent("usage-drop")
        await until("both conversations are held") { model.conversation(.main).isLoaded && model.conversation(agent).isLoaded }
        XCTAssertFalse(model.isWriting(.main), "a row wrote before any reply was begun")
        XCTAssertFalse(model.isWriting(agent))

        // The assistant begins a reply in the main chat: that row writes, and no other.
        source.setAnswering(true)
        await until("the main chat's row writes") { model.isWriting(.main) }
        XCTAssertFalse(model.isWriting(agent), "a sub-agent's row wrote while the assistant did")
        // The reply grows: it is in work still.
        source.post(Item(id: "reply", at: Drawing.now, .message(Message(.assistant, "Looking"))))
        await until("the reply's first words") { model.conversation(.main).items.last?.id == "reply" }
        XCTAssertTrue(model.isWriting(.main), "the row was at rest while its reply grew")
        // The reply is whole: the row is at rest.
        source.setAnswering(false)
        await until("the main chat's row is at rest") { !model.isWriting(.main) }

        // A sub-agent writes: its own row, and not the main chat's.
        source.setAnswering(true, in: agent)
        await until("the sub-agent's row writes") { model.isWriting(agent) }
        XCTAssertFalse(model.isWriting(.main), "the main chat's row wrote while a sub-agent did")
        // Its answer ends before it is whole: at rest again.
        try? await source.stopAnswering(in: agent)
        await until("the sub-agent's row is at rest") { !model.isWriting(agent) }
        // The row and the conversation say one thing: the line under a reply in work reads the same.
        for chat in [Chat.main, agent] {
            XCTAssertEqual(model.isWriting(chat), model.conversation(chat).isAnswering)
        }
    }

    // MARK: How the dots move

    func testTheDotsRiseAndFallOneAfterTheOtherAndAWaveTakesTheAppsMeasure() {
        XCTAssertEqual(Motion.wave, 1.2, accuracy: 0.001)
        let moments = stride(from: 0.0, to: Motion.wave, by: Motion.wave / 240).map { $0 }
        let lifts = moments.map { Wave.lifts(at: $0) }
        XCTAssertTrue(lifts.allSatisfy { $0.count == 3 && $0.allSatisfy { $0 >= 0 && $0 <= 1 } }, "a dot sank under its line or rose past its highest")
        // Each dot is at its highest once in a wave: the first before the second before the third.
        var highest: [TimeInterval] = []
        for dot in 0..<3 {
            let top = lifts.map { $0[dot] }.max() ?? 0
            XCTAssertEqual(top, 1, accuracy: 0.01, "dot \(dot) never rises whole")
            highest.append(moments[lifts.firstIndex { $0[dot] == top } ?? 0])
        }
        XCTAssertLessThan(highest[0], highest[1])
        XCTAssertLessThan(highest[1], highest[2])
        // A wave begins with every dot on its line and ends so, and then it begins again.
        XCTAssertEqual(Wave.lifts(at: 0), [0, 0, 0])
        XCTAssertEqual(Wave.lifts(at: Motion.wave * 0.9), [0, 0, 0])
        for moment in [0.1, 0.5, 0.8] {
            let one = Wave.lifts(at: moment), next = Wave.lifts(at: moment + Motion.wave)
            for dot in 0..<3 { XCTAssertEqual(one[dot], next[dot], accuracy: 0.0001) }
        }
    }

    // MARK: What is drawn

    private struct Picture {
        let pixels: NSBitmapImageRep
        let scale: CGFloat

        /// Red, green and blue from 0 to 255 at a place counted in the picture's own dots from the top left.
        func colour(_ x: Int, _ y: Int) -> [Int] {
            guard let colour = pixels.colorAt(x: x, y: y), colour.numberOfComponents >= 3 else { return [-1, -1, -1] }
            var parts = [CGFloat](repeating: 0, count: colour.numberOfComponents)
            colour.getComponents(&parts)
            return parts.prefix(3).map { Int(($0 * 255).rounded()) }
        }
    }

    private struct Dot: Hashable {
        let x: Int
        let y: Int
    }

    /// One sign of the sidebar's first column: its dots, counted from its own upper left
    /// corner, and their colours.
    private struct Seen {
        let dots: Set<Dot>
        let colours: [[Int]]
    }

    private func far(_ one: [Int], _ other: [Int]) -> Int {
        zip(one, other).map { abs($0 - $1) }.max() ?? 255
    }

    /// The colour most of a sign's dots have.
    private func most(_ colours: [[Int]]) -> [Int] {
        var counts: [[Int]: Int] = [:]
        for colour in colours { counts[colour, default: 0] += 1 }
        return counts.max { $0.value < $1.value }?.key ?? [-1, -1, -1]
    }

    /// The sidebar as the app draws it at a width, kept as a picture when TILO_SHOTS_DIR names a folder.
    private func drawn(_ model: AssistantModel, _ scheme: ColorScheme, width: CGFloat, as name: String) async -> Picture {
        if let faces = ProcessInfo.processInfo.environment["TILO_FONTS_DIR"] { Typeface.register(in: URL(fileURLWithPath: faces)) }
        let size = CGSize(width: width, height: 360)
        let view = AssistantRoot(model: model) {
            Sidebar().background(Token.content)
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
            let file = "sidebar-\(name)-\(Int(width))-\(scheme == .dark ? "dark" : "light").png"
            try? pixels.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: folder).appendingPathComponent(file))
        }
        return Picture(pixels: pixels, scale: CGFloat(pixels.pixelsWide) / size.width)
    }

    /// The first two signs of the sidebar's first column, top to bottom: the head's and the main
    /// chat's. A sign begins 18 or 20 points from the left edge and is 18 wide, and the words
    /// beside it begin at 44. Whatever between 16 and 40 points is not the colour of the ground
    /// 14 points from the edge on its own line of dots belongs to a sign: so a chosen row's
    /// ground is no part of its sign. A sign is at least 8 points high.
    private func signs(in picture: Picture) -> [Seen] {
        let scale = picture.scale
        let beside = Int(14 * scale), left = Int(16 * scale), right = Int(40 * scale)
        var found: [Seen] = []
        var lines: [(y: Int, inked: [Int])] = []
        func close() {
            defer { lines = [] }
            guard CGFloat(lines.count) >= 8 * scale, let top = lines.first?.y else { return }
            let first = lines.flatMap(\.inked).min() ?? left
            var dots: Set<Dot> = []
            var colours: [[Int]] = []
            for line in lines {
                for x in line.inked {
                    dots.insert(Dot(x: x - first, y: line.y - top))
                    colours.append(picture.colour(x, line.y))
                }
            }
            found.append(Seen(dots: dots, colours: colours))
        }
        for y in 0..<Int(150 * scale) {
            let ground = picture.colour(beside, y)
            let inked = (left..<right).filter { far(picture.colour($0, y), ground) > 40 }
            if inked.isEmpty { close() } else { lines.append((y, inked)) }
        }
        close()
        return Array(found.prefix(2))
    }

    /// How much of two signs is not shared: 0 for one sign drawn twice, towards 1 for two that differ.
    private func apart(_ one: Seen, _ other: Seen) -> Double {
        let all = one.dots.union(other.dots).count
        return all == 0 ? 0 : 1 - Double(one.dots.intersection(other.dots).count) / Double(all)
    }

    private func model(_ source: MemorySource = Drawing.source()) async -> AssistantModel {
        let model = AssistantModel(source: source)
        model.start()
        await until("the main chat is held") { model.conversation(.main).isLoaded && model.person != nil }
        return model
    }

    /// The owner's picture: the logo beside "Assistant" and the same logo beside "Main chat".
    /// The head keeps the logo; the row shows another sign. In both themes, at the Mac's
    /// sidebar and at an iPhone's width, with the main chat chosen and not.
    func testTheHeadAndTheMainChatsRowDoNotShowTheSameSign() async {
        for scheme in [ColorScheme.light, .dark] {
            for (width, name) in [(Measure.sidebar, "mac"), (CGFloat(393), "phone")] {
                for chosen in [true, false] {
                    let model = await model()
                    if !chosen { model.place = .assistantView }
                    let seen = signs(in: await drawn(model, scheme, width: width, as: "\(name)-\(chosen ? "chosen" : "plain")"))
                    let what = "\(scheme), \(name), the main chat \(chosen ? "chosen" : "not chosen")"
                    guard seen.count == 2 else { XCTFail("\(what): \(seen.count) signs were found where the head's and the row's are"); continue }
                    XCTAssertGreaterThan(apart(seen[0], seen[1]), 0.3, "\(what): the head and the row show the same sign")
                    // The head's is the logo still: more than one colour, the assistant's own most of all.
                    let logo = most(seen[0].colours)
                    XCTAssertGreaterThan(seen[0].colours.filter { far($0, logo) > 40 }.count, seen[0].colours.count / 8,
                                         "\(what): the head's sign is of one colour, so it is not the logo")
                    // The row's is of one colour, and that is not the logo's: it is in the row's ink.
                    let ink = most(seen[1].colours)
                    XCTAssertGreaterThan(far(ink, logo), 60, "\(what): the row's sign is in the logo's colour \(logo)")
                }
            }
        }
    }

    /// While the assistant writes in the main chat, the three dots of its sign take the
    /// assistant's colour, which is the colour of the logo's middle; at rest nothing of the
    /// sign has it. The bubble around them keeps the row's ink.
    func testWhileTheAssistantWritesTheSignsDotsTakeItsColourAndAtRestNothingOfTheSignHasIt() async {
        for scheme in [ColorScheme.light, .dark] {
            let source = Drawing.source()
            let model = await model(source)
            let rests = signs(in: await drawn(model, scheme, width: Measure.sidebar, as: "at-rest"))
            guard rests.count == 2 else { XCTFail("\(scheme): \(rests.count) signs at rest"); continue }
            let logo = most(rests[0].colours), ink = most(rests[1].colours)
            XCTAssertEqual(rests[1].colours.filter { far($0, logo) <= 40 }.count, 0, "\(scheme): at rest the row's sign has the assistant's colour")

            source.setAnswering(true)
            await until("the main chat's row writes") { model.isWriting(.main) }
            let writes = signs(in: await drawn(model, scheme, width: Measure.sidebar, as: "writing"))
            guard writes.count == 2 else { XCTFail("\(scheme): \(writes.count) signs while writing"); continue }
            XCTAssertGreaterThanOrEqual(writes[1].colours.filter { far($0, logo) <= 40 }.count, 6,
                                        "\(scheme): while the assistant writes, the dots are not in its colour \(logo)")
            XCTAssertGreaterThan(writes[1].colours.filter { far($0, ink) <= 12 }.count, writes[1].colours.count / 3,
                                 "\(scheme): the bubble lost the row's ink \(ink) while the assistant writes")
        }
    }

    // MARK: What is read out

    /// A row that writes is read out as writing, by the word the conversation says under a
    /// reply in work, and a row at rest says nothing of it.
    func testARowThatWritesIsReadOutAsWritingByTheConversationsOwnWord() async throws {
        let started = try await Started.app()
        defer { started.end() }
        let drive = started.drive, source = started.source.inner
        typealias W = AssistantWords
        let agent = "Usage drop: investigate, Coder, working"
        func value(_ label: String) -> String? { drive.nodes(label, role: "AXButton").first?.value }
        XCTAssertEqual(value(W.mainChat.said), "", "the main chat's row said something of writing at rest")
        XCTAssertEqual(value(agent), "")

        source.setAnswering(true)
        await drive.until("the main chat's row is read out as writing") { value(W.mainChat.said) == W.answering.said }
        XCTAssertEqual(value(agent), "", "a sub-agent that does not write was read out as writing")
        source.setAnswering(false)
        await drive.until("the main chat's row says nothing of writing again") { value(W.mainChat.said) == "" }

        source.setAnswering(true, in: .subAgent("usage-drop"))
        await drive.until("the sub-agent's row is read out as writing") { value(agent) == W.answering.said }
        XCTAssertEqual(value(W.mainChat.said), "")
        drive.picture("mac-window-a-sub-agent-writes")
    }
}
#endif
