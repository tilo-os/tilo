/// The widgets the person keeps on their shell: what plays (`music`), who is here (`team`), today
/// and the month around it (`calendar`), and what this machine is doing (`system_monitor`).
///
/// Each rests wherever the person dropped it, as fractions of the room it can move in on the
/// display it was dropped on, so it keeps to its edge when the display's resolution changes; a
/// press on its quiet form opens its card in place, the card's header drags it, and its menu
/// changes when it shows or takes it off the shell with the Undo that brings it back. The choices
/// are the person's settings (`settings.widgets`), written through the settings' own actions, so
/// Settings, the shell, a script and an agent keep the same record.
///
/// One widget's card is open at a time, and only by a press: nothing opens on hover or on a timer.
/// A widget shows only when it has something to say — music only while something plays, the people
/// here only while someone is — as the Rust desktop did.
view widget_layer(a: Layout) {
    // The widget whose card is open: none while every card is folded.
    state open: Option<WidgetKind> = .none
    // The widget the person just removed: said with the Undo that keeps it again.
    state removed: Option<WidgetKind> = .none
    // The room a widget can move in: the display's own size in logical pixels, which is what a
    // drag reports and what its saved fractions are of.
    let across = device.screen.width.to_float()
    let down = device.screen.height.to_float()
    let display = device.screen.output
    layers(fill) {
        widget(.music, a, across, down, display, open, removed)
        widget(.team, a, across, down, display, open, removed)
        widget(.calendar, a, across, down, display, open, removed)
        widget(.system_monitor, a, across, down, display, open, removed)
        if let gone = removed {
            widget_removed(gone, removed)
        }
    }
}

/// One widget, when the person keeps it and it has something to say: its quiet form or its card,
/// where they left it, and its menu.
view widget(
    kind: WidgetKind,
    a: Layout,
    across: Float,
    down: Float,
    display: Int,
    open: State<Option<WidgetKind>>,
    removed: State<Option<WidgetKind>>,
) {
    let prefs = widget_prefs(kind)
    if widget_shown(kind, prefs, a) {
        let unfolded = is_kind(open, kind)
        let width = widget_width(kind, prefs, unfolded)
        let place = widget_place(kind, prefs, display)
        ui.FloatingPanel(
            width: width,
            label: widget_name(kind),
            x: widget_left(across, place.at(0) ?? 0.5, width),
            y: widget_top(down, place.at(1) ?? 0.5, widget_height(kind, unfolded)),
        ) {
            on moved(at): place_widget(kind, at, across, down, display)
            header: {
                widget_header(kind, a, prefs, unfolded, open, removed)
            }
            body: {
                if kind == .music {
                    widget_music(unfolded)
                } else if kind == .team {
                    widget_team(unfolded)
                } else if kind == .calendar {
                    widget_calendar(unfolded)
                } else {
                    widget_load(unfolded)
                }
            }
        }
    }
}

/// A widget's header: what it is and, on the card, its close and menu. The quiet form's header is
/// a press that opens the card; the card's is what a drag takes hold of.
view widget_header(
    kind: WidgetKind,
    a: Layout,
    prefs: settings.WidgetPrefs,
    unfolded: Bool,
    open: State<Option<WidgetKind>>,
    removed: State<Option<WidgetKind>>,
) {
    row(gap: .related, align: .center, fill) {
        if unfolded {
            ui.Icon(
                if kind == .music {
                    .music
                } else if kind == .team {
                    .users
                } else if kind == .calendar { .calendar } else { .activity },
                size: .small,
            )
            ui.Text(widget_name(kind), role: .label, lines: 1, truncate, fill)
            ui.MenuButton("Menu", icon: .more_horizontal, icon_only: true, role: .plain) {
                ui.MenuItem("Back after a pause", checked: prefs.show == .pause) {
                    on choose: settings.set_widget_show(
                        settings.widgets,
                        widget: widget_key(kind),
                        show: "pause",
                    )
                }
                ui.MenuItem("Always on this strip", checked: prefs.show == .always) {
                    on choose: settings.set_widget_show(
                        settings.widgets,
                        widget: widget_key(kind),
                        show: "always",
                    )
                }
                ui.MenuItem("Only when I ask", checked: prefs.show == .ask) {
                    on choose: settings.set_widget_show(
                        settings.widgets,
                        widget: widget_key(kind),
                        show: "ask",
                    )
                }
                ui.MenuSeparator()
                ui.MenuItem("Remove from the shell", icon: .minus, destructive: true) {
                    on choose: settings.set_widget_enabled(
                        settings.widgets,
                        widget: widget_key(kind),
                        enabled: false,
                    )
                        .then(set(removed, .some(kind)))
                        .then(set(open, .none))
                }
            }
            ui.Button("Fold", icon: .minimize_2, icon_only: true, role: .plain) {
                on press: set(open, .none)
            }
        } else {
            row(gap: .related, align: .center, fill) {
                ui.Icon(
                if kind == .music {
                    .music
                } else if kind == .team {
                    .users
                } else if kind == .calendar { .calendar } else { .activity },
                size: .small,
            )
                ui.Text(widget_glance(kind, a), role: .label, lines: 1, truncate, fill)
                ui.Button(
                    "Open {widget_name(kind)}",
                    icon: .chevron_up,
                    icon_only: true,
                    role: .plain,
                ) {
                    on press: set(open, .some(kind))
                }
            }
        }
    }
}

