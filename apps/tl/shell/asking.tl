use tilo.invocations as invocations

/// What waits for the person's yes: every call that needs it, read from its own record in the
/// workspace's invocations. A press whose call needs their yes, an agent's question and a
/// device's confirmation (an administrator's password, a network's secret, a Bluetooth pairing,
/// a portal's or a file chooser's question) are all such records, and this is the one place a
/// person answers any of them, on every device of theirs and on the web alike. No window draws
/// its own.
///
/// A question the person caused with a press here stands beside the control they pressed, where
/// they are looking, never over it. Every other one stands with the banners, and all of them wait
/// in the notification centre. An answer is the record's own `resolve`, as any caller would call
/// it, so it is admitted, attributed and kept in history like every other action; a question
/// answered on another device, or by someone else, leaves this view as its record stops waiting.
///
/// The folder a question that asks for files shows, as this device reads it: the shell's own
/// look at the disk while the person chooses, never what they chose.
type Picking {
    /// The folder shown, by its path; empty until a question asks for files.
    folder: Text = ""
    /// What the folder holds, as it was last read.
    entries: List<host.FileEntry>
    /// The places the picker offers beside the folder: removable media, where a drive plugged
    /// in now is mounted, so a person can choose a folder on it.
    places: List<host.FileEntry>
}

root picking: Picking

default view picking_folder for picking

/// The folder a question that asks for files shows, as opening the picking state shows it: what
/// it holds, folders first. Answering happens in the question's own card (`files_asked`).
view picking_folder(p: Picking) {
    if p.folder == "" {
        ui.EmptyState(
            "No folder open",
            message: "A question that asks for files opens one here",
            icon: .folder,
            inline: true,
        )
    } else {
        ui.Section(p.folder) {
            for e in p.entries.sort_by(e -> if e.folder { 0 } else { 1 }) {
                ui.ListRow(e.name, icon: if e.folder { .folder } else { .file })
            }
        }
    }
}

/// Every call that waits for the person looking, newest first.
fn waiting() -> List<invocations.Invocation> {
    return query(invocations.Invocation, status: .waiting, person: me)
        .filter(q -> q.asked != .none)
        .sort_by(q -> 0 - asked_at(q))
}

/// Since when `q` waits, in milliseconds since the Unix epoch.
fn asked_at(q: invocations.Invocation) -> Int {
    return q.asked?.asked_at_ms ?? q.started_ms
}

/// The questions a press in the window `w` on this device made, newest first.
fn asked_in(w: device.Window) -> List<invocations.Invocation> {
    return waiting().filter(q -> pressed_in(q, w))
}

/// Whether the press that asked `q` was made in the window `w`: a window shown from another
/// device by that device's id, a window of this device by this device's.
fn pressed_in(q: invocations.Invocation, w: device.Window) -> Bool {
    if let p = q.asked?.pressed {
        if p.instance != w.key {
            return false
        }
        return p.device == w.device
            or not w.elsewhere and w.device == "" and p.device == this_device()
    }
    return false
}

/// The questions no window here stands for: an agent's, a device's, and a press made on another
/// device or in a window that is gone. A press on the shell's own surfaces is not one of them: it
/// stands beside the control it pressed (`asked_over_shell`). Newest first.
fn asked_elsewhere() -> List<invocations.Invocation> {
    return waiting()
        .filter(q -> not on_screen(q))
        .filter(q -> not device.windows.all.any(w -> not w.elsewhere and pressed_in(q, w)))
}

/// Whether `q` was asked by a press on the shell's own surfaces: the shell's screen, which no
/// window stands for.
fn on_screen(q: invocations.Invocation) -> Bool {
    return q.asked?.pressed?.instance == "screen"
}

/// How many questions wait for the person: what the bell counts beside the unread notices.
fn asked_count() -> Int {
    return waiting().len()
}

/// This device, by the key its records name it by; empty until the session says.
fn this_device() -> Text {
    return device.devices.devices.filter(d -> d.local).at(0)?.id ?? ""
}

/// Whether `q` is answered here: a question whose call runs on another device (what it asks
/// for there is typed or chosen there) is answered on that device, or declined anywhere.
fn answered_here(q: invocations.Invocation) -> Bool {
    if let at = q.asked?.record?.device {
        let here = this_device()
        return here == "" or at == here
    }
    return true
}

/// The address `q`'s answer is called on: its asked record, `<workspace>/<record>`.
fn key_of(q: invocations.Invocation) -> Text {
    if let a = q.asked {
        return "{a.record.workspace}/{a.record.record}"
    }
    return q.id
}

