import AssistantCore
import SwiftUI

/// A conversation: its pieces from the oldest at the top to the newest at the foot, where the
/// view rests.
public struct ConversationView: View {
    private let chat: Chat
    /// For a picture of the secret's field filled; see `SecretCard.pictured`.
    private let picturedSecret: String
    @Environment(AssistantModel.self) private var model
    @Environment(\.wording) private var wording

    public init(chat: Chat, picturedSecret: String = "") {
        self.chat = chat
        self.picturedSecret = picturedSecret
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
                        if conversation.isLoaded, rows.isEmpty { empty }
                        if let agent = liveAgent, let thing = agent.live { LiveThingPlace(thing: thing) }
                        ForEach(rows) { row in
                            RowView(row: row, picturedSecret: picturedSecret).id(row.id)
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
    let picturedSecret: String
    @Environment(AssistantModel.self) private var model

    var body: some View {
        switch row {
        case .mark(let words, _):
            TimeMark(words: words)
        case .task(let line, let isImportant, _):
            TaskLineView(line: line, isImportant: isImportant)
        case .caption(let words, _):
            Fine(words: words)
        case .piece(let item, let marked):
            if marked { ImportantMark() }
            piece(item)
        }
    }

    @ViewBuilder
    private func piece(_ item: Item) -> some View {
        switch item.content {
        case .message(let message):
            MessageView(message: message)
        case .report(let line):
            Bubble { MarkdownText(text: line) }
        case .reminder(let words):
            Bubble { MarkdownText(text: words) }
        case .question(let asked):
            Bubble { MarkdownText(text: asked.text) }
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
