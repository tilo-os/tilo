import AssistantCore
import ShellWeb
import SwiftUI
import UniformTypeIdentifiers

/// The message field: tall, with room around it, Markdown formatted in place while it is typed,
/// attachments and voice under it. Return sends, Shift Return makes a line.
public struct Composer: View {
    private let chat: Chat
    @Environment(AssistantModel.self) private var model
    @Environment(\.colorScheme) private var scheme
    @Environment(\.dynamicTypeSize) private var textSize
    @State private var picksFile = false
    @State private var listening = Listening()

    public init(chat: Chat) {
        self.chat = chat
    }

    public var body: some View {
        let text = Binding<String>(
            get: { model.drafts[chat]?.text ?? "" },
            set: { typed in
                var draft = model.drafts[chat] ?? Draft(text: "")
                draft.text = typed
                model.drafts[chat] = draft
            })
        let files = model.drafts[chat]?.files ?? []
        let isEmpty = (model.drafts[chat] ?? Draft(text: "")).isEmpty
        VStack(alignment: .leading, spacing: 8) {
            VStack(alignment: .leading, spacing: 10) {
            if let answered = model.drafts[chat]?.answers {
                // What is answered stands above the words, with a cross that drops it.
                AnsweredLine(answered: answered, drop: { model.drafts[chat]?.answers = nil })
            }
            HStack(alignment: .top, spacing: 12) {
                ZStack(alignment: .topLeading) {
                    if text.wrappedValue.isEmpty {
                        Text(placeholder).font(.field15).foregroundStyle(Token.ink3)
                            .lineLimit(textSize.isAccessibilitySize ? 2 : 1).minimumScaleFactor(textSize.isAccessibilitySize ? 0.7 : 1)
                            .allowsHitTesting(false).accessibilityHidden(true)
                    }
                    MarkdownField(text: text, label: placeholder, focus: model.fieldCalls, sent: { model.conversation(chat).items }) {
                        model.send(in: chat)
                    }
                        // As tall as its words up to its tallest, whatever room is left: the conversation gives way, not the field.
                        .fixedSize(horizontal: false, vertical: true)
                        .offset(y: -FieldStyle.rise)
                }
                .frame(minHeight: 26, alignment: .top)
                // While an answer is being written and nothing is typed, the same place stops it.
                let stops = isEmpty && model.conversation(chat).isAnswering
                Button {
                    if stops { model.stopAnswering(in: chat) } else { model.send(in: chat) }
                } label: {
                    Glyph(stops ? .stop : .ret).frame(minWidth: 26, minHeight: 22, alignment: .trailing).contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .foregroundStyle(isEmpty && !stops ? Token.ink3 : Token.ink)
                .disabled(isEmpty && !stops)
                .accessibilityLabel((stops ? AssistantWords.stop : AssistantWords.send).said)
            }
            }
            .padding(16)
            .background {
                // The shadow is the ground's alone. Set on the field whole, its line cast one onto the
                // ground inside it, and on an iPhone its words and its arrow each cast one too.
                RoundedRectangle(cornerRadius: Measure.bubbleRadius).fill(Token.content)
                    .shadow(color: Token.fieldShadow.color(scheme), radius: 1.5, y: 1)
            }
            .overlay { RoundedRectangle(cornerRadius: Measure.bubbleRadius).strokeBorder(Token.line, lineWidth: 1) }

            if !files.isEmpty {
                Flow(spacing: 8, lineSpacing: 6) {
                    ForEach(files) { file in
                        HStack(spacing: 6) {
                            Text(file.name).font(.tag115).foregroundStyle(Token.ink2).lineLimit(1)
                            Button {
                                remove(file)
                            } label: {
                                Glyph(.x, side: 11)
                            }
                            .buttonStyle(.plain)
                            .foregroundStyle(Token.ink3)
                            .accessibilityLabel(AssistantWords.removeFile.said(["name": file.name]))
                        }
                        .padding(.horizontal, 9).padding(.vertical, 3)
                        .background(Token.raised, in: Capsule())
                    }
                }
                .padding(.horizontal, 6)
            }

            HStack(spacing: 14) {
                Button {
                    picksFile = true
                } label: {
                    Glyph(.plus).contentShape(Rectangle().inset(by: -6))
                }
                .buttonStyle(.plain)
                .accessibilityLabel(AssistantWords.attach.said)
                .tip(AssistantWords.attach.said)
                Button {
                    listening.toggle(into: text)
                } label: {
                    Glyph(.mic).contentShape(Rectangle().inset(by: -6))
                }
                .buttonStyle(.plain)
                .foregroundStyle(listening.isOn ? Token.you : Token.ink2)
                .accessibilityLabel((listening.isOn ? AssistantWords.stopDictating : AssistantWords.dictate).said)
                .tip((listening.isOn ? AssistantWords.stopDictating : AssistantWords.dictate).said)
                if let problem = listening.problem {
                    Text(problem).font(.small12).foregroundStyle(Token.ink3).lineLimit(2)
                }
                if chat == .main, model.setup == .ready, text.wrappedValue.isEmpty,
                   files.isEmpty, model.drafts[chat]?.answers == nil, !listening.isOn {
                    Menu(AssistantWords.askAboutLookRound.said) {
                        ForEach([AssistantWords.askLookRoundSettings, .askLookRoundOn,
                                 .askLookRoundSchedule, .askLookRoundOff], id: \.self) { prompt in
                            Button(prompt.said) { model.begin(prompt.said) }
                        }
                    }
                    .menuStyle(.button)
                    .buttonStyle(.plain)
                    .font(.small12)
                }
                Spacer(minLength: 0)
                #if os(macOS)
                // The drawing shows how to send while something is being typed.
                if !text.wrappedValue.isEmpty {
                    Text(AssistantWords.fieldHint.said).font(.small12).foregroundStyle(Token.ink3)
                        .accessibilityLabel(AssistantWords.fieldHint.said.spoken)
                }
                #endif
            }
            .foregroundStyle(Token.ink2)
            .padding(.horizontal, 6)
        }
        .frame(maxWidth: Measure.column)
        .padding(.horizontal, Measure.columnGutter)
        .padding(.bottom, 14)
        .frame(maxWidth: .infinity)
        // As tall as it is, whatever room is left: where room is short the conversation gives way.
        .fixedSize(horizontal: false, vertical: true)
        .fileImporter(isPresented: $picksFile, allowedContentTypes: [.item], allowsMultipleSelection: true) { picked in
            guard case .success(let urls) = picked else { return }
            var draft = model.drafts[chat] ?? Draft(text: "")
            for url in urls where !draft.files.contains(where: { $0.url == url }) {
                // The file stays the app's to read until the message is sent or the file is taken off.
                _ = url.startAccessingSecurityScopedResource()
                draft.files.append(Draft.File(url: url))
            }
            model.drafts[chat] = draft
        }
        .dropDestination(for: URL.self) { urls, _ in
            var draft = model.drafts[chat] ?? Draft(text: "")
            for url in urls where url.isFileURL && !draft.files.contains(where: { $0.url == url }) {
                draft.files.append(Draft.File(url: url))
            }
            model.drafts[chat] = draft
            return !urls.isEmpty
        }
    }

    private var placeholder: String {
        (chat == .main ? AssistantWords.messageAssistant : AssistantWords.messageSubAgent).said
    }

    private func remove(_ file: Draft.File) {
        file.url.stopAccessingSecurityScopedResource()
        model.drafts[chat]?.files.removeAll { $0.url == file.url }
    }
}

/// Dictation into the field: the shell's, which turns speech into words on this device.
@MainActor
@Observable
final class Listening {
    private(set) var isOn = false
    private(set) var problem: String?
    @ObservationIgnored private var dictation: Dictation?

