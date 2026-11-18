app so.tilo.lang.gallery "Gallery" (icon: .layout_grid, category: .system)

/// The toolkit's components as an app written in tilolang places them: each component of the
/// toolkit, in the groups people meet them in, with the values an app would give it. One window
/// holds every group: the sidebar names them, the content shows the one chosen, and a group's
/// file (`controls.tl`, `media.tl`, ...) adds its type here, a page to the window and a scenario
/// that fills it. A test fails while a component of the toolkit is shown in none of them.
/// A piece of work on the team's board, in its list, and talked about.
type Item {
    title: Text
    notes: RichText
    stage: Text
    reactions: Map<Set<Actor>>
}

/// The gallery: a window of every group. A group's file adds its page to the gallery's sidebar
/// and, where a control on it is bound to a field, the field here. The root is made with the
/// package and holds every value the pages show, so a window opens on its components from the
/// first frame, whatever the document holds.
type Gallery {
    /// The note the basics' text input writes.
    basics_note: Text = ""
    /// The report the controls' page edits: every control there writes one of these.
    controls_title: Text = "Controls"
    controls_by_email: Bool = false
    controls_repeating: Bool = false
    /// Whether the workspace holds the report locked, which nobody here can change.
    controls_locked: Bool = true
    controls_margin: Float = 12.0
    controls_share: Float = 0.6
    controls_room: Text = "Studio"
    controls_urgency: Urgency = .none
    controls_owner: Option<Actor> = .none
    controls_reviewer: Option<Actor> = .none
    controls_starts: Date = Date("2026-11-03")
    controls_clock: Time = Time("2026-11-03T09:30:00Z")
    controls_ends: Option<Date> = .none
    controls_accent: Text = "#3a7bd5"
    controls_shortcut: Text = "Ctrl+K"
    controls_recipient: Text = ""
    controls_note: Text = ""
    controls_notes: Text = ""
    controls_signed: Text = "Ada Lovelace"
    controls_background: Bool = true
    controls_lettering: Bool = true
    /// The backlog's items, and the line being written at its foot.
    backlog_items: Collection<Item>
    backlog_draft: RichText
    /// The release's items, one list per stage.
    release_items: Collection<Item>
    /// The data page's collections: a table's rows, a tree's nodes, a conversation's lines, a
    /// grid's cards and a document's changes.
    data_rows: Collection<Row>
    data_files: Collection<Node>
    data_messages: Collection<Message>
    data_cards: Collection<Card>
    data_changes: Collection<Change>
    /// The media page's library, how far its player stands, and the line being written.
    media_photos: Collection<Photo>
    media_at: Float = 12.0
    media_source: Text = ""
    /// The shell page's dock items.
    chrome_apps: Collection<Pinned>
}

root Gallery

default view gallery for Gallery

/// A gallery whose root is made opens filled: the platform applies this once to the root, as it
/// applies every migration, as the root is made and before the window that asked for it is drawn.
migration action seed(g: Gallery) {
    fill(g)
}