/// The questions a press in the window `w` made, standing beside the control the person pressed:
/// the newest one's popover is anchored to that control's rect, so it never covers what they
/// pressed. It takes Escape, or a press outside it, as its way out, declining it, as the system's
/// questions always have; the older ones wait behind it. More than two wait in the notification
/// centre. A press whose record says where no control was (a palette's row, a key with no control
/// of its own) stands over the window.
view asked_over(w: device.Window) {
    let asked = asked_in(w)
    if not asked.is_empty() {
        if let q = asked.at(0) {
            if let a = q.asked {
                if let control = a.pressed?.control {
                    ui.Popover(
                        label: title_of(a),
                        open: true,
                        anchor: [
                            control.x.to_float(),
                            control.y.to_float(),
                            control.w.to_float(),
                            control.h.to_float(),
                        ],
                    ) {
                        on dismiss: host.resolve(
                            resource(key_of(q), of: host.Invocation),
                            resolution: "decline",
                        )
                        asked_at_press(q, a, title_of(a), asked.len() - 1)
                    }
                } else if let at = a.pressed?.at {
                    // A press the window gave no control for: its point is all it said, and the
                    // popover's answer stands under it.
                    ui.Popover(
                        label: title_of(a),
                        open: true,
                        at: [at.x.to_float(), at.y.to_float()],
                    ) {
                        on dismiss: host.resolve(
                            resource(key_of(q), of: host.Invocation),
                            resolution: "decline",
                        )
                        asked_at_press(q, a, title_of(a), asked.len() - 1)
                    }
                } else {
                    ui.ModalDialog(
                        title: title_of(a),
                        open: true,
                        destructive: a.destructive,
                        icon: .triangle_alert,
                    ) {
                        on dismiss: host.resolve(
                            resource(key_of(q), of: host.Invocation),
                            resolution: "decline",
                        )
                        asked_at_press(q, a, "", asked.len() - 1)
                    }
                }
            }
        }
    }
}

/// The newest question a press on the shell's own surfaces made, standing beside the control the
/// person pressed, over the shell and every surface on it: the same popover a press in a window
/// raises, anchored to the control in the shell's own room. It takes Escape, or a press outside
/// it, as its way out; the older ones wait in the notification centre.
view asked_over_shell() {
    let asked = waiting().filter(q -> on_screen(q)).sort_by(q -> 0 - asked_at(q))
    if let q = asked.at(0) {
        if let a = q.asked {
            if let control = a.pressed?.control {
                ui.Popover(
                    label: title_of(a),
                    open: true,
                    anchor: [
                        control.x.to_float(),
                        control.y.to_float(),
                        control.w.to_float(),
                        control.h.to_float(),
                    ],
                ) {
                    on dismiss: host.resolve(
                        resource(key_of(q), of: host.Invocation),
                        resolution: "decline",
                    )
                    asked_at_press(q, a, title_of(a), asked.len() - 1)
                }
            }
        }
    }
}

/// What a question that stands at the press says under `headline`: what it would do and the
/// answers it offers, the hint Shift would skip it by where Shift would, and the `more` other
/// questions that wait behind it.
view asked_at_press(q: invocations.Invocation, a: invocations.Asked, headline: Text, more: Int) {
    question_body(key_of(q), answered_here(q), a, headline)
    if skips_with_shift(a) {
        ui.Text("Hold Shift to skip", role: .caption, secondary: true)
    }
    if more > 0 {
        more_asked(more)
    }
}

/// What a question asks, as its title: the asker's own words, else the action's name.
fn title_of(a: invocations.Asked) -> Text {
    return if a.words != "" { a.words } else { a.name }
}

/// Whether Shift would skip `a`: what does not leave the system, which a press answers itself.
/// What leaves is never skipped, and a record that does not say its effect promises no skip
/// either.
fn skips_with_shift(a: invocations.Asked) -> Bool {
    if let effect = a.effect {
        return effect != .outside
    }
    return false
}

/// The questions that stand with the banners: the newest two no window here stands for, and one
/// where the display is a phone's, too short to hold two across it. They are banners, so a
/// question is the floating card a notice rises as in the corner (`roomy`, as `banners_over`
/// reads it): a card left to size itself there came out as wide as its own padding, and every
/// line in it wrapped to nothing.
view asked_banners(roomy: Bool) {
    let asked = asked_elsewhere()
    if not asked.is_empty() {
        let shown = if roomy { 2 } else { 1 }
        let more = asked.len() - shown
        let last = asked.take(shown).last()
        stack(gap: .related) {
            for q in asked.take(shown) {
                // The count of the questions that do not fit is the last card's own foot: the
                // banner and its way to the rest are one surface, never a line standing under it.
                let is_last = if let l = last { key_of(l) == key_of(q) } else { false }
                asked_banner(q, roomy: roomy, more: if is_last { more } else { 0 })
            }
        }
    }
}