    func toggle(into text: Binding<String>) {
        if isOn {
            dictation?.stop()
            isOn = false
            return
        }
        problem = nil
        let dictation = dictation ?? Dictation()
        self.dictation = dictation
        let before = text.wrappedValue
        dictation.heard = { words in
            text.wrappedValue = before.isEmpty || before.hasSuffix(" ") || before.hasSuffix("\n") ? before + words : before + " " + words
        }
        dictation.ended = { [weak self] in self?.isOn = false }
        Task {
            do {
                try await dictation.start()
                self.isOn = true
            } catch {
                let notAllowed = (error as? Dictation.Problem) == .notAllowed
                self.problem = (notAllowed ? AssistantWords.dictationNotAllowed : AssistantWords.dictationUnavailable).said
            }
        }
    }
}

extension Markdown.Mark {
    var isListItem: Bool {
        if case .listItem = kind { true } else { false }
    }
}

/// How the field shows Markdown while it is typed: the words formatted in place, every
/// character kept, Markdown's own signs out of sight but where the person is writing.
@MainActor
enum FieldStyle {
    /// The ground under code in a line: its colour, kept with the letters it lies under. The
    /// drawing pads code by one point above and below and five beside, and rounds it by five.
    nonisolated static let ground = NSAttributedString.Key("so.tilo.assistant.field.ground")
    nonisolated static let groundPad: CGFloat = 5
    nonisolated static let groundRadius: CGFloat = 5

