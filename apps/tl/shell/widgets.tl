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
/// The room is what the style's own chrome leaves, as the session measured the bar and the dock
/// it laid out (`device.room`): a widget rests inside it, never under either, at whatever size
/// the person's text scale and dock settings gave them and on whichever edge the dock rides.
///
/// One widget's card is open at a time, and only by a press: nothing opens on hover or on a timer.
/// A widget shows only when it has something to say — music only while something plays, the people
/// here only while someone is — as the Rust desktop did.
view widget_layer(a: Layout) {
    // The widget whose card is open: none while every card is folded.
    state open: Option<WidgetKind> = .none
    // The widget the person just removed: said with the Undo that keeps it again.
    state removed: Option<WidgetKind> = .none
    // The room a widget can move in: what the bar and the dock leave of the display, in logical
    // pixels, which is what a drag reports and what its saved fractions are of.
    let room = widget_room()
    let display = device.screen.output
    layers(fill) {
        widget(.music, a, room, display, open, removed)
        widget(.team, a, room, display, open, removed)
        widget(.calendar, a, room, display, open, removed)
        widget(.system_monitor, a, room, display, open, removed)
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
    room: List<Float>,
    display: Int,
    open: State<Option<WidgetKind>>,
    removed: State<Option<WidgetKind>>,
) {
    let prefs = widget_prefs(kind)
    let unfolded = is_kind(open, kind)
    if widget_shown(kind, prefs, a, unfolded) {
        let width = widget_width(kind, prefs, unfolded)
        let place = widget_place(kind, prefs, display)
        ui.FloatingPanel(
            width: width,
            label: widget_name(kind),
            x: widget_left(room, place.at(0) ?? 0.5, width),
            y: widget_top(room, place.at(1) ?? 0.5, widget_height(kind, unfolded)),
        ) {
            on moved(at): place_widget(kind, at, room, display)
            // The trailing edge draws the card wider or narrower, and the width stays with the
            // person's settings for the next time the card opens.
            on resized(w): settings.resize_widget(
                settings.widgets,
                widget: widget_key(kind),
                width: w,
            )
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
                    on choose: settings
                        .set_widget_enabled(
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
                    on press: settings
                        .set_widget_enabled(
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
action place_widget(kind: WidgetKind, at: List<Int>, room: List<Float>, display: Int) (
    label: "Move a widget",
) {
    let x = (at.at(0) ?? 0).to_float() - widget_margin() - (room.at(0) ?? 0.0)
    let y = (at.at(1) ?? 0).to_float() - widget_margin() - (room.at(1) ?? 0.0)
    let aside = ((room.at(2) ?? 0.0) - 2.0 * widget_margin()).max(1.0)
    let along = ((room.at(3) ?? 0.0) - 2.0 * widget_margin()).max(1.0)
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
    } else { settings.widgets.system_monitor }
}

/// Whether `kind` shows now: the person keeps it, and it has something to say. A widget kept to
/// "only when I ask" shows while the person holds Super, as the rest of what they reach for does;
/// one kept to "back after a pause" steps aside while the person types and comes back once they
/// pause, unless they are holding Super for it. An open card stays: the person is using it. Every
/// widget stays away while another surface has the shell.
fn widget_shown(kind: WidgetKind, prefs: settings.WidgetPrefs, a: Layout, unfolded: Bool) -> Bool {
    if not prefs.enabled {
        return false
    }
    let asked = device.overlays.peek != .none
    if prefs.show == .ask and not asked {
        return false
    }
    if prefs.show == .pause and device.input.typing and not asked and not unfolded {
        return false
    }
    // Another surface has the shell — quick settings, the overview, search, the centre, a
    // sheet — and the widgets stay away under it, as the Rust desktop's did, rather than show
    // through around its edges.
    let o = device.overlays
    let resting = o.here == ""
        or o.here == "none"
        or o.here == "switcher" and not o.switcher_revealed
    if not resting and not asked {
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
    } else if kind == .calendar { [1.0, 0.25] } else { [0.0, 1.0] }
}

/// The gap a widget keeps from the display's edge, in logical pixels.
fn widget_margin() -> Float {
    16.0
}

/// The room a widget can move in on this display, as `x, y, width, height` in logical pixels:
/// what the shell's bar and dock leave, measured by the session from the chrome its style laid
/// out (`device.room`), so a widget rests clear of both at any size they are and on any edge they
/// ride. Before the first frame has measured it — a host that draws no shell chrome at all — the
/// whole display is the room.
fn widget_room() -> List<Float> {
    let r = device.room
    if r.width > 0.0 and r.height > 0.0 {
        [r.x, r.y, r.width, r.height]
    } else {
        [0.0, 0.0, device.screen.width.to_float(), device.screen.height.to_float()]
    }
}

/// Where a widget's left edge stands, from a fraction across the room it can move in.
fn widget_left(room: List<Float>, across: Float, width: Float) -> Float {
    let free = ((room.at(2) ?? 0.0) - 2.0 * widget_margin() - width).max(0.0)
    (room.at(0) ?? 0.0) + widget_margin() + free * across.clamp(0.0, 1.0)
}

/// Where a widget's top edge stands, from a fraction down the room it can move in.
fn widget_top(room: List<Float>, down: Float, height: Float) -> Float {
    let free = ((room.at(3) ?? 0.0) - 2.0 * widget_margin() - height).max(0.0)
    (room.at(1) ?? 0.0) + widget_margin() + free * down.clamp(0.0, 1.0)
}

/// How wide a widget stands: its card's width as the person left it, or the quiet form's.
fn widget_width(kind: WidgetKind, prefs: settings.WidgetPrefs, unfolded: Bool) -> Float {
    if unfolded { prefs.width ?? widget_card_width(kind) } else { widget_glance_width(kind) }
}

/// How tall a widget stands, as much as a place needs to know before its view measures: its card's
/// height, else its quiet form's.
fn widget_height(kind: WidgetKind, unfolded: Bool) -> Float {
    if unfolded {
        if kind == .calendar { 360.0 } else if kind == .system_monitor { 300.0 } else { 240.0 }
    } else if kind == .calendar { 72.0 } else { 64.0 }
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
    } else if kind == .calendar { std.say("Calendar") } else { std.say("System load") }
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
        (100.0 * (machine.memory.total - machine.memory.available).to_float() / machine.memory.total.to_float())
            .max(0.0)
    } else { 0.0 }
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
                let play = r.controls.find(c -> c.transport == "play-pause")
                if let c = play {
                    live_control(r, c, on_bar: not unfolded)
                }
            }
            if unfolded {
                if let along = r.progress {
                    if r.elapsed != "" {
                        row(gap: .related, align: .center) {
                            ui.Text(r.elapsed, role: .caption, secondary: true, numeric: true)
                            ui.Slider(value: along, label: "Position in {live_name(r)}", fill) {
                                on change(v): device.seek_live(device.live, key: r.key, fraction: v)
                            }
                            ui.Text(r.remaining, role: .caption, secondary: true, numeric: true)
                        }
                    } else {
                        ui.ProgressBar(along, label: live_name(r))
                    }
                }
                row(gap: .related, align: .center, distribute: .center) {
                    for c in r.controls.filter(
                        c -> c.transport == "previous" or c.transport == "next",
                    ) {
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
    // Who is here besides this person, from the session's own reading, each with the face the bar
    // draws of them: the count the quiet form says is the same list.
    let here = device.people.rows.filter(p -> not p.agent)
    stack(gap: .related) {
        // Folded, their faces alone; open, each once beside their name and the way to the list.
        if unfolded {
            for p in here {
                row(gap: .related, align: .center) {
                    member_face(p)
                    ui.Text(p.name, role: .label, lines: 1, truncate, fill)
                }
            }
            ui.Button("Show who is here", icon: .users, role: .plain) {
                on press: device.show(device.people)
            }
        } else {
            row(gap: .tight, align: .center, wrap: true) {
                for p in here {
                    member_face(p)
                }
            }
        }
    }
}

/// One person as the shell draws them beside their name: their member's face, ring and all, in
/// the colour their presence carries, so the same person looks the same here as on the bar and
/// in the roster.
view member_face(p: device.Person) {
    if let face = face_of(p) {
        ui.Avatar(face, colour: p.colour, ring: true)
    } else {
        ui.Avatar(name: p.name, colour: p.colour, ring: true)
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
    } else { t.first_weekday - t.week_start + 7 }
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
    } else { t.weekday - t.week_start + 7 }
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
            } else { 0.0 }
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