/// One question standing with the banners: a banner's width in the corner, or across a display
/// too narrow to keep it there, and rising from the edge it stands by as a notice does.
view asked_banner(q: invocations.Invocation, roomy: Bool, more: Int) {
    if let a = q.asked {
        ui.FloatingPanel(width: if roomy { 360.0 }, fill: not roomy, slide: true) {
            footer: {
                if more > 0 {
                    more_asked(more)
                }
            }
            question_body(
                key_of(q),
                answered_here(q),
                a,
                if a.words != "" { a.words } else { a.name },
            )
        }
    }
}

/// The way to the questions that do not fit: the notification centre, where every one waits.
view more_asked(more: Int) {
    ui.Button(
        if more == 1 { "1 more question" } else { "{more} more questions" },
        icon: .bell,
        size: .small,
        role: .plain,
    ) {
        on press: device.show_surface(device.overlays, surface: "centre")
    }
}

/// Every question that waits, newest first, at the head of the notification centre.
view asking_list() {
    let asked = waiting()
    if not asked.is_empty() {
        ui.Section("Waiting for you") {
            for q in asked {
                asked_card(q)
            }
        }
    }
}

/// One question: who asks, what it would do and to what, what the asker shows, what the person
/// types, picks or chooses, and the answers it offers. Its main button is what the call does,
/// marked destructive where Undo would not bring back what it takes away, and then it says so
/// and offers no "always".
view asked_card(q: invocations.Invocation) {
    if let a = q.asked {
        question(key_of(q), answered_here(q), a)
    }
}

/// The question `a`, answered at `key`, and here or on the device that asked it.
view question(key: Text, here: Bool, a: invocations.Asked) {
    ui.Card {
        // The card says the action's name on the answer that runs it: its headline carries only
        // what the asker says of its own, so the name is no word beside a control that a press
        // by that name would land on instead.
        question_body(key, here, a, a.words)
    }
}

