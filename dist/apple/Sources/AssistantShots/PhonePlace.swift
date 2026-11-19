#if os(iOS)
import AssistantCore
import AssistantDrawing
import AssistantUI
import SwiftUI
import UIKit

/// What the picture tool asks of a window as the system does: where something is read out, and
/// the menu a held finger would be handed.
extension PhoneShots {
    /// Where something that is read out as `words` stands; `except` leaves out what also says that.
    static func spoken(_ object: NSObject, _ words: String, except: String? = nil) -> CGRect? {
        if (object as? UIView)?.isHidden == true || object.accessibilityElementsHidden { return nil }
        if object.isAccessibilityElement {
            let said = (object.accessibilityLabel ?? "") + "\n" + (object.accessibilityValue ?? "")
            guard said.contains(words), except.map({ !said.contains($0) }) ?? true else { return nil }
            return object.accessibilityFrame
        }
        var children: [NSObject] = []
        if let listed = object.accessibilityElements {
            children = listed.compactMap { $0 as? NSObject }
        } else {
            let count = object.accessibilityElementCount()
            if count != NSNotFound, count > 0 {
                children = (0..<count).compactMap { object.accessibilityElement(at: $0) as? NSObject }
            } else {
                children = (object as? UIView)?.subviews ?? []
            }
        }
        for child in children { if let place = spoken(child, words, except: except) { return place } }
        return nil
    }
    static func holders(_ view: UIView) -> [(UIView, UIContextMenuInteraction)] {
        view.interactions.compactMap { held in (held as? UIContextMenuInteraction).map { (view, $0) } } + view.subviews.flatMap(holders)
    }
    static func leaves(_ menu: UIMenu) -> [UIMenuElement] {
        menu.children.flatMap { ($0 as? UIMenu).map(leaves) ?? [$0] }
    }
    /// The rows of a menu's configuration, in their order.
    static func menuRows(of configuration: UIContextMenuConfiguration) -> [UIMenuElement]? {
        typealias Provider = @convention(block) ([UIMenuElement]) -> UIMenu?
        guard configuration.responds(to: Selector(("actionProvider"))), let raw = configuration.value(forKey: "actionProvider"),
              let menu = unsafeBitCast(raw as AnyObject, to: Provider.self)([])
        else { return nil }
        return leaves(menu)
    }

    /// The scroll view that holds a place of the window; a text's own is not meant.
    static func conversationScroll(holding point: CGPoint, in view: UIView) -> UIScrollView? {
        for sub in view.subviews.reversed() { if let found = conversationScroll(holding: point, in: sub) { return found } }
        guard let found = view as? UIScrollView, !(view is UITextView), !view.isHidden, view.convert(view.bounds, to: nil).contains(point)
        else { return nil }
        return found
    }

    /// The drawing's conversations; `long`: older lines stand above the main chat's, which is then
    /// longer than the screen; `arrives`: how long after they are asked for a conversation's pieces come.
    static func drawnSource(long: Bool, arrives: Duration? = nil) -> MemorySource {
        var content = Drawing.content()
        if long {
            content.main = (1...30).map { line in
                Item(id: Item.ID("older-\(line)"), at: Drawing.now.addingTimeInterval(Double(line) * 60 - 200_000),
                     .message(Message(.assistant, "More was said before, line \(line).")))
            } + content.main
        }
        return MemorySource(content, now: { Drawing.now }, arrives: arrives)
    }

