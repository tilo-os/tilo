# Counter

An example of **resources and actions**.

A number that changes only through actions. `Counter` is a `#[derive(Model)]`
resource: setting the count is an edit at its path
(`Counter::paths().count.set(42)`), `add` and `reset` are its `#[action]`
verbs, and the model's `check` keeps the count in range whichever way it
changes. The buttons are bound to the verbs rather than to click handlers.
That one decision gives the app validation for every caller, undo, a
confirmation for the destructive reset, a disabled view for a viewer, and a
surface the palette, agents and scripts use exactly as the buttons do.

Read:

- `src/lib.rs`: the model and its check, its verbs, and a view that binds buttons with `actions.bind_invocation` and `cx.button`.
- `tests/app.rs`: the same edits and verbs through `Replica` (no UI) and through `TestApp` (the running app).

Start your own app from it:

```bash
tilo-app new my-app --from-example counter
```

Run and verify it in the Tilo checkout:

```bash
tilo-app run examples/apps/counter
tilo-app check examples/apps/counter
```

`tilo-app examples` lists every example, and `docs/examples.md` in the Tilo repository explains how examples differ from templates.