/// What the person just took off the shell, with the Undo that keeps it again.
view widget_removed(kind: WidgetKind, removed: State<Option<WidgetKind>>) {
    row(at: .bottom, fill, align: .center, distribute: .center) {
        ui.Card {
            row(gap: .related, align: .center) {
                ui.Icon(.info, tone: .info)
                ui.Text("{widget_name(kind)} removed", role: .label)
                ui.Button("Undo", icon: .undo_2, role: .plain) {
                    on press: settings.set_widget_enabled(
                        settings.widgets,
                        widget: widget_key(kind),
                        enabled: true,
                    )
                        .then(set(removed, .none))
                }
                ui.Button("Dismiss", icon: .x, icon_only: true, role: .plain) {
                    on press: set(removed, .none)
                }
            }
        }
    }
}

/// Puts the widget `kind` where a drag let go, at `at` on this display: the place the person
/// left it at, as fractions of the room it can move in there.
action place_widget(
    kind: WidgetKind,
    at: List<Int>,
    across: Float,
    down: Float,
    display: Int,
) (label: "Move a widget") {
    let x = (at.at(0) ?? 0).to_float() - widget_margin()
    let y = (at.at(1) ?? 0).to_float() - widget_margin() - widget_bar_room()
    let aside = (across - 2.0 * widget_margin()).max(1.0)
    let along = (down - widget_margin() - widget_bar_room() - widget_dock_room()).max(1.0)
    settings.place_widget(
        settings.widgets,
        widget: widget_key(kind),
        display: display.to_text(),
        x: (x / aside).clamp(0.0, 1.0),
        y: (y / along).clamp(0.0, 1.0),
    )
}

/// A widget's choices as the person's settings hold them.
fn widget_prefs(kind: WidgetKind) -> settings.WidgetPrefs {
    if kind == .music {
        settings.widgets.music
    } else if kind == .team {
        settings.widgets.team
    } else if kind == .calendar {
        settings.widgets.calendar
    } else {
        settings.widgets.system_monitor
    }
}

/// Whether `kind` shows now: the person keeps it, and it has something to say. A widget kept to
/// "only when I ask" shows while the person holds Super, as the rest of what they reach for does.
fn widget_shown(kind: WidgetKind, prefs: settings.WidgetPrefs, a: Layout) -> Bool {
    if not prefs.enabled {
        return false
    }
    if prefs.show == .ask and device.overlays.peek == .none {
        return false
    }
    if kind == .music {
        return music_now() != .none
    }
    if kind == .team {
        return not a.people.is_empty()
    }
    true
}

/// What this machine plays, as the music widget shows it: the music live now, on this device or
/// another of the person's, else nothing.
fn music_now() -> Option<device.LiveItem> {
    device.live.rows.filter(r -> r.kind == "music" or r.key == "player").last()
}

/// A widget's place on this display: the place the person left it at, else the corner or edge it
/// first rests in. Fractions of the room it can move in there.
fn widget_place(kind: WidgetKind, prefs: settings.WidgetPrefs, display: Int) -> List<Float> {
    let key = display.to_text()
    if let kept = prefs.places.find(p -> p.display == key) {
        return [kept.x, kept.y]
    }
    if kind == .music {
        [1.0, 1.0]
    } else if kind == .team {
        [1.0, 0.5]
    } else if kind == .calendar {
        [1.0, 0.25]
    } else {
        [0.0, 1.0]
    }
}

/// The gap a widget keeps from the display's edge, in logical pixels.
fn widget_margin() -> Float {
    16.0
}

/// The room the style's bar keeps at the top of the screen, in logical pixels: a widget rests
/// under it, as the Rust desktop's widgets rested under the bar, and never covers it.
fn widget_bar_room() -> Float {
    44.0
}