    /// Where each message of the main chat says it stands to someone who cannot see it, against
    /// where a held finger reaches it, at the chat's first showing, and whether the chat rests at
    /// its end there. `way` is how the chat comes to be shown: `chat` with the list of
    /// conversations under it; `start` the same as the app itself starts (its view is made before
    /// its model knows who is signed in); `late` as `start`, with the chat's pieces arriving a
    /// second after they are asked for, as a site's do; `active` the view made once the app is in
    /// front; `list` the chat opened from the list of conversations; `alone` without the list.
    /// A finger reaches a message where the menu it would be handed copies that message's
    /// words: the menu is asked for as the system asks and never opened, and Copy writes a
    /// pasteboard of the tool's own. `long`: a chat longer than the screen; `dark`: in the dark look.
    /// A line FAULT is the tool's own refusal.
    static func place(in window: UIWindow, first way: String, long: Bool, dark: Bool) async {
        Typeface.registerBundled()
        Spoken.wake()
        let kept = Kept("place.txt", word: "PLACE")
        defer { kept.say("done") }
        let late = way == "late"
        let model = AssistantModel(source: drawnSource(long: long, arrives: late ? .seconds(1) : nil))
        model.showsPanelInMain = false
        let board = UIPasteboard.withUniqueName()
        model.pasteboard = board
        if way != "start", !late { await model.prime() }
        // `active` and `list`: the view is made once the app is in front, as a person's own start has it.
        if way == "active" || way == "list" {
            while UIApplication.shared.applicationState != .active { try? await Task.sleep(for: .milliseconds(100)) }
            try? await Task.sleep(for: .milliseconds(500))
        }
        // `list`: the list of conversations stands first, and the chat is opened from it.
        if way == "list" { model.place = .assistantView }
        let look: ColorScheme = dark ? .dark : .light
        let view = way == "alone"
            ? Screens.rooted(model, look) { VStack(spacing: 0) { ConversationView(chat: .main); Composer(chat: .main) } }
            : Screens.rooted(model, look) { PhoneRoot(path: way == "list" ? [] : [.chat(.main)]) }
        window.overrideUserInterfaceStyle = dark ? .dark : .light
        window.rootViewController = UIHostingController(rootView: view)
        window.layoutIfNeeded()
        if way == "start" || late { model.start() }
        // The chat is there once its model has read it; then the window settles.
        while model.conversation(.main).items.isEmpty { try? await Task.sleep(for: .milliseconds(100)) }
        try? await Task.sleep(for: .milliseconds(1500))
        if way == "list" {
            model.place = .chat(.main)
            try? await Task.sleep(for: .milliseconds(1500))
        }
        let shown = "first=\(way)\(long ? ", a chat longer than the screen" : "")\(dark ? ", dark" : "")"

        // The chat's messages, the newest first: the line each is read out by, and the words Copy takes of it.
        var messages: [(id: Item.ID, line: String)] = []
        var copied: [String: Item.ID] = [:]
        for item in model.conversation(.main).items.reversed() {
            guard case .message(let message) = item.content else { continue }
            let plain = PieceWords(of: item.id, message.text).plain
            guard let line = plain.split(separator: "\n").first else { continue }
            messages.append((item.id, String(line.prefix(40))))
            copied[plain] = item.id
        }
        /// Where each message says it stands, to someone who cannot see it.
        func readOut() -> [Item.ID: CGRect] {
            var places: [Item.ID: CGRect] = [:]
            for message in messages {
                if let place = spoken(window, message.line, except: AssistantWords.replyingTo.said) { places[message.id] = place }
            }
            return places
        }
        let menu = [AssistantWords.reply, .quote, .copy, .selectText].map(\.said)
        // UIKit's own menus (a text's, a bar's) are not a message's, and refuse to be asked out of turn.
        let held = holders(window).filter { pair in
            guard let answers = pair.1.delegate as? NSObject else { return false }
            return Bundle(for: type(of: answers)).bundleIdentifier != "com.apple.UIKitCore"
        }
        /// The message a finger held at a place of the window would be handed the menu of.
        func reached(_ point: CGPoint) async -> Item.ID? {
            for (view, interaction) in held {
                let inside = view.convert(point, from: nil)
                guard view.bounds.contains(inside),
                      let given = interaction.delegate?.contextMenuInteraction(interaction, configurationForMenuAtLocation: inside),
                      let rows = menuRows(of: given), rows.map(\.title) == menu, let copy = rows[2] as? UIAction
                else { continue }
                board.items = []
                copy.performWithSender(nil, target: nil)
                // Copy is done once its words are on the pasteboard.
                var asked = 0
                while board.string == nil, asked < 20 {
                    try? await Task.sleep(for: .milliseconds(10))
                    asked += 1
                }
                return board.string.flatMap { copied[$0] }
            }
            return nil
        }
        /// Between which heights, and which places across, a finger reaches a message.
        struct Span {
            var top: CGFloat
            var foot: CGFloat
            var left: CGFloat
            var right: CGFloat
        }
        let across: CGFloat = 60
        let down: CGFloat = 4
        func reaches() async -> [Item.ID: Span] {
            var found: [Item.ID: Span] = [:]
            for y in stride(from: down, to: window.bounds.height, by: down) {
                for x in stride(from: across / 2, to: window.bounds.width, by: across) {
                    guard let id = await reached(CGPoint(x: x, y: y)) else { continue }
                    var span = found[id] ?? Span(top: y, foot: y, left: x, right: x)
                    span.foot = y
                    span.left = min(span.left, x)
                    span.right = max(span.right, x)
                    found[id] = span
                }
            }
            return found
        }

        kept.say("\(shown): asked on a screen \(Int(window.bounds.width)) by \(Int(window.bounds.height)), \(messages.count) messages in the chat")
        save(picture(of: window), "place-\(way)\(long ? "-long" : "")\(dark ? "-dark" : "").png")
        // What is read out is taken before anything is asked of the window.
        let read = readOut()
        let drawn = await reaches()
        guard let any = drawn.values.first else {
            return kept.say("FAULT \(shown): a held finger reaches no message anywhere in the window")
        }
        let rests = conversationScroll(holding: CGPoint(x: any.left, y: any.top), in: window)
        let sight = rests.map { $0.convert($0.bounds.inset(by: $0.adjustedContentInset), to: nil) } ?? window.bounds
        var off: [String] = []
        var stand = 0
        var judged = ""
        for (index, message) in messages.enumerated() {
            let name = index == 0 ? "the newest message" : "message \(index + 1) from the end"
            switch (drawn[message.id], read[message.id]) {
            case (let span?, let frame?):
                stand += 1
                // A message cut by the upper edge of what shows may have its first words out of sight: not judged.
                if span.top <= sight.minY + down { continue }
                // The middle of what is read out lies on the message, to the scan's step.
                let away = max(span.top - down - frame.midY, frame.midY - span.foot - down, 0)
                let beside = max(span.left - across - frame.midX, frame.midX - span.right - across, 0)
                if away > 0 || beside > 0 {
                    off.append("\(name) says its middle is \(Int(frame.midX)) across and \(Int(frame.midY)) down, and a finger reaches it between \(Int(span.top)) and \(Int(span.foot)) down")
                }
                if judged.isEmpty {
                    judged = "\(name) is read out between \(Int(frame.minY)) and \(Int(frame.maxY)) down and reached between \(Int(span.top)) and \(Int(span.foot))"
                }
            case (let span?, nil):
                stand += 1
                off.append("\(name) is reached between \(Int(span.top)) and \(Int(span.foot)) down and is not read out at all")
            case (nil, let frame?):
                if sight.contains(CGPoint(x: frame.midX, y: frame.midY)) {
                    off.append("\(name) says it stands between \(Int(frame.minY)) and \(Int(frame.maxY)) down, where no finger reaches it")
                }
            case (nil, nil):
                break
            }
        }
        // A chat shown for the first time rests at its end: its newest message is in sight, where a finger reaches it.
        if let last = messages.first, drawn[last.id] == nil {
            let says = read[last.id].map { "; it says it stands between \(Int($0.minY)) and \(Int($0.maxY)) down" } ?? ""
            off.append("the chat does not rest at its end: its newest message is reached nowhere\(says)")
        }
        let rest = rests.map { (scroll: UIScrollView) -> String in
            let inset = scroll.adjustedContentInset
            let short = scroll.contentSize.height - scroll.bounds.height + inset.bottom - scroll.contentOffset.y
            return "the conversation rests at \(Int(scroll.contentOffset.y)) of \(Int(scroll.contentSize.height)) with \(Int(scroll.bounds.height)) shown, \(Int(inset.top)) of it under what stands above and \(Int(inset.bottom)) under what stands below: \(Int(short)) short of its end"
        } ?? "no scroll view holds the messages"
        if off.isEmpty, stand > 0 {
            kept.say("\(shown): the chat rests at its end, and each of the \(stand) messages in view is read out where a held finger reaches it (\(judged); \(rest))")
        } else {
            kept.say("FAULT \(shown): \(off.prefix(4).joined(separator: "; ")); a finger reaches \(stand) messages; \(rest)")
        }
    }
}
#endif
