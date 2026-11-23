import AssistantCore
import SwiftUI

#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// A piece's words as its menu takes them: what Copy puts on the pasteboard, and what Select
/// text lays out for a person to mark a part of.
public struct PieceWords: Identifiable, Equatable, Sendable {
    /// The piece they are the words of.
    public var id: Item.ID
    /// Markdown, as the piece holds it.
    public var text: String
    /// Whether the words are laid out to quote a part of them: Quote in the piece's menu.
    public var quotes = false

    public init(of piece: Item.ID, _ text: String) {
        self.id = piece
        self.text = text
    }

    /// The words as plain text, every line where it stood.
    public var plain: String { Markdown.copied(text) }
}

/// What a person can do with a piece's words, in the order the piece's menu offers it.
public enum PieceAction: CaseIterable, Sendable {
    case reply, quote, copy, selectText

    var words: AssistantWords {
        switch self {
        case .reply: .reply
        case .quote: .quote
        case .copy: .copy
        case .selectText: .selectText
        }
    }
}

extension AssistantModel {
    /// Do what a row of a piece's menu says.
    public func take(_ action: PieceAction, of words: PieceWords) {
        switch action {
        case .reply:
            reply(to: words.id, saying: words.text)
        case .quote:
            // A quote is a part of the words: they are laid out, and the person selects it there.
            var laid = words
            laid.quotes = true
            selecting = laid
        case .copy:
            #if os(macOS)
            pasteboard.clearContents()
            pasteboard.setString(words.plain, forType: .string)
            #else
            pasteboard.string = words.plain
            #endif
        case .selectText:
            selecting = words
        }
    }
}

/// The menu of a piece of words: a long press on an iPhone, the second button or Control and a
/// click on a Mac.
struct PieceMenu: ViewModifier {
    let words: PieceWords?
    /// Whether, on a Mac, a drag marks the words where they stand: a message's do.
    var marks = false
    @Environment(AssistantModel.self) private var model

    func body(content: Content) -> some View {
        if let words {
            content
                #if os(macOS)
                .modifier(MarkedInPlace(rows: marks ? PieceAction.allCases.map { action in
                    SecondButtonView.Row(title: action.words.said) { model.take(action, of: words) }
                } : nil))
                #else
                // Lifted by its menu on an iPhone, the piece keeps its round corners.
                .contentShape(.contextMenuPreview, RoundedRectangle(cornerRadius: Measure.bubbleRadius))
                // A swipe to the right answers the piece, as Reply in its menu does.
                .modifier(SwipeToReply { model.take(.reply, of: words) })
                #endif
                .contextMenu {
                    ForEach(PieceAction.allCases, id: \.self) { action in
                        Button(action.words.said) { model.take(action, of: words) }
                    }
                }
        } else {
            content
        }
    }
}

/// A piece's words laid out for a person to mark a part of them and copy it: the system's own
/// text view, with its handles and its menu on an iPhone and a drag on a Mac. Nothing in it can
/// be changed.
struct SelectTextSheet: View {
    let words: PieceWords
    @Environment(\.dismiss) private var dismiss
    @Environment(AssistantModel.self) private var model
    /// The part of the words that is marked now. Empty: none is.
    @State private var marked = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text((words.quotes ? AssistantWords.selectToQuote : AssistantWords.selectText).said).font(.title15).foregroundStyle(Token.ink).accessibilityAddTraits(.isHeader)
                Spacer(minLength: 0)
                // With words marked, they can be answered: Quote puts them above the field.
                Button(AssistantWords.quote.said) { quote() }
                    .buttonStyle(.plain)
                    .font(.small12)
                    .foregroundStyle(marked.isEmpty ? Token.ink3 : Token.ink)
                    .disabled(marked.isEmpty)
                    .padding(.trailing, 10)
                Button {
                    dismiss()
                } label: {
                    Glyph(.x).contentShape(Rectangle().inset(by: -6))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Token.ink3)
                .keyboardShortcut(.cancelAction)
                .accessibilityLabel(AssistantWords.close.said)
            }
            SelectableWords(words: words.plain, marked: $marked, quote: quote)
        }
        .padding(16)
        #if os(macOS)
        .frame(minWidth: 320, idealWidth: 460, minHeight: 260, idealHeight: 380)
        #else
        // Under the sheet's own handle, and as wide and as tall as the sheet stands.
        .padding(.top, 8)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        #endif
        .background(Token.content)
    }

    /// Answer the marked part of the piece: it stands above the field, and the words are put away.
    private func quote() {
        let part = marked.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !part.isEmpty else { return }
        model.reply(to: words.id, saying: words.text, part: part)
        dismiss()
    }
}

#if os(macOS)
/// On a Mac a drag marks a message's words where they stand. Words that can be marked answer the
/// second button with the system's own menu for text, so a view of the app's lies over them
/// and takes the second button alone: `rows` are the menu it opens. Nil: nothing is marked in
/// place, and the piece's own menu answers the second button.
private struct MarkedInPlace: ViewModifier {
    let rows: [SecondButtonView.Row]?

    func body(content: Content) -> some View {
        if let rows {
            content
                .textSelection(.enabled)
                .overlay { SecondButton(rows: rows).accessibilityHidden(true) }
        } else {
            content
        }
    }
}

