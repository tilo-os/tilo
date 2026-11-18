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

/// The everyday parts of a window: navigation, text and buttons, chips, a card of facts, an
/// empty state and a page of a stack.
type Basics {
    title: Text
    note: Text
    /// Who looks after the basics.
    owner: Option<Actor>
    /// When the basics last changed.
    updated: Time
}

/// The team's items as a list, with the composer a conversation has at its foot.
type Backlog {
    title: Text
    items: Collection<Item>
    draft: RichText
}

/// The team's items on a board, a list per stage.
type Release {
    title: Text
    items: Collection<Item>
}

/// The gallery: a window of every group. A group's file adds its type here and a page to the
/// gallery's sidebar.
type Gallery {
    basics: Collection<Basics>
    backlogs: Collection<Backlog>
    releases: Collection<Release>
    feedback: Collection<Feedback>
    data: Collection<Data>
    controls: Collection<Controls>
    media: Collection<Media>
    places: Collection<Places>
    chrome: Collection<ShellParts>
    customs: Collection<Custom>
}

root Gallery

default view gallery for Gallery

/// The group the window opens on: the first one holding anything, so a window staged from a
/// scenario opens on what that scenario filled, and one with nothing in it still opens on a page.
fn first_page(g: Gallery) -> Text {
    if not g.basics.is_empty() {
        "basics"
    } else if not g.controls.is_empty() {
        "controls"
    } else if not g.feedback.is_empty() {
        "feedback"
    } else if not g.backlogs.is_empty() {
        "backlogs"
    } else if not g.releases.is_empty() {
        "releases"
    } else if not g.data.is_empty() {
        "data"
    } else if not g.media.is_empty() {
        "media"
    } else if not g.places.is_empty() {
        "places"
    } else if not g.chrome.is_empty() {
        "chrome"
    } else if not g.customs.is_empty() { "customs" } else { "basics" }
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

/// Whether the chosen group holds a document its page is drawn from.
fn page_filled(g: Gallery, page: Text) -> Bool {
    if page == "basics" {
        not g.basics.is_empty()
    } else if page == "controls" {
        not g.controls.is_empty()
    } else if page == "feedback" {
        not g.feedback.is_empty()
    } else if page == "backlogs" {
        not g.backlogs.is_empty()
    } else if page == "releases" {
        not g.releases.is_empty()
    } else if page == "data" {
        not g.data.is_empty()
    } else if page == "media" {
        not g.media.is_empty()
    } else if page == "places" {
        not g.places.is_empty()
    } else if page == "chrome" { not g.chrome.is_empty() } else { not g.customs.is_empty() }
}

/// The gallery's window: each group in the sidebar under what it is for, the one chosen shown
/// beside it, so every component the toolkit describes is a place away rather than a window of
/// its own. A window with no documents yet offers to fill them, because a gallery with nothing in
/// it says nothing.
view gallery(g: Gallery) {
    state showing: Text = first_page(g)
    ui.NavShell(content_kind: .page, title: page_title(showing), sidebar_title: "Gallery") {
        actions: {
            ui.SearchButton()
            ui.MenuButton("More", icon: .more_horizontal, icon_only: true) {
                ui.MenuItem("Show the examples", icon: .layout_grid) {
                    on choose: show_examples(g)
                }
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
            if page_filled(g, showing) and showing == "backlogs" {
                if let b = g.backlogs.first() {
                    backlog_list(b, fill)
                }
            } else if page_filled(g, showing) and showing == "releases" {
                if let r = g.releases.first() {
                    release_board(r, fill)
                }
            } else if page_filled(g, showing) and showing == "places" {
                if let p = g.places.first() {
                    places(p, fill)
                }
            } else {
                ui.Scroll {
                    stack(padding: .group, max: 72rem) {
                        if not page_filled(g, showing) {
                            ui.EmptyState(
                                "The examples are not loaded",
                                message: "Every group is here; fill them with the values an app gives its components.",
                                icon: .layout_grid,
                                action: "Show the examples",
                            ) {
                                on act: show_examples(g)
                            }
                        } else if showing == "basics" {
                            if let b = g.basics.first() {
                                basics(b)
                            }
                        } else if showing == "controls" {
                            if let c = g.controls.first() {
                                controls(c)
                            }
                        } else if showing == "feedback" {
                            if let f = g.feedback.first() {
                                feedback(f)
                            }
                        } else if showing == "data" {
                            if let d = g.data.first() {
                                data(d)
                            }
                        } else if showing == "media" {
                            if let m = g.media.first() {
                                media(m)
                            }
                        } else if showing == "chrome" {
                            if let x = g.chrome.first() {
                                shell_parts(x)
                            }
                        } else if let c = g.customs.first() {
                            custom(c)
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
view basics(b: Basics) {
    state last: Text = ""
    ui.Text(
        "Text, rich text and a quote, buttons, chips, a card of facts and an empty state.",
        role: .caption,
        secondary: true,
    )
    stack(max: 44rem, gap: .section) {
        parts(b, last)
    }
}

/// Text, buttons, chips, a card of facts, an empty state and a page of a stack.
view parts(b: Basics, last: State<Text>) {
    part("Text", "The type scale: a title, body text at the reading measure, a caption.") {
        ui.Text("Release planning", role: .large_title)
        ui.Text("Body text says what a thing is, at the reading measure.")
        ui.Time(b.updated, role: .caption, secondary: true)
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
            ui.Avatar(b.owner, name: "Ada Lovelace")
            ui.Avatar(name: "Grace Hopper", square: true)
        }
    }
    part("Text input", "One line a person types, kept in the field it is bound to.") {
        ui.TextInput(bind: b.note, placeholder: "A note for the team")
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
view backlog_list(b: Backlog) {
    state last: Text = ""
    ui.Docked {
        foot: {
            stack(gap: .related) {
                if last != "" {
                    ui.Text(last, role: .caption, secondary: true)
                }
                row(gap: .related) {
                    ui.RichInput(bind: b.draft, placeholder: "Message the team", fill)
                    ui.Button("Send", icon: .send, icon_only: true)
                }
            }
        }
        ui.VirtualList(selection_follows_focus: true, in_column: true) {
            for x in b.items {
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
view release_board(r: Release) {
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
                    let cards = r.items.filter(x -> x.stage == stage)
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

/// The basics, looked after by the person looking.
scenario "basics" {
    let g = root Gallery
    g.basics.insert(Basics(title: "Basics", owner: invoker, updated: 2026-06-30T09:00))
}

/// A team's backlog a week into a release.
scenario "a backlog" {
    let g = root Gallery
    let b = g.backlogs.insert(Backlog(title: "Backlog"))
    b.items.insert(
        Item(
            title: "Write the release notes",
            notes: RichText("Lead with the sync fix, then the new board."),
            stage: "To do",
        ),
    )
    b.items.insert(Item(title: "Screenshots for the docs", notes: RichText(""), stage: "Doing"))
    b.items.insert(Item(title: "Bump the version", notes: RichText(""), stage: "Done"))
}

/// The same release on a board.
scenario "a release board" {
    let g = root Gallery
    let r = g.releases.insert(Release(title: "Release"))
    r.items.insert(Item(title: "Write the release notes", notes: RichText(""), stage: "To do"))
    r.items.insert(Item(title: "Translate the onboarding", notes: RichText(""), stage: "To do"))
    r.items.insert(Item(title: "Screenshots for the docs", notes: RichText(""), stage: "Doing"))
    r.items.insert(Item(title: "Bump the version", notes: RichText(""), stage: "Done"))
}

/// Every group at once: the window a person opens the gallery to see.
scenario "every family" {
    invoke(show_examples(root Gallery))
}
