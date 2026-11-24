use tilo.invocations as invocations

/// What waits for the person's yes: every call that needs it, read from its own record in the
/// workspace's invocations. A press whose call needs their yes, an agent's question and a
/// device's confirmation (an administrator's password, a network's secret, a Bluetooth pairing,
/// a portal's or a file chooser's question) are all such records, and this is the one place a
/// person answers any of them, on every device of theirs and on the web alike. No window draws
/// its own.
///
/// A question the person caused with a press here stands over the window they pressed in, where
/// they are looking. Every other one stands with the banners, and all of them wait in the
/// notification centre. An answer is the record's own `resolve`, as any caller would call it, so
/// it is admitted, attributed and kept in history like every other action; a question answered
/// on another device, or by someone else, leaves this view as its record stops waiting.

/// The folder a question that asks for files shows, as this device reads it: the shell's own
/// look at the disk while the person chooses, never what they chose.
type Picking {
    /// The folder shown, by its path; empty until a question asks for files.
    folder: Text = ""
    /// What the folder holds, as it was last read.
    entries: List<host.FileEntry>
}

root picking: Picking

/// Every call that waits for the person looking, newest first.
fn waiting() -> List<invocations.Invocation> {
    return query(invocations.Invocation, status: .waiting, person: me)
        .filter(q -> q.asked.is_some())
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

/// Whether the press that asked `q` was made in the window `w`.
fn pressed_in(q: invocations.Invocation, w: device.Window) -> Bool {
    if let p = q.asked?.pressed {
        return p.instance == w.key and p.device == w.device
    }
    return false
}

/// The questions no window here stands for: an agent's, a device's, and a press made on another
/// device or in a window that is gone. Newest first.
fn asked_elsewhere() -> List<invocations.Invocation> {
    return waiting().filter(q -> not device.windows.all.any(w -> w.local and pressed_in(q, w)))
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
    if let at = q.asked?.record.device {
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
    return q.id ?? ""
}

/// The questions a press in the window `w` made, over its lower edge where the person pressed.
/// The newest stands first; more than two wait in the notification centre.
view asked_over(w: device.Window) {
    let asked = asked_in(w)
    if not asked.is_empty() {
        stack(at: .bottom, fill, align: .center, gap: .related, padding: .group) {
            for q in asked.take(2) {
                asked_card(q)
            }
            if asked.len() > 2 {
                more_asked(asked.len() - 2)
            }
        }
    }
}

/// The questions that stand with the banners: the newest two no window here stands for.
view asked_banners() {
    let asked = asked_elsewhere()
    if not asked.is_empty() {
        stack(gap: .related) {
            for q in asked.take(2) {
                asked_card(q)
            }
            if asked.len() > 2 {
                more_asked(asked.len() - 2)
            }
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
    let lines = a.facts.filter(f -> f.label == "")
    let labelled = a.facts.filter(f -> f.label != "")
    // What the person types and picks until they answer: the card's own, never kept.
    state typed: List<Text> = a.takes.map(t -> t.initial ?? "")
    state picked: List<Int> = []
    // A phrase to type, or a field the program needs filled: the answer waits until it
    // holds something.
    let unfilled = a.takes.any(t -> not t.secret and not t.chord and (t.initial ?? "") == "" and (typed.at(a.takes.index_of(t) ?? 0) ?? "") == "")
    let can_always = a.offers.contains(.approve_and_grant) and a.verb != "" and a.plural.is_some() and not a.destructive
    ui.Card {
        stack(gap: .related) {
            asker(a)
            ui.Text(if a.words != "" { a.words } else { a.name }, role: .subheading)
            if let on = a.on {
                ui.Text(on, secondary: true)
            }
            for f in lines {
                ui.Text(f.value, secondary: true)
            }
            if not labelled.is_empty() {
                ui.PropertyList {
                    for f in labelled {
                        ui.Property(f.label, f.value)
                    }
                }
            }
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
                                } else {
                                    [picked, [i]].flat_map(p -> p)
                                },
                            )
                        }
                    }
                }
                if let files = a.files {
                    files_asked(key, files, typed)
                }
            }
            if a.files.is_none() or not here {
                row(gap: .related, align: .center, wrap: true) {
                    if here and not a.choices.is_empty() and not a.many {
                        for c in a.choices {
                            ui.Button(c, size: .small, enabled: not unfilled) {
                                on press: host.resolve(
                                    resource(key, of: host.Invocation),
                                    resolution: "approve",
                                    choice: a.choices.index_of(c),
                                    typed: .some(typed),
                                )
                            }
                        }
                        spacer
                        ui.Button("Don't answer", size: .small, role: .plain) {
                            on press: decline(key)
                        }
                    } else {
                        if here and can_always {
                            ui.Button("Always let {asker_name(a)} {a.verb} {a.plural ?? ""}", size: .small, role: .plain) {
                                on press: host.resolve(resource(key, of: host.Invocation), resolution: "approve_and_grant")
                            }
                        }
                        spacer
                        ui.Button("Don't allow", size: .small, role: .secondary) {
                            on press: decline(key)
                        }
                        if here {
                            ui.Button(
                                a.name,
                                size: .small,
                                enabled: not unfilled,
                                role: if a.destructive { .destructive } else { .primary },
                            ) {
                                on press: host.resolve(
                                    resource(key, of: host.Invocation),
                                    resolution: "approve",
                                    chosen: .some(picked),
                                    typed: .some(typed),
                                )
                            }
                        }
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
            pick: chosen -> host.resolve(
                resource(key, of: host.Invocation),
                resolution: "approve",
                typed: .some(typed),
                files: .some(chosen),
            ).then(close_folder(p)),
            cancel: decline(key).then(close_folder(p)),
            mode: if files.mode == .save { "save" } else if files.mode == .folder { "folder" } else { "open" },
            multiple: files.many,
            types: files.types.map(t -> t.patterns),
            name: files.name ?? "",
            home: "~",
        )
    }
}

/// Who asks, by name: an agent's, a device's or a program's, or an app's title.
fn asker_name(a: invocations.Asked) -> Text {
    if let agent = a.by.agent {
        return agent.name
    }
    if let program = a.by.app {
        return program.title
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
        ui.Icon(if a.by.agent.is_some() { .bot } else if a.by.device.is_some() { .monitor } else if a.by.program.is_some() { .terminal } else { .app_window })
        stack(gap: .tight) {
            ui.Text(asker_name(a), role: .caption)
            if let author = a.by.app?.author {
                ui.Text(author.name, role: .caption, secondary: true)
            }
        }
        spacer
        ui.Text(a.workspace_name, role: .caption, secondary: true)
    }
}

/// Decline the question at `key`: its asked record's own `resolve`, called by its address as any
/// caller calls it.
action decline(key: Text) (label: "Don't allow") {
    host.resolve(resource(key, of: host.Invocation), resolution: "decline")
}

/// Show the folder `path` to choose files in, as this device reads it now.
action open_folder(p: Picking, path: Text) uses files {
    match File.list(path) {
        .ok(found) => {
            p.folder = path
            p.entries = found
        }
        .err(_) => {}
    }
}

/// Stop showing a folder: the question that asked for files is answered.
action close_folder(p: Picking) {
    p.folder = ""
    p.entries = []
}
