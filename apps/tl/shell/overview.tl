/// The overview: what the workspace holds, to search and move things in, over the windows it
/// covers. It reads the layout and the windows the session publishes (`device.windows`),
/// never a copy, and every choice it offers is an action the session publishes
/// (`so.tilo.session`), so the layout stays the session's: each change is attributed and
/// undoable.
///
/// The picture is the layout's: each strip top to bottom, its windows left to right at the share
/// of the strip's width they keep, a group's other tabs as narrower slices beside
/// the tab in front, under the strip's name. Each place shows the window's own live picture
/// (`ui.WindowPreview`), the compositor's, under its name in the card's head and the app it
/// shows below it; it is a surface of the shell: the shell's bar stays above it, so it holds no
/// sidebar of its own. The page keeps its gutter, so the overview stands inside the shell the way
/// every other page does.
view overview() {
    state query: Text = device.overlays.spotlight
    let windows = device.windows.all.sort_by(w -> w.place)
    let here = windows.filter(w -> w.workspace_key == front_workspace())
    page(full) {
        stack(gap: .section) {
            row(gap: .related, align: .center, wrap: true) {
                ui.Text("Overview", role: .heading)
                ui.Text(front_workspace_name(), role: .caption, secondary: true)
                spacer
                ui.Button("New workspace", icon: .plus, size: .small) {
                    on press: session.new_workspace()
                }
                ui.Button("Join a workspace", icon: .users, size: .small) {
                    on press: session.join_workspace()
                }
            }
            ui.TextInput(bind: query, placeholder: "Search windows") {
                on submit: session.search(query: query)
            }
            workspaces_here()
            if query == "" {
                if here.is_empty() {
                    ui.EmptyState(
                        "No windows",
                        message: "Open something to see its window",
                        icon: .app_window,
                        inline: true,
                    )
                }
                for s in window_strips(here) {
                    picture(here, s)
                }
            } else {
                found(windows.matching(query))
            }
        }
    }
}

/// The workspaces the person is in, as chips that switch to them, and how many people are here.
view workspaces_here() {
    if not layout.workspaces.is_empty() or not layout.people.is_empty() {
        row(gap: .related, align: .center, wrap: true) {
            for w in layout.workspaces.sort_by(w -> w.name) {
                ui.Chip(
                    w.name,
                    icon: if w.current { .layers } else { .folder },
                    selected: w.current,
                ) {
                    on press: session.switch_workspace(workspace: w.key)
                }
            }
            ui.Facts {
                ui.Fact(access_words(), icon: .lock)
                if not layout.people.is_empty() {
                    ui.Fact("{layout.people.len()} here", icon: .users)
                }
            }
        }
    }
}

/// Who the workspace in front is shared with, naming people while it helps and counting once it
/// stops: "Shared with Ada and 4 others" is a fact where five names are a wall of text.
fn access_words() -> Text {
    let me = device.people.me.name
    let others = workspace.members.people.filter(m -> m.name() != me)
    let first = others.first()
    let name = if let f = first { f.name() } else { "" }
    let second = if let s = others.at(1) { s.name() } else { "" }
    let more = others.len() - 1
    if others.is_empty() {
        std.say("Only you")
    } else if others.len() == 1 {
        std.say("Shared with {name}")
    } else if others.len() == 2 {
        std.say("Shared with {name} and {second}")
    } else {
        std.say("Shared with {name} and {more} others")
    }
}

/// One strip: its windows as places leading to trailing, each at its share of the strip's
/// width, opening what it shows and carrying its menu of moves. A phone's output holds one place
/// across, so there the strip's places stand one under the other.
view picture(windows: List<device.Window>, strip: Text) {
    let fronts = windows
        .filter(v -> v.strip_key == strip and v.tab_index == 0)
        .sort_by(v -> v.place)
    ui.Section(strip_name(strip)) {
        if space.compact {
            stack(gap: .related) {
                for w in fronts {
                    window_place(w, 0.0, peek: false, compact: true, roomy: false)
                    for v in behind_tabs(windows, w) {
                        window_place(v, 0.0, peek: false, compact: true, roomy: false)
                    }
                }
            }
        } else {
            let roomy = space.width_class == .expanded or space.width_class == .wide
            row(gap: .related, align: .start) {
                for w in fronts {
                    window_place(w, w.share, peek: false, compact: false, roomy: roomy)
                    for v in behind_tabs(windows, w) {
                        // A tab behind stands 0.75 as wide as its picture is tall, and a picture
                        // is 1.6 times as wide as it is tall: beside the window in front, that is
                        // 0.469 of its width, a slice that keeps its picture and its name and no
                        // commands of its own.
                        window_place(v, w.share * 0.469, peek: true, compact: false, roomy: roomy)
                    }
                }
            }
        }
    }
}