/// What the gallery shows with nothing in it yet: one document of every group, so a person meets
/// the components rather than an empty window. Each group fills only while it stands empty, so a
/// scenario may fill them too.
action fill(g: Gallery) {
    if g.backlog_items.is_empty() {
        g.backlog_items.insert(
            Item(
                title: "Write the release notes",
                notes: RichText("Lead with the sync fix, then the new board."),
                stage: "To do",
            ),
        )
        g.backlog_items.insert(
            Item(title: "Screenshots for the docs", notes: RichText(""), stage: "Doing"),
        )
        g.backlog_items.insert(Item(title: "Bump the version", notes: RichText(""), stage: "Done"))
    }
    if g.release_items.is_empty() {
        g.release_items.insert(
            Item(title: "Write the release notes", notes: RichText(""), stage: "To do"),
        )
        g.release_items.insert(
            Item(title: "Translate the onboarding", notes: RichText(""), stage: "To do"),
        )
        g.release_items.insert(
            Item(title: "Screenshots for the docs", notes: RichText(""), stage: "Doing"),
        )
        g.release_items.insert(Item(title: "Bump the version", notes: RichText(""), stage: "Done"))
    }
    if g.data_rows.is_empty() {
        g.data_rows.insert(Row(title: "Readme.md", state: "Edited", size: 1204))
        g.data_rows.insert(Row(title: "release.toml", state: "New", size: 312))
        g.data_rows.insert(Row(title: "notes.txt", state: "Locked", size: 48))
    }
    if g.data_files.is_empty() {
        let project = g.data_files.insert(Node(title: "Project", folder: true))
        let src = g.data_files.insert(Node(title: "src", parent: project, folder: true, size: 0))
        g.data_files.insert(Node(title: "main.rs", parent: src, folder: false, size: 880))
        g.data_files.insert(Node(title: "board.rs", parent: src, folder: false, size: 2416))
        let docs = g.data_files.insert(Node(title: "docs", parent: project, folder: true, size: 0))
        g.data_files.insert(
            Node(title: "release-notes.md", parent: docs, folder: false, size: 1204),
        )
    }
    if g.data_messages.is_empty() {
        g.data_messages.insert(
            Message(
                title: "a",
                from: "Ada",
                body: "Are we ready to ship the release tomorrow?",
                at: 2026-06-29T16:05Z,
                mine: false,
            ),
        )
        g.data_messages.insert(
            Message(
                title: "b",
                from: "Sam",
                body: "The build is green. One screenshot is left for the docs.",
                at: 2026-06-29T16:07Z,
                mine: true,
            ),
        )
        g.data_messages.insert(
            Message(
                title: "c",
                from: "Sam",
                body: "I'll finish it first thing.",
                at: 2026-06-29T16:07Z,
                mine: true,
            ),
        )
        g.data_messages.insert(
            Message(
                title: "d",
                from: "Ada",
                body: "Screenshots are in. Then let us go.",
                at: 2026-06-30T08:44Z,
                mine: false,
            ),
        )
        g.data_messages.insert(
            Message(
                title: "e",
                from: "Sam",
                body: "Shipping now.",
                at: 2026-06-30T08:46Z,
                mine: true,
            ),
        )
    }
    if g.data_cards.is_empty() {
        g.data_cards.insert(Card(title: "Design", count: 12))
        g.data_cards.insert(Card(title: "Engineering", count: 34))
        g.data_cards.insert(Card(title: "Support", count: 7))
    }
    if g.data_changes.is_empty() {
        g.data_changes.insert(
            Change(title: "a", who: "Ada", summary: "Rewrote the summary", at: 2026-06-30T08:42Z),
        )
        g.data_changes.insert(
            Change(title: "b", who: "Sam", summary: "Moved to Done", at: 2026-06-30T09:10Z),
        )
    }
    if g.media_photos.is_empty() {
        g.media_photos.insert(Photo(title: "Dawn", place: "Black Forest", picture: photo()))
        g.media_photos.insert(Photo(title: "Blue hour", place: "Lake Constance", picture: photo()))
        g.media_photos.insert(Photo(title: "Dunes", place: "Erg Chebbi", picture: photo()))
        g.media_photos.insert(
            Photo(title: "Pines in fog", place: "Harz", picture: photo(), tall: true),
        )
    }
    if g.media_source == "" {
        g.media_source = "text(\"Hills at dawn\")"
    }
    if g.chrome_apps.is_empty() {
        g.chrome_apps.insert(Pinned(title: "Notes"))
        g.chrome_apps.insert(Pinned(title: "Tasks"))
        g.chrome_apps.insert(Pinned(title: "Mail"))
    }
}

/// What the chosen group is called, over its page.
fn page_title(page: Text) -> Text {
    if page == "basics" {
        std.say("Basics")
    } else if page == "controls" {
        std.say("Controls")
    } else if page == "feedback" {
        std.say("Feedback")
    } else if page == "backlogs" {
        std.say("Backlog")
    } else if page == "releases" {
        std.say("Release board")
    } else if page == "data" {
        std.say("Data")
    } else if page == "media" {
        std.say("Media")
    } else if page == "places" {
        std.say("Places")
    } else if page == "chrome" {
        std.say("Shell parts")
    } else if page == "customs" { std.say("Your own") } else { std.say("Gallery") }
}

