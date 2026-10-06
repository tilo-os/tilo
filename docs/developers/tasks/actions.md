# Add one capability once

Declare edits in the existing `#[resource]` implementation in `src/lib.rs`.
The starter's clear operation is an example:

```rust
#[action]
pub fn clear(&mut self) {
    self.name.clear();
}
```

Bind the generated descriptor in the view:

```rust
let clear = cx.button("starter-clear", Greeting::CLEAR.invoke());
```

The label is a Fluent key. A text field uses the same door:
`cx.input(|state| state.name.clone(), Greeting::SET_NAME)`.
Read visual values with `cx.text` or another bound shared control so peer
edits and undo update the view too. Keep render caches out of the model.

That resource action is the public capability for buttons, keyboard input,
accessibility, scripts and agents. Resource edits participate in the system's
history, undo and CRDT synchronization; an app does not add its own action bus
or sync implementation. Use typed effects for external work rather than
pretending a network request or file write is an undoable resource edit.

Effect ids share the root resource's action surface. `InspectPackage` becomes
`inspect-package`; keep names distinct across effect enums and root actions.
The effect's `operation` names its vocabulary, not an id prefix.

Test the action through the generated `TestApp` harness. Enumerate the
permission-filtered action descriptors and invoke their stable identities;
display labels and pointer coordinates do not identify an operation. Confirmed
effects and role restrictions use the same host door as the controls.

```bash
tilo-app test --json
tilo-app inspect --audit --json
```

More: [A resource workflow](../../sdk-resource-journey.md),
[Scripts and agents](../../agent-action-api.md).