/// The room a dock keeps at the bottom edge of the screen, in logical pixels: a widget rests
/// above it, as the Rust desktop's widgets did, and never covers it.
fn widget_dock_room() -> Float {
    let s = shell
    if s.dock_shown and s.dock_edge == .bottom { 72.0 } else { 0.0 }
}

/// Where a widget's left edge stands, from a fraction across the room it can move in.
fn widget_left(room_px: Float, across: Float, width: Float) -> Float {
    let room = (room_px - 2.0 * widget_margin() - width).max(0.0)
    widget_margin() + room * across.clamp(0.0, 1.0)
}

/// Where a widget's top edge stands, from a fraction down the room it can move in.
fn widget_top(room_px: Float, down: Float, height: Float) -> Float {
    let top = widget_margin() + widget_bar_room()
    let room = (room_px - top - widget_margin() - widget_dock_room() - height).max(0.0)
    top + room * down.clamp(0.0, 1.0)
}

/// How wide a widget stands: its card's width as the person left it, or the quiet form's.
fn widget_width(kind: WidgetKind, prefs: settings.WidgetPrefs, unfolded: Bool) -> Float {
    if unfolded {
        prefs.width ?? widget_card_width(kind)
    } else {
        widget_glance_width(kind)
    }
}

/// How tall a widget stands, as much as a place needs to know before its view measures: its card's
/// height, else its quiet form's.
fn widget_height(kind: WidgetKind, unfolded: Bool) -> Float {
    if unfolded {
        if kind == .calendar {
            360.0
        } else if kind == .system_monitor {
            300.0
        } else {
            240.0
        }
    } else if kind == .calendar {
        72.0
    } else {
        64.0
    }
}

/// The width a widget's card rests at before the person resizes it.
fn widget_card_width(kind: WidgetKind) -> Float {
    if kind == .calendar { 340.0 } else if kind == .system_monitor { 320.0 } else { 300.0 }
}

/// The width a widget's quiet form takes.
fn widget_glance_width(kind: WidgetKind) -> Float {
    if kind == .calendar { 220.0 } else if kind == .music { 260.0 } else { 220.0 }
}

/// A widget's name: what it is, in the person's words.
fn widget_name(kind: WidgetKind) -> Text {
    if kind == .music {
        std.say("Music")
    } else if kind == .team {
        std.say("People here")
    } else if kind == .calendar {
        std.say("Calendar")
    } else {
        std.say("System load")
    }
}

/// What a widget's quiet form says beside its glyph: the one fact it rests on.
fn widget_glance(kind: WidgetKind, a: Layout) -> Text {
    if kind == .music {
        if let r = music_now() { live_name(r) } else { "" }
    } else if kind == .team {
        std.say("{a.people.len()} here")
    } else if kind == .calendar {
        "{weekday_of(device.time)} {device.time.day}"
    } else { load_words() }
}

/// What this machine is doing, in four words: its processors, then its memory.
fn load_words() -> Text {
    let machine = device.processes.machine
    let memory = if machine.memory.total > 0 {
        (100.0 * (machine.memory.total - machine.memory.available).to_float()
            / machine.memory.total.to_float()).max(0.0)
    } else {
        0.0
    }
    std.say("{machine.cpu.total.to_int()}% cpu · {memory.to_int()}% memory")
}

/// The music widget: what plays, with its own controls, the quiet form its name and play button
/// and the card its name, where it is along, and its transport.
view widget_music(unfolded: Bool) {
    if let r = music_now() {
        stack(gap: .related) {
            row(gap: .related, align: .center) {
                ui.Icon(.music, size: .small)
                stack(fill, gap: .none) {
                    ui.Text(live_name(r), role: .label, lines: 1, truncate)
                    if unfolded and r.subtitle != "" {
                        ui.Text(r.subtitle, role: .caption, secondary: true, lines: 1, truncate)
                    }
                }
                let play = r.controls.find(c -> c.id == "play-pause")
                if let c = play {
                    live_control(r, c, on_bar: not unfolded)
                }
            }
            if unfolded {
                if let along = r.progress {
                    if r.elapsed != "" {
                        row(gap: .related, align: .center) {
                            ui.Text(r.elapsed, role: .caption, secondary: true, numeric: true)
                            ui.Slider(
                                value: along,
                                label: "Position in {live_name(r)}",
                                fill,
                            ) {
                                on change(v): device.seek_live(device.live, key: r.key, fraction: v)
                            }
                            ui.Text(r.remaining, role: .caption, secondary: true, numeric: true)
                        }
                    } else {
                        ui.ProgressBar(along, label: live_name(r))
                    }
                }
                row(gap: .related, align: .center, distribute: .center) {
                    for c in r.controls.filter(c -> c.id == "previous" or c.id == "next") {
                        live_control(r, c, on_bar: false)
                    }
                }
                if r.app != "" {
                    ui.Button("Open {r.source}", icon: .external_link, role: .plain) {
                        on press: session.open_app(app: r.app)
                    }
                }
            }
        }
    }
}

