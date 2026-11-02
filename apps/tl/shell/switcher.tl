/// The window switcher: the windows of the desk in the order Alt+Tab walks them, in a panel over
/// what the person is in. Holding the modifier and pressing Tab is the session's walk; this view
/// shows where the walk has reached, and what letting go chooses.
///
/// It reads the desk's windows (`session.window`), whose `recency` is their place in the walk,
/// and lifts the one the walk reached (`focused`). Alt+` walks only the app in front's windows,
/// so with `switcher_scope` `app` the panel is those windows alone, named by the app and how far
/// along them the walk is. Each card shows the window's own live picture where the compositor
/// has it, else the app's glyph, over its title and the app it shows; the keys under the cards
/// are the ones the walk answers to. A desk holding more windows than fit shows the page the
/// walk has reached, with its place among them.
///
/// A flick past Alt+Tab only switches windows: the desk shows this panel once the session says
/// its walk has outlasted the flick (`switcher_revealed`), so nothing flashes at a person
/// bouncing between two windows.
///
/// A narrow display gets smaller cards and a shorter page, so the switcher stays whole over
/// what it covers rather than running past it.
///
/// Every choice it offers is the session's action: going to a window is `session.focus`, the
/// same as a press on its strip.
view switcher(d: Desk) {
    let desk = d.windows.filter(w -> w.workspace == d.workspace).sort_by(w -> w.recency)
    let pick = desk.find(w -> w.focused)
    let front_app = if let p = pick { p.app_key } else { "" }
    let windows = if d.switcher_scope == "app" { desk.filter(w -> w.app_key == front_app) } else { desk }
    let count = windows.len()
    let at = if let p = pick { walk_place(windows, p) } else { 1 }
    let from = if at > page(space.compact) / 2 { at - page(space.compact) / 2 } else { 1 }
    let shown = windows.filter(w -> walk_place(windows, w) >= from and walk_place(windows, w) < from + page(space.compact))
    let this_app = if d.switcher_scope == "app" {
        if let p = pick { p.origin_app } else { "" }
    } else { "" }
    let caption = if this_app != "" { "{this_app} · {at} of {count}" } else if count > page(space.compact) { "{at} of {count}" } else { "" }
    row(fill, align: .center, distribute: .center, padding: .related) {
        ui.FloatingPanel {
            body: {
                if count == 0 {
                    ui.EmptyState("No windows", message: "Open something and it shows here", icon: .app_window, inline: true)
                } else {
                    stack(gap: .section) {
                        row(gap: .related, align: .center, distribute: .center, wrap: true) {
                            for w in shown {
                                window_card(w)
                            }
                        }
                        row(gap: .related, align: .center, distribute: .center, wrap: true) {
                            key_hints_switcher(this_app)
                            if caption != "" {
                                ui.Text(caption, role: .caption, secondary: true)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// How many windows a page of the switcher holds: as many cards as a display shows at its
/// smallest, and three on a display too narrow for a row of them, so a desk of more windows
/// pages rather than running off the display.
fn page(compact: Bool) -> Int {
    if compact { 3 } else { 6 }
}

/// A window's place in the walk over `windows`: one for the first one it visits. The walk's own
/// order (`recency`) is the desk's, so two windows of one app keep their places among the rest
/// and the app's page counts them alone.
fn walk_place(windows: List<Win>, w: Win) -> Int {
    return windows.filter(v -> v.recency <= w.recency).len()
}

/// One window of the walk: its picture or its app's glyph, its title, and the app it shows.
/// The card the walk has reached is the lifted one. A narrow display stands the picture beside
/// the names rather than over them, so a phone shows a page of windows rather than one.
view window_card(w: Win) {
    ui.Card(item: w, selected: w.focused, min: if space.compact { 5rem } else { 11rem }) {
        if space.compact {
            row(gap: .tight, align: .center) {
                window_face(w)
                window_names(w)
            }
        } else {
            stack(gap: .tight) {
                window_face(w)
                window_names(w)
            }
        }
        on activate: session.focus(window(w))
    }
}

/// The window's title and the app it shows, under its picture or beside it.
view window_names(w: Win) {
    stack(gap: .tight) {
        ui.Text(w.title, role: .label, lines: 1, truncate)
        ui.Text(w.origin_app, role: .caption, secondary: true, lines: 1, truncate)
    }
}

/// What a window's card shows of the window itself: its live picture, drawn by the compositor
/// where this names it, over the app's glyph. A window that may not be pictured (its app holds
/// secrets) shows the glyph alone, marked as withheld.
view window_face(w: Win) {
    layers(min: if space.compact { 4rem } else { 11rem }, aspect: 1.6) {
        ui.WindowPreview(w.key)
        if w.preview != "live" {
            row(fill, align: .center, distribute: .center) {
                ui.Icon(if w.preview == "withheld" { .eye_off } else { w.icon }, size: .large, secondary: true)
            }
        }
    }
}
