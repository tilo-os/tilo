# Shared `WorkspaceTree` across compositor / portal / peer-sync — RFC

> **Status:** spec, not yet implemented. Closes audit finding **C-3** from `docs/superpowers/audits/2026-05-22-nirios-fresh-bugs.md`.

## Problem

`hosted::run_with_full_options` (`quick-shell/src/compositor/hosted.rs`) currently makes **three independent copies** of the `WorkspaceTree`:

| Copy | Owner | Used by |
|---|---|---|
| `tree` (moved into `build_nirios_root`) | `Chrome` state inside `App` | Shell chrome + WS-Settings overlays |
| `shared_tree: Arc<Mutex<WorkspaceTree>>` (line 82) | `ShellNetwork::spawn` | Peer-sync replica + libp2p actor |
| `shared_tree: Arc<Mutex<WorkspaceTree>>` (line 211) | `GrantStoreHandle::new` | Portal D-Bus backend |

Three copies, three identities. They diverge as soon as anyone mutates one.

**Observable consequences:**

1. **Grant added via portal → invisible in WS-Settings.** Portal's `OpenFile` request resolves an `Allow` outcome → `Workspace::grants.insert(...)` runs on the portal's clone. The Permissions tab in WS-Settings reads from the shell's clone — the grant doesn't appear there until the user restarts.
2. **Workspace added via shell → not synced to peers.** The user clicks "+" on the wheel disc → `ChromeState::tree.append_empty_workspace(...)` mutates the shell's clone. The peer-sync network replica is mutating its own; the next `SyncResp` ships a stale snapshot.
3. **Owner-only revocation works locally but doesn't reach the network replica** — the gestures land on the shell's clone, never reach the peer-shared one.

The hosted boot path's own `// TODO` at line 209-210 acknowledges this: "Full integration with the live shell tree (the `Rc<RefCell<ChromeState>>` chain) is a follow-up unit." This RFC is that follow-up.

## Design goals

1. **Single source of truth.** Exactly one `WorkspaceTree` instance per running compositor; every subsystem holds a reference, not a copy.
2. **Subsystem isolation preserved.** Compositor, portal, and peer-sync each get the access pattern they need (read for compositor, write for portal grant mutations, read+write for peer-sync apply_changes), but they all see the same underlying state.
3. **No giant lock-contention regression.** The shell repaints at ~60 Hz reading the tree on every layout pass. Coarse-grained locking would tank UI throughput.
4. **Back-compat for existing tests.** The 435 lib tests today construct `WorkspaceTree` directly in fixtures; the new shape needs to keep that path working (or provide a one-line shim).

## Proposed shape

```rust
// New type alias for the shared handle:
pub type WorkspaceTreeHandle = std::sync::Arc<parking_lot::RwLock<WorkspaceTree>>;
```

**Why `parking_lot::RwLock`** instead of `std::sync::Mutex`:
- Many readers (shell paint, portal grant lookup, peer-sync export) run in parallel
- One writer at a time is acceptable (the read-modify-write pattern in `Chrome::with_tree_mut` is the only writer pattern)
- `parking_lot` is already a transitive dep via wgpu; doesn't add to compile times
- Read locks are extremely cheap when uncontended (~10ns)

**Why one handle, not split read/write halves**:
- Three subsystems each need both reads + writes (compositor reads for paint + writes via shell mutations; portal reads grants + writes on allow; peer-sync reads for export + writes on apply_changes)
- Splitting would force every subsystem to hold both — same total surface, more glue

### Threading the handle

```rust
// In hosted::run_with_full_options:
let tree_handle: WorkspaceTreeHandle = Arc::new(RwLock::new(tree));

// Compositor side — Chrome holds the handle, not the tree:
let chrome = build_nirios_root_with_tree_handle(tree_handle.clone());

// Portal side:
let grant_store = GrantStoreHandle::new(tree_handle.clone());

// Peer-sync side:
let network = ShellNetwork::spawn(tree_handle.clone(), ...);
```

Each `clone()` is a refcount bump on the `Arc`, not a deep clone of the tree. The actual tree exists once.

### Chrome's mutation pattern

