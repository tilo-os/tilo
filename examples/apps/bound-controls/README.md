# Bound Controls

An example of **bound controls**.

Every standard control bound to a resource field: `cx.input` for text,
`cx.checkbox` for a boolean, and `cx.control` for anything built from a
`LocalState` (here a `NumberInput`, a `Slider` and a `Dropdown`). A bound
control shows what the resource holds and invokes an action with each value
the user commits, so it follows edits from collaborators, agents and undo
with no code of its own, and disables itself when the action is forbidden.

Read:

- `src/lib.rs`: one field and one action per control; the validation lives in the actions.
- `tests/app.rs`: each control edits its field, controls follow edits made elsewhere, a viewer cannot edit.

Start your own app from it:

```bash
tilo-app new my-app --from-example bound-controls
```

Run and verify it in the Tilo checkout:

```bash
tilo-app run examples/apps/bound-controls
tilo-app check examples/apps/bound-controls
```

`tilo-app examples` lists every example, and `docs/examples.md` in the Tilo repository explains how examples differ from templates.