/// The tabs behind `w` in its strip, in their order.
fn behind_tabs(windows: List<device.Window>, w: device.Window) -> List<device.Window> {
    return windows
        .filter(v -> v.group_key == w.group_key and v.tab_index > 0)
        .sort_by(v -> v.tab_index)
}

/// One window's place: its live picture, the app it shows and the device it runs on when that is
/// not this one, lifted while it has focus, with a menu that moves it to another strip or
/// workspace, closes it, or asks about it. Its card takes `weight` of the strip's width, so a
/// place stands as wide as the window stands in its strip, up to the width a place shows at on
/// this screen: past that the picture would stand taller than the page it is on, so a window
/// wider than the screen allows is pictured no wider than that. A compact output holds one place
/// across, so there the card takes the room it is given instead of a share of a strip.
///
/// A `peek` is a tab beside the window in front of it, in a slice in which the window's
/// commands and the buttons of an app that stopped answering would not fit: it keeps the
/// picture and its name, and its commands are the window's own when it comes forward. It is
/// pictured no wider than its 0.469 slice of what a place may take, so a row of places never
/// hands a tab behind more width than the window it stands behind.
///
/// `compact` and `roomy` are the strip's room (`picture`), never the card's own, which is the
/// card's size.
view window_place(w: device.Window, weight: Float, peek: Bool, compact: Bool, roomy: Bool) {
    let cap = if compact {
        100rem
    } else if roomy {
        if peek { 12rem } else { 26rem }
    } else if peek { 9rem } else { 20rem }
    ui.Card(
        item: w,
        title: w.title,
        selected: w.focused,
        fill(if compact { 0.0 } else { weight }),
        max: cap,
    ) {
        stack(gap: .tight) {
            layers(aspect: 1.6) {
                window_picture(w)
            }
            if peek {
                ui.Text(app_line(w), role: .caption, secondary: true, lines: 1, truncate)
            } else {
                row(gap: .tight, align: .center) {
                    ui.Text(app_line(w), role: .caption, secondary: true, lines: 1, truncate)
                    spacer
                    this_window_menu(w, .none, false)
                }
            }
        }
        if w.not_responding and not peek {
            hung_window(w)
        }
        on activate: session.focus(window(w))
    }
}

/// The app a window shows, and the device it runs on when it is not this one: a window on the
/// person's other device is named with it until it can be pictured live.
fn app_line(w: device.Window) -> Text {
    if w.device_name != "" {
        return "{w.origin_app} on {w.device_name}"
    }
    return w.origin_app
}

/// What a search finds among the person's windows, in every workspace: each one, going there
/// or moving it here. What apps hold is the platform search's, not the overview's.
view found(elsewhere: List<device.Window>) {
    if elsewhere.is_empty() {
        ui.EmptyState("Nothing matches", icon: .search, inline: true)
    } else {
        ui.Section("Windows") {
            for w in elsewhere.take(16) {
                window_line(w)
            }
        }
    }
}

/// One window a search found, as a row that opens there, with the way to bring it here.
view window_line(w: device.Window) {
    ui.ListRow(
        w.title,
        secondary: app_line(w),
        meta: w.workspace,
        item: w,
        icon: if w.preview == "withheld" { .eye_off } else { .app_window },
    ) {
        trailing: {
            this_window_menu(w, .none, false)
        }
        on activate: session.focus(window(w))
    }
}

/// The strips a list of windows stands on, top to bottom.
fn window_strips(windows: List<device.Window>) -> List<Text> {
    let in_order = windows.sort_by(w -> w.place).map(w -> w.strip_key).filter(s -> s != "")
    return in_order.group(s -> s).map(p -> p.key)
}

/// The name of the strip `key`: its own, or the app of the first window on it when it has none,
/// so no strip is named by where it stands.
fn strip_name(key: Text) -> Text {
    if let w = device.windows.all.filter(w -> w.strip_key == key).sort_by(v -> v.place).first() {
        if w.strip != "" {
            return w.strip
        }
        return w.origin_app
    }
    return ""
}