`ChromeState::tree` becomes `WorkspaceTreeHandle`. Every mutating helper in `chrome_state.rs` (currently 6+: append_empty_workspace, rename_workspace, set_maximized, set_width, etc.) acquires a write lock:

```rust
impl ChromeState {
    pub fn append_empty_workspace(&self, name: String) -> WorkspaceId {
        self.tree.write().append_empty_workspace(name)
    }
    pub fn rename_workspace(&self, id: WorkspaceId, name: String) {
        self.tree.write().rename_workspace(id, name);
    }
    // ...
}
```

The paint path reads via `self.tree.read()`. The RwLock guarantees readers see consistent snapshots and writers are serialised.

### Portal's grant mutation

`GrantStoreHandle::insert(workspace_id, app_id, capability)` and `::revoke(...)` acquire a write lock on the same handle:

```rust
impl GrantStoreHandle {
    pub fn insert(&self, ws: WorkspaceId, app: &AppId, cap: Capability) {
        let mut tree = self.tree.write();
        if let Some(w) = tree.workspaces.iter_mut().find(|w| w.id == ws) {
            w.grants.entry(app.clone()).or_default().push(cap);
        }
        // CRDT: mirror through `tree.doc` so peer-sync ships the change.
        tree.doc_insert_grant(ws, app, cap);
    }
}
```

The `doc_insert_grant` call (new method on WorkspaceTree following the existing `doc_set_maximized` / `doc_set_width` pattern) keeps the cached field + the underlying `automerge::AutoCommit` in lockstep — so peer-sync's `export_changes_since` picks up the new grant on the next tick.

### Peer-sync's apply path

`WorkspaceTreeReplica::handle_sync_resp` already calls `apply_changes` on its (formerly local) tree. Now it acquires a write lock on the shared handle:

```rust
fn handle_sync_resp(&self, msg: &SyncResp, sender: PeerId) {
    let mut tree = self.tree.write();
    if let Err(e) = automerge::apply_changes(&mut tree.doc, &msg.changes) {
        tracing::error!(...);
        return;
    }
    tree.rebuild_cache_from_doc();
}
```

The `rebuild_cache_from_doc` call after `apply_changes` is critical — that's how the new automerge state reaches the cached `Vec<Workspace>` that everyone else reads.

## Test contract

Two integration tests prove the unification (both audit-named):

### `grant_inserted_via_portal_is_visible_in_ws_settings`
```rust
let handle = WorkspaceTreeHandle::default_test_handle();
let portal = GrantStoreHandle::new(handle.clone());
let chrome = ChromeState::new_with_handle(handle.clone());

// Portal-side insert:
portal.insert(WorkspaceId(1), &AppId::test("camera-app"),
              Capability::Camera { scope: GrantScope::Session });

// Shell-side read:
let grants = chrome.workspace_grants(WorkspaceId(1));
assert!(grants.iter().any(|(app, _)| app.as_str() == "camera-app"));
```

### `workspace_added_via_chrome_is_visible_in_peer_sync_export`
```rust
let handle = WorkspaceTreeHandle::default_test_handle();
let replica = WorkspaceTreeReplica::new(handle.clone());
let chrome = ChromeState::new_with_handle(handle.clone());

let heads_before = replica.current_heads();
chrome.append_empty_workspace("New".into());
let changes = replica.export_changes_since(&heads_before);

assert!(!changes.is_empty(), "shell mutation should appear in peer-sync export");
```

## Migration path

The refactor is invasive (~6 file changes, ~30 mutation sites) but mechanical. Recommended subagent-driven sequence:

### Phase A: introduce the type without changing semantics (~1 hour)
- Add `WorkspaceTreeHandle = Arc<RwLock<WorkspaceTree>>` to `state.rs`
- Add `WorkspaceTree::shared(self) -> WorkspaceTreeHandle` constructor
- Existing code unchanged — just new API surface available

### Phase B: migrate Chrome (~2 hours)
- `ChromeState::tree: WorkspaceTreeHandle`
- Every mutating helper acquires `write()`; every reader acquires `read()`
- Update `build_nirios_root` to take a handle
- Run the lib ledger — should be 435 still (tests using `Chrome::new(tree)` still work via a shim that wraps in `Arc<RwLock>`)