/// The people-here widget: who else is in this workspace, each their face and name, and the way to
/// the roster where their cards are.
view widget_team(unfolded: Bool) {
    let people = layout.people
    let members = workspace.members.people
    stack(gap: .related) {
        row(gap: .tight, align: .center, wrap: true) {
            for p in people {
                if let m = members.find(m -> m.name() == p.name) {
                    avatar(m)
                } else {
                    ui.Avatar(name: p.name)
                }
            }
        }
        if unfolded {
            for p in people {
                row(gap: .related, align: .center) {
                    if let m = members.find(m -> m.name() == p.name) {
                        avatar(m)
                    } else {
                        ui.Avatar(name: p.name)
                    }
                    ui.Text(p.name, role: .label, lines: 1, truncate, fill)
                }
            }
            ui.Button("Show who is here", icon: .users, role: .plain) {
                on press: session.toggle_people()
            }
        }
    }
}

/// Today's date and, open, the month around it, from the person's own week.
view widget_calendar(unfolded: Bool) {
    let t = device.time
    stack(gap: .related) {
        row(gap: .related, align: .center) {
            ui.Text("{t.day}", role: .title, numeric: true)
            stack(gap: .none, fill) {
                ui.Text(weekday_of(t), role: .label)
                ui.Text(t.month_name, role: .caption, secondary: true)
            }
        }
        if unfolded {
            month_grid(t)
        }
    }
}

/// The month around today: its weeks from the person's first day, each day in its column, today
/// marked.
view month_grid(t: device.Time) {
    let lead = if t.first_weekday >= t.week_start {
        t.first_weekday - t.week_start
    } else {
        t.first_weekday - t.week_start + 7
    }
    grid(columns: 7, gap: .tight) {
        for column in [0, 1, 2, 3, 4, 5, 6] {
            ui.Text(t.weekdays.at(column) ?? "", role: .caption, secondary: true)
        }
        for week in [0, 1, 2, 3, 4, 5] {
            for column in [0, 1, 2, 3, 4, 5, 6] {
                let day = week * 7 + column - lead + 1
                if day >= 1 and day <= t.days_in_month {
                    if day == t.day {
                        ui.Badge("{day}", tone: .success)
                    } else {
                        ui.Text("{day}", role: .caption, numeric: true)
                    }
                } else {
                    spacer()
                }
            }
        }
    }
}

/// Today's weekday name, from the week the person's own week starts on.
fn weekday_of(t: device.Time) -> Text {
    let at = if t.weekday >= t.week_start {
        t.weekday - t.week_start
    } else {
        t.weekday - t.week_start + 7
    }
    t.weekday_names.at(at) ?? ""
}

/// What the machine is doing: its processors and memory now, and how they got there.
view widget_load(unfolded: Bool) {
    let machine = device.processes.machine
    stack(gap: .related) {
        row(gap: .related, align: .center) {
            ui.Text("{machine.cpu.total.to_int()}%", role: .title, numeric: true)
            ui.Text("cpu", role: .caption, secondary: true)
        }
        if unfolded {
            ui.Sparkline(
                machine.cpu.history,
                label: "Processor load",
                max: .some(100.0),
                leading_dot: .some(2.0),
            )
            let gigabyte = 1000000000.0
            let used = if machine.memory.total > 0 {
                (machine.memory.total - machine.memory.available).to_float() / gigabyte
            } else {
                0.0
            }
            let total = machine.memory.total.to_float() / gigabyte
            ui.Text(
                "{used.to_int()} of {total.to_int()} GB memory",
                role: .caption,
                secondary: true,
                numeric: true,
            )
            if let hottest = machine.thermal.first() {
                ui.Text(
                    "{hottest.name} {hottest.celsius.to_int()}°",
                    role: .caption,
                    secondary: true,
                )
            }
        }
    }
}

/// The widgets a person can keep on their shell.
enum WidgetKind {
    /// Music
    music
    /// People here
    team
    /// Calendar
    calendar
    /// System load
    system_monitor
}

/// The key the person's settings keep a widget's choices under.
fn widget_key(kind: WidgetKind) -> Text {
    if kind == .music {
        "music"
    } else if kind == .team {
        "team"
    } else if kind == .calendar { "calendar" } else { "system_monitor" }
}

/// Whether `held` names the widget `kind`.
fn is_kind(held: Option<WidgetKind>, kind: WidgetKind) -> Bool {
    if let k = held { k == kind } else { false }
}