    static func base(_ scheme: ColorScheme) -> [NSAttributedString.Key: Any] {
        [.font: PlatformFont.tilo(15), .foregroundColor: Token.ink.platform(scheme),
         .paragraphStyle: paragraph(first: 0, rest: 0)]
    }

    /// A line of the drawing for letters of this size: 1.45 times as tall.
    private static func line(_ size: CGFloat) -> CGFloat {
        (PlatformFont.tilo(size).pointSize * 1.45).rounded()
    }

    /// The system's text view sets letters at the foot of a taller line, where the drawing has
    /// them in its middle: the field is drawn this much higher, so its first line stands where
    /// the drawing's does and where the words "Message your assistant" stood before it.
    static var rise: CGFloat {
        let font = PlatformFont.tilo(15)
        return max(0, (line(15) - (font.ascender - font.descender)) / 2)
    }

    static func apply(to storage: NSTextStorage, selection: NSRange?, scheme: ColorScheme) {
        let length = storage.length
        let marks = Markdown.marks(in: storage.string)
        func range(_ range: Range<Int>) -> NSRange? {
            self.range(range, in: length)
        }
        storage.beginEditing()
        storage.setAttributes(base(scheme), range: NSRange(location: 0, length: length))
        for mark in marks {
            guard let range = range(mark.range), range.length > 0 else { continue }
            switch mark.kind {
            case .strong:
                restyle(storage, range, bold: true)
            case .emphasis:
                restyle(storage, range, italic: true)
            case .strike:
                storage.addAttribute(.strikethroughStyle, value: NSUnderlineStyle.single.rawValue, range: range)
            case .code:
                storage.addAttribute(.font, value: PlatformFont.tiloMono(13), range: range)
                // Its ground reaches over its two signs, which give it the drawing's room on both sides.
                let from = max(0, range.location - 1), to = min(length, range.location + range.length + 1)
                storage.addAttribute(FieldStyle.ground, value: Token.raised.platform(scheme),
                                     range: NSRange(location: from, length: to - from))
            case .link:
                storage.addAttribute(.foregroundColor, value: Token.asst.platform(scheme), range: range)
            case .heading(let level):
                let size: CGFloat = level == 1 ? 20 : (level == 2 ? 17 : 15)
                storage.addAttributes([.font: PlatformFont.tilo(size, bold: true),
                                       .paragraphStyle: paragraph(first: 0, rest: 0, size: size)], range: range)
            case .listItem:
                // A list stands a little off the words before it, and its entries begin where
                // their lines go on under them: the room the drawing gives a list's number.
                let follows = marks.contains { $0.range.upperBound + 1 == mark.range.lowerBound && $0.isListItem }
                let before: CGFloat = follows || mark.range.lowerBound == 0 ? 0 : 4
                var rest: CGFloat = 20
                if let sign = marks.first(where: { $0.kind == .marker && mark.range.contains($0.range.lowerBound) }),
                   sign.range.upperBound < length, let signRange = self.range(sign.range, in: length) {
                    // A list's number is set in figures of one width, as a page sets it.
                    let face = mark.kind == .listItem(ordered: true) ? PlatformFont.tiloFigures(15) : PlatformFont.tilo(15)
                    storage.addAttribute(.font, value: face, range: signRange)
                    let written = storage.attributedSubstring(from: signRange).string + " "
                    let taken = (written as NSString).size(withAttributes: [.font: face]).width
                    // In a face whose number is wider than the drawing's room, the lines go on where the words begin.
                    rest = max(rest, 4.5 + taken)
                    if rest - 4.5 - taken > 0 {
                        storage.addAttribute(.kern, value: rest - 4.5 - taken, range: NSRange(location: sign.range.upperBound, length: 1))
                    }
                }
                storage.addAttribute(.paragraphStyle, value: paragraph(first: 4.5, rest: rest, before: before), range: range)
            case .quote:
                storage.addAttributes([.foregroundColor: Token.ink2.platform(scheme), .paragraphStyle: paragraph(first: 0, rest: 12)],
                                      range: range)
            case .codeBlock:
                storage.addAttributes([.font: PlatformFont.tiloMono(13), .backgroundColor: Token.raised.platform(scheme)], range: range)
            case .syntax:
                storage.addAttribute(.foregroundColor, value: Token.ink3.platform(scheme), range: range)
            case .marker:
                // A list's number, a quote's sign and a fence stay as they are typed, in the words' own colour.
                break
            }
        }
        let caret = selection.map { $0.location..<($0.location + $0.length) }
        for hidden in Markdown.hidden(in: marks, selection: caret) {
            guard let range = range(hidden) else { continue }
            // Still in the text, and taking no room: a sign shows again when the caret comes to it.
            storage.addAttributes([.font: PlatformFont.systemFont(ofSize: 0.01), .foregroundColor: PlatformColor.clear], range: range)
            // A sign of code that is out of sight leaves the room the drawing pads code with.
            if range.length == 1, storage.attribute(FieldStyle.ground, at: range.location, effectiveRange: nil) != nil {
                storage.addAttribute(.kern, value: FieldStyle.groundPad, range: range)
            }
        }
        storage.endEditing()
    }