/// What the question says: who asks, what it would do and to what, what the asker shows, what
/// the person types, picks or chooses, and the answers it offers. `headline` is its own line at
/// the top, empty where the surface drawing it names the action itself.
///
/// Where an answer follows the person's own press, the keyboard starts on that answer, so Enter
/// finishes what they started. Where an agent, a program or a device asked, it starts on the way
/// out instead: Enter never approves a call nobody here began, and "always let" never takes it.
view question_body(key: Text, here: Bool, a: invocations.Asked, headline: Text) {
    let lines = a.facts.filter(f -> f.label == "")
    let labelled = a.facts.filter(f -> f.label != "")
    // What the person types and picks until they answer: the card's own, never kept.
    state typed: List<Text> = a.takes.map(t -> t.initial ?? "")
    state picked: List<Int> = []
    // What the person writes in each field of the call they may change, by its place: the call
    // as asked until they change it, and the form shut until they open it.
    state written: List<Text> = a.form.map(f -> asked_value(f))
    state changing: Bool = false
    // A phrase to type, or a field the program needs filled: the answer waits until it
    // holds something.
    let unfilled = a.takes.any(
        t -> not t.secret
            and not t.chord
            and (t.initial ?? "") == ""
            and (typed.at(a.takes.index_of(t) ?? 0) ?? "") == "",
    )
    let can_always = a.offers.contains(.approve_and_grant)
        and a.verb != ""
        and a.plural != .none
        and not a.destructive
    stack(gap: .related) {
        asker(a)
        if headline != "" {
            ui.Text(headline, role: .subheading)
        }
        if let on = a.on {
            ui.Text(on, secondary: true)
        }
        for f in lines {
            ui.Text(f.value, secondary: true)
        }
        if changing and here {
            // The call's arguments the person may change, each in its field: a flag turns,
            // every other field is written in.
            for f in a.form {
                let i = a.form.index_of(f) ?? 0
                if let _ = f.value.on {
                    ui.Toggle(f.label, shown: (written.at(i) ?? asked_value(f)) == "yes") {
                        on check(on: Bool): set(
                            written,
                            written_at(a.form, written, i, flag_text(on)),
                        )
                    }
                } else {
                    field(f.label) {
                        ui.TextInput(
                            bind: written.at(i),
                            placeholder: f.shows ?? asked_value(f),
                            lines: if f.multiline { 4 } else { 1 },
                        )
                    }
                }
            }
        } else if not labelled.is_empty() {
            ui.PropertyList {
                for f in labelled {
                    ui.Property(f.label, f.value)
                }
            }
        }
        // What else the call writes, beyond the record it names: each app and what the step
        // does there, read from the action's declaration into the asked record.
        std.action_effects(a.effects, writer: e -> e.app, what: e -> e.what)
        if a.destructive {
            ui.Text("Undo can't bring this back", secondary: true)
        }
        if let why = a.retry {
            ui.Banner(why, kind: .warning)
        }
        if not here {
            ui.Text("This is answered on the device that asked it", secondary: true)
        } else {
            for t in a.takes {
                field(t.label) {
                    ui.TextInput(
                        bind: typed.at(a.takes.index_of(t) ?? 0),
                        placeholder: t.label,
                        secret: t.secret and not t.shown,
                    )
                }
            }
            if a.many {
                for c in a.choices {
                    let i = a.choices.index_of(c) ?? 0
                    ui.Button(c, size: .small, toggled: picked.contains(i)) {
                        on press: set(
                            picked,
                            if picked.contains(i) {
                                picked.filter(p -> p != i)
                            } else { [picked, [i]].flat_map(p -> p) },
                        )
                    }
                }
            }
            if let files = a.files {
                files_asked(key, files, typed)
            }
        }
        if a.files == .none or not here {
            row(gap: .related, align: .center, wrap: true) {
                if here and not a.choices.is_empty() and not a.many {
                    for c in a.choices {
                        ui.Button(
                            c,
                            size: .small,
                            role: if a.destructive { .destructive } else { .secondary },
                            enabled: not unfilled,
                            description: "Fill in what it asks for first",
                        ) {
                            on press: host.resolve(
                                resource(key, of: host.Invocation),
                                resolution: "approve",
                                choice: a.choices.index_of(c),
                                typed: .some(typed),
                            )
                        }
                    }
                    spacer
                    ui.Button(
                        a.decline ?? "Don't answer",
                        size: .small,
                        role: .plain,
                        default_focus: a.pressed == .none or not skips_with_shift(a),
                    ) {
                        on press: host.resolve(
                            resource(key, of: host.Invocation),
                            resolution: "decline",
                        )
                    }
                } else {
                    spacer
                    if here and not a.form.is_empty() and not changing {
                        ui.Button("Change", icon: .pencil, size: .small, role: .plain) {
                            on press: set(changing, true)
                        }
                    }
                    // Where the answer itself is given on the device that asked, the way out is
                    // the only answer this surface offers. The keyboard starts on the way out
                    // unless Shift would skip the question: Enter never answers an irreversible
                    // call nobody here could take back.
                    ui.Button(
                        a.decline ?? "Don't allow",
                        size: .small,
                        role: .secondary,
                        default_focus: a.pressed == .none or not here or not skips_with_shift(a),
                    ) {
                        on press: host.resolve(
                            resource(key, of: host.Invocation),
                            resolution: "decline",
                        )
                    }
                    if here {
                        ui.Button(
                            a.name,
                            size: .small,
                            enabled: not unfilled,
                            description: "Fill in what it asks for first",
                            role: if a.destructive { .destructive } else { .primary },
                            default_focus: a.pressed != .none and skips_with_shift(a),
                        ) {
                            on press: host.resolve(
                                resource(key, of: host.Invocation),
                                resolution: "approve",
                                chosen: .some(picked),
                                typed: .some(typed),
                                edits: changes_of(a.form, written),
                            )
                        }
                    }
                }
            }
            // Answering for good is its own line under the answers: the card is as wide as what
            // the call does and its answers, never padded out to this longer sentence.
            if here and can_always and (a.choices.is_empty() or a.many) {
                row {
                    ui.Button(
                        "Always let {asker_name(a)} {a.verb} {a.plural ?? ""}",
                        size: .small,
                        role: .plain,
                    ) {
                        on press: host.resolve(
                            resource(key, of: host.Invocation),
                            resolution: "approve_and_grant",
                        )
                    }
                }
            }
        }
    }
}

/// The files a question asks for, chosen in the system's file picker among the folders this
/// device holds; choosing answers the question at `key`, and closing the picker declines it.
view files_asked(key: Text, files: invocations.FilesAsked, typed: List<Text>) {
    let p = root Picking
    let start = files.folder ?? "~"
    if p.folder == "" {
        ui.Button("Choose", size: .small, role: .primary) {
            on press: open_folder(p, start)
        }
    } else {
        std.file_picker(
            folder: p.folder,
            entries: p.entries,
            go: path -> open_folder(p, path),
            pick: chosen -> host
                .resolve(
                    resource(key, of: host.Invocation),
                    resolution: "approve",
                    typed: .some(typed),
                    files: .some(chosen),
                )
                .then(close_folder(p)),
            cancel: host
                .resolve(resource(key, of: host.Invocation), resolution: "decline")
                .then(close_folder(p)),
            mode: if files.mode == .save {
                "save"
            } else if files.mode == .folder { "folder" } else { "open" },
            multiple: files.many,
            types: files.types.map(t -> t.patterns),
            name: files.name ?? "",
            places: p.places,
            roots: p.places.map(place -> place.path),
            home: "~",
        )
    }
}

