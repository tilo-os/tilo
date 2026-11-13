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
/// The run holds the items the shell's record keeps, which a drag rearranges. The apps running
/// now stand beside it (`dock_running_tiles`): a running app the person never pinned names no
/// record, so a drop could carry nothing of it — it is drawn, and pressed, and not dragged.
view dock_run(s: Shell, d: Desk) {
    let standing = s.dock_edge == .left or s.dock_edge == .right
    let across = s.dock_fit == .full
    let running = unfiled_running(s, d)
    if standing {
        stack(gap: .tight, align: .center) {
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
            dock_running_tiles(s, d, running)
        }
    } else if across {
        scroll(axis: .horizontal) {
            row(gap: .tight, align: .center) {
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
                dock_running_tiles(s, d, running)
            }
        }
    } else {
        scroll(axis: .horizontal) {
            row(gap: .tight, align: .center) {
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
                dock_running_tiles(s, d, running)
            }
        }
    }
}

/// The apps running on this device that the dock's record does not keep, one item an app: the
/// window of it the person was in last, named and drawn as that window is. A window of another
/// of the person's devices is not a window of this desk's dock, and an app the record already
/// holds (pinned, or pinned and running) stands in the run, once.
fn unfiled_running(s: Shell, d: Desk) -> List<DockItem> {
    let windows = d.windows.filter(w -> w.local).sort_by(w -> w.recency)
    return windows
        .filter(w -> front_of_its_app(windows, w))
        .filter(w -> not s.dock_items.any(i -> i.key == w.app_key))
        .map(w -> running_item(w))
}

/// Whether `w` is the window of its app the person was in last: the first of `windows` showing
/// that app, they being in the order of the walk.
fn front_of_its_app(windows: List<Win>, w: Win) -> Bool {
    if let front = windows.find(v -> v.app_key == w.app_key) {
        return front.key == w.key
    }
    return false
}

/// The dock's item for the running window `w`: the window's app, by its `app_key`, named as the
/// window names it, with the window's own glyph.
fn running_item(w: Win) -> DockItem {
    return DockItem(
        key: w.app_key,
        title: w.origin_app,
        icon: w.icon,
        pinned: false,
        running: true,
        badge: 0,
        attention: false,
    )
}

/// The running apps the record does not keep, each a tile of its own beside the run: a press
/// opens or focuses its window, and it keeps its badge. It is not a `ui.DragItem`, which names
/// the record an item stands for: a running app the person never pinned has one only here, not
/// in the shell's record. Pinning the app puts it in the run, where it is rearranged.
view dock_running_tiles(s: Shell, d: Desk, running: List<DockItem>) {
    if not running.is_empty() {
        row(gap: .tight, align: .center) {
            for item in running {
                dock_tile(s, d, item)
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
action open_item(s: Shell, desk: Desk, key: Text (label: "App")) {
    if let w = desk.windows.find(w -> w.app_key == key) {
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
