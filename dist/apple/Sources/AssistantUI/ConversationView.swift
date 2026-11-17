import AssistantCore
import SwiftUI

/// A conversation: its pieces from the oldest at the top to the newest at the foot, where the
/// view rests.
public struct ConversationView: View {
    private let chat: Chat
    /// For a picture of the secret's field filled; see `SecretCard.pictured`.
    private let picturedSecret: String
    /// A line that stands at the conversation's beginning and scrolls with it, where the window has
    /// no room to keep it in sight above.
    private let head: String?
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording

    public init(chat: Chat, picturedSecret: String = "", head: String? = nil) {
        self.chat = chat
        self.picturedSecret = picturedSecret
        self.head = head
    }

    public var body: some View {
        let conversation = model.conversation(chat)
        let rows = conversation.rows(in: chat, wording)
        let ready = chat == .main ? model.tasks(.readyForReview).count : 0
        GeometryReader { space in
            ScrollViewReader { reader in
                ScrollView {
                    VStack(alignment: .leading, spacing: 0) {
                      VStack(alignment: .leading, spacing: 8) {
                        if let head {
                            Text(head).font(.small12).foregroundStyle(Token.ink3).accessibilityLabel(head.spoken)
                        }
                        if conversation.isLoaded, rows.isEmpty { empty }
                        if let agent = liveAgent, let thing = agent.live { LiveThingPlace(thing: thing) }
                        ForEach(rows) { row in
                            RowView(row: row, chat: chat, picturedSecret: picturedSecret).id(row.id)
                        }
                        if conversation.isAnswering {
                            Fine(words: "\(AssistantWords.answering.said)…").padding(.leading, 14)
                        }
                        if ready > 0 { ReviewLine(ready: ready) }
                        if let problem = model.problem {
                            Text(problem).font(.small125).foregroundStyle(Token.you).frame(maxWidth: .infinity)
                        }
                      }
                      Color.clear.frame(height: 14).id("end")
                    }
                    .frame(maxWidth: Measure.column)
                    .padding(.horizontal, Measure.columnGutter)
                    .frame(maxWidth: .infinity)
                    .frame(minHeight: space.size.height, alignment: .bottom)
                }
                .defaultScrollAnchor(.bottom)
                .onChange(of: rows.last?.id) {
                    withAnimation(.easeOut(duration: 0.2)) { reader.scrollTo("end", anchor: .bottom) }
                }
                .onChange(of: model.shownItem) {
                    guard let item = model.shownItem else { return }
                    withAnimation(.easeOut(duration: 0.2)) { reader.scrollTo("piece-\(item)", anchor: .center) }
                    model.shownItem = nil
                }
            }
        }
    }

    private var liveAgent: SubAgent? {
        if case .subAgent(let id) = chat { model.subAgent(id) } else { nil }
    }

    /// The app starts empty: before anything was said, the conversation says so.
    private var empty: some View {
        VStack(spacing: 6) {
            Mark(side: 28)
            Text(AssistantWords.nothingYet.said).font(.tilo(15, .semibold)).foregroundStyle(Token.ink)
            Text(AssistantWords.nothingYetBody.said).font(.body14).foregroundStyle(Token.ink2)
        }
        .frame(maxWidth: .infinity)
        .padding(.bottom, 40)
    }
}

/// One thing the conversation draws under another.
private struct RowView: View {
    let row: Conversation.Row
    let chat: Chat
    let picturedSecret: String
    @Environment(AssistantModel.self) private var model

    var body: some View {
        switch row {
        case .mark(let words, _):
            TimeMark(words: words)
        case .task(let line, let isImportant, let before):
            TaskLineView(line: line, isImportant: isImportant, before: before, chat: chat)
            if let replies = model.openReplies[before] { Replies(replies: replies) }
        case .caption(let words, _):
            Fine(words: words)
        case .piece(let item, let marked):
            if marked { ImportantMark() }
            PieceView(item: item, picturedSecret: picturedSecret)
        }
    }
}

