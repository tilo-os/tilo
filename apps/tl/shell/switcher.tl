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
    let desk = d.windows.filter(w -> w.workspace_key == front_workspace(d)).sort_by(w -> w.recency)
    let pick = desk.find(w -> w.focused)
    let front_app = if let p = pick { p.app_key } else { "" }
    let windows = if d.switcher_scope == "app" {
        desk.filter(w -> w.app_key == front_app)
    } else { desk }
    let count = windows.len()
    let per_page = page_size(space.compact, space.width, space.height)
    let at = if let p = pick { walk_place(windows, p) } else { 1 }
    let from = if at > per_page / 2 { at - per_page / 2 } else { 1 }
    let shown = windows.filter(
        w -> walk_place(windows, w) >= from and walk_place(windows, w) < from + per_page,
    )
    let this_app = if d.switcher_scope == "app" {
        if let p = pick { p.origin_app } else { "" }
    } else { "" }
    let caption = if this_app != "" {
        "{this_app} · {at} of {count}"
    } else if count > per_page { "{at} of {count}" } else { "" }
    row(fill, align: .center, distribute: .center, padding: .related) {
        ui.FloatingPanel {
            body: {
                if count == 0 {
                    ui.EmptyState(
                        "No windows",
                        message: "Open something to switch to it",
                        icon: .app_window,
                        inline: true,
                    )
                } else {
                    stack(gap: .section) {
                        // A wall of windows: each card keeps the gap cards stand by.
                        row(gap: .sibling, align: .center, distribute: .center, wrap: true) {
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

/// How many windows one page of the switcher holds: the cards one line of the panel holds, times
/// the lines that fit under it at the text size the person reads. A card is never narrower than
/// its widget's own floor (ten lines of text beside the names, thirteen over them, which is what
/// a card is sized by whichever smaller `min` the view asks for), and a line of cards is as tall
/// as that floor's picture plus the names under it.
fn page_size(compact: Bool, width: Float, height: Float) -> Int {
    let card = if compact { 14.0 } else { 18.0 }
    let line = if compact { 10.0 } else { 14.0 }
    let across = (width - 2.0) / card
    let down = (height - 8.0) / line
    let page = if across < 1.0 or down < 1.0 { 1 } else { across.to_int() * down.to_int() }
    if page > 6 { 6 } else { page }
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
        if w.not_responding {
            hung_window(w)
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

/// What a window's card shows of the window itself: its live picture where this names it, over the
/// app's glyph. A window that may not be pictured (its app holds secrets) shows the glyph alone,
/// marked as withheld.
view window_face(w: Win) {
    layers(min: if space.compact { 4rem } else { 11rem }, aspect: 1.6) {
        window_picture(w)
    }
}

/// A window's own live picture, drawn from the window itself in the place this reports
/// (`ui.WindowPreview`). Where there is none to draw, the app's glyph stands in it, so the place
/// still says which window it is; a window that holds secrets is marked as withheld.
///
/// Every place a window is pictured frames this at the window's shape: the switcher's cards and
/// the overview's places, so the same window never looks like two things.
view window_picture(w: Win) {
    layers {
        ui.WindowPreview(w.key)
        if w.preview != "live" {
            row(fill, align: .center, distribute: .center) {
                ui.Icon(
                    if w.preview == "withheld" { .eye_off } else { w.icon },
                    size: .large,
                    secondary: true,
                )
            }
        }
    }
}
