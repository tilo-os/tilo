/// The parts the shell is made of, as a view places them: a bar or a dock's body, a run of
/// items a person reorders by dragging one, a view picked up and dragged to another, the target
/// that takes it, and the divider a conversation marks its first unread line with. Each draws
/// with the theme's own finish and rhythm, as the shell and the system search use them.
type ShellParts {
    title: Text
    /// What the dock's items stand for: the apps pinned to it.
    apps: Collection<Pinned>
    /// Who is at the apps' tabs.
    owner: Option<Actor>
}

/// One app on a dock: what it stands for and its name.
type Pinned {
    title: Text
}

/// The shell's page: each kind in a section of its own.
view shell_parts(x: ShellParts) {
    state last: Text = ""
    ui.Text(
        "The parts a bar or a dock is made of: its ground, its tabs, its chips and what it drags.",
        role: .caption,
        secondary: true,
    )
    stack(max: 48rem, gap: .section) {
        part("Chrome", "The ground a bar or a dock stands on, in the theme's finish.") {
            ui.Chrome(name: "A bar", shape: .square, edge: .top) {
                row(gap: .related, align: .center, padding: .related) {
                    ui.Text("A bar's body", role: .caption)
                    ui.Button("Open", icon: .folder, role: .plain) {
                        on press: set(last, "Open")
                    }
                }
            }
            ui.Chrome(name: "A dock", shape: .rounded, edge: .bottom) {
                row(gap: .related, align: .center, padding: .related) {
                    ui.Text("A dock's body, inset and rounded", role: .caption)
                }
            }
        }
        part(
            "Tab strip",
            "A frame's tabs: the one in front lifted, who is at each, and a changed one marked.",
        ) {
            ui.TabStrip(x.apps.first(), focused: true) {
                for pinned in x.apps {
                    ui.TabItem(
                        pinned,
                        title: pinned.title,
                        icon: .file_text,
                        modified: pinned.title == "Tasks",
                    ) {
                        ui.TabFace(x.owner, name: "Ada Lovelace")
                    }
                }
            }
        }
        part("Bar chip", "What a bar holds: who is here, an invite, a state it stands for.") {
            row(gap: .related, align: .center, wrap: true) {
                ui.BarChip("Who is here", label: "Ada and Bo", trailing: .chevron_down) {
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
        part("Drag run", "Items a person reorders by dragging one, across or stacked.") {
            ui.DragRun(axis: .horizontal, distribute: .start, name: "Pinned") {
                on drop(moved: Pinned, at: Int): mark_moved(last, moved, at)
                for pinned in x.apps {
                    ui.DragItem(pinned, title: pinned.title) {
                        on activate: set(last, pinned.title)
                        ui.Button(pinned.title, icon: .folder, icon_only: true) {
                            on press: set(last, pinned.title)
                        }
                    }
                }
            }
            ui.DragRun(axis: .vertical, align: .stretch, name: "Pinned, stacked") {
                on drop(moved: Pinned, at: Int): mark_moved(last, moved, at)
                for pinned in x.apps {
                    ui.DragItem(pinned, title: pinned.title) {
                        on activate: set(last, pinned.title)
                        ui.ListRow(pinned.title)
                    }
                }
            }
        }
        part(
            "Drag source and drop target",
            "A view picked up and carried to a target that takes it.",
        ) {
            for pinned in x.apps {
                ui.DragSource(pinned.id(), title: pinned.title) {
                    ui.ListRow(pinned.title, icon: .folder)
                }
            }
            ui.DropTarget {
                on drop(target: host.Resource): dropped(last, target)
                ui.Card(title: "A drop target") {
                    ui.Text(
                        "A pinned app let go over this card is taken here.",
                        role: .caption,
                        secondary: true,
                    )
                }
            }
        }
        part("Section divider", "The line a conversation marks its first unread message with.") {
            ui.SectionDivider(tone: .accent)
        }
    }
}

/// The shell group, with three apps pinned to the dock.
scenario "shell parts" {
    let g = root Gallery
    let s = g.chrome.insert(ShellParts(title: "Shell", owner: invoker))
    s.apps.insert(Pinned(title: "Notes"))
    s.apps.insert(Pinned(title: "Tasks"))
    s.apps.insert(Pinned(title: "Mail"))
}

/// Note the item moved, in the window's last answer.
action mark_moved(last: Text, moved: Pinned, at: Int) {
    last = "Moved {moved.title} to {at}"
}

/// Note what a drop target took, in the window's last answer.
action dropped(last: Text, target: host.Resource) {
    last = "Dropped {meta(target).noun}"
}