/// The gallery's window: each group in the sidebar under what it is for, the one chosen shown
/// beside it, so every component the toolkit describes is a place away rather than a window of
/// its own. A window with no documents yet offers to fill them, because a gallery with nothing in
/// it says nothing.
view gallery(g: Gallery) {
    state showing: Text = "basics"
    ui.NavShell(content_kind: .page, title: page_title(showing), sidebar_title: "Gallery") {
        actions: {
            ui.SearchButton()
            ui.MenuButton("More", icon: .more_horizontal, icon_only: true) {
                ui.MenuItem("Copy link", icon: .link)
                ui.MenuItem("Settings", icon: .settings)
            }
        }
        sidebar: {
            ui.NavList(selected: showing) {
                ui.NavSection("Everyday parts") {
                    ui.NavItem("basics", "Basics", icon: .layout_grid) {
                        on select: set(showing, "basics")
                    }
                    ui.NavItem("controls", "Controls", icon: .settings) {
                        on select: set(showing, "controls")
                    }
                    ui.NavItem("feedback", "Feedback", icon: .circle_check) {
                        on select: set(showing, "feedback")
                    }
                }
                ui.NavSection("Collections") {
                    ui.NavItem("backlogs", "Backlog", icon: .list) {
                        on select: set(showing, "backlogs")
                    }
                    ui.NavItem("releases", "Release board", icon: .layout_grid) {
                        on select: set(showing, "releases")
                    }
                    ui.NavItem("data", "Data", icon: .table) {
                        on select: set(showing, "data")
                    }
                }
                ui.NavSection("Media and places") {
                    ui.NavItem("media", "Media", icon: .image) {
                        on select: set(showing, "media")
                    }
                    ui.NavItem("places", "Places", icon: .map_pin) {
                        on select: set(showing, "places")
                    }
                }
                ui.NavSection("The shell") {
                    ui.NavItem("chrome", "Shell parts", icon: .monitor) {
                        on select: set(showing, "chrome")
                    }
                }
                ui.NavSection("Your own") {
                    ui.NavItem("customs", "Charts and views", icon: .pencil) {
                        on select: set(showing, "customs")
                    }
                }
            }
        }
        content: {
            // A list, a board and a canvas fill the content where they stand; a page of parts
            // scrolls down it.
            if showing == "backlogs" {
                backlog_list(g, fill)
            } else if showing == "releases" {
                release_board(g, fill)
            } else if showing == "places" {
                places(g, fill)
            } else {
                ui.Scroll {
                    stack(padding: .group, max: 72rem) {
                        if showing == "basics" {
                            basics(g)
                        } else if showing == "controls" {
                            controls(g)
                        } else if showing == "feedback" {
                            feedback(g)
                        } else if showing == "data" {
                            data(g)
                        } else if showing == "media" {
                            media(g)
                        } else if showing == "chrome" {
                            shell_parts(g)
                        } else {
                            custom(g)
                        }
                    }
                }
            }
        }
    }
}

/// A component as the gallery shows it: its name, one line of what it is for, and the component
/// in its variants and states, side by side.
template view part(name: Text, about: Text, content: View) {
    ui.Section(name) {
        ui.Text(about, role: .caption, secondary: true)
        content
    }
}

/// The basics' page: the parts a window is built from, in sections of their own.
view basics(g: Gallery) {
    state last: Text = ""
    ui.Text(
        "Text, rich text and a quote, buttons, chips, a card of facts and an empty state.",
        role: .caption,
        secondary: true,
    )
    stack(max: 44rem, gap: .section) {
        parts(g, last)
    }
}

