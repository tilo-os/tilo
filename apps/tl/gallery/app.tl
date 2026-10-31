app so.tilo.lang.gallery "Gallery" (icon: .layout_grid, category: .system)

/// The toolkit's components as an app written in tilolang places them: each `ui.*` component,
/// in the groups people meet them in, with the values an app would give it. Each group is a
/// file of its own (`controls.tl`, `charts.tl`, ...) with its own window and scenario, and a
/// test fails while a component of the toolkit is shown in none of them.

/// A piece of work on the team's board, in its list, and talked about.
type Item {
    title: Text (title)
    notes: RichText (body)
    stage: Text
    reactions: Map<Set<Principal>>
}

/// The everyday parts of a window: navigation, text and buttons, chips, a card of facts and an
/// empty state.
type Basics {
    title: Text (title)
    note: Text
    /// Who looks after the basics.
    owner: Option<Principal>
    /// When the basics last changed.
    updated: Time
}

/// The team's items as a list, with the composer a conversation has at its foot.
type Backlog {
    title: Text (title)
    items: Collection<Item>
    draft: RichText
}

/// The team's items on a board, a lane per stage.
type Release {
    title: Text (title)
    items: Collection<Item>
}

/// The gallery: a window of each group. A group's file adds its type here and a row to the
/// gallery's list.
type Gallery {
    basics: Collection<Basics>
    backlogs: Collection<Backlog>
    releases: Collection<Release>
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
        }
    }
}

/// The basics' window: places in the sidebar, the parts in the content.
view basics(b: Basics) {
    lens place: Text = "parts"
    lens last: Text = ""
    ui.NavShell(content_kind: .page, title: "Basics", sidebar_title: .some("Gallery")) {
        sidebar: {
            ui.NavList(selected: place) {
                ui.NavItem("parts", "Parts", icon: .layout_grid) {
                    on select: set(place, "parts")
                }
                ui.NavSection("Collections") {
                    ui.NavItem("list", "List", icon: .list, count: .some(3)) {
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

/// Text, buttons, chips, a card of facts and an empty state.
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
            ui.Avatar(b.owner, name: .some("Ada Lovelace"))
        }
        ui.TextInput(value: b.note, placeholder: "A note for the team")
    }
    ui.Card(title: "This week", count: 3) {
        ui.PropertyList {
            ui.Property("Owner", "Ada Lovelace")
            ui.Property("Due", "Friday")
            ui.Property("Stage", "Review")
        }
    }
    ui.EmptyState("No archived items", message: "Archived items show here.", icon: .archive, inline: true)
}

/// The backlog's window: its items as rows, each with a menu, a swipe and a hover toolbar,
/// over a composer.
view backlog_list(b: Backlog) {
    ui.NavShell(content_kind: .list, title: "Backlog", count: .some(b.items.len())) {
        content: {
            ui.Docked {
                foot: {
                    row(gap: .related) {
                        ui.RichInput(value: b.draft, placeholder: "Message the team", fill)
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
        ui.ListRow(x.title, secondary: .some(x.stage))
        if not x.notes.is_empty() {
            ui.RichText(x.notes)
        }
        ui.Reactions(reactions: x.reactions)
        if x.stage == "Doing" {
            ui.Quote("Can we ship the screenshots with the notes?", caption: .some("Replying to"))
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
            ui.Board(name: .some("Release")) {
                for stage in ["To do", "Doing", "Done"] {
                    let cards = r.items.filter(x -> x.stage == stage)
                    ui.BoardLane(title: .some(stage), count: .some(cards.len()), action: .some("New item")) {
                        for x in cards {
                            ui.BoardCard(x, title: .some(x.title)) {
                                ui.Text(x.stage, role: .caption, secondary: true)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The basics, looked after by the person looking.
scenario "basics" {
    let g = root Gallery
    g.basics.insert(Basics(title: "Basics", owner: .some(invoker), updated: 2026-06-30T09:00Z))
}

/// A team's backlog a week into a release.
scenario "a backlog" {
    let g = root Gallery
    let b = g.backlogs.insert(Backlog(title: "Backlog"))
    b.items.insert(Item(title: "Write the release notes", notes: RichText("Lead with the sync fix, then the new board."), stage: "To do"))
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