### Phase C: migrate portal (~1.5 hours)
- `GrantStoreHandle` already holds an `Arc<Mutex<WorkspaceTree>>` — swap to `Arc<RwLock>`
- `hosted::run_with_full_options` line 211 takes `tree_handle.clone()` instead of cloning the tree
- Add `tree.doc_insert_grant` / `tree.doc_revoke_grant` so peer-sync picks up portal mutations
- New test: `grant_inserted_via_portal_is_visible_in_ws_settings`

### Phase D: migrate peer-sync (~1.5 hours)
- `WorkspaceTreeReplica::tree: WorkspaceTreeHandle`
- `apply_changes` writes; `export_changes_since` reads
- `hosted::run_with_full_options` line 82 takes `tree_handle.clone()`
- New test: `workspace_added_via_chrome_is_visible_in_peer_sync_export`

### Phase E: cleanup (~30min)
- Remove the `tree.clone()` calls in hosted.rs
- Update `// TODO` comment at line 209-210 — replace with "Full integration via WorkspaceTreeHandle landed in <SHA>"

Total scope: ~6 hours of focused work. Recommended dispatched as a single worker (the phases are tightly coupled — a partial landing would leave the system in a worse state than today).

## Risks

| Risk | Mitigation |
|---|---|
| RwLock contention under high paint rate | `parking_lot::RwLock` read locks are ~10ns uncontended; only matters if a write lock is held for >1 frame, which the proposed helpers don't do |
| Existing test fixtures break | Phase A's shim (`Chrome::new` takes either a tree or a handle) preserves them |
| Peer-sync receives changes that don't match cached state | `rebuild_cache_from_doc` after every `apply_changes` keeps the cached fields in sync; existing tests assert this invariant |
| Lock-ordering deadlock | All helpers acquire `write()` for the duration of a single method; no nested locks; no other locks involved |
| Async D-Bus side holding the lock too long | Portal's `insert` takes the write lock, mutates, releases — no async-await holding it |

## What this RFC does NOT solve

- **The collaboration model for *concurrent* writes.** Two peers both mutating `Workspace::grants` for the same workspace will go through automerge's CRDT merge, but the cached-materialisation re-derive may produce surprising results (last-write-wins on the cache pass). That's an existing concern, separate from C-3.
- **Per-workspace lock granularity.** Today every mutation locks the whole tree. If contention becomes a measured problem, the next refactor splits to per-workspace `Arc<RwLock<Workspace>>`. Premature today.
- **D-Bus async borrowing.** zbus methods return futures. The portal's `insert` holds the lock briefly and synchronously — no async-await across the lock. If a future flow needs to hold across an `await`, that's a separate design issue.

## Why not just use `Rc<RefCell<>>` like the shell already does internally?

The shell's `Chrome` lives behind `Rc<RefCell<ChromeState>>` for single-threaded interior mutability. But peer-sync runs on a separate tokio runtime (`ShellNetwork::spawn` spawns a tokio task); portal runs zbus on its own tokio runtime. Cross-thread access requires `Arc<RwLock<>>` semantics.

The compositor's main-thread `Rc<RefCell<>>` and the peer-sync/portal `Arc<RwLock<>>` aren't compatible. The choice: lift the compositor to `Arc<RwLock<>>` too. Cost: ~10ns per read (vs ~2ns for `RefCell::borrow`). Imperceptible at 60 Hz.

## Open questions for the implementing session

1. **Async D-Bus issue revisited.** Should `GrantStoreHandle::insert` be sync or async? If sync (recommended), the zbus method that invokes it can still be async; the lock guard just doesn't cross `.await`. Confirm during Phase C.
2. **Test fixture handling.** Are there existing tests that construct a bare `Workspace` then `tree.push(workspace)` directly? If yes, they'll need a `tree.write()` somewhere. Easier to provide a `WorkspaceTreeHandle::test_default()` helper that returns a pre-wrapped handle with a default tree inside.
3. **Should `Chrome` expose `tree_handle()` for tests that want the raw `Arc<RwLock>`?** Yes — needed for the two new integration tests. Add it.
