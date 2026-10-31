/// The window switcher: the windows of the desk by recency, as Alt+Tab walks them, with the
/// app each shows and who else is on it. Holding the modifier and pressing Tab is the
/// session's; this view is the panel that shows what the walk has reached, and what a press
/// chooses.
///
/// It reads the desk's windows (`session.window`, most recently used first); every choice it
/// offers is the session's action, and letting go of the modifier goes to the chosen one.
view switcher(d: Desk) {
    let windows = d.windows.filter(w -> w.workspace == d.workspace).sort_by(w -> w.recency)
    if windows.is_empty() {
        ui.EmptyState("No windows", message: "Open something and it shows here", icon: .app_window, inline: true)
    } else {
        ui.Scroll {
            stack(gap: .related) {
                grid(min: 9rem) {
                    for w in windows.take(20) {
                        window_tile(w)
                    }
                }
                key_hints_switcher()
            }
        }
    }
}

/// One window of the walk: its app's glyph, its title, the app it shows, and a lift while it is
/// the chosen one.
view window_tile(w: Win) {
    ui.Card(item: w, title: w.title, selected: w.focused) {
        stack(gap: .related, align: .center) {
            ui.Icon(if w.preview == "withheld" { .eye_off } else { .app_window }, size: .large, secondary: not w.focused)
            ui.Text(w.title, role: .label, lines: 1, truncate)
            ui.Text(w.origin_app, role: .caption, secondary: true, lines: 1)
        }
        on activate: focus_window(w)
    }
}
