/// The parts the shell is made of, as a view places them: a bar or a dock's body, a run of
/// items a person reorders by dragging one, and the divider a conversation marks its first
/// unread line with. Each draws with the theme's own finish and rhythm, as the shell and the
/// system search use them.
type ShellParts {
    title: Text
    /// What the dock's items stand for: the apps pinned to it.
    apps: Collection<Pinned>
}

/// One app on a dock: what it stands for and its name.
type Pinned {
    title: Text
}

default view shell_parts for ShellParts

/// The shell window: each kind in a section of its own page.
view shell_parts(x: ShellParts) {
    state last: Text = ""
    std.page(x) {
        ui.Section("Chrome") {
            ui.Chrome(name: "A bar", shape: .square, edge: .top) {
                row(gap: .related, align: .center) {
                    ui.Text("A bar's body", role: .caption)
                    ui.Button("Open", icon: .folder, role: .plain) {
                        on press: set(last, "Open")
                    }
                }
            }
            ui.Chrome(name: "A dock", shape: .rounded, edge: .bottom) {
                row(gap: .related, align: .center) {
                    ui.Text("A dock's body, inset and rounded", role: .caption)
                }
            }
        }
        ui.Section("A bar's controls") {
            ui.Text(
                "The chips a bar holds: people, an invite, and a state it stands for.",
                role: .caption,
                secondary: true,
            )
            row(gap: .related, align: .center) {
                ui.BarChip("Who is here", trailing: .chevron_down) {
                    on press: set(last, "Who is here")
                }
                ui.BarChip("Invite", icon: .user_plus) {
                    on press: set(last, "Invite")
                }
                ui.BarChip("Bo is viewing", label: "Bo is viewing") {
                    on press: set(last, "Shared")
                }
            }
        }
        ui.Section("A run of items") {
            ui.DragRun(axis: .horizontal, distribute: .center, name: "Pinned") {
                on drop(moved: Pinned, at: Int): mark_moved(last, moved, at)
                for pinned in x.apps {
                    ui.DragItem(pinned, title: pinned.title) {
                        on activate: set(last, pinned.title)
                        ui.Button(pinned.title, icon: .folder, role: .plain, icon_only: true) {
                            on press: set(last, pinned.title)
                        }
                    }
                }
            }
            ui.DragRun(axis: .vertical, name: "Pinned, standing") {
                on drop(moved: Pinned, at: Int): mark_moved(last, moved, at)
                for pinned in x.apps {
                    ui.DragItem(pinned, title: pinned.title) {
                        on activate: set(last, pinned.title)
                        ui.ListRow(pinned.title)
                    }
                }
            }
        }
        ui.Section("Unread") {
            ui.SectionDivider(tone: .accent)
            ui.Text(
                "The line above marks where a reader's unread messages start.",
                role: .caption,
                secondary: true,
            )
        }
    }
}

/// The shell group, with three apps pinned to the dock.
scenario "shell parts" {
    let g = root Gallery
    let s = g.chrome.insert(ShellParts(title: "Shell"))
    s.apps.insert(Pinned(title: "Notes"))
    s.apps.insert(Pinned(title: "Tasks"))
    s.apps.insert(Pinned(title: "Mail"))
}

/// Note the item moved, in the window's last answer.
action mark_moved(last: Text, moved: Pinned, at: Int) {
    last = "Moved {moved.title} to {at}"
}
