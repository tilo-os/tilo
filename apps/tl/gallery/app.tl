app so.tilo.lang.gallery "Gallery" (icon: .layout_grid, category: .system)

/// The toolkit's components as an app written in tilolang places them: each component of the
/// toolkit, in the groups people meet them in, with the values an app would give it. Each group is a
/// file of its own (`controls.tl`, `charts.tl`, ...) with its own window and scenario, and a
/// test fails while a component of the toolkit is shown in none of them.
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

/// The team's items on a board, a lane per stage.
type Release {
    title: Text
    items: Collection<Item>
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

/// The gallery's window: the toolkit's families, each opening in a window of its own, with what
/// it holds under its name.
view gallery(g: Gallery) {
    std.page(g) {
        ui.Section("Everyday parts") {
            for x in g.basics {
                ui.ListRow(
                    x.title,
                    secondary: "Type, buttons, chips, cards and empty states",
                    item: x,
                    icon: .layout_grid,
                )
            }
            for x in g.controls {
                ui.ListRow(
                    x.title,
                    secondary: "Fields, toggles, sliders and pickers",
                    item: x,
                    icon: .settings,
                )
            }
            for x in g.feedback {
                ui.ListRow(
                    x.title,
                    secondary: "Banners, badges, progress and numbers",
                    item: x,
                    icon: .eye,
                )
            }
        }
        ui.Section("Collections") {
            for x in g.backlogs {
                ui.ListRow(
                    x.title,
                    secondary: "Rows with menus, swipes and reactions",
                    item: x,
                    icon: .list,
                )
            }
            for x in g.releases {
                ui.ListRow(
                    x.title,
                    secondary: "A board, a lane per stage",
                    item: x,
                    icon: .layout_grid,
                )
            }
            for x in g.data {
                ui.ListRow(
                    x.title,
                    secondary: "Tables, trees, conversations and history",
                    item: x,
                    icon: .table,
                )
            }
        }
        ui.Section("Media and places") {
            for x in g.media {
                ui.ListRow(
                    x.title,
                    secondary: "Photos, players, codes and surfaces",
                    item: x,
                    icon: .image,
                )
            }
            for x in g.places {
                ui.ListRow(
                    x.title,
                    secondary: "A canvas that pans and zooms",
                    item: x,
                    icon: .map_pin,
                )
            }
        }
        ui.Section("The shell") {
            for x in g.chrome {
                ui.ListRow(
                    x.title,
                    secondary: "Bars, docks, tabs and dragging",
                    item: x,
                    icon: .monitor,
                )
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

/// The release's window: a lane per stage, each card an item.
view release_board(r: Release) {
    std.page(r, canvas: true) {
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

/// An item on the release's board as its row: its title; the lane it stands in says its stage.
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
