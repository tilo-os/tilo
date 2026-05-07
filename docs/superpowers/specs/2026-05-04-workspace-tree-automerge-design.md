# WorkspaceTree → Automerge Migration

**Status:** spec — awaiting plan + execution.
**Owner:** _(unassigned)_.
**Branch:** TBD off `nirios-shell`.

## Problem

`WorkspaceTree` (`quick-shell/src/shell/state.rs:324-345`) is the single source of truth for the per-session shell state: workspace list, active workspace, per-strip active-group pointer, collaborators. It is currently a plain `serde::{Serialize, Deserialize}` struct and persists to disk via `serde_json` (`quick-shell/src/shell/persist.rs`).

The peer-sync plan (Phase 1 Task 1.3) requires three operations on the tree:

```rust
fn current_heads(&self) -> Vec<automerge::ChangeHash>;
fn export_changes_since(&self, since: &[automerge::ChangeHash]) -> Vec<Vec<u8>>;
fn apply_changes(&mut self, changes: &[Vec<u8>]) -> Result<(), automerge::AutomergeError>;
```

These cannot be implemented on a plain serde struct — they require the tree to be backed by an `automerge::AutoCommit` document so changes can be exported as opaque CRDT change-blobs and applied to peer replicas. **This blocks peer-sync Phase 1 Tasks 1.3 + 1.4 and every later phase.**