    private static func range(_ range: Range<Int>, in length: Int) -> NSRange? {
        range.upperBound <= length ? NSRange(location: range.lowerBound, length: range.count) : nil
    }

    private static func paragraph(first: CGFloat, rest: CGFloat, size: CGFloat = 15, before: CGFloat = 0) -> NSParagraphStyle {
        let paragraph = NSMutableParagraphStyle()
        paragraph.minimumLineHeight = line(size)
        paragraph.maximumLineHeight = line(size)
        paragraph.paragraphSpacingBefore = before
        paragraph.firstLineHeadIndent = first
        paragraph.headIndent = rest
        return paragraph
    }

    /// Make a stretch bold or slanted, keeping the size and the face it has.
    private static func restyle(_ storage: NSTextStorage, _ range: NSRange, bold: Bool = false, italic: Bool = false) {
        var changes: [(NSRange, PlatformFont)] = []
        storage.enumerateAttribute(.font, in: range) { value, part, _ in
            guard let font = value as? PlatformFont else { return }
            changes.append((part, changed(font, bold: bold, italic: italic)))
        }
        for (part, font) in changes {
            storage.addAttribute(.font, value: font, range: part)
            // A face with no slanted cut is slanted by hand.
            if italic, !isItalic(font) { storage.addAttribute(.obliqueness, value: 0.18, range: part) }
        }
    }

    private static func changed(_ font: PlatformFont, bold: Bool, italic: Bool) -> PlatformFont {
        #if os(macOS)
        var font = font
        if bold { font = NSFontManager.shared.convert(font, toHaveTrait: .boldFontMask) }
        if italic { font = NSFontManager.shared.convert(font, toHaveTrait: .italicFontMask) }
        return font
        #else
        var traits = font.fontDescriptor.symbolicTraits
        if bold { traits.insert(.traitBold) }
        if italic { traits.insert(.traitItalic) }
        guard let descriptor = font.fontDescriptor.withSymbolicTraits(traits) else { return font }
        return UIFont(descriptor: descriptor, size: font.pointSize)
        #endif
    }

    private static func isItalic(_ font: PlatformFont) -> Bool {
        #if os(macOS)
        NSFontManager.shared.traits(of: font).contains(.italicFontMask)
        #else
        font.fontDescriptor.symbolicTraits.contains(.traitItalic)
        #endif
    }

    /// How tall the words stand in a field this wide.
    static func height(of storage: NSAttributedString, width: CGFloat, scheme: ColorScheme) -> CGFloat {
        let measured = storage.length == 0 ? NSAttributedString(string: " ", attributes: base(scheme)) : storage
        let room = CGSize(width: width, height: .greatestFiniteMagnitude)
        #if os(macOS)
        let box = measured.boundingRect(with: room, options: [.usesLineFragmentOrigin, .usesFontLeading])
        #else
        let box = measured.boundingRect(with: room, options: [.usesLineFragmentOrigin, .usesFontLeading], context: nil)
        #endif
        return ceil(box.height)
    }
}

/// Lays the field's words out as the system does, and draws the ground under code as the
/// drawing has it: rounded, with room around the letters.
final class FieldLayout: NSLayoutManager {
    /// Where code's grounds lie, in the text container's own measure, each with its colour.
    func grounds() -> [(rect: CGRect, colour: PlatformColor)] {
        guard let storage = textStorage, let container = textContainers.first else { return [] }
        var found: [(rect: CGRect, colour: PlatformColor)] = []
        let face = PlatformFont.tiloMono(13)
        ensureLayout(for: container)
        storage.enumerateAttribute(FieldStyle.ground, in: NSRange(location: 0, length: storage.length), options: []) { value, range, _ in
            guard let colour = value as? PlatformColor else { return }
            let glyphs = glyphRange(forCharacterRange: range, actualCharacterRange: nil)
            let none = NSRange(location: NSNotFound, length: 0)
            enumerateEnclosingRects(forGlyphRange: glyphs, withinSelectedGlyphRange: none, in: container) { rect, _ in
                // As tall as the code's own letters with a point above and below, wherever the line's foot is.
                let glyph = self.glyphIndex(for: CGPoint(x: rect.minX + 0.5, y: rect.midY), in: container)
                let line = self.lineFragmentRect(forGlyphAt: glyph, effectiveRange: nil)
                let baseline = line.minY + self.location(forGlyphAt: glyph).y
                let tall = min(face.ascender - face.descender + 2, line.height)
                // Never past its own line: the last line's foot is the field's.
                let top = min(max(baseline - face.ascender - 1, line.minY), line.maxY - tall)
                found.append((CGRect(x: rect.minX, y: top, width: rect.width, height: tall), colour))
            }
        }
        return found
    }

