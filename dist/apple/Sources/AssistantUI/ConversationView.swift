import AssistantCore
import QuickLook
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
    /// Whether the conversation's end is in sight: the person has not scrolled away from it.
    @State private var atEnd = true
    /// Whether something came or grew under what the person scrolled up to.
    @State private var moreBelow = false
    /// Whether the height of the line that said an answer was being written is kept after
    /// the answer ended, so that nothing above it moves. The next piece takes its place.
    @State private var keepsFoot = false
    /// Whether the steps of the turn at work are shown under its line. A person opens them.
    @State private var stepsOpen = false
    /// How tall what shows of the conversation is, once its scroll view has said so: a turn's
    /// opened steps take a share of it. Nothing until then.
    @State private var shownTall: CGFloat = 0
    /// How many scrolls the view has asked for itself to bring the end back, and not yet seen through.
    /// Where one of them puts the conversation is never the person scrolling away from the end.
    @State private var ownScrolls = 0

    public init(chat: Chat, picturedSecret: String = "", head: String? = nil) {
        self.chat = chat
        self.picturedSecret = picturedSecret
        self.head = head
    }

    public var body: some View {
        let conversation = model.conversation(chat)
        // The reply its model is writing now: the newest piece, while an answer is being written.
        let written = conversation.isAnswering ? conversation.items.last?.id : nil
        let rows = conversation.rows(in: chat, wording)
        let shown = model.shownItem.flatMap { item in
            model.place == .chat(chat) ? rows.first { $0.id == "piece-\(item)" }?.id : nil
        }
        let ready = chat == .main ? model.tasks(.readyForReview).count : 0
        // What came while the person was away stands at the top of the stretch that came since.
        let since = chat == .main ? model.sinceLeft : nil
        let sinceFirst = since.flatMap { conversation.first(since: $0.left) }
        let sinceRow = sinceFirst.flatMap { first in rows.first { $0.item == first }?.id }
        // Under the newest reply whose model gave no answer for the key's sake: the way back to the key.
        let way = model.replyWay(in: chat)
        GeometryReader { area in
            ScrollViewReader { reader in
                ScrollView {
                    VStack(alignment: .leading, spacing: 0) {
                      VStack(alignment: .leading, spacing: 8) {
                        if let head {
                            Text(head).font(.small12).foregroundStyle(Token.ink3).accessibilityLabel(head.spoken)
                        }
                        if conversation.isLoaded, rows.isEmpty, !setsUp { empty }
                        if let agent = liveAgent, let thing = agent.live { LiveThingPlace(thing: thing) }
                        ForEach(rows) { row in
                            if let since, row.id == sinceRow { SinceLeftCard(since: since) }
                            RowView(row: row, chat: chat, picturedSecret: picturedSecret, way: way?.item == row.item ? way : nil)
                                .id(row.id)
                        }
                        if let since, sinceRow == nil, conversation.isLoaded { SinceLeftCard(since: since) }
                        // Her message stands and no turn has taken it up for a while: one quiet line
                        // says so under it. Where a step of the first start is shown, that step says why.
                        if model.isNotTakenUp(chat), !setsUp {
                            Fine(words: AssistantWords.notTakenUp.said)
                                .frame(maxWidth: .infinity, alignment: .trailing)
                                .padding(.trailing, 14)
                        }
                        // What stands between the person and a first answer is said where that answer would stand.
                        if chat == .main { SetupStep() }
                        if conversation.isAnswering || keepsFoot {
                            // One line says what the turn does now; a press opens its steps so far.
                            TurnLine(turn: conversation.isAnswering ? turn(of: conversation) : nil, open: $stepsOpen,
                                     tallest: shownTall > 0 ? (shownTall * Measure.stepsShare).rounded(.down) : .infinity)
                                .padding(.leading, 14)
                                .opacity(conversation.isAnswering ? 1 : 0)
                                .accessibilityHidden(!conversation.isAnswering)
                        }
                        if ready > 0 { ReviewLine(ready: ready) }
                        if let problem = model.problem {
                            Text(problem).font(.small125).foregroundStyle(Token.you).frame(maxWidth: .infinity)
                        }
                      }
                      Color.clear.frame(height: Measure.columnFoot).id("end")
                    }
                    .frame(maxWidth: Measure.column)
                    .padding(.horizontal, Measure.columnGutter)
                    .frame(maxWidth: .infinity)
                    .frame(minHeight: area.size.height, alignment: .bottom)
                }
                .defaultScrollAnchor(.bottom)
                // What scrolls out under the bar above or the field below leaves softly: no piece is
                // cut flat there, its round corners made square.
                .overlay(alignment: .top) { EdgeFade(edge: .top) }
                .overlay(alignment: .bottom) { EdgeFade(edge: .bottom) }
                // Where the conversation rests is held by hand: when the conversation, or what
                // shows of it, changes its height while the person is at the end, the end is brought
                // back at once, whatever the reading says of where the view rests; and only a
                // scroll takes a person away from the end, or back to it.
                // A scroll view whose place is read all the time does not keep its end by itself on
                // an iPhone: it opened at its oldest piece and lost its newest to the keyboard.
                .onScrollGeometryChange(for: Resting.self) { place in
                    Resting(tall: place.contentSize.height, shown: place.containerSize.height,
                            below: place.contentSize.height - place.visibleRect.maxY)
                } action: { was, now in
                    if shownTall != now.shown { shownTall = now.shown }
                    if was.tall != now.tall || was.shown != now.shown {
                        if atEnd { rest(reader) }
                    } else {
                        // The end is in sight while what shows reaches to within two lines of it.
                        let inSight = now.below <= 40
                        // A scroll the view asked for itself, set before the conversation is laid out,
                        // may throw a long one to its top for a moment: that is not the person leaving.
                        if atEnd != inSight, inSight || ownScrolls == 0 { atEnd = inSight }
                        if inSight, moreBelow { moreBelow = false }
                    }
                }
                .onAppear { if shown == nil { rest(reader) } }
                // The height the conversation has changes when a bar or the keyboard comes or goes, and
                // a scroll view does not tell its reader of every such step: a chat that is the first
                // thing its window shows gets its bar's height in the pass that lays the window out, and
                // the reader above is not called for it. The height itself says so here.
                .onChange(of: area.size.height) {
                    if atEnd { rest(reader) }
                }
                // Scrolled up, the view stays where the person put it, and a quiet mark says that
                // more came below. A press on it brings the end back.
                .overlay(alignment: .bottom) {
                    if moreBelow {
                        Button {
                            moreBelow = false
                            atEnd = true
                            reader.scrollTo("end", anchor: .bottom)
                        } label: {
                            Tag(words: AssistantWords.moreBelow.said)
                        }
                        .buttonStyle(.plain)
                        .padding(.bottom, 8)
                    }
                }
                .onChange(of: conversation.items.last) {
                    if !atEnd, !newestIsOwn(conversation) { moreBelow = true }
                }
                .onChange(of: conversation.isAnswering) { was, now in
                    keepsFoot = was && !now
                    // The next turn's steps are closed until its person opens them.
                    if !now { stepsOpen = false }
                }
                .onChange(of: chat) {
                    keepsFoot = false
                    stepsOpen = false
                    moreBelow = false
                    // Another conversation opens on its newest piece, wherever the last one was left.
                    atEnd = true
                    reader.scrollTo("end", anchor: .bottom)
                }
                .onChange(of: rows.last?.id) {
                    keepsFoot = false
                    // A new piece brings the end into view for a person who stands at it, or who sent
                    // it. At once: an eased scroll to the end begins at the conversation's top.
                    guard atEnd || newestIsOwn(conversation) else { return }
                    atEnd = true
                    reader.scrollTo("end", anchor: .bottom)
                }
                .onChange(of: shown, initial: true) {
                    guard let shown else { return }
                    atEnd = false
                    withAnimation(.easeOut(duration: 0.2)) { reader.scrollTo(shown, anchor: .center) }
                    model.shownItem = nil
                }
            }
        }
        .environment(\.writtenPiece, written)
    }

    /// What the turn at work does, as the newest reply that says so carries it.
    private func turn(of conversation: Conversation) -> Message.Turn? {
        for item in conversation.items.reversed() {
            if case .message(let message) = item.content, let turn = message.turn { return turn }
        }
        return nil
    }

    /// Whether the newest piece is what the person wrote themselves.
    private func newestIsOwn(_ conversation: Conversation) -> Bool {
        if case .message(let message)? = conversation.items.last?.content { message.author == .person } else { false }
    }

    /// Bring the conversation's end into view: at once, and once more when the window has finished
    /// what it is laying out, if the person still rests at the end. An end set in the middle of a
    /// layout pass is set for a height that is still changing, and stays where it was put.
    private func rest(_ reader: ScrollViewProxy) {
        ownScrolls += 1
        reader.scrollTo("end", anchor: .bottom)
        Task { @MainActor in
            if atEnd { reader.scrollTo("end", anchor: .bottom) }
            ownScrolls -= 1
        }
    }

    private var liveAgent: SubAgent? {
        if case .subAgent(let id) = chat { model.subAgent(id) } else { nil }
    }

    /// Whether the main chat has something to say of its own first run. Then it is not empty.
    private var setsUp: Bool {
        guard chat == .main else { return false }
        switch model.setup {
        case .unknown, .ready: return false
        case .findingAssistant, .makingWorkspace, .makingAssistant, .noAssistant, .chatPermission, .needsProvider, .needsModel: return true
        }
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

/// What of a conversation's scroll view says where it rests: how tall the conversation is, how
/// tall what shows of it is, and how much of it lies under what shows.
private struct Resting: Equatable {
    var tall: CGFloat
    var shown: CGFloat
    var below: CGFloat
}

/// One thing the conversation draws under another.
private struct RowView: View {
    let row: Conversation.Row
    let chat: Chat
    let picturedSecret: String
    /// The way back to the key that stands under this piece, where one does.
    var way: AssistantModel.ReplyWay?
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
            if let way { ReplyWayButton(offered: way) }
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
    @Environment(\.writtenPiece) private var writtenPiece
    @Environment(AssistantModel.self) private var model

    var body: some View {
        switch item.content {
        case .message(let message):
            MessageView(message: message, piece: item.id, isWritten: item.id == writtenPiece)
        case .report(let line):
            Bubble(words: PieceWords(of: item.id, line)) { MarkdownText(text: line) }
        case .reminder(let words):
            Bubble(words: PieceWords(of: item.id, words)) { MarkdownText(text: words) }
        case .question(let asked):
            Bubble(words: PieceWords(of: item.id, asked.text)) { MarkdownText(text: asked.text) }
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
    /// The piece these words are.
    let piece: Item.ID
    /// Whether its model is writing it now.
    var isWritten = false
    @Environment(AssistantModel.self) private var model
    /// The file a press on its tag fetched, shown by the system's own viewer.
    @State private var shown: URL?

    var body: some View {
        if let answered = message.answers {
            // The message this one answers stands above its words; a press goes there.
            Button {
                model.shownItem = answered.piece
            } label: {
                AnsweredLine(answered: answered)
            }
            .buttonStyle(.plain)
            .frame(maxWidth: Measure.bubbleWidth, alignment: message.author == .person ? .trailing : .leading)
            .frame(maxWidth: .infinity, alignment: message.author == .person ? .trailing : .leading)
        }
        if !message.text.isEmpty {
            Bubble(tone: tone, words: PieceWords(of: piece, message.text), marks: true) {
                if isAddress {
                    // An address alone is shown as what it is, letter by letter.
                    Text(message.text).font(.tiloMono(12.5))
                } else {
                    // What a person wrote stands as they wrote it; a reply that is being written is read as it will stand.
                    MarkdownText(text: message.text, isWritten: isWritten && message.author != .person)
                }
            }
            // The person's own words are read out as theirs; everything else in a conversation is
            // whoever it is with, or says who it is from in the line above it.
            .modifier(SaidByPerson(words: message.author == .person ? Markdown.plain(message.text) : nil))
        }
        if !message.attachments.isEmpty {
            Flow(spacing: 8, lineSpacing: 6) {
                ForEach(message.attachments) { attachment in
                    Button {
                        Task {
                            if let file = await model.file(of: attachment.id) { shown = file }
                        }
                    } label: {
                        Tag(words: attachment.words())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("\(AssistantWords.attachment.said): \(attachment.words())")
                }
            }
            .frame(maxWidth: .infinity, alignment: message.author == .person ? .trailing : .leading)
            // Quick Look on the Mac, the system's viewer on the phone: a file is looked at there.
            .quickLookPreview($shown)
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

extension EnvironmentValues {
    /// The piece whose reply is being written now, in the conversation a view stands in.
    @Entry var writtenPiece: Item.ID?
}