/// A task's replies, open under its line: each as the conversation shows a piece, set in under
/// the line's arrow with a thread beside them.
private struct Replies: View {
    let replies: [Item]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if replies.isEmpty { Fine(words: AssistantWords.noReplies.said) }
            ForEach(replies) { PieceView(item: $0, picturedSecret: "") }
        }
        .padding(.leading, 27)
        .overlay(alignment: .leading) {
            RoundedRectangle(cornerRadius: 1).fill(Token.line).frame(width: 2).padding(.leading, 7)
                .accessibilityHidden(true)
        }
    }
}

/// One piece of a conversation, as the drawing draws its kind.
private struct PieceView: View {
    let item: Item
    let picturedSecret: String
    @Environment(AssistantModel.self) private var model

    var body: some View {
        switch item.content {
        case .message(let message):
            MessageView(message: message)
        case .report(let line):
            Bubble { MarkdownText(text: line) }
        case .reminder(let words):
            Bubble { MarkdownText(text: words) }
        case .question(let asked):
            Bubble { MarkdownText(text: asked.text) }
                // Read out as what it is: a question, and that it waits.
                .modifier(WaitsForAnswer(waits: asked.waits))
            QuickAnswers(choices: asked.choices, chosen: asked.answer) { model.answer(asked.id, with: $0.id) }
        case .secret(let asked):
            SecretCard(request: asked, pictured: picturedSecret)
        case .connection(let offered):
            ConnectionCard(offer: offered)
        case .event(let card):
            EventCardView(card: card)
        case .brief(let brief):
            BriefView(brief: brief, isImportant: item.isImportant)
        }
    }
}

/// Reads a question out as one that waits, while it does.
private struct WaitsForAnswer: ViewModifier {
    let waits: Bool

    func body(content: Content) -> some View {
        if waits {
            content.accessibilityElement(children: .combine).accessibilityValue(AssistantWords.waitsForAnswer.said)
        } else {
            content
        }
    }
}

/// Reads a bubble out as the person's own words, when it is.
private struct SaidByPerson: ViewModifier {
    let words: String?

    func body(content: Content) -> some View {
        if let words {
            content.accessibilityElement(children: .ignore).accessibilityLabel("\(AssistantWords.you.said): \(words)")
                .accessibilityAddTraits(.isStaticText)
        } else {
            content
        }
    }
}

/// Words from the person, the assistant or the main agent, with what came with them.
private struct MessageView: View {
    let message: Message
    @Environment(AssistantModel.self) private var model
    @Environment(\.openURL) private var openURL

    var body: some View {
        if !message.text.isEmpty {
            Bubble(tone: tone) {
                if isAddress {
                    // An address alone is shown as what it is, letter by letter.
                    Text(message.text).font(.tiloMono(12.5))
                } else {
                    MarkdownText(text: message.text)
                }
            }
            #if os(macOS)
            .textSelection(.enabled)
            #endif
            // The person's own words are read out as theirs; everything else in a conversation is
            // whoever it is with, or says who it is from in the line above it.
            .modifier(SaidByPerson(words: message.author == .person ? Markdown.plain(message.text) : nil))
        }
        if !message.attachments.isEmpty {
            Flow(spacing: 8, lineSpacing: 6) {
                ForEach(message.attachments) { attachment in
                    Button {
                        Task {
                            if let file = await model.file(of: attachment.id) { openURL(file) }
                        }
                    } label: {
                        Tag(words: attachment.name)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("\(AssistantWords.attachment.said): \(attachment.name)")
                }
            }
            .frame(maxWidth: .infinity, alignment: message.author == .person ? .trailing : .leading)
        }
        ForEach(message.live) { LiveThingPlace(thing: $0) }
    }

    private var tone: BubbleTone {
        switch message.author {
        case .person: .me
        case .assistant: .plain
        case .mainAgent: .lead
        }
    }

    private var isAddress: Bool {
        let text = message.text.trimmingCharacters(in: .whitespacesAndNewlines)
        return !text.contains(" ") && !text.contains("\n") && (text.hasPrefix("https://") || text.hasPrefix("http://"))
    }
}
