# Counter

An example of **resources and actions**.

A number that changes only through actions. `Counter` is a `#[derive(Model)]`
resource: setting the count is an edit at its path
(`Counter::paths().count.set(42)`), `add` and `reset` are its `#[action]`
verbs, and the model's `check` keeps the count in range whichever way it
changes. The buttons are bound to the verbs rather than to click handlers.
That one decision gives the app validation for every caller, undo (so the
destructive reset runs at once and offers Undo), a disabled view for a viewer, and a
surface the palette, agents and scripts use exactly as the buttons do.

Read:

- `src/lib.rs`: the model and its check, its verbs, and a view that binds buttons with `actions.bind_invocation` and `cx.button`.
- `tests/app.rs`: the same edits and verbs through `Replica` (no UI) and through `HeadlessApp` (the running app).

Start your own app from it:

```bash
tilolang new my-app --from-example counter
```

Run and verify it in the Tilo checkout:

```bash
tilolang run examples/apps/counter
tilolang check examples/apps/counter
```

`tilolang examples` lists every example.
