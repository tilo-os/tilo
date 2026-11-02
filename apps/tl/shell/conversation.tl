/// The conversation (shell.md §8.1): the agent chat and the REPL in one view, over a run of the
/// session's agent. The transcript is the run's own, as the session publishes it: what the
/// person asked and told the agent, the agent's words between its steps, and each program
/// either of them ran as a step. A step shows the program in the code type; what it changed,
/// a row each; and what it printed, as references. Nothing is drawn from the agent's words that a
/// step does not back: the agent's words are words, and what it did is its steps.
///
/// It stands on the floating material, as the composer does, and draws with the shared
/// components and theme roles only, so every theme reaches it.
view conversation(r: Run) {
    let d = root Desk
    ui.FloatingPanel {
        header: {
            row(gap: .related, align: .center, wrap: true) {
                ui.Text(r.title, role: .subheading)
                spacer
                phase_badge(r.phase)
            }
        }
        body: {
            stack(gap: .section) {
                for e in d.run_entries.filter(x -> x.run == r.key) {
                    if e.step {
                        program_step(d, r, e)
                    } else {
                        said(r, e)
                    }
                }
                if r.live != "" {
                    said_now(r)
                }
            }
        }
    }
}

/// The run a place shows, when its record is one (`agent-run:<key>`, as the session publishes
/// a run): the canvas draws it as its conversation. `.none` for any other record.
fn placed_run(record: Text) -> Option<Run> {
    let d = root Desk
    let prefix = "agent-run:"
    if record.find(prefix) == 0 {
        let key = record.slice(prefix.len(), record.len())
        d.runs.find(r -> r.key == key)
    } else { .none }
}

/// Words in the conversation: the person's to the agent, or the agent's between its steps,
/// under who said them.
view said(r: Run, e: RunEntry) {
    stack(gap: .tight) {
        ui.Text(if e.by_agent { r.agent } else { "You" }, role: .label, secondary: true)
        ui.Text(e.text)
    }
}

/// What the agent is saying now, as the model streams it, until its turn ends.
view said_now(r: Run) {
    stack(gap: .tight) {
        ui.Text(r.agent, role: .label, secondary: true)
        ui.Text(r.live, secondary: true)
    }
}

/// One program run in the conversation, by the person or the agent: the program as code, how it
/// ended when it did not run to its end, what it answered when that is more than what it names,
/// the resources it changed and the ones it printed.
view program_step(d: Desk, r: Run, e: RunEntry) {
    let refs = d.entry_refs.filter(x -> x.entry == e.key)
    let printed = refs.filter(x -> not x.changed)
    let changed = refs.filter(x -> x.changed)
    stack(gap: .related) {
        row(gap: .related, align: .center, wrap: true) {
            ui.Text(if e.by_agent { r.agent } else { "You" }, role: .label, secondary: true)
            outcome_badge(e.outcome)
        }
        ui.Text(e.text, role: .code)
        if e.answer != "" and (e.outcome != "done" or printed.is_empty()) {
            ui.Text(e.answer, role: .code, secondary: true)
        }
        for x in changed {
            ui.ListRow(x.label, secondary: .some(x.change), icon: .pencil)
        }
        for x in printed {
            reference(r, x)
        }
    }
}

/// A resource a step printed, as a reference: a row naming it and its type. Activating it puts
/// the resource on the canvas beside the conversation (§3): one arranging action, the person's,
/// undone as one step. The step recorded the resource by its id; the place takes the resource
/// that id names, of whichever app.
view reference(r: Run, x: EntryRef) {
    let a = arrangement
    ui.ListRow(if x.label == "" { x.resource } else { x.label }, secondary: .some(type_label(x.type_kind)), icon: .file) {
        on activate: session.place(resource: resource(x.resource, of: host.Resource), x: beside_x(a, r), y: beside_y(a, r))
    }
}

/// The canvas place of the conversation over `r`, by its strip; none while it is up over the
/// desk rather than on the canvas.
fn run_strip(a: Arrangement, r: Run) -> Text {
    for p in a.places {
        if p.record == "agent-run:{r.key}" {
            return p.strip
        }
    }
    ""
}

/// Where a reference activated in the conversation over `r` stands: just right of the
/// conversation on the canvas, level with its top; where the canvas's work is when the
/// conversation is not on it.
fn beside_x(a: Arrangement, r: Run) -> Int {
    let strip = run_strip(a, r)
    for s in a.strips {
        if s.key == strip and s.placed {
            return s.canvas_x + s.canvas_width + 32
        }
    }
    here_x(a)
}

/// See [`beside_x`].
fn beside_y(a: Arrangement, r: Run) -> Int {
    let strip = run_strip(a, r)
    for s in a.strips {
        if s.key == strip and s.placed {
            return s.canvas_y
        }
    }
    here_y(a)
}

/// A type as a person reads it: its name without the app's prefix, `notes.Note` as `Note`.
fn type_label(kind: Text) -> Text {
    let dot = kind.find(".")
    if dot >= 0 {
        type_label(kind.slice(dot + 1, kind.len()))
    } else { kind }
}

/// Where a run is, as a status badge.
view phase_badge(phase: Text) {
    if phase == "approved" or phase == "accepted" {
        ui.Badge("Starting", tone: .neutral)
    } else if phase == "planning" or phase == "executing" {
        ui.Badge("Working", tone: .info)
    } else if phase == "waiting" {
        ui.Badge("Waiting for you", tone: .warning)
    } else if phase == "done" {
        ui.Badge("Done", tone: .success)
    } else if phase == "failed" {
        ui.Badge("Could not finish", tone: .danger)
    } else {
        ui.Badge("Stopped", tone: .neutral)
    }
}

/// How a step that did not run to its end ended, as a status badge; nothing for one that did.
view outcome_badge(outcome: Text) {
    if outcome == "rejected" {
        ui.Badge("Did not compile", tone: .danger)
    } else if outcome == "preview" {
        ui.Badge("Waiting to confirm", tone: .warning)
    } else if outcome == "asked" {
        ui.Badge("Waiting for an answer", tone: .warning)
    } else if outcome != "done" {
        ui.Badge("Stopped", tone: .danger)
    }
}