/// A field's value as the call was asked with it, as the person reads and writes it: a flag
/// `yes` or `no`.
fn asked_value(f: invocations.FormField) -> Text {
    if let on = f.value.on {
        return flag_text(on)
    }
    return f.value.text ?? ""
}

/// A flag as the person reads and writes it.
fn flag_text(on: Bool) -> Text {
    return if on { "yes" } else { "no" }
}

/// `written` with the place `at` holding `value`, every other place what it held, or its field's
/// value as asked where nothing was written there yet.
fn written_at(
    form: List<invocations.FormField>,
    written: List<Text>,
    at: Int,
    value: Text,
) -> List<Text> {
    return form.map(
        f -> if (form.index_of(f) ?? 0) == at {
            value
        } else {
            written.at(form.index_of(f) ?? 0) ?? asked_value(f)
        },
    )
}

/// One change the person made to the call before approving it: a field, by its path in the
/// call's arguments, and what they wrote in it. The record's `resolve` takes a list of them.
type CallEdit {
    path: List<Text>
    text: Text
}

/// What the person changed in `form`, as the record's `resolve` takes it: each field whose
/// written value is not the call's own, none where they changed nothing.
fn changes_of(form: List<invocations.FormField>, written: List<Text>) -> Option<json.Value> {
    let edits = form
        .filter(
            f -> (written.at(form.index_of(f) ?? 0) ?? asked_value(f)) != asked_value(f),
        )
        .map(
            f -> CallEdit(path: f.path, text: written.at(form.index_of(f) ?? 0) ?? asked_value(f)),
        )
    if edits.is_empty() {
        return .none
    }
    return match json.parse(json.encode(edits)) {
        .ok(changes) => .some(changes),
        .err(_) => .none,
    }
}

/// Who asks, by name: an agent's, a device's or a program's, or an app's title.
fn asker_name(a: invocations.Asked) -> Text {
    if let agent = a.by.agent {
        return agent.name
    }
    if let asking_app = a.by.app {
        return asking_app.title
    }
    if let d = a.by.device {
        return d.name
    }
    if let program = a.by.program {
        return program.name
    }
    return ""
}

/// Who asks, as its own parts: an agent, a device or a program by its name, an app by its title
/// with who wrote it beneath, and the workspace the call acts in.
view asker(a: invocations.Asked) {
    row(gap: .related, align: .center) {
        if let picture = a.picture {
            // What the question is about shows itself: a program's own artwork.
            ui.Avatar(name: asker_name(a), picture: picture, square: true)
        } else {
            ui.Icon(
                if a.by.agent != .none {
                    .bot
                } else if a.by.device != .none {
                    .monitor
                } else if a.by.program != .none { .terminal } else { .app_window },
            )
        }
        // A long name says less rather than pushing the workspace out of the card: each line
        // takes the room it is given and ends in an ellipsis.
        stack(gap: .tight, min: 6rem) {
            ui.Text(asker_name(a), role: .caption, lines: 1, truncate)
            if let author = a.by.app?.author {
                ui.Text(author.name, role: .caption, secondary: true, lines: 1, truncate)
            }
        }
        spacer
        ui.Text(a.workspace_name, role: .caption, secondary: true, lines: 1, truncate)
    }
}

/// Decline the question at `key`: its asked record's own `resolve`, called by its address as any
/// caller calls it.
action decline(key: Text) (label: "Don't allow") {
    host.resolve(resource(key, of: host.Invocation), resolution: "decline")
}

/// Show the folder `path` to choose files in, as this device reads it now, with removable
/// media as a place the picker offers.
action open_folder(p: Picking, path: Text) uses files {
    match File.list(path) {
        .ok(found) => {
            p.folder = path
            p.entries = found
        }
        .err(_) => {}
    }
    match File.stat("/media") {
        .ok(held) => {
            p.places = if let mount = held { [mount] } else { [] }
        }
        .err(_) => {
            p.places = []
        }
    }
}

/// Stop showing a folder: the question that asked for files is answered.
action close_folder(p: Picking) {
    p.folder = ""
    p.entries = []
}
