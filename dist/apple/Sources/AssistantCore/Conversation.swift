import Foundation

/// A conversation as a device holds it: the pieces it was given, kept up to date by each change.
public struct Conversation: Equatable, Sendable {
    public private(set) var items: [Item] = []
    /// Whether whoever it is with is writing an answer now.
    public private(set) var isAnswering = false
    /// Whether the source has said what the conversation holds.
    public private(set) var isLoaded = false

    public init() {}

    public mutating func apply(_ change: ConversationChange) {
        switch change {
        case .all(let all):
            items = all
            isLoaded = true
        case .added(let new):
            // A piece told twice stands once.
            for item in new {
                if let index = items.firstIndex(where: { $0.id == item.id }) { items[index] = item } else { items.append(item) }
            }
        case .changed(let item):
            if let index = items.firstIndex(where: { $0.id == item.id }) { items[index] = item }
        case .removed(let id):
            items.removeAll { $0.id == id }
        case .answering(let on):
            isAnswering = on
        }
    }

    /// Older pieces, put before what is held.
    public mutating func prepend(_ earlier: [Item]) {
        items = earlier.filter { old in !items.contains { $0.id == old.id } } + items
    }

    /// One thing the conversation draws under another.
    public enum Row: Equatable, Sendable, Identifiable {
        /// A day and time between pieces: "Today 7:30".
        case mark(String, before: Item.ID)
        /// The line that names a task above its pieces, with the Important mark where they carry it.
        case task(TaskLine, isImportant: Bool, before: Item.ID)
        /// The line over what the main agent wrote to a sub-agent.
        case caption(String, before: Item.ID)
        /// A piece. `marked` when it shows the Important mark itself, because no line above it does.
        case piece(Item, marked: Bool)

        public var id: String {
            switch self {
            case .mark(_, let item): "mark-\(item)"
            case .task(_, _, let item): "task-\(item)"
            case .caption(_, let item): "caption-\(item)"
            case .piece(let item, _): "piece-\(item.id)"
            }
        }

        /// The piece it stands before, or is.
        public var item: Item.ID {
            switch self {
            case .mark(_, let item), .task(_, _, let item), .caption(_, let item): item
            case .piece(let item, _): item.id
            }
        }
    }

    /// The piece that what happened since the person left stands before: the first that came
    /// after they left and is drawn. Nil when every piece held is older: then it stands at the foot.
    public func first(since left: Date) -> Item.ID? {
        items.first { $0.at > left && !$0.showsNothing }?.id
    }

    /// What the conversation draws, top to bottom: marks where time passed, a task's line over
    /// the pieces that belong to it, and the pieces.
    public func rows(in chat: Chat, _ wording: Wording) -> [Row] {
        var rows: [Row] = []
        var before: Item?
        for (index, item) in items.enumerated() {
            // A piece that shows nothing has no row, and no mark, line or caption stands for it.
            if item.showsNothing { continue }
            // A sub-agent's window says in its head when it was started: its first piece needs no mark.
            var marked = wording.marks(item.at, after: before?.at)
            if before == nil, chat != .main { marked = false }
            if marked { rows.append(.mark(wording.mark(item.at), before: item.id)) }
            if let line = item.task, marked || before?.task?.task != line.task {
                let run = items[index...].prefix { $0.task?.task == line.task }
                rows.append(.task(line, isImportant: run.contains(where: \.isImportant), before: item.id))
            }
            if case .message(let message) = item.content, case .mainAgent(let word) = message.author {
                rows.append(.caption(wording.caption(word, at: item.at), before: item.id))
            }
            var ownMark = item.isImportant && item.task == nil
            if case .brief = item.content { ownMark = false }
            rows.append(.piece(item, marked: ownMark))
            before = item
        }
        return rows
    }
}
