/// The dock: a run of items on the edge the person set — the launcher, the apps and
/// resources they pinned, and the apps running now — each with its badge, and the ones that
/// want the person marked. It is a view of the person's `Shell` record and of the desk;
/// every change it offers is one of `chrome.tl`'s actions.
///
/// The gesture is Rust (`ui.DragRun`, the toolkit's run): the person picks an item up, the
/// run shows where it will land, and letting go runs one action — `place_item` — which is
/// what an agent or a shortcut runs too, attributed and undone like any change.
///
/// Nothing at all while it is off or hidden for now (`dock_shows`: the person's `dock_shown`,
/// and when its `Showing` says). The ground sits in the view that places it on the output's
/// edge: edge to edge and square on the `full` fit, a rounded body inset from the edges on the
/// floating one, over what is under it. A dock on the top or bottom edge runs along that edge;
/// one on the leading or trailing edge stands up it.
view dock(s: Shell, d: Desk) {
    if dock_shows(s) {
        dock_run(s, d)
    }
}

/// The dock's items in a run the person rearranges by dragging one onto another place.
/// Letting go runs `place_item`, the one action the gesture commits; a press on an item opens
/// what it stands for. A floating dock is one compact group centred on its edge; a dock that
/// rides the whole edge is the taskbar: the dock's items leading, then the time and the
/// device's status trailing, in the one bar. A dock standing up a leading or trailing edge
/// runs top to bottom.
view dock_run(s: Shell, d: Desk) {
    let standing = s.dock_edge == .left or s.dock_edge == .right
    let across = s.dock_fit == .full
    if standing {
        ui.DragRun(axis: .vertical, distribute: .center) {
            on drop(moved: DockItem, at: Int): place_item(s, moved, at)
            for item in s.dock_items {
                if item.pinned or s.dock_running {
                    ui.DragItem(item, title: item.title) {
                        on activate: dock_open(s, item)
                        dock_tile(s, d, item)
                    }
                }
            }
        }
    } else if across {
        // The taskbar: the dock's items leading, the time and the device's status trailing,
        // in the one bar.
        ui.DragRun(axis: .horizontal) {
            trailing: {
                clock(d)
                bell(d)
            }
            on drop(moved: DockItem, at: Int): place_item(s, moved, at)
            for item in s.dock_items {
                if item.pinned or s.dock_running {
                    ui.DragItem(item, title: item.title) {
                        on activate: dock_open(s, item)
                        dock_tile(s, d, item)
                    }
                }
            }
        }
    } else {
        ui.DragRun(axis: .horizontal, distribute: .center) {
            on drop(moved: DockItem, at: Int): place_item(s, moved, at)
            for item in s.dock_items {
                if item.pinned or s.dock_running {
                    ui.DragItem(item, title: item.title) {
                        on activate: dock_open(s, item)
                        dock_tile(s, d, item)
                    }
                }
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