/// Text, buttons, chips, a card of facts, an empty state and a page of a stack.
view parts(g: Gallery, last: State<Text>) {
    part("Text", "The type scale: a title, body text at the reading measure, a caption.") {
        ui.Text("Release planning", role: .large_title)
        ui.Text("Body text says what a thing is, at the reading measure.")
        ui.Time(2026-06-30T09:00Z, role: .caption, secondary: true)
    }
    part("Button", "What a press does, by its role: the one primary, the usual, the destructive.") {
        row(gap: .related, wrap: true) {
            ui.Button("Save", icon: .check, role: .primary) {
                on press: set(last, "Save")
            }
            ui.Button("Upload", icon: .upload) {
                on press: set(last, "Upload")
            }
            ui.Button("Delete", icon: .trash, role: .destructive) {
                on press: set(last, "Delete")
            }
            ui.Button("Archived", icon: .archive, enabled: false)
        }
    }
    part("Rich text and a quote", "A message's own text, and the message it answers above it.") {
        ui.BlockEditor(RichText("Lead with the sync fix, then the new board."))
        ui.Quote("Can we ship the screenshots with the notes?", caption: "Replying to")
    }
    part("Chip and avatar", "A tag a thing carries, and who looks after it.") {
        row(gap: .related, wrap: true, align: .center) {
            ui.Chip("Design", icon: .tag)
            ui.Chip("Due Friday", icon: .calendar, tone: .warning)
            ui.Chip("Shipped", icon: .check, tone: .success, selected: true)
            ui.Avatar(workspace.me, name: "Ada Lovelace")
            ui.Avatar(name: "Grace Hopper", square: true)
        }
    }
    part("Text input", "One line a person types, kept in the field it is bound to.") {
        ui.TextInput(bind: g.basics_note, placeholder: "A note for the team")
    }
    part("Card", "A summary of one thing on a surface of its own, here with its facts.") {
        ui.Card(title: "This week", count: 3) {
            ui.PropertyList {
                ui.Property("Owner", "Ada Lovelace")
                ui.Property("Due", "Friday")
                ui.Property("Stage", "Review")
            }
        }
    }
    part("Empty state", "What a place says while it holds nothing, and how to fill it.") {
        ui.EmptyState(
            "No archived items",
            message: "Archive an item to put it aside.",
            icon: .archive,
            inline: true,
        )
    }
    part("A page of a stack", "A section opened in a page of its own, one level deep.") {
        ui.PageSlide(level: 1) {
            ui.Text("Opened from the panel, with the way back above it.", role: .caption)
        }
    }
}

/// The backlog's page: its items as rows, each with a menu, a swipe and a hover toolbar,
/// over a composer.
view backlog_list(g: Gallery) {
    state last: Text = ""
    ui.Docked {
        foot: {
            stack(gap: .related) {
                if last != "" {
                    ui.Text(last, role: .caption, secondary: true)
                }
                row(gap: .related) {
                    ui.RichInput(bind: g.backlog_draft, placeholder: "Message the team", fill)
                    ui.Button("Send", icon: .send, icon_only: true)
                }
            }
        }
        ui.VirtualList(selection_follows_focus: true, in_column: true) {
            for x in g.backlog_items {
                ui.ListItem(item: x) {
                    menu: {
                        ui.EmojiBar(["👍", "🎉", "👀"])
                        ui.MenuItem("Open", icon: .arrow_right)
                        ui.MenuSeparator()
                        ui.MenuItem("Delete", icon: .trash, destructive: true)
                    }
                    swipe: {
                        ui.SwipeAction("Archive", icon: .archive)
                    }
                    change: {
                        ui.ChangeMark(x)
                    }
                    item_row(x)
                }
            }
        }
    }
}

/// One item in the list: its row, what it says, its reactions and the message it answers, with
/// a menu, a swipe and a hover toolbar of its own.
view item_row(x: Item) {
    stack(gap: .tight) {
        ui.ListRow(x.title, secondary: x.stage)
        ui.Reactions(reactions: x.reactions)
    }
    ui.ContextMenu {
        ui.MenuItem("Copy link", icon: .link)
    }
    ui.Swipe {
        trailing: {
            ui.SwipeAction("Delete", icon: .trash, destructive: true)
        }
    }
    ui.HoverToolbar {
        ui.Button("Reply", icon: .reply, icon_only: true)
        ui.EmojiPicker()
    }
}

/// The release's page: a list per stage, each card an item.
view release_board(g: Gallery) {
    state last: Text = ""
    stack(gap: .section, fill) {
        ui.Text(
            "A board, a list per stage, with cards an app drags between them.",
            role: .caption,
            secondary: true,
        )
        ui.Scroll(across: true) {
            ui.Board(name: "Release") {
                for stage in ["To do", "Doing", "Done"] {
                    let cards = g.release_items.filter(x -> x.stage == stage)
                    ui.BoardList(title: stage, count: cards.len(), action: "New item") {
                        for x in cards {
                            ui.BoardCard(x) {
                                on activate: note_item(last, x)
                                board_item_row(x)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Note the item a row opened, in the page's answer.
action note_item(last: Text, x: Item) {
    last = "Opened {x.title}"
}

/// An item on the release's board as its row: its title; the list it stands in says its stage.
view board_item_row(x: Item) {
    row(x.title)
}

/// The basics, with a note the person looking wrote.
scenario "basics" {
    let g = root Gallery
    fill(g)
    g.basics_note = "Written from the text input."
}

/// Every group filled, as the gallery's first start fills it.
scenario "every family" {
    invoke(fill(root Gallery))
}
