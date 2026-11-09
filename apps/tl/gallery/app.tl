app so.tilo.lang.gallery "Gallery" (icon: .layout_grid, category: .system)

/// The toolkit's components as an app written in tilolang places them: each `ui.*` component,
/// in the groups people meet them in, with the values an app would give it. Each group is a
/// file of its own (`controls.tl`, `charts.tl`, ...) with its own window and scenario, and a
/// test fails while a component of the toolkit is shown in none of them.
/// A piece of work on the team's board, in its list, and talked about.
type Item {
    title: Text
    notes: RichText
    stage: Text
    reactions: Map<Set<Principal>>
}

/// The everyday parts of a window: navigation, text and buttons, chips, a card of facts, an
/// empty state and a page of a stack.
type Basics {
    title: Text
    note: Text
    /// Who looks after the basics.
    owner: Option<Principal>
    /// When the basics last changed.
    updated: Time
}

/// The team's items as a list, with the composer a conversation has at its foot.
type Backlog {
    title: Text
    items: Collection<Item>
    draft: RichText
}

/// The team's items on a board, a lane per stage.
type Release {
    title: Text
    items: Collection<Item>
    /// Who leads the release: at each of its tabs.
    lead: Option<Principal>
}

/// The gallery: a window of each group. A group's file adds its type here and a row to the
/// gallery's list.
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
}

root Gallery

default view gallery for Gallery

default view basics for Basics

default view backlog_list for Backlog

default view release_board for Release

/// The gallery's window: each group, opening in a window of its own.
view gallery(g: Gallery) {
    std.page(g) {
        ui.Section("Groups") {
            for x in g.basics {
                ui.ListRow(x.title, item: x, icon: .layout_grid)
            }
            for x in g.backlogs {
                ui.ListRow(x.title, item: x, icon: .list)
            }
            for x in g.releases {
                ui.ListRow(x.title, item: x, icon: .layout_grid)
            }
            for x in g.feedback {
                ui.ListRow(x.title, item: x, icon: .eye)
            }
            for x in g.data {
                ui.ListRow(x.title, item: x, icon: .table)
            }
            for x in g.controls {
                ui.ListRow(x.title, item: x, icon: .settings)
            }
            for x in g.media {
                ui.ListRow(x.title, item: x, icon: .image)
            }
            for x in g.places {
                ui.ListRow(x.title, item: x, icon: .map_pin)
            }
            for x in g.chrome {
                ui.ListRow(x.title, item: x, icon: .monitor)
            }
        }
    }
}

/// The basics' window: places in the sidebar, the parts in the content.
view basics(b: Basics) {
    state place: Text = "parts"
    state last: Text = ""
    ui.NavShell(content_kind: .page, title: "Basics", sidebar_title: "Gallery") {
        sidebar: {
            ui.NavList(selected: place) {
                ui.NavItem("parts", "Parts", icon: .layout_grid) {
                    on select: set(place, "parts")
                }
                ui.NavSection("Collections") {
                    ui.NavItem("list", "List", icon: .list, count: 3) {
                        on select: set(place, "list")
                    }
                    ui.NavItem("board", "Board", icon: .layout_grid) {
                        on select: set(place, "board")
                    }
                }
            }
        }
        actions: {
            ui.SearchButton()
            ui.MenuButton("More", icon: .more_horizontal, icon_only: true) {
                ui.MenuItem("Rename", icon: .pencil) {
                    on choose: set(last, "Rename")
                }
                ui.MenuItem("Delete", icon: .trash, destructive: true) {
                    on choose: set(last, "Delete")
                }
            }
        }
        content: {
            ui.Scroll {
                page {
                    stack(gap: .section) {
                        parts(b, last)
                    }
                }
            }
        }
    }
}

/// Text, buttons, chips, a card of facts, an empty state and a page of a stack.
view parts(b: Basics, last: State<Text>) {
    ui.Section("Text and buttons") {
        ui.Text("Release planning", role: .large_title)
        ui.Text("Body text says what a thing is, at the reading measure.")
        ui.Time(b.updated, role: .caption, secondary: true)
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
        }
        row(gap: .related, wrap: true) {
            ui.Chip("Design", icon: .tag)
            ui.Chip("Due Friday", icon: .calendar)
            ui.Avatar(b.owner, name: "Ada Lovelace")
        }
        ui.TextInput(bind: b.note, placeholder: "A note for the team")
    }
    ui.Card(title: "This week", count: 3) {
        ui.PropertyList {
            ui.Property("Owner", "Ada Lovelace")
            ui.Property("Due", "Friday")
            ui.Property("Stage", "Review")
        }
    }
    ui.EmptyState(
        "No archived items",
        message: "Archive an item to put it aside.",
        icon: .archive,
        inline: true,
    )
    ui.Section("A page of a stack") {
        ui.PageSlide(level: 1) {
            ui.Text(
                "A section opened in its own page of the panel, one level deep.",
                role: .caption,
            )
        }
    }
}

/// The backlog's window: its items as rows, each with a menu, a swipe and a hover toolbar,
/// over a composer.
view backlog_list(b: Backlog) {
    ui.NavShell(content_kind: .list, title: "Backlog", count: b.items.len()) {
        content: {
            ui.Docked {
                foot: {
                    row(gap: .related) {
                        ui.RichInput(bind: b.draft, placeholder: "Message the team", fill)
                        ui.Button("Send", icon: .send, icon_only: true)
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
    }
}

/// One item in the list: its row, what it says, its reactions and the message it answers, with
/// a menu, a swipe and a hover toolbar of its own.
view item_row(x: Item) {
    stack(gap: .tight) {
        ui.ListRow(x.title, secondary: x.stage)
        if not x.notes.is_empty() {
            ui.BlockEditor(x.notes)
        }
        ui.Reactions(reactions: x.reactions)
        if x.stage == "Doing" {
            ui.Quote("Can we ship the screenshots with the notes?", caption: "Replying to")
        }
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

/// The release's window: its items as tabs, as a frame holds them, over a lane per stage, each
/// card an item.
view release_board(r: Release) {
    std.page(r, canvas: true) {
        ui.TabStrip(r.items.first(), focused: true) {
            for x in r.items {
                ui.TabItem(x, title: x.title, icon: .file_text, modified: x.stage == "Doing") {
                    ui.TabFace(r.lead, name: "Ada Lovelace")
                }
            }
        }
        ui.Scroll(across: true) {
            ui.Board(name: "Release") {
                for stage in ["To do", "Doing", "Done"] {
                    let cards = r.items.filter(x -> x.stage == stage)
                    ui.BoardLane(title: stage, count: cards.len(), action: "New item") {
                        for x in cards {
                            ui.BoardCard(x) {
                                board_item_row(x)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// An item on the release's board as its row: its title, and the stage it is in.
view board_item_row(x: Item) {
    row(x.title, secondary: x.stage)
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
    let r = g.releases.insert(Release(title: "Release", lead: invoker))
    r.items.insert(Item(title: "Write the release notes", notes: RichText(""), stage: "To do"))
    r.items.insert(Item(title: "Translate the onboarding", notes: RichText(""), stage: "To do"))
    r.items.insert(Item(title: "Screenshots for the docs", notes: RichText(""), stage: "Doing"))
    r.items.insert(Item(title: "Bump the version", notes: RichText(""), stage: "Done"))
}
