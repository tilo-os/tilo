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
type Gallery (title: name) {
    /// What the gallery is called: its name, so no field a page edits is taken for its title.
    name: Text = "Gallery"
    /// The note the basics' text input writes.
    basics_note: Text = ""
    /// The report the controls' page edits: every control there writes one of these.
    controls_title: Text = "Controls"
    controls_by_email: Bool = false
    controls_repeating: Bool = false
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
    controls_background: Bool = true
    controls_lettering: Bool = true
    /// The values the states page's samples show: a box off and one on, a name, one that is
    /// wrong, a share and a room.
    states_off: Bool = false
    states_on: Bool = true
    states_name: Text = "Ada Lovelace"
    states_wrong: Text = "Ada_99"
    states_share: Float = 0.4
    states_room: Text = "Studio"
    /// The backlog's items.
    backlog_items: Collection<Item>
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

/// What a new gallery starts with: one document of every group, so a person meets the components
/// rather than an empty window.
initial action fill(g: Gallery) {
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
    g.data_rows.insert(Row(title: "Readme.md", state: "Edited", size: 1204))
    g.data_rows.insert(Row(title: "release.toml", state: "New", size: 312))
    g.data_rows.insert(Row(title: "notes.txt", state: "Locked", size: 48))
    let project = g.data_files.insert(Node(title: "Project", folder: true))
    let src = g.data_files.insert(Node(title: "src", parent: project, folder: true, size: 0))
    g.data_files.insert(Node(title: "main.rs", parent: src, folder: false, size: 880))
    g.data_files.insert(Node(title: "board.rs", parent: src, folder: false, size: 2416))
    let docs = g.data_files.insert(Node(title: "docs", parent: project, folder: true, size: 0))
    g.data_files.insert(Node(title: "release-notes.md", parent: docs, folder: false, size: 1204))
    g.data_messages.insert(
        Message(
            title: "Ready to ship?",
            from: "Ada",
            body: "Are we ready to ship the release tomorrow?",
            at: 2026-06-29T16:05Z,
            mine: false,
        ),
    )
    g.data_messages.insert(
        Message(
            title: "The build is green",
            from: "Sam",
            body: "The build is green. One screenshot is left for the docs.",
            at: 2026-06-29T16:07Z,
            mine: true,
        ),
    )
    g.data_messages.insert(
        Message(
            title: "The last screenshot",
            from: "Sam",
            body: "I'll finish it first thing.",
            at: 2026-06-29T16:07Z,
            mine: true,
        ),
    )
    g.data_messages.insert(
        Message(
            title: "Screenshots are in",
            from: "Ada",
            body: "Screenshots are in. Then let us go.",
            at: 2026-06-30T08:44Z,
            mine: false,
        ),
    )
    g.data_messages.insert(
        Message(
            title: "Shipping now",
            from: "Sam",
            body: "Shipping now.",
            at: 2026-06-30T08:46Z,
            mine: true,
        ),
    )
    g.data_cards.insert(Card(title: "Design", count: 12))
    g.data_cards.insert(Card(title: "Engineering", count: 34))
    g.data_cards.insert(Card(title: "Support", count: 7))
    g.data_changes.insert(Change(title: "Rewrote the summary", summary: "Rewrote the summary"))
    g.data_changes.insert(Change(title: "Moved to Done", summary: "Moved to Done"))
    g.media_photos.insert(Photo(title: "Dawn", place: "Black Forest", picture: photo()))
    g.media_photos.insert(Photo(title: "Blue hour", place: "Lake Constance", picture: photo()))
    g.media_photos.insert(Photo(title: "Dunes", place: "Erg Chebbi", picture: photo()))
    g.media_photos.insert(Photo(title: "Pines in fog", place: "Harz", picture: photo(), tall: true))
    g.media_source = "text(\"Hills at dawn\")"
    g.chrome_apps.insert(Pinned(title: "Notes"))
    g.chrome_apps.insert(Pinned(title: "Tasks", kind: .tasks))
    g.chrome_apps.insert(Pinned(title: "Mail", kind: .mail))
}

/// What the chosen group is called, over its page.
fn page_title(page: Text) -> Text {
    if page == "basics" {
        std.say("Basics")
    } else if page == "controls" {
        std.say("Controls")
    } else if page == "states" {
        std.say("States")
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
    } else if page == "customs" {
        std.say("Charts and views")
    } else if page == "series" {
        std.say("Trends and series")
    } else if page == "shapes" {
        std.say("Shares and shapes")
    } else if page == "flows" {
        std.say("Flows and trees")
    } else if page == "type" { std.say("Type and scripts") } else { std.say("Gallery") }
}

/// The gallery's window: each group in the sidebar under what it is for, the one chosen shown
/// beside it, so every component the toolkit describes is a place away rather than a window of
/// its own.
view gallery(g: Gallery) {
    state showing: Text = "basics"
    ui.NavShell(content_kind: .page, title: page_title(showing), sidebar_title: "Gallery") {
        actions: {
            ui.SearchButton()
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
                    ui.NavItem("states", "States", icon: .mouse_pointer_2) {
                        on select: set(showing, "states")
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
                ui.NavSection("Charts") {
                    ui.NavItem("series", "Trends and series", icon: .bar_chart) {
                        on select: set(showing, "series")
                    }
                    ui.NavItem("shapes", "Shares and shapes", icon: .gauge) {
                        on select: set(showing, "shapes")
                    }
                    ui.NavItem("flows", "Flows and trees", icon: .list_tree) {
                        on select: set(showing, "flows")
                    }
                }
                ui.NavSection("Text") {
                    ui.NavItem("type", "Type and scripts", icon: .italic) {
                        on select: set(showing, "type")
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
                // The scroller stays as the sidebar picks another page, so it is told which page
                // it shows: each opens at its top.
                ui.Scroll(shows: showing) {
                    stack(padding: .group, max: 72rem) {
                        if showing == "basics" {
                            basics(g)
                        } else if showing == "controls" {
                            controls(g)
                        } else if showing == "states" {
                            states(g)
                        } else if showing == "feedback" {
                            feedback(g)
                        } else if showing == "data" {
                            data(g)
                        } else if showing == "media" {
                            media(g)
                        } else if showing == "chrome" {
                            shell_parts(g)
                        } else if showing == "type" {
                            type_and_scripts(g)
                        } else if showing == "series" {
                            series_charts(g)
                        } else if showing == "shapes" {
                            shape_charts(g)
                            pies()
                        } else if showing == "flows" {
                            flow_charts(g)
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
    part(
        "Money",
        "An amount as the person's own number style writes it: whole amounts without cents, fractional ones with two places.",
    ) {
        row(gap: .group, wrap: true, align: .center) {
            ui.Money(5000000, role: .title)
            ui.Money(420000)
            ui.Money(1250000000)
            ui.Money(990000, secondary: true)
        }
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
            ui.Button("Details", role: .plain) {
                on press: set(last, "Details")
            }
            ui.Button("Open in the browser", icon: .external_link, icon_only: true) {
                on press: set(last, "Open in the browser")
            }
            ui.Button(
                "Archived",
                icon: .archive,
                enabled: false,
                description: "Nothing is chosen to archive",
            )
        }
    }
    part("Rich text and a quote", "A message's own text, and the message it answers above it.") {
        ui.BlockEditor(RichText("Lead with the sync fix, then the new board."))
        ui.Quote("Can we ship the screenshots with the notes?", caption: "Replying to")
    }
    part(
        "Chip and avatar",
        "A tag a thing carries, one to pick or take off, and who looks after it.",
    ) {
        row(gap: .related, wrap: true, align: .center) {
            ui.Chip("Design", icon: .tag)
            ui.Chip("Due Friday", icon: .calendar, tone: .warning)
            ui.Chip("Shipped", icon: .check, tone: .success, selected: true)
            ui.Chip("Mine", selected: false) {
                on press: set(last, "Mine")
            }
            ui.Chip("ada@example.com") {
                on remove: set(last, "Removed ada@example.com")
            }
            // The people who look after it wrap together, each a circle in their own colour.
            row(gap: .related, align: .center) {
                ui.Avatar(invoker, name: "Ada Lovelace")
                ui.Avatar(name: "Grace Hopper")
            }
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
    part("Menu button", "The \"…\" a bar keeps what does not fit behind, open to its items.") {
        ui.MenuButton("More", icon: .more_horizontal, icon_only: true) {
            ui.MenuItem("Copy link", icon: .link)
            ui.MenuItem("Settings", icon: .settings)
        }
    }
    part("Empty state", "What a place says while it holds nothing, and how to fill it.") {
        ui.EmptyState(
            "No archived items",
            message: "Archive an item to put it aside.",
            icon: .archive,
            action: "Open the backlog",
            inline: true,
        ) {
            on act: set(last, "Open the backlog")
        }
    }
    part("A page of a stack", "A section opened in a page of its own, one level deep.") {
        ui.PageSlide(level: 1) {
            ui.Text("A page one level deep, slid in over the one that opened it.", role: .caption)
        }
    }
}

/// The type page: every text role, and text in every script the toolkit sets. Its own page, so
/// the faces it is set in (serif, mono, each script's) are read when it opens, not at the start.
view type_and_scripts(g: Gallery) {
    ui.Text(
        "The type scale and the scripts text is set in, each in the face it needs.",
        role: .caption,
        secondary: true,
    )
    stack(max: 44rem, gap: .section) {
        part("Type scale", "Every role from the largest title down to code.") {
            ui.Text("Planning", role: .display)
            ui.Text("Release planning", role: .large_title)
            ui.Text("Every role comes from one body size", role: .title)
            ui.Text("A section's heading", role: .heading)
            ui.Text("A group inside the section", role: .subheading)
            ui.Text("Body text says what a thing is, at the reading measure.")
            ui.Text("A control's label, or a row's title in a dense list", role: .label)
            ui.Text(
                "Reading text, as an article sets it: office, affinity, a naïve café.",
                role: .reading,
            )
            ui.Text("let greeting = \"Hello, 世界\"", role: .code)
            ui.Time(2026-06-30T09:00Z, role: .caption, secondary: true)
        }
        part(
            "Scripts",
            "Every script the toolkit sets: right to left, mixed, without spaces, emoji.",
        ) {
            ui.Text("مرحبًا بالعالم")
            ui.Text("שלום עולם")
            ui.Text("नमस्ते दुनिया")
            ui.Text("নমস্কার বিশ্ব")
            ui.Text("สวัสดีชาวโลก")
            ui.Text("東京の天気")
            ui.Text("上海的早晨")
            ui.Text("Invoice 123 for חשבון ٤٥٦, total €78")
            ui.Text("👩🏽‍💻 👨‍👩‍👧‍👦 🏳️‍🌈 🇪🇺")
        }
    }
}

/// The backlog's page: its items as rows, each with a menu, a swipe and a hover toolbar,
/// over the line that adds the next one.
view backlog_list(g: Gallery) {
    // The line being written is the person's own, as any composer's is: kept in the window, not
    // in the gallery everyone shares, so two people typing never overwrite each other, and the
    // field is called what it says.
    state draft: RichText = RichText("")
    ui.Docked {
        foot: {
            row(gap: .related) {
                ui.RichInput(bind: draft, placeholder: "Add an item to the backlog", fill) {
                    on submit: add_item(g, draft).then(set(draft, RichText("")))
                }
                ui.Button("Add", icon: .plus, icon_only: true) {
                    on press: add_item(g, draft).then(set(draft, RichText("")))
                }
            }
        }
        stack(padding: .group, gap: .related, fill) {
            ui.Text(
                "Rows of a collection: each with a menu, a swipe, a toolbar on hover and its reactions.",
                role: .caption,
                secondary: true,
            )
            ui.VirtualList(selection_follows_focus: true, in_column: true, fill) {
                for x in g.backlog_items {
                    ui.ListItem(item: x, done: x.stage == "Done") {
                        menu: {
                            ui.EmojiBar(["👍", "🎉", "👀"]) {
                                on pick(e): react(x, e)
                            }
                            ui.MenuItem("Mark done", icon: .check) {
                                on choose: finish(x)
                            }
                            ui.MenuSeparator()
                            ui.MenuItem("Delete", icon: .trash, destructive: true) {
                                on choose: remove_item(g, x)
                            }
                        }
                        swipe: {
                            ui.SwipeAction("Done", icon: .check) {
                                on swipe: finish(x)
                            }
                        }
                        change: {
                            ui.ChangeMark(x)
                        }
                        item_row(g, x)
                    }
                }
            }
        }
    }
}

/// One item in the list: its row, what it says and its reactions, with a menu, a swipe and a
/// hover toolbar of its own.
view item_row(g: Gallery, x: Item) {
    stack(gap: .tight) {
        ui.ListRow(x.title, secondary: x.stage)
        if not x.reactions.keys().is_empty() {
            ui.Reactions(reactions: x.reactions) {
                on toggle(e): react(x, e)
            }
        }
    }
    ui.ContextMenu {
        ui.MenuItem("Mark done", icon: .check) {
            on choose: finish(x)
        }
    }
    ui.Swipe {
        trailing: {
            ui.SwipeAction("Delete", icon: .trash, destructive: true) {
                on swipe: remove_item(g, x)
            }
        }
    }
    ui.HoverToolbar {
        ui.Button("Mark done", icon: .check, icon_only: true) {
            on press: finish(x)
        }
        ui.EmojiPicker() {
            on pick(e): react(x, e)
        }
    }
}

/// Adds what the backlog's line says as an item to do.
action add_item(g: Gallery, text: RichText) {
    require not text.is_empty() "Write the item first"
    g.backlog_items.insert(Item(title: text.excerpt, notes: text, stage: "To do"))
}

/// Marks an item done.
action finish(x: Item) {
    require x.stage != "Done" "Already done"
    x.stage = "Done"
}

/// Takes an item off the backlog.
destructive action remove_item(g: Gallery, x: Item) {
    g.backlog_items.remove(x)
}

/// Reacts to an item with an emoji, or takes the person's reaction back.
action react(x: Item, emoji: Text) {
    let mine = if let who = x.reactions.get(emoji) { who.any(p -> p == invoker) } else { false }
    if mine { x.reactions.remove(emoji, invoker) } else { x.reactions.add(emoji, invoker) }
}

/// The release's page: a list per stage, each card an item.
view release_board(g: Gallery) {
    state last: Text = ""
    // The board keeps the column's edge itself, so it scrolls across to the pane's edges.
    stack(gap: .tight, fill) {
        stack(padding: .group) {
            ui.Text(
                "A board, a list per stage, with cards an app drags between them.",
                role: .caption,
                secondary: true,
            )
        }
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
    g.basics_note = "Written from the text input."
}

/// Every group filled, as the gallery's first start fills it.
scenario "every family" {
    root Gallery
}
