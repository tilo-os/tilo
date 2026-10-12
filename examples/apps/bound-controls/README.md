# Bound Controls

An example of **bound controls**.

Every standard control bound to a resource field by its path: `cx.input`
for text, `cx.checkbox` for a boolean, `cx.control` for anything built from a
`LocalState` (here a `NumberInput` and a `Slider`), and `cx.control_map` for a
control whose value is not the field's type (a `Dropdown` of names for an
`Atom`). A bound control shows what the resource holds at its path and edits
it there with each value the user commits, so it follows edits from
collaborators, agents and undo with no code of its own, and disables itself
for a viewer.

Read:

- `src/lib.rs`: one field per control, bound by `cx.paths()`; the model's `check` keeps every edit in range.
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
