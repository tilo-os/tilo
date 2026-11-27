/// Reporting a problem: the screen dims to the desktop's ground, every window of the workspace in
/// front stands as a card with its own live picture, and the person chooses the window the report
/// is about — a click on its card, or a rectangle dragged over it. The report's box then opens
/// about that window, as pointing at what is wrong did before the shell was tilolang: what the
/// window is and the part the person points at in it are what the report carries.
///
/// A shortcut, the palette and an agent start a pick (`report_problem`); a card press and a drag
/// are picks of their own (`pick_report_window`, `pick_report_region`); Cancel leaves with nothing
/// attached (`cancel_report`). The pick is also kept on the shell's document
/// (`report_picks`), so what the report was made about is the person's own to read afterwards.
///
/// Picking reads the windows the session publishes (`device.windows`) and leans on nothing app-local:
/// the report is filed where every report is, by the session's own shortcut.
///
/// One window a report is about, as the shell keeps it: the tab it stands in, the name a person
/// reads it by, and the app it shows.
type ReportPick {
    key: Text
    title: Text
    app_name: Text
}

/// How wide a window's card stands in the picker, in canvas pixels, and how tall: the window's
/// shape, wide enough to see which window it is.
fn report_card_width() -> Int {
    return 520
}

/// See [`report_card_width`].
fn report_card_height() -> Int {
    return 360
}

/// The room between two cards.
fn report_gap() -> Int {
    return 24
}

/// How many cards stand across.
fn report_columns() -> Int {
    return 3
}

/// The windows a report may be about: the ones the workspace in front shows, in the order the
/// person meets them. A layout that names no workspace in front (nothing is arranged yet) offers
/// every window there is.
fn report_windows() -> List<device.Window> {
    let all = device.windows.all.sort_by(w -> w.place)
    let front = front_workspace()
    if front == "" {
        return all
    }
    return all.filter(w -> w.workspace_key == front)
}

/// Where the picker stands the window at `index` among the ones it offers: the corner and size of
/// its card, in canvas pixels. What the drawing and the rectangle the person drags both read, so a
/// rectangle picks exactly the cards it covers.
fn report_cell(index: Int) -> List<Int> {
    let columns = report_columns()
    let across = columns - 1
    let column = if across > 0 { index - index / columns * columns } else { 0 }
    let row = index / columns
    return [
        report_gap() + column * (report_card_width() + report_gap()),
        report_gap() + row * (report_card_height() + report_gap()),
        report_card_width(),
        report_card_height(),
    ]
}

/// The window the rectangle `area` (its x, y, width and height in canvas pixels) covers first, by
/// its key: empty for a rectangle over none of them.
fn report_window_in(area: List<Int>) -> Text {
    let x = area.at(0) ?? 0
    let y = area.at(1) ?? 0
    let width = area.at(2) ?? 0
    let height = area.at(3) ?? 0
    if width <= 0 or height <= 0 {
        return ""
    }
    let keys = report_windows().map(w -> w.key)
    for key in keys {
        let cell = report_cell(keys.index_of(key) ?? 0)
        let left = cell.at(0) ?? 0
        let top = cell.at(1) ?? 0
        let right = left + (cell.at(2) ?? 0)
        let bottom = top + (cell.at(3) ?? 0)
        if x < right and left < x + width and y < bottom and top < y + height {
            return key
        }
    }
    return ""
}

/// Start a pick: what a report about a problem is about, chosen on the whole screen. The windows
/// stand as cards; one is picked, and the report's box opens about it.
action report_problem(s: Shell) (icon: .flag, label: "Report a problem", group: "Feedback") {
    s.report_picks = []
    s.report_area = []
    s.reporting = true
}

/// The window `key` is what the report is about: it joins the report, the picker closes, and the
/// report's box opens about the window, the person free to point at the part of it that is wrong.
action pick_report_window(s: Shell, key: Text (label: "Window")) {
    if let w = report_windows().find(v -> v.key == key) {
        s.report_picks = [ReportPick(key: w.key, title: w.title, app_name: w.origin_app)]
        s.report_area = []
        s.reporting = false
        session.focus(window: window(w))
        session.shortcut(action: "report_problem")
    }
}

/// The rectangle `area` the person dragged over the picker — its x, y, width and height in canvas
/// pixels — picks the window it covers first, as a click on that card does. A rectangle over none
/// of them keeps the picker up, its outline saying what was covered.
action pick_report_region(s: Shell, area: List<Int>) (label: "Pick the window a report is about") {
    let key = report_window_in(area)
    if key != "" {
        if let w = report_windows().find(v -> v.key == key) {
            s.report_picks = [ReportPick(key: w.key, title: w.title, app_name: w.origin_app)]
            s.report_area = []
            s.reporting = false
            session.focus(window: window(w))
            session.shortcut(action: "report_problem")
        }
    } else {
        s.report_area = area
    }
}

/// Leave the pick with nothing attached: no window, no report.
action cancel_report(s: Shell) (icon: .x, label: "Cancel", group: "Feedback") {
    s.reporting = false
    s.report_picks = []
    s.report_area = []
}

/// The picker over the windows: the desktop's ground for a screen that dims, one card per window
/// with its live picture, and the line saying what a click and a drag do. It takes the pointer, so
/// nothing of a window under it is pressed while it is up.
view report_picker() {
    let windows = report_windows()
    let keys = windows.map(w -> w.key)
    // The picker's room, read here: each pin of the loop stands in a box its own size.
    let compact = space.compact
    layers(fill) {
        ui.Map(name: "What the report is about", ground: true, outline: shell.report_area, fill) {
            on frame(at: List<Int>): pick_report_region(shell, at)
            for w in windows {
                let cell = report_cell(keys.index_of(w.key) ?? 0)
                ui.MapPin(
                    name: w.title,
                    item: w.key,
                    x: cell.at(0) ?? 0,
                    y: cell.at(1) ?? 0,
                    width: cell.at(2) ?? 0,
                    height: cell.at(3) ?? 0,
                ) {
                    ui.Card(item: w, title: w.title, fill(1.0)) {
                        stack(gap: .tight, fill) {
                            window_face(w, compact)
                            window_names(w)
                        }
                        on activate: pick_report_window(shell, w.key)
                    }
                }
            }
        }
        row(at: .top, fill, align: .center, distribute: .center, padding: .group) {
            ui.Card {
                row(gap: .related, align: .center) {
                    ui.Text(
                        if windows.is_empty() {
                            "Nothing is open to report about."
                        } else {
                            "Click the window that isn't working, or drag over it."
                        },
                        role: .label,
                    )
                    ui.Button("Cancel") {
                        on press: cancel_report(shell)
                    }
                }
            }
        }
    }
}
