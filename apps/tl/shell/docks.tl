/// The dock: a run of items on the edge the person set — the launcher, the apps and
/// resources they pinned, and the apps running now — each with its badge, and the ones that
/// want the person marked. It is a view of the person's `Shell` record and of the desk;
/// every change it offers is one of `chrome.tl`'s actions.
///
/// The gesture is Rust (`ui.DragRun`, the toolkit's run): the person picks an item up, the
/// run shows where it will land, and letting go runs one action — `place_item` — which is
/// what an agent or a shortcut runs too, attributed and undone like any change.

/// The dock, nothing at all while it is off.
view dock(s: Shell, d: Desk) {
    if s.dock_shown {
        // The ground: edge to edge and square, or inset and rounded, floating over what is
        // under it. A dock on the top or bottom edge runs along it; one on the leading or
        // trailing edge stands up it.
        if s.dock_edge == .top or s.dock_edge == .bottom {
            if s.dock_fit == .full {
                row(gap: .related, align: .center, padding: .tight) {
                    dock_run(s, d)
                }
            } else {
                row(gap: .related, align: .center, padding: .related) {
                    dock_run(s, d)
                }
            }
        } else {
            if s.dock_fit == .full {
                stack(gap: .related, align: .center) {
                    dock_run(s, d)
                }
            } else {
                stack(gap: .related, align: .center) {
                    dock_run(s, d)
                }
            }
        }
    }
}

/// The dock's items in a run the person rearranges by dragging one onto another place.
/// Letting go runs `place_item`, the one action the gesture commits; a press on an item opens
/// what it stands for.
view dock_run(s: Shell, d: Desk) {
    ui.DragRun(axis: if s.dock_edge == .top or s.dock_edge == .bottom { .horizontal } else { .vertical }) {
        on drop(moved: DockItem, at: Int): place_item(s, moved, at)
        for item in s.dock_items {
            ui.DragItem(item, title: item.title) {
                on activate: dock_open(s, item)
                dock_tile(s, d, item)
            }
        }
    }
}

/// One dock item: its glyph, its badge, and the mark that it wants the person.
view dock_tile(s: Shell, d: Desk, item: DockItem) {
    stack(gap: .tight, align: .center) {
        ui.Button(item.title, icon: item.icon, icon_only: true, role: .plain) {
            on press: dock_open(s, item)
        }
        if item.badge > 0 {
            ui.Badge(count: item.badge, tone: if item.attention { .warning } else { .neutral })
        }
        if item.attention {
            ui.Badge(dot: true, tone: .warning)
        }
    }
}

/// Opens what a dock item stands for: launches the app, or goes to the resource.
action dock_open(s: Shell, item: DockItem) (icon: .external_link, label: "Open {item.title}") {
    outside("session.launch", item.key)
}