/// The Mac's second button over a message: see `SecondButtonView`.
struct SecondButton: NSViewRepresentable {
    let rows: [SecondButtonView.Row]

    func makeNSView(context: Context) -> SecondButtonView {
        SecondButtonView()
    }

    func updateNSView(_ view: SecondButtonView, context: Context) {
        view.rows = rows
    }
}

/// A view that lies over a message and takes two presses alone: the second button, and Control
/// with a click. For those it opens the message's menu. Every other press goes past it to the
/// words under it, so a click and a drag reach them as if it were not there.
final class SecondButtonView: NSView {
    /// One row of the menu: what it says and what it does.
    struct Row {
        let title: String
        let act: () -> Void
    }

    var rows: [Row] = []

    override func hitTest(_ point: NSPoint) -> NSView? {
        guard let event = NSApp.currentEvent else { return nil }
        let asksForMenu = event.type == .rightMouseDown
            || (event.type == .leftMouseDown && event.modifierFlags.contains(.control))
        return asksForMenu ? super.hitTest(point) : nil
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        let menu = NSMenu()
        for (index, row) in rows.enumerated() {
            let item = NSMenuItem(title: row.title, action: #selector(chose(_:)), keyEquivalent: "")
            item.target = self
            item.tag = index
            menu.addItem(item)
        }
        return menu
    }

    @objc private func chose(_ item: NSMenuItem) {
        guard rows.indices.contains(item.tag) else { return }
        rows[item.tag].act()
    }
}

/// Words in the system's text view: they can be marked and copied, and not changed.
struct SelectableWords: NSViewRepresentable {
    let words: String
    /// The part that is marked now, told as it changes.
    @Binding var marked: String
    /// Quote the marked part: what the row in the text's own menu does.
    let quote: () -> Void
    @Environment(\.colorScheme) private var scheme

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.drawsBackground = false
        scroll.borderType = .noBorder
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        let view = NSTextView(usingTextLayoutManager: false)
        view.isEditable = false
        view.isSelectable = true
        view.isRichText = false
        view.drawsBackground = false
        view.textContainerInset = .zero
        view.textContainer?.lineFragmentPadding = 0
        view.textContainer?.widthTracksTextView = true
        view.isVerticallyResizable = true
        view.isHorizontallyResizable = false
        view.autoresizingMask = [.width]
        view.minSize = .zero
        view.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        view.delegate = context.coordinator
        scroll.documentView = view
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.parent = self
        guard let view = scroll.documentView as? NSTextView else { return }
        if view.string != words { view.string = words }
        view.font = PlatformFont.tilo(14)
        view.textColor = Token.ink.platform(scheme)
    }

    /// Hears what is marked, and puts Quote first in the menu the text opens over marked words.
    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: SelectableWords

        init(_ parent: SelectableWords) {
            self.parent = parent
        }

        func textViewDidChangeSelection(_ notification: Notification) {
            guard let view = notification.object as? NSTextView else { return }
            let part = (view.string as NSString).substring(with: view.selectedRange())
            if parent.marked != part { parent.marked = part }
        }

        func textView(_ view: NSTextView, menu: NSMenu, for event: NSEvent, at charIndex: Int) -> NSMenu? {
            guard view.selectedRange().length > 0 else { return menu }
            let item = NSMenuItem(title: AssistantWords.quote.said, action: #selector(quoted), keyEquivalent: "")
            item.target = self
            menu.insertItem(item, at: 0)
            return menu
        }

        @objc private func quoted() {
            parent.quote()
        }
    }
}
#else
/// Words in the system's text view: they can be marked and copied, and not changed.
struct SelectableWords: UIViewRepresentable {
    let words: String
    /// The part that is marked now, told as it changes.
    @Binding var marked: String
    /// Quote the marked part: what the row in the text's own menu does.
    let quote: () -> Void
    @Environment(\.colorScheme) private var scheme

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.isEditable = false
        view.isSelectable = true
        view.backgroundColor = .clear
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.alwaysBounceVertical = true
        view.delegate = context.coordinator
        return view
    }

    func updateUIView(_ view: UITextView, context: Context) {
        context.coordinator.parent = self
        if view.text != words { view.text = words }
        view.font = PlatformFont.tilo(14)
        view.textColor = Token.ink.platform(scheme)
    }

    /// Hears what is marked, and puts Quote first in the menu the system opens over marked words.
    @MainActor
    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: SelectableWords

        init(_ parent: SelectableWords) {
            self.parent = parent
        }

        func textViewDidChangeSelection(_ view: UITextView) {
            let range = view.selectedRange
            let part = range.length > 0 ? ((view.text ?? "") as NSString).substring(with: range) : ""
            if parent.marked != part { parent.marked = part }
        }

        func textView(_ view: UITextView, editMenuForTextIn range: NSRange, suggestedActions: [UIMenuElement]) -> UIMenu? {
            guard range.length > 0 else { return nil }
            let quote = UIAction(title: AssistantWords.quote.said) { [weak self] _ in self?.parent.quote() }
            return UIMenu(children: [quote] + suggestedActions)
        }
    }
}
#endif
