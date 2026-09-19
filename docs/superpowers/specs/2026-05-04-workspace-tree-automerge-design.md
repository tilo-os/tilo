# WorkspaceTree Automerge design

**Status:** Implemented. The live model is in `crates/session/shell/src/shell/state.rs`.

## Decision

`WorkspaceTree` keeps ordinary Rust collections as a cached view of an
`automerge::AutoCommit` document. Mutators update both representations, while
incoming changes update the document and rebuild the cache.

This gives collaboration code the change-oriented API it needs:

```rust
fn current_heads(&self) -> Vec<automerge::ChangeHash>;
fn export_changes_since(&self, since: &[automerge::ChangeHash]) -> Vec<Vec<u8>>;
fn apply_changes(&mut self, changes: &[Vec<u8>]) -> Result<(), automerge::AutomergeError>;
```

It also preserves direct reads such as `tree.workspaces.iter()` throughout the
shell. Making every consumer query Automerge directly would add substantial
schema plumbing to rendering and input code without improving merge behavior.

## Document shape

The root document contains:

- `workspaces`: a list of workspace maps, with nested strips, window groups,
  and tabs;
- `active`: the active workspace id;
- `active_strip`: workspace-to-strip ids;
- `active_group`: strip-to-group ids;
- `collaborators`: collaborator records.

Names use Automerge text objects. Identifiers and booleans use scalar values.
`TileContent` stores its discriminant separately from a serialized payload;
concurrent writes to a payload are therefore last-writer-wins rather than a
field-by-field merge.

## Invariants

- Every public mutation must update both the cache and document.
- Applying remote changes must rebuild the cache before readers observe it.
- Debug builds compare the two representations after mutations so a missed
  write-through fails near its source.
- Concurrent list additions must survive on both peers; conflicting structural
  moves must resolve deterministically.
- Schema changes require compatibility work because peers exchange Automerge
  changes, not version-neutral JSON snapshots.

The cached representation is intentionally a performance and compatibility
layer, not a second source of truth. Persistence and transport code must retain
the Automerge document whenever it contains state not recoverable from the
cache.

## Verification

The `shell` state and collaboration tests cover change export/application,
convergence, concurrent additions and moves, and cache/document consistency.
The owning code and those tests are the source of truth for the current schema.