    override func drawBackground(forGlyphRange glyphsToShow: NSRange, at origin: CGPoint) {
        super.drawBackground(forGlyphRange: glyphsToShow, at: origin)
        for ground in grounds() {
            let rect = ground.rect.offsetBy(dx: origin.x, dy: origin.y)
            ground.colour.setFill()
            // Text is only ever drawn on the main thread.
            MainActor.assumeIsolated {
                #if os(macOS)
                NSBezierPath(roundedRect: rect, xRadius: FieldStyle.groundRadius, yRadius: FieldStyle.groundRadius).fill()
                #else
                UIBezierPath(roundedRect: rect, cornerRadius: FieldStyle.groundRadius).fill()
                #endif
            }
        }
    }
}

/// Stepping through what the person sent in a conversation, from its empty field: Up brings the
/// newest message back, Up again the one before, Down the next, and past the newest the field is
/// empty again. The messages are the conversation's own pieces, read again at every step: this
/// keeps only which of them stands in the field, and nothing once the person types, moves the
/// caret or presses Escape.
struct Stepping {
    /// The message whose words stand in the field.
    private var message: Item.ID?
    /// The words as they were put in the field.
    private var words: String?

    var isOn: Bool { message != nil }

    /// Whether the field still holds what a step put there, the caret at its end: the next Up or
    /// Down is a step. Anything else in the field is the person's own writing.
    func stands(_ text: String, caret: NSRange) -> Bool {
        words == text && caret == NSRange(location: (text as NSString).length, length: 0)
    }

    /// What the person wrote in these pieces, oldest first. A message sent again right after
    /// itself is one step, not two that look alike.
    static func own(in items: [Item]) -> [(message: Item.ID, words: String)] {
        var found: [(message: Item.ID, words: String)] = []
        for item in items {
            guard case .message(let said) = item.content, said.author == .person,
                  !said.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            else { continue }
            if found.last?.words == said.text { found.removeLast() }
            found.append((message: item.id, words: said.text))
        }
        return found
    }

    /// One step back: the words to put in the field. Nil where nothing older was sent.
    mutating func older(in items: [Item]) -> String? {
        let own = Stepping.own(in: items)
        let before = message.flatMap { shown in own.firstIndex { $0.message == shown } } ?? own.count
        guard before > 0 else { return nil }
        message = own[before - 1].message
        words = own[before - 1].words
        return words
    }

    /// One step forward: the words to put in the field. Past the newest there are none, and the
    /// stepping is over.
    mutating func newer(in items: [Item]) -> String {
        let own = Stepping.own(in: items)
        guard let shown = message, let at = own.firstIndex(where: { $0.message == shown }), at + 1 < own.count else {
            self = Stepping()
            return ""
        }
        message = own[at + 1].message
        words = own[at + 1].words
        return own[at + 1].words
    }
}

#if os(macOS)
import AppKit

/// The message field's own text view. It keeps the modifier keys of the key it was last given,
/// so Return and Shift Return are told apart by the key itself, however the key came.
final class FieldTextView: NSTextView {
    private(set) var keyFlags: NSEvent.ModifierFlags = []