The previous worker (#156 / #157) confirmed the plan author's note that "if WorkspaceTree is pure-serde-not-automerge, this phase needs a larger refactor".

## Goals

1. `WorkspaceTree` exposes `current_heads`, `export_changes_since`, `apply_changes` against a real automerge document with the actual workspace state encoded as automerge primitives — so concurrent edits from two peers genuinely merge (not last-write-wins on a JSON blob).
2. Existing read API (`tree.workspaces.iter()`, `tree.active`, `tree.active_strip[ws]`, etc.) is preserved without churning every consumer.
3. JSON persistence keeps working for now — on-disk format migration is a follow-up.

## Non-goals

- Full bidirectional CRDT semantics for every nested mutation (e.g., concurrent moves of the same tab across strips). The first cut writes through every mutation but lossy merges of conflicting structural ops are acceptable in v1.
- Migrating `quick-ui`'s widget-local CRDT docs (notes, editor) — they are already automerge-backed and untouched by this work.
- Schema versioning / forward-compat. Future schema bumps live in their own follow-up.

## Approach

**Cached materialisation** — keep the existing struct fields as a derived view; add a backing `doc: AutoCommit` that mirrors the cache for export/apply:

```rust
pub struct WorkspaceTree {
    pub workspaces: Vec<Workspace>,
    pub active: WorkspaceId,
    pub active_strip: HashMap<WorkspaceId, StripId>,
    #[serde(default)]
    pub active_group: HashMap<StripId, GroupId>,
    pub collaborators: Vec<Collaborator>,

    /// Automerge backing doc that mirrors the public cache.
    /// Lazily seeded; mutations write to BOTH sides.
    /// `apply_changes` rebuilds the cache from this doc.
    #[serde(skip, default = "AutoCommit::new")]
    doc: AutoCommit,
}
```

### Why cached, not pure

The pure-doc alternative (every read goes through `doc.get(...)`) would touch every consumer in `topbar_chrome.rs`, `sidebar_wheel_disc.rs`, every chrome and overlay module, every test file. ~1000+ lines of test code uses field literals (`WorkspaceTree { workspaces: vec![...], ..., }`). That's a multi-week refactor with substantial regression risk.

Cached materialisation lets every existing reader keep using `tree.workspaces.iter()` while peer-sync gains the export/apply hooks. The cost: every mutator must write through to both sides.

### Encoding shape

The doc encodes the tree at `automerge::ROOT` as a `Map`:

- `workspaces: List<Map>` — each workspace is `{ id: u64, name: String, strips: List<Map>, ... }`.
- `active: u64` — the active workspace id.
- `active_strip: Map<u64, u64>` — workspace_id → strip_id.
- `active_group: Map<u64, u64>` — strip_id → group_id.
- `collaborators: List<Map>` — collaborator records.

Strips and window-groups follow the same `Map { id, name, ... }` shape with nested `List`s for their children.

`Tab.title`, `Workspace.name`, `Collaborator.display_name` use automerge `Text` (CRDT-safe collaborative editing). `id` fields use automerge `ScalarValue::Uint`. Booleans like `WindowGroup.maximized` use `ScalarValue::Boolean`.

`TileContent` is the awkward variant — it's an enum with payload data. **Decision:** encode the discriminant as a string scalar (`"notes"` | `"editor"` | `"files"` | `"terminal"` | `"browser"` | `"placeholder"`) plus the payload as a serde-JSON blob in a sibling `Bytes` field. That gives us schema flexibility without committing to per-variant CRDT structures yet — payload merges are last-write-wins, which is fine for v1.

### Mutator inventory

To enumerate the writes we'll need to mirror:

| File | Method | Mutates |
|---|---|---|
| `state.rs` | `bootstrap_default` | full tree (lazy first-use) |
| `state.rs` | `set_active` | `active` |
| `state.rs` | `switch_up` / `switch_down` | `active` |
| `state.rs` | `append_empty_workspace` | `workspaces` push |
| `state.rs` | `set_active_group` | `active_group` |
| `state.rs` | `nudge_active_group_off_closing` | `active_group` |
| `state.rs` | `move_tab_across_strips` | nested move |
| `overlay_host.rs` | `harvest_pending_maximize_changes` | `WindowGroup.maximized` |
| `overlay_host.rs` | `harvest_pending_width_changes` | `WindowGroup.width` |
| `overlay_host.rs` | `harvest_pending_cross_strip_detach` | nested move |
| `overlay_host.rs` | `add_workspace_click_handler` | `workspaces` push |
| `topbar_chrome.rs` | rename via WS-settings General tab | `Workspace.name` |
| `workspace_drop.rs` | `apply_drop` | tab insert |

(`grep -rn "tree\\." quick-shell/src` for the canonical list.)

## Phasing

The migration is too big for one PR. Split into 4 ordered units:

### Unit A — backing field + 3 new methods + Default sweep

- Add `doc: AutoCommit` field, derive `Default`, mark `#[serde(skip)]`.
- Sweep ~27 struct-literal sites to use `..Default::default()` (one-line edit each).
- Implement `current_heads`, `export_changes_since`, `apply_changes` (the latter accepts changes, applies to doc, calls `rebuild_cache_from_doc`).
- Implement `rebuild_cache_from_doc` walking the doc → repopulating `workspaces`/`active`/etc.
- Test: build tree A, write directly via `tree_a.doc.put(ROOT, "active", 42)`, export, apply to tree B, assert `tree_b.active == 42` after `apply_changes`.
- **Deliverable:** API surface stable, peer-sync code can compile against it. Scenario where two peers actually exchange a workspace is NOT yet wired.
- **Estimated diff:** +400 lines. **Risk:** low — additive.

### Unit B — wire `bootstrap_default` + simple top-level mutators

- `bootstrap_default` seeds the doc.
- `set_active`, `switch_up`, `switch_down` write to `doc` AND cache.
- `append_empty_workspace` appends to BOTH the cache list AND the doc list.
- Test: peer A appends workspace via `append_empty_workspace`, exports changes, applies to peer B, peer B's cache shows the new workspace.
- **Deliverable:** workspace-list-level convergence works.
- **Risk:** medium — getting the automerge `List` ops right.

### Unit C — wire `harvest_*` mutators (nested writes)

- Map `WindowGroup.maximized` and `width` writes through the doc tree (workspaces[i].strips[j].window_groups[k].maximized).
- Test: peer A toggles maximize, peer B sees the flip after sync.
- **Risk:** medium-high — nested path management is the trickiest automerge bit.

### Unit D — wire `move_tab_across_strips` + `apply_drop`

- Tab moves across strips: in automerge, this is a `delete` + `insert` on two different `List`s. The deterministic id of the tab matters for merging concurrent moves.
- Test: peer A moves tab, peer B receives the move + its own concurrent maximize doesn't get clobbered.
- **Risk:** high — concurrent structural mutations are where CRDT semantics get exercised hardest.

## Tests

Required:

1. `empty_tree_has_no_heads_or_changes`
2. `add_workspace_then_export_changes_yields_nonempty`
3. `apply_changes_from_one_tree_to_another_converges_workspaces`
4. `rebuild_cache_from_doc_round_trips_workspaces`
5. `concurrent_maximize_toggles_converge_to_last_writer_wins` (Unit C)
6. `concurrent_workspace_appends_both_survive` (Unit B)
7. `concurrent_tab_moves_resolve_deterministically` (Unit D)

Existing concept_parity_spec (26 tests), e2e_shell_flows (15 tests), all `--lib` tests must stay green at every Unit's end.

## Risks

- **Cache drift**: a write that hits the cache but skips the doc → peer-sync silently drops the change. Mitigation: a `#[cfg(debug_assertions)]` `assert_cache_matches_doc()` that runs after each mutator; CI gate.
- **AutoCommit Clone**: if `AutoCommit: !Clone` in the version we use, `WorkspaceTree: Clone` breaks. Mitigation: manual `Clone` impl that does `AutoCommit::load(&self.doc.clone().save())`.
- **Persistence format change**: serde-JSON persistence becomes lossy if `doc` carries more state than the cache (it shouldn't, but a write that lands in the doc and not the cache would survive a restart only if persistence migrates to automerge save/load). Mitigation: dual-write to JSON + automerge save in `persist::save`, prefer automerge on load if present.
- **Test fixture churn**: `..Default::default()` works only if every field is `Default`. Mitigation: derive `Default` for `WorkspaceTree`, ensure all field types support it (Vec / HashMap / `WorkspaceId::default()` exists / `AutoCommit::new` is free).

## Out of scope (follow-ups)

- Persistence format migration to automerge save/load — this lives in its own future spec because cross-version compat is hairy.
- `quick-ui`'s widget-local CRDT docs syncing across peers (presence, notes content) — that's already automerge-backed and lives one layer down; peer-sync's Phase 4 (foreign-surface streaming) handles it.
- Schema versioning. Today every peer must run the same shell binary.

## Acceptance

The migration is "done" when:

1. `peer_sync::WorkspaceTreeReplica::run_one_round()` (Phase 1 Task 1.4) converges two replicas where peer A appended a workspace, set_active'd a tab, and toggled maximize — and peer B's tree matches across all three after one sync round.
2. The 26 concept_parity_spec, 15 e2e_shell_flows, all `--lib` tests stay green.
3. The cache-drift sanity assertion (debug-only) doesn't fire under any existing test.

Once that lands, peer-sync Phase 1 Tasks 1.3/1.4 are unblocked and the rest of the peer-sync plan (Phase 3 shell integration, Phase 4 surface streaming, Phase 5/6) can resume.
