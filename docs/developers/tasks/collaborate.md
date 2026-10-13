# Keep collaboration, undo and history on the resource path

```bash
tilo-app new shared-list --from-example shared-checklist
cd shared-list
tilo-app test
```

The app starts empty and uses shared controls, theme values and Fluent labels.
Read `src/lib.rs` for the model and `tests/app.rs` for executable examples.
Without creating a project, retrieve just either file with
`tilo-app examples shared-checklist --file tests/app.rs --json` (or
`--file src/lib.rs`). This guide also answers `tilo-app guide undo`,
`guide history` and `guide sync`, including `--json`.

Put durable fields in the `Model` resource and edits in `#[action]` methods.
Bind controls to generated descriptors through `ViewCx`. One action becomes
one attributed version and undo step; a bulk operation belongs in one action.
Use `Collection` item ids from `ActionCx::id()` so an edit follows its item.
`String` fields merge concurrent text edits; booleans and numbers resolve as
registers. Put visual caches and participant-local state outside the shared
model. External effects need their own effect contract; replaying a resource
edit must not repeat a network request or file write.

The example's production-path tests show what to assert:

- `concurrent_adds_keep_both_items`: fork `Replica<Checklist>`, give the two
  replicas different `as_author` values, apply edits and `sync_with` the peer.
  Check both items survive and both states agree.
- `undo_takes_back_only_my_own_actions`: invoke the action through `TestApp`,
  merge a peer edit and check the rendered result. Undo removes the local
  action while retaining the peer's item; redo restores the local action.
- `history_is_attributed`: inspect `Replica::versions()` and check the author
  and action summary. Use host-provided identity in the running app.

The resource runtime supplies storage, history, per-author undo and merge
semantics. On Tilo, workspace collaborators and roles govern access; agents
use the same permission-filtered resource/action door as other collaborators.
Device delivery uses the host's collaboration transport. Replica tests prove
merge behavior; sign-in, authorization and delivery between actual devices
still need host integration tests. They do not require an app-local database,
undo stack, identity service or sync loop.

## Evolve an existing resource

Changing a Rust field type does not convert documents stored on other
devices. `Collection<T>` stores a map keyed by item id, with order keys and
item values; an Automerge list at that path is a different storage shape.
An absent collection reads as empty. An `expected a collection` error means
a present value has the wrong shape: inspect its actual type and conflicts
before changing it. Keep the retained document; do not replace it with defaults.

Declare `#[resource(version = N, migrate = path)]` when changing the storage
schema. The migration receives the document and prior version. Preserve
item ids, values and order while converting the known old shape. The runtime
runs the callback on a fork and applies the housekeeping change only if it
succeeds. Test an actual old document, migration failure leaving it unchanged,
and a second open preserving the migrated content. A fresh empty app test does
not exercise an upgrade.

Next: `tilo-app guide actions` to expose the capability or `guide verify` to
inspect its public surface.
More: [Templates and examples](../../examples.md#examples),
[A resource workflow](../../sdk-resource-journey.md).