    override func keyDown(with event: NSEvent) {
        keyFlags = event.modifierFlags
        super.keyDown(with: event)
    }
}

/// The system's text view as the message field.
struct MarkdownField: NSViewRepresentable {
    @Binding var text: String
    let label: String
    /// Counts up when the field should take the caret.
    let focus: Int
    /// The conversation's pieces as the app holds them now: what Up in an empty field steps through.
    let sent: () -> [Item]
    let onSend: () -> Void
    @Environment(\.colorScheme) private var scheme

    static let tallest: CGFloat = 220

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
        let view = FieldTextView(usingTextLayoutManager: false)
        view.textContainer?.replaceLayoutManager(FieldLayout())
        view.delegate = context.coordinator
        view.isRichText = false
        view.allowsUndo = true
        view.drawsBackground = false
        view.textContainerInset = .zero
        view.textContainer?.lineFragmentPadding = 0
        view.textContainer?.widthTracksTextView = true
        view.isVerticallyResizable = true
        view.isHorizontallyResizable = false
        view.autoresizingMask = [.width]
        view.minSize = .zero
        view.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        // Markdown's signs stay as they are typed.
        view.isAutomaticQuoteSubstitutionEnabled = false
        view.isAutomaticDashSubstitutionEnabled = false
        view.isAutomaticTextReplacementEnabled = false
        view.setAccessibilityLabel(label)
        view.string = text
        scroll.documentView = view
        context.coordinator.focus = focus
        context.coordinator.style(view, scheme)
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.parent = self
        guard let view = scroll.documentView as? NSTextView else { return }
        if view.string != text {
            view.string = text
            // The app put other words in the field (a message went out, words were dictated or begun
            // for the person): what Undo remembered was said of words that are gone.
            context.coordinator.history.removeAllActions()
        }
        view.setAccessibilityLabel(label)
        context.coordinator.style(view, scheme)
        if context.coordinator.focus != focus {
            context.coordinator.focus = focus
            view.window?.makeFirstResponder(view)
            view.setSelectedRange(NSRange(location: (view.string as NSString).length, length: 0))
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: NSScrollView, context: Context) -> CGSize? {
        guard let width = proposal.width, width.isFinite, width > 0, let view = nsView.documentView as? NSTextView,
              let storage = view.textStorage
        else { return nil }
        let height = FieldStyle.height(of: storage, width: width, scheme: scheme)
        return CGSize(width: width, height: min(max(height, 22), Self.tallest))
    }

    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: MarkdownField
        var focus = 0
        /// Which sent message stands in the field, while the person steps through them.
        private var stepping = Stepping()
        /// While a step puts its words in the field: the changes it makes are not the person's.
        private var puts = false
        /// What Undo takes back in this field: the field's own, apart from the window's.
        let history = UndoManager()

        init(_ parent: MarkdownField) {
            self.parent = parent
        }

        func undoManager(for view: NSTextView) -> UndoManager? {
            history
        }

        func textDidChange(_ notification: Notification) {
            guard let view = notification.object as? NSTextView else { return }
            parent.text = view.string
            leaveSteppingIfTheirs(view)
            style(view, parent.scheme)
        }

        func textViewDidChangeSelection(_ notification: Notification) {
            guard let view = notification.object as? NSTextView else { return }
            leaveSteppingIfTheirs(view)
            style(view, parent.scheme)
        }

        func textDidEndEditing(_ notification: Notification) {
            guard let view = notification.object as? NSTextView else { return }
            style(view, parent.scheme, writing: false)
        }

        func textView(_ textView: NSTextView, doCommandBy selector: Selector) -> Bool {
            // While a letter is being put together, every key is the input method's and the system's.
            guard !textView.hasMarkedText() else { return false }
            switch selector {
            case #selector(NSResponder.insertNewline(_:)):
                let flags = (textView as? FieldTextView)?.keyFlags ?? NSApp.currentEvent?.modifierFlags ?? []
                if flags.contains(.shift) {
                    textView.insertNewlineIgnoringFieldEditor(nil)
                } else {
                    parent.onSend()
                }
                return true
            case #selector(NSResponder.moveUp(_:)):
                return step(textView, older: true)
            case #selector(NSResponder.moveDown(_:)):
                return step(textView, older: false)
            case #selector(NSResponder.cancelOperation(_:)):
                // Escape lets a message that was brought back go. Anything else in the field is not its to take.
                guard stepping.stands(textView.string, caret: textView.selectedRange()) else { return false }
                stepping = Stepping()
                put("", in: textView)
                return true
            default:
                return false
            }
        }

