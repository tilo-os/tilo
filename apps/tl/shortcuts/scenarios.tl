// What the screenshots and the layout check stand on: a shortcut being written, and one saved.
scenario "a shortcut being written" -> Workbench {
    let d = workbench.drafts.insert(Draft(name: "Morning tidy"))
    d.shortcut_id = "morning-tidy"
    d.program.inputs.insert(Input(name: "folder", ty: .text, description: "Which folder to tidy"))
    let write = d.program.steps.insert(
        Step(
            step_id: "write",
            head: .invoke,
            app_id: "so.tilo.notes",
            action_kind: "notes.note",
            action_name: "write",
            action_label: "Write in a note",
            resource_label: "Note",
            target: "notes.note:1",
        ),
    )
    write.params.insert(
        Param(name: "body", ty: "text", required: true, description: "What to write"),
    )
    let body = write.args.insert(Arg(name: "body", ty: "text", pick: .literal))
    body.value.kind = .literal
    body.value.literal = "Good morning"
    let when = d.program.steps.insert(Step(step_id: "when", head: .branch))
    let condition = when.slots.insert(Slot(name: "condition", ty: "bool", pick: .choice))
    condition.value.kind = .input
    condition.value.name = "folder"
    when.condition.left.kind = .input
    when.condition.left.name = "folder"
    let ask = d.program.steps.insert(
        Step(step_id: "say", head: .ask, parent: .some(when), branch: .then),
    )
    ask.ask_description = "What should it say?"
    let quiet = d.program.steps.insert(
        Step(step_id: "quiet", head: .set, parent: .some(when), branch: .otherwise),
    )
    let value = quiet.slots.insert(Slot(name: "value", ty: "text", pick: .literal))
    value.value.kind = .literal
    value.value.literal = "nothing"
    d.program.triggers.insert(
        Trigger(kind: .schedule_day, hour: 8.0, minute: 0.0, weekdays: ["monday", "friday"]),
    )
    d.grants.insert(Grant(label: "Write in Note", kind: "changes something"))
    d.problems.insert(Problem(path: "1", message: "What to write needs a value"))
    workbench.notice = "Saved “Morning tidy”"
    workbench
}

scenario "a saved shortcut" -> Workbench {
    let s = workbench.saved.insert(Saved(id: "tidy", name: "Tidy notes"))
    s.program.inputs.insert(
        Input(name: "folder", ty: .text, description: "Which folder", value: "~/notes"),
    )
    let list = s.program.steps.insert(
        Step(step_id: "list", head: .invoke, app_id: "so.tilo.files", action_name: "list"),
    )
    list.action_label = "List a folder"
    let made = workbench.suggested.insert(Suggested(name: "Morning digest"))
    let first = made.program.steps.insert(
        Step(step_id: "list", head: .invoke, app_id: "so.tilo.files", action_name: "list"),
    )
    first.action_label = "List a folder"
    made.changes = ["Step 1 changes"]
    made.problems = ["2: it needs a value"]
    workbench
}
