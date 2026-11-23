/// What waits for the person's yes: every call that needs it, read from its own asked record in
/// `tilo.invocations`. A press whose call needs their yes, an agent's question and a device's
/// confirmation are all such records, and this is the one place a person answers any of them, on
/// every device of theirs and on the web alike. No window draws its own.
///
/// A question the person caused with a press here stands over the window they pressed in, where
/// they are looking. Every other one stands with the banners, and all of them wait in the
/// notification centre. An answer is the record's own `resolve`, as any caller would call it, so
/// it is admitted, attributed and kept in history like every other action; a question answered
/// on another device, or by someone else, leaves this view as its record stops waiting.
///
/// The questions as the host reads them for the person: the session from the records that sync
/// to this device, a server from its own. The views only read them.
type Asking (origin: session.asking) {
    questions: Collection<Asked>
    /// What each question shows before it is answered, each naming its question.
    facts: Collection<AskedFact>
    /// The choices a question answers with instead of yes or no, each naming its question.
    choices: Collection<AskedChoice>
}

/// One question: a call that waits for the person's yes.
type Asked (origin: session.asked) {
    /// The asked record's address, `<workspace>/<record>`: what the answer is called on.
    key: Text merge register
    /// The workspace the call acts in, by its name.
    workspace_name: Text merge register
    /// The action, by its label (`Set alarm`).
    name: Text merge register
    /// What the asker says, where it says something of its own (an agent's question); empty
    /// where the action's label says it.
    words: Text merge register
    /// What the call acts on, by its name; empty where it names nothing.
    on: Text merge register
    /// Whether it takes away what Undo does not bring back.
    destructive: Bool
    /// What "always" lets the asker do, as the action's verb and the plural of the resources it
    /// acts on (`set`, `alarms`); `can_always` says whether always is offered at all.
    verb: Text merge register
    plural: Text merge register
    can_always: Bool
    /// Who asks: `agent`, `app` or `device`.
    by: Text merge register
    /// The agent's or the device's name, or the app's title.
    by_name: Text merge register
    /// Who wrote the app that asks, by name; empty for none.
    author: Text merge register
    /// Since when it waits, in milliseconds since the Unix epoch.
    asked_at_ms: Int
    /// Where the press that asked it was made: the device and the window's tab, both empty
    /// for a question no press here made.
    pressed_device: Text merge register
    pressed_window: Text merge register
    /// The conversation an agent's question stands in; empty for none.
    conversation: Text merge register
}

/// A fact a question shows: a label and its value, as the asker's app words them.
type AskedFact (origin: session.asked_fact) {
    key: Text merge register
    question: Text merge register
    label: Text merge register
    value: Text merge register
}

/// One of a question's choices, by its place among them.
type AskedChoice (origin: session.asked_choice) {
    key: Text merge register
    question: Text merge register
    index: Int
    label: Text merge register
}

root asking: Asking

default view asking_list for Asking

/// The questions a press in the window `w` on this device made, newest first.
fn asked_in(w: device.Window) -> List<Asked> {
    let a = root Asking
    return a.questions
        .filter(q -> q.pressed_window == w.key and q.pressed_device == w.device)
        .sort_by(q -> 0 - q.asked_at_ms)
}

/// The questions no window here stands for: an agent's, a device's, and a press made on another
/// device or in a window that is gone. Newest first.
fn asked_elsewhere() -> List<Asked> {
    let a = root Asking
    return a.questions
        .filter(q -> not device.windows.all.any(w -> w.local and q.pressed_window == w.key and q.pressed_device == w.device))
        .sort_by(q -> 0 - q.asked_at_ms)
}

/// How many questions wait for the person: what the bell counts beside the unread notices.
fn asked_count() -> Int {
    let a = root Asking
    return a.questions.len()
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
view asking_list(a: Asking) {
    if not a.questions.is_empty() {
        ui.Section("Waiting for you") {
            for q in a.questions.sort_by(q -> 0 - q.asked_at_ms) {
                asked_card(q)
            }
        }
    }
}

/// One question: who asks, what it would do and to what, what the asker shows, and the answers
/// it offers. Its main button is what the call does, marked destructive where Undo would not
/// bring back what it takes away.
view asked_card(q: Asked) {
    let a = root Asking
    let facts = a.facts.filter(f -> f.question == q.key)
    let choices = a.choices.filter(c -> c.question == q.key).sort_by(c -> c.index)
    ui.Card {
        stack(gap: .related) {
            asker(q)
            ui.Text(if q.words != "" { q.words } else { q.name }, role: .subheading)
            if q.on != "" {
                ui.Text(q.on, secondary: true)
            }
            if not facts.is_empty() {
                ui.PropertyList {
                    for f in facts {
                        ui.Property(f.label, f.value)
                    }
                }
            }
            row(gap: .related, align: .center, wrap: true) {
                if not choices.is_empty() {
                    for c in choices {
                        ui.Button(c.label, size: .small) {
                            on press: answer(q, resolution: "approve", choice: c.index)
                        }
                    }
                    spacer
                    ui.Button("Don't answer", size: .small, role: .plain) {
                        on press: answer(q, resolution: "decline", choice: -1)
                    }
                } else {
                    if q.can_always {
                        ui.Button("Always let {q.by_name} {q.verb} {q.plural}", size: .small, role: .plain) {
                            on press: answer(q, resolution: "approve_and_grant", choice: -1)
                        }
                    }
                    spacer
                    ui.Button("Don't allow", size: .small, role: .secondary) {
                        on press: answer(q, resolution: "decline", choice: -1)
                    }
                    ui.Button(
                        q.name,
                        size: .small,
                        role: if q.destructive { .destructive } else { .primary },
                    ) {
                        on press: answer(q, resolution: "approve", choice: -1)
                    }
                }
            }
        }
    }
}

/// Who asks, as its own parts: an agent or a device by its name, an app by its title with who
/// wrote it beneath, and the workspace the call acts in.
view asker(q: Asked) {
    row(gap: .related, align: .center) {
        ui.Icon(if q.by == "agent" { .bot } else if q.by == "device" { .monitor } else { .app_window })
        stack(gap: .tight) {
            ui.Text(q.by_name, role: .caption)
            if q.author != "" {
                ui.Text(q.author, role: .caption, secondary: true)
            }
        }
        spacer
        ui.Text(q.workspace_name, role: .caption, secondary: true)
    }
}

/// Answer the question `q`: its asked record's own `resolve`, called by its address as any caller
/// calls it. `choice` is the choice's place among the question's choices, `-1` for none.
action answer(q: Asked, resolution: Text, choice: Int) (label: "Answer") {
    host.resolve(
        resource(q.key, of: host.Invocation),
        resolution: resolution,
        choice: if choice < 0 { .none } else { .some(choice) },
    )
}