        /// Up or Down in an empty field, or while a sent message stands in it as it was brought
        /// back: a step through what the person sent in this conversation. False: the key is the
        /// text field's own, and moves the caret.
        private func step(_ view: NSTextView, older: Bool) -> Bool {
            let stands = stepping.stands(view.string, caret: view.selectedRange())
            if !stands { stepping = Stepping() }
            if older {
                guard stands || view.string.isEmpty else { return false }
                // Where nothing older was sent, the key is still the stepping's and the field stays.
                if let words = stepping.older(in: parent.sent()) { put(words, in: view) }
                return stepping.isOn
            }
            guard stands else { return false }
            put(stepping.newer(in: parent.sent()), in: view)
            return true
        }

        /// Put words in the field as an edit of the person's own: the draft follows, and Undo takes it back.
        private func put(_ words: String, in view: NSTextView) {
            let whole = NSRange(location: 0, length: (view.string as NSString).length)
            view.breakUndoCoalescing()
            guard view.shouldChangeText(in: whole, replacementString: words) else { return }
            puts = true
            view.textStorage?.replaceCharacters(in: whole, with: words)
            view.didChangeText()
            let end = NSRange(location: (words as NSString).length, length: 0)
            view.setSelectedRange(end)
            puts = false
            view.scrollRangeToVisible(end)
        }

        /// The person typed or moved the caret: what stands in the field is theirs, and the arrows
        /// are a text field's again.
        private func leaveSteppingIfTheirs(_ view: NSTextView) {
            if !puts, stepping.isOn, !stepping.stands(view.string, caret: view.selectedRange()) { stepping = Stepping() }
        }

        func style(_ view: NSTextView, _ scheme: ColorScheme, writing: Bool? = nil) {
            // While a letter is being put together, the system owns how it looks.
            guard !view.hasMarkedText(), let storage = view.textStorage else { return }
            let writing = writing ?? (view.window?.firstResponder === view)
            FieldStyle.apply(to: storage, selection: writing ? view.selectedRange() : nil, scheme: scheme)
            view.typingAttributes = FieldStyle.base(scheme)
            view.insertionPointColor = Token.ink.platform(scheme)
        }
    }
}
#else
import UIKit

/// The system's text view as the message field.
struct MarkdownField: UIViewRepresentable {
    @Binding var text: String
    let label: String
    let focus: Int
    /// The conversation's pieces as the app holds them now.
    let sent: () -> [Item]
    let onSend: () -> Void
    @Environment(\.colorScheme) private var scheme

    static let tallest: CGFloat = 220

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    func makeUIView(context: Context) -> FieldView {
        // The system's own layout of text, with the field's way of drawing code's ground.
        let storage = NSTextStorage()
        let layout = FieldLayout()
        storage.addLayoutManager(layout)
        let container = NSTextContainer(size: CGSize(width: 0, height: CGFloat.greatestFiniteMagnitude))
        container.widthTracksTextView = true
        layout.addTextContainer(container)
        let view = FieldView(frame: .zero, textContainer: container)
        view.kept = storage
        view.delegate = context.coordinator
        view.backgroundColor = .clear
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.isScrollEnabled = false
        // Markdown's signs stay as they are typed.
        view.smartQuotesType = .no
        view.smartDashesType = .no
        view.accessibilityLabel = label
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.text = text
        view.send = { [weak coordinator = context.coordinator] in coordinator?.parent.onSend() }
        view.steps = { [weak coordinator = context.coordinator, weak view] in
            guard let coordinator, let view else { return false }
            return coordinator.steps(in: view)
        }
        view.step = { [weak coordinator = context.coordinator, weak view] key in
            guard let coordinator, let view else { return }
            coordinator.step(view, key)
        }
        context.coordinator.focus = focus
        context.coordinator.style(view, scheme)
        return view
    }

