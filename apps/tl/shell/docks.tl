/// The dock: a run of items on the edge the person set (the launcher, the apps and resources they
/// pinned, and the apps running now), each with its badge, and the ones that want the person
/// marked. It is a view of the person's `Shell` record and of the desk; every change it offers is
/// one of `chrome.tl`'s actions.
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
    if dock_shows(s, d) {
        dock_run(s, d)
    }
}

/// The dock's items in a run the person rearranges by dragging one onto another place.
/// Letting go runs `place_item`, the one action the gesture commits; a press on an item opens
/// what it stands for. A floating dock is one compact group centred on its edge; a dock that
/// rides the whole edge is the taskbar: the dock's items leading, then the time and the
/// device's status trailing, in the one bar the theme draws as a bar's. A dock standing up a
/// leading or trailing edge runs top to bottom. A dock along the top or the foot holding more
/// than its edge has room for scrolls along it, so the dock stays on the output.
///
/// Every item is one the shell's record holds. An app that is running but was never pinned is
/// not drawn here: a run's drop carries the record an item stands for, and an item built from
/// the desk has none, so it could not be dragged with the others
/// (`drag_run_component.rs`, the drop's `Value::Resource`). Pinning the app puts it on the dock.
view dock_run(s: Shell, d: Desk) {
    let standing = s.dock_edge == .left or s.dock_edge == .right
    let across = s.dock_fit == .full
    if standing {
        ui.DragRun(axis: .vertical, distribute: .center) {
            on drop(moved: DockItem, at: Int): place_item(s, moved, at)
            for item in s.dock_items {
                if item.pinned or s.dock_running {
                    ui.DragItem(item, title: item.title) {
                        on activate: open_item(s, d, item.key)
                        dock_tile(s, d, item)
                    }
                }
            }
        }
    } else if across {
        scroll(axis: .horizontal) {
            ui.DragRun(axis: .horizontal) {
                trailing: {
                    clock(d)
                    bell(d)
                }
                on drop(moved: DockItem, at: Int): place_item(s, moved, at)
                for item in s.dock_items {
                    if item.pinned or s.dock_running {
                        ui.DragItem(item, title: item.title) {
                            on activate: open_item(s, d, item.key)
                            dock_tile(s, d, item)
                        }
                    }
                }
            }
        }
    } else {
        scroll(axis: .horizontal) {
            ui.DragRun(axis: .horizontal, distribute: .center) {
                on drop(moved: DockItem, at: Int): place_item(s, moved, at)
                for item in s.dock_items {
                    if item.pinned or s.dock_running {
                        ui.DragItem(item, title: item.title) {
                            on activate: open_item(s, d, item.key)
                            dock_tile(s, d, item)
                        }
                    }
                }
            }
        }
    }
}

/// What a press on a dock item does, by the app the item stands for: a window of that app on this
/// desk comes to the front, so the desk says which window the person is in and the top bar's
/// commands are its own; the window is the one the item was drawn for, so a press never opens a
/// second copy. An app with no window open is opened (`session.open_app`), and the launcher
/// shows.
///
/// It takes the app's key and not the item: an item the shell's record holds reaches an action as
/// a record reference, and the press names the one thing it needs, so a run's item reaches it
/// without a `DockItem` parameter to fit.
action open_item(s: Shell, d: Desk, key: Text) {
    if let w = d.windows.find(w -> w.app_key == key) {
        session.focus(window: window(w))
    } else if key == "launcher" {
        device.show_surface(device.desk, surface: "launcher")
    } else { session.open_app(app: key) }
}

/// One dock item: its glyph, with its badge on the glyph's top trailing corner (the count of what
/// is new, or the mark that it wants the person), as a dock has always badged an app. The count is
/// the button's own (`count:`), so it stands on the glyph and never on the tile beside it, and the
/// name read out says it.
view dock_tile(s: Shell, d: Desk, item: DockItem) {
    let name = if item.badge == 0 { item.title } else { "{item.title}, {item.badge} new" }
    ui.Button(
        name,
        icon: item.icon,
        icon_only: true,
        role: .plain,
        count: item.badge,
        dot: item.attention,
        alert: item.attention,
    ) {
        on press: open_item(s, d, item.key)
    }
}
