/// The data views: a table of columns and rows, a tree of nodes, a conversation, a grid of cards
/// and the history of a document, each reading a collection of the app's own.
type Data {
    title: Text
    /// The rows of the table: each an item with what its columns read.
    rows: Collection<Row>
    /// The files of the tree, each naming its parent by its key.
    files: Collection<Node>
    /// The messages of the conversation, oldest first.
    messages: Collection<Message>
    /// The cards of the grid.
    cards: Collection<Card>
    /// The changes of the document, oldest first.
    changes: Collection<Change>
}

/// One row of the table: what its columns read.
type Row {
    title: Text
    state: Text
    size: Int
}

/// One file of the tree: where it stands, and what it is.
type Node {
    title: Text
    /// The file it stands under; none at the top of the tree.
    parent: Option<Node> = .none
    folder: Bool
    size: Int
}

/// One message of the conversation: who said it, when, and what.
type Message {
    title: Text
    from: Text
    body: Text
    at: Time
    mine: Bool
}

/// One card of the grid: what it stands for and how many things it holds.
type Card {
    title: Text
    count: Int
}

/// One change of the document: who made it, when, and what it did.
type Change {
    title: Text
    who: Text
    summary: Text
    at: Time
}

default view data for Data

/// The data window: each data view as an app places it, over the window's collection.
view data(d: Data) {
    state last: Text = ""
    std.page(d) {
        ui.Section("Table") {
            ui.DataTable(name: "Files") {
                columns: {
                    ui.DataTableColumn("Name", sortable: true, width: .flex)
                    ui.DataTableColumn("State")
                    ui.DataTableColumn("Size", kind: .numeric)
                }
                for x in d.rows {
                    ui.DataTableRow(x) {
                        cells: {
                            ui.DataTableCell("Name", value: x.title)
                            ui.DataTableCell("State", value: x.state)
                            ui.DataTableCell("Size", value: "{x.size}")
                        }
                    }
                }
                on select(x: Row): open(x, front: false)
                on activate(x: Row): open(x)
            }
        }
        ui.Section("Tree") {
            ui.TreeView(name: "Project", fill: false, hide_root: true) {
                for f in d.files {
                    ui.TreeViewNode(f, f.title, parent: f.parent, branch: f.folder) {
                        row: {
                            ui.TreeViewRow(
                                icon: if f.folder { .folder } else { .file_text },
                                trailing: if not f.folder { "{f.size} B" },
                            )
                        }
                    }
                }
                on select(x: Node): open(x, front: false)
                on activate(x: Node): open(x)
            }
        }
        ui.Section("Conversation") {
            // The divider a view that draws a conversation's own rows marks a day with; the
            // list below places the same one over each day it shows.
            ui.SectionDivider("Jun 30")
            ui.ConversationList(name: "Conversation with Ada") {
                for m in d.messages {
                    ui.ConversationEntry(m, m.body, author: m.from, time: m.at, outgoing: m.mine)
                }
            }
        }
        ui.Section("Cards") {
            ui.CardGrid(name: "Collections") {
                for c in d.cards {
                    ui.CardGridCard(c, title: c.title, count: c.count) {
                        on activate: open(c)
                        ui.Text("Open it to see what it holds.", role: .caption, secondary: true)
                    }
                }
            }
        }
        ui.Section("History") {
            ui.ActivityTimeline(newest_first: true) {
                for c in d.changes {
                    ui.ActivityEntry(c, c.summary, time: c.at, kind: .details)
                }
                on revert(x: Change): open(x)
                on open(x: Change): open(x)
            }
        }
    }
}

/// The data group, over a release a team is working on.
scenario "data" {
    let g = root Gallery
    let d = g.data.insert(Data(title: "Data"))
    d.rows.insert(Row(title: "Readme.md", state: "Edited", size: 1204))
    d.rows.insert(Row(title: "release.toml", state: "New", size: 312))
    d.rows.insert(Row(title: "notes.txt", state: "Locked", size: 48))
    let project = d.files.insert(Node(title: "Project", folder: true))
    let src = d.files.insert(Node(title: "src", parent: project, folder: true, size: 0))
    d.files.insert(Node(title: "main.rs", parent: src, folder: false, size: 880))
    d.messages.insert(
        Message(
            title: "a",
            from: "Ada",
            body: "Are we ready to ship?",
            at: 2026-06-30T08:40Z,
            mine: false,
        ),
    )
    d.messages.insert(
        Message(
            title: "b",
            from: "Sam",
            body: "The build is green.",
            at: 2026-06-30T08:42Z,
            mine: true,
        ),
    )
    d.messages.insert(
        Message(
            title: "c",
            from: "Ada",
            body: "Then let us go.",
            at: 2026-06-30T08:44Z,
            mine: false,
        ),
    )
    d.cards.insert(Card(title: "Design", count: 12))
    d.cards.insert(Card(title: "Engineering", count: 34))
    d.cards.insert(Card(title: "Support", count: 7))
    d.changes.insert(
        Change(title: "a", who: "Ada", summary: "Rewrote the summary", at: 2026-06-30T08:42Z),
    )
    d.changes.insert(
        Change(title: "b", who: "Sam", summary: "Moved to Done", at: 2026-06-30T09:10Z),
    )
}