    func updateUIView(_ view: FieldView, context: Context) {
        context.coordinator.parent = self
        if view.text != text { view.text = text }
        view.accessibilityLabel = label
        context.coordinator.style(view, scheme)
        if context.coordinator.focus != focus {
            context.coordinator.focus = focus
            view.becomeFirstResponder()
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: FieldView, context: Context) -> CGSize? {
        guard let width = proposal.width, width.isFinite, width > 0 else { return nil }
        let height = FieldStyle.height(of: uiView.textStorage, width: width, scheme: scheme)
        uiView.isScrollEnabled = height > Self.tallest
        return CGSize(width: width, height: min(max(height, 22), Self.tallest))
    }

    @MainActor
    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: MarkdownField
        var focus = 0
        /// Which sent message stands in the field, while the person steps through them.
        private var stepping = Stepping()
        /// While a step puts its words in the field: the changes it makes are not the person's.
        private var puts = false

        init(_ parent: MarkdownField) {
            self.parent = parent
        }

        func textViewDidChange(_ textView: UITextView) {
            parent.text = textView.text
            leaveSteppingIfTheirs(textView)
            style(textView, parent.scheme)
        }

        func textViewDidChangeSelection(_ textView: UITextView) {
            leaveSteppingIfTheirs(textView)
            style(textView, parent.scheme)
        }

        /// Whether Up, Down and Escape on a keyboard are the stepping's now: the field is empty, or
        /// a sent message stands in it as it was brought back. Everywhere else they are the text field's.
        func steps(in view: UITextView) -> Bool {
            view.text.isEmpty || stepping.stands(view.text, caret: view.selectedRange)
        }

        /// One key of a keyboard, as on a Mac: Up brings back what the person sent before, Down what
        /// they sent after, Escape lets it go.
        func step(_ view: UITextView, _ key: FieldView.Key) {
            let stands = stepping.stands(view.text, caret: view.selectedRange)
            if !stands { stepping = Stepping() }
            switch key {
            case .up:
                guard stands || view.text.isEmpty else { return }
                if let words = stepping.older(in: parent.sent()) { put(words, in: view) }
            case .down:
                if stands { put(stepping.newer(in: parent.sent()), in: view) }
            case .escape:
                guard stands else { return }
                stepping = Stepping()
                put("", in: view)
            }
        }

        /// Put words in the field as an edit of the person's own: the draft follows, and Undo takes it back.
        private func put(_ words: String, in view: UITextView) {
            puts = true
            if let whole = view.textRange(from: view.beginningOfDocument, to: view.endOfDocument) {
                view.replace(whole, withText: words)
            }
            view.selectedRange = NSRange(location: (words as NSString).length, length: 0)
            puts = false
            parent.text = view.text
            style(view, parent.scheme)
        }

        /// The person typed or moved the caret: what stands in the field is theirs.
        private func leaveSteppingIfTheirs(_ view: UITextView) {
            if !puts, stepping.isOn, !stepping.stands(view.text, caret: view.selectedRange) { stepping = Stepping() }
        }

        func textViewDidEndEditing(_ textView: UITextView) {
            style(textView, parent.scheme, writing: false)
        }

        func style(_ view: UITextView, _ scheme: ColorScheme, writing: Bool? = nil) {
            // While a letter is being put together, the system owns how it looks.
            guard view.markedTextRange == nil else { return }
            let writing = writing ?? view.isFirstResponder
            FieldStyle.apply(to: view.textStorage, selection: writing ? view.selectedRange : nil, scheme: scheme)
            view.typingAttributes = FieldStyle.base(scheme)
            view.tintColor = Token.asst.platform(scheme)
        }
    }
}

/// The field on an iPhone. On the glass keyboard Return makes a line and the arrow sends; with a
/// keyboard attached Return sends and Shift Return makes a line, and Up in an empty field steps
/// through what the person sent, as on a Mac.
final class FieldView: UITextView {
    var send: (() -> Void)?
    /// Whether Up, Down and Escape step through what the person sent now, and one such step.
    var steps: (() -> Bool)?
    var step: ((Key) -> Void)?
    /// The words the view lays out: a text view made over a layout of its own does not keep them itself.
    var kept: NSTextStorage?

    /// The keys of a keyboard the field takes while the person steps through what they sent.
    enum Key {
        case up, down, escape
    }

    override var keyCommands: [UIKeyCommand]? {
        // While a letter is being put together, every key is the input method's and the system's.
        guard markedTextRange == nil else { return nil }
        var keys: [(String, Selector)] = [("\r", #selector(sendPressed))]
        // The arrows are the field's own only where they step: everywhere else they move the caret.
        if steps?() == true {
            keys += [(UIKeyCommand.inputUpArrow, #selector(upPressed)), (UIKeyCommand.inputDownArrow, #selector(downPressed)),
                     (UIKeyCommand.inputEscape, #selector(escapePressed))]
        }
        return keys.map { input, action in
            let command = UIKeyCommand(input: input, modifierFlags: [], action: action)
            command.wantsPriorityOverSystemBehavior = true
            return command
        }
    }

    @objc private func sendPressed() {
        send?()
    }

    @objc private func upPressed() {
        step?(.up)
    }

    @objc private func downPressed() {
        step?(.down)
    }

    @objc private func escapePressed() {
        step?(.escape)
    }
}
#endif
