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

/// One change of the document: what it did. Who made it and when are the change's own, kept when
/// it was made.
type Change {
    title: Text
    summary: Text
}

/// The data page: each data view as an app places it, over the page's collection.
view data(g: Gallery) {
    state last: Text = ""
    ui.Text(
        "Tables, trees, conversations, cards and history, each reading a collection of the app's own.",
        role: .caption,
        secondary: true,
    )
    if last != "" {
        ui.Text(last, role: .caption, secondary: true)
    }
    stack(max: 72rem, gap: .section) {
        part("Data table", "A collection's rows in columns: sort by a column, select a row.") {
            ui.DataTable(name: "Files") {
                columns: {
                    ui.DataTableColumn("Name", sortable: true, width: .flex)
                    ui.DataTableColumn("State")
                    ui.DataTableColumn("Size", kind: .numeric)
                }
                for x in g.data_rows {
                    ui.DataTableRow(x) {
                        cells: {
                            ui.DataTableCell("Name", value: x.title)
                            ui.DataTableCell("State", value: x.state)
                            ui.DataTableCell("Size", value: "{x.size} B")
                        }
                    }
                }
                on select(x: Row): picked(last, x)
                on activate(x: Row): opened(last, x)
            }
        }
        part(
            "Tree view",
            "Nested things, as a sidebar holds a project's folders; a folder opens in place.",
        ) {
            // A tree stands as a sidebar does, so what a row says at its end stays beside it.
            row {
                stack(max: 22rem, fill) {
                    ui.TreeView(name: "Project", fill: false, hide_root: true) {
                        for f in g.data_files {
                            ui.TreeViewNode(f, f.title, parent: f.parent, branch: f.folder) {
                                row: {
                                    ui.TreeViewRow(
                                        icon: if f.folder { .folder } else { .file_text },
                                        trailing: if not f.folder { "{f.size} B" },
                                    )
                                }
                            }
                        }
                        on select(x: Node): picked_node(last, x)
                        on activate(x: Node): opened_node(last, x)
                    }
                }
            }
        }
        part(
            "Conversation",
            "Each line on its own side, a run of one person's under their name once, a divider where the day changes.",
        ) {
            ui.ConversationList(name: "Conversation with Ada") {
                for m in g.data_messages {
                    if m.mine {
                        ui.ConversationEntry(m, m.body, time: m.at, outgoing: true, delivery: .read)
                    } else {
                        ui.ConversationEntry(m, m.body, author: m.from, time: m.at)
                    }
                }
            }
        }
        part("Card grid", "Collections as cards in columns, as many as the width holds.") {
            ui.CardGrid(name: "Collections") {
                for c in g.data_cards {
                    ui.CardGridCard(c, title: c.title, count: c.count) {
                        on activate: opened_card(last, c)
                        ui.Text(
                            "A collection's cards, as an app its own.",
                            role: .caption,
                            secondary: true,
                        )
                    }
                }
            }
        }
        part(
            "Activity timeline",
            "What happened to a document, newest first, each change open to revert.",
        ) {
            ui.ActivityTimeline(newest_first: true) {
                for c in g.data_changes {
                    if let made = created(c) {
                        ui.ActivityEntry(
                            c,
                            c.summary,
                            author: made.who,
                            time: made.at,
                            kind: .details,
                        )
                    }
                }
                on revert(x: Change): reverted(last, x)
                on open(x: Change): opened_change(last, x)
            }
        }
        part(
            "Timeline",
            "A week as hours, events where their times put them, and the line where now is.",
        ) {
            ui.Timeline(
                range: .week,
                start: Time("2026-11-03T09:00:00Z"),
                show_now: true,
                label: "This week",
            ) {
                ui.TimelineItem(
                    title: "Standup",
                    start: Time("2026-11-03T09:00:00Z"),
                    end: Time("2026-11-03T09:15:00Z"),
                    tone: .first,
                )
                ui.TimelineItem(
                    title: "Design review",
                    start: Time("2026-11-03T10:00:00Z"),
                    end: Time("2026-11-03T11:30:00Z"),
                    location: "Studio",
                    tone: .third,
                )
                ui.TimelineItem(
                    title: "Release",
                    start: Time("2026-11-05T00:00:00Z"),
                    end: Time("2026-11-05T00:00:00Z"),
                    all_day: true,
                    tone: .fifth,
                )
            }
        }
    }
}

/// The data group, over a release a team is working on.
scenario "data" {
    let g = root Gallery
    invoke(note_change(g, "Rewrote the summary"), by: person("Ada"), at: 2026-06-30T08:42Z)
    invoke(note_change(g, "Added the screenshots"), by: agent("Scout"), at: 2026-06-30T09:05Z)
}

/// Note a change to the document, made by whoever asks for it.
action note_change(g: Gallery, summary: Text) {
    g.data_changes.insert(Change(title: summary, summary: summary))
}

/// Note the row the table picked, in the page's answer.
action picked(last: Text, row: Row) {
    last = "Selected {row.title}"
}

/// Note the row the table opened, in the page's answer.
action opened(last: Text, row: Row) {
    last = "Opened {row.title}"
}

/// Note the file the tree picked, in the page's answer.
action picked_node(last: Text, node: Node) {
    last = "Selected {node.title}"
}

/// Note the file the tree opened, in the page's answer.
action opened_node(last: Text, node: Node) {
    last = "Opened {node.title}"
}

/// Note the card the grid opened, in the page's answer.
action opened_card(last: Text, card: Card) {
    last = "Opened {card.title}"
}

/// Note the change the history took back, in the page's answer.
action reverted(last: Text, change: Change) {
    last = "Reverted {change.summary}"
}

/// Note the change the history opened, in the page's answer.
action opened_change(last: Text, change: Change) {
    last = "Opened {change.summary}"
}
