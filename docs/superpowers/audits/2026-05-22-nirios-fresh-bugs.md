# 2026-05-22 — nirios fresh systematic bug audit

Post-Phase-7 (native default), post-Sandbox-4.3 (xdg-desktop-portal),
post-Peer-sync-4 (H.264 streaming + wl_buffer readback) compositor
audit. The predecessor logical-bugs audit
(`docs/superpowers/plans/2026-04-30-nirios-logical-bugs.md`) landed
all 12 units; this report walks the new code surfaces introduced
since and identifies a fresh wave of bugs.

Repository state at audit time:
- Worktree: `/var/home/quinten/Development/quickdesktop/.claude/worktrees/bug-audit`
- Branch: `bug-audit` (forked from `nirios-shell`)
- Superproject HEAD: `07cb1c6` ("chore(quick-shell): bump for DRM
  connectors + paint (Phase 5.2)").
- Submodules: `quick-shell@2b4fae8e`, `os@694ee700`,
  `quick-fs-fuse@a5e321ec`, apps/* at their bumped pointers.

All findings cite `<absolute-path>:<line>`. Read-only audit; no source
edits in this commit.

---

## Section A — Methodology

What I looked at:
- **Compositor mainloop** (`compositor/native.rs`, `state.rs`,
  `frame.rs`, `test_control.rs`, `tab_binding.rs`,
  `surface_readback.rs`, `hosted.rs`) — Phase 4-7 chrome wireup.
- **Sandbox / portal D-Bus** (`sandbox/*`, `portal/*`) — Phase 4.3
  grant flow, seccomp blob path, bridge socket lifecycle.
- **Peer-sync / streaming** (`network/*`, `shell/network.rs`) —
  libp2p actor, automerge replica + sync messages, H.264 encode,
  surface readback.
- **State / persistence** (`shell/state.rs`, `shell/persist.rs`) —
  WorkspaceTree automerge migration (#157), `next_id` allocation.
- **DRM backend** (`compositor/backend/drm/*`) — paint bridge,
  runtime mainloop, output enumeration.

What I skipped:
- Visual / cosmetic bugs in the chrome widgets (covered by the
  predecessor audit; ~20 widgets pre-existed).
- Apps submodules (terminal/file-explorer/browser); the scope was
  the OS layer and the compositor.
- `quick-fs-fuse`; no recent activity in scope.
- Spec-only files in `docs/superpowers/specs/`.

For each focus area I ran `rg` for the relevant patterns
(`unwrap()`/`expect()`/`unsafe`/`Arc<Mutex`/`#[serde(skip)]`/integer
casts) and then traced each call site. Items I could trace to a
concrete exploitable consequence are P0/P1; smells without a
reproduction path are P3.

---

## Section B — Severity ladder

- **P0** — Data loss, panic on user-driven input, sandbox bypass,
  or peer-sync silently corrupts state.
- **P1** — Unusable feature, security-impacting (e.g. world-readable
  secret), or a documented "phase X follow-up" that ships a misleading
  success response to clients.
- **P2** — Latent bug (no current trigger, would activate on a
  planned change) or correctness-cosmetic.
- **P3** — Smell, future-bug-shaped, no concrete reproduction.

---

## Section C — Anchor findings (high-impact, reproducible)

### C-1 — `next_id()` is a process-local counter; cross-peer IDs collide silently (P0)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/state.rs:73-77`

`next_id()` is an `AtomicU64` that starts at `1` per process. Two
peers running `cargo run -p quick-desktop` in parallel both mint
`WorkspaceId(2)`, `WorkspaceId(3)`, etc. When they sync via
`apply_changes`, the doc's `workspaces` list ends up with two
distinct entries that share the same `id` field. The cache
rebuild at `state.rs:744` (`self.workspaces.iter().position(|w| w.id == ws_id)`)
finds **one** workspace and treats both as the same — workspace A's
strips collide with workspace B's strips, names overwrite, grants merge.

Repro (code-path inspection):
1. Peer P1 calls `tree.append_empty_workspace("Foo")` → ws_id=2.
2. Peer P2 (separate process, fresh counter) calls
   `tree.append_empty_workspace("Bar")` → ws_id=2.
3. Peers sync. Both `workspaces` list entries have `id=2`.
4. `rebuild_cache_from_doc` walks the list; the second entry's
   `name` clobbers "Foo" → "Bar", and either workspace's strips
   are folded together.

Same family applies to `StripId`, `GroupId`, `TabId`, `FileId`.

Suggested fix: replace `next_id()` with a `(peer_id_hash << 48) | counter`
scheme, or use UUIDs (or automerge-native `ObjId`s) for the cache-side
identifiers. Until then, the WorkspaceTree merge story is broken.

Suggested test:
`tests::two_peers_with_independent_next_id_collide_under_sync` —
construct two `WorkspaceTree`s with their own counters, append a
workspace in each, sync via the hub, and assert
`tree_a.workspaces.len() == tree_b.workspaces.len() == 2` (not 1).

### C-2 — `persist::load_or_default` drops the automerge doc (P0)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/persist.rs:24-27`
`/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/state.rs:512`

`WorkspaceTree::doc` is `#[serde(skip, default = "AutoCommit::new")]`,
so JSON-load on startup discards the entire CRDT history. After a
restart, `tree.doc` is empty. Any peer that dials in immediately
after sees `export_changes_since(&[])` return an empty Vec — the
local user's existing workspaces never propagate to peers, and any
local mutations are doc-fresh (no history to merge with the peer
side).

The bootstrap path does call `seed_doc_from_cache` at
`state.rs:1569` from `bootstrap_default`. But the persist load path
at `persist.rs:24-26` does not — it goes
`json → serde → WorkspaceTree { doc: AutoCommit::new() }` and
returns straight to the caller.

Repro:
1. User runs `quick-desktop`, creates workspace "Research", quits.
2. State persists to `~/.config/quick-desktop/state.json`. The
   `workspaces` cache is preserved; `doc` is dropped.
3. User restarts `quick-desktop --enable-peer-sync`. `load_or_default`
   reads the JSON; `tree.doc` is empty.
4. A peer joins via mDNS. Peer asks for `SyncReq { since: [] }`.
   The local replica replies with an empty `SyncResp`.
5. Workspace "Research" never reaches the peer.

Suggested fix: in `persist::load_or_default` (and `load_tree_from`),
call `tree.seed_doc_from_cache()` before returning. This rehydrates
the doc from the cache so subsequent peer-sync runs export the
correct changes.

Suggested test:
`tests::persisted_tree_export_changes_after_load_returns_non_empty` —
write a tree to disk with one appended workspace, reload via
`load_tree_from`, assert `tree.export_changes_since(&[])` is
non-empty.

### C-3 — Two separate `Arc<Mutex<WorkspaceTree>>` clones break collaboration + portal grants end-to-end (P0)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/compositor/hosted.rs:82, 211`

`hosted::run_with_full_options` clones the loaded `WorkspaceTree`
**twice** into `Arc<Mutex<_>>` wrappers:
- Line 82: `let shared_tree = Arc::new(Mutex::new(tree.clone()))` →
  handed to `ShellNetwork::spawn` for peer-sync.
- Line 211: `let shared_tree = std::sync::Arc::new(std::sync::Mutex::new(tree.clone()))` →
  handed to `GrantStoreHandle::new` for the portal D-Bus backend.

Both are *independent copies*. The third copy (`tree` itself, not
wrapped) is moved into `build_nirios_root(&tree)` and then into `App`.

Consequences:
- A user grants a filesystem capability through the prompt pump.
  The grant is stored in the **portal's** clone of the tree. The
  shell's `ChromeState::tree` is a different object — the next
  portal request via `GrantStoreHandle::lookup` reads the
  portal's clone (correct), but if the user reads the grants list
  in the WS-Settings Permissions tab (which is driven by the
  shell's tree), the new grant is invisible.
- A peer issues a `SyncReq`; the network replica owns its own clone.
  Local UI mutations to `tree` (via `ChromeState`) never propagate
  to the network clone. Peer-sync ships a stale snapshot.

The code openly acknowledges this with a TODO at line 209-210
("Full integration with the live shell tree (the
`Rc<RefCell<ChromeState>>` chain) is a follow-up unit"), and the
Phase 3 plan says the same. But the published surface
(`--enable-peer-sync`, `--enable-portals`) implies the feature
works.

Suggested fix: thread one `Arc<Mutex<WorkspaceTree>>` through both
`ShellNetwork::spawn` and `GrantStoreHandle::new`, *and* mirror
every `ChromeState::tree` mutation onto it. The simplest landing is
to make `Chrome` own an `Arc<Mutex<WorkspaceTree>>` rather than an
inline `WorkspaceTree`; all 6+ mutating helpers in
`chrome_state.rs` need to lock-and-write.

Suggested test:
`tests::grant_inserted_via_portal_is_visible_in_ws_settings`,
`tests::workspace_added_via_chrome_is_visible_in_peer_sync_export`.
Both assert that a mutation in one subsystem reaches the other.

### C-4 — `test_control` ClickAt/ClickNamed return `Ok` but never execute (P1)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/compositor/native.rs:134-173`
`/var/home/quinten/Development/quickdesktop/quick-shell/src/compositor/test_control.rs:141-176`

`dispatch_command` for `Command::ClickAt` and `Command::ClickNamed`
returns a `Response::Ok { frame: 0 }` plus a `Vec<PlannedAction>`.
The `pump_test_control_conn` helper in `native.rs:148-154` reads the
planned-actions vec and logs `received N planned action(s); execution
is Phase 2b (not yet wired)`, then writes the `Ok` response back.

The protocol contract from the spec
(`docs/superpowers/specs/2026-05-16-real-input-e2e-design.md`) is
that `Ok` means "click executed". The current code returns `Ok`
while silently discarding the action, leaving the client with no
machine-readable signal that nothing happened.

The dispatcher's own unit tests (`test_control.rs:301-335`) only
assert that the `PlannedAction` vec has the right shape — they never
assert the action took effect. That's a tautology-style test (the
predecessor audit's "verification mantra" surface) — it passes today
and would still pass if the transport silently dropped every
planned action, which is exactly what's happening.

Suggested fix: until the wireup lands, return
`Response::Err { code: "not_implemented", message: "click execution is
Phase 2b" }` for any command that produces a non-empty
`Vec<PlannedAction>`. That's a one-line change in `native.rs` that
keeps test clients honest.

Suggested test:
`tests::click_at_returns_not_implemented_until_phase_2b` — drive a
ClickAt command through the unix socket, assert the response is an
`Err` with code `not_implemented`. Will go red when Phase 2b lands;
that's the trigger to delete this test.

### C-5 — `SandboxProfile::seccomp = Some(_)` emits a hardcoded fd-10, never actually loading the filter (P1)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/sandbox/profile.rs:272-280`

```rust
if self.seccomp.is_some() {
    // Phase 1 placeholder: real fd-passing arrives in Phase 2 ...
    argv.push("--seccomp".to_string());
    argv.push("10".to_string());
}
```

Whenever a caller sets `profile.seccomp = Some(path)`, the rendered
bwrap argv carries `--seccomp 10` — a literal fd number. Bubblewrap
expects an actual open fd on that number; the spawn helper in
`sandbox/spawn.rs:99-100` does **not** pre-open the seccomp.bpf
file on fd 10. So bwrap will either fail (if fd 10 is closed) or
attempt to use whatever stale fd happens to be at 10 (if a parent
file descriptor leaked there). In neither case is the deny-list
filter compiled by `compile_workspace_seccomp` actually applied to
the sandboxed process.

Consequences:
- Any caller that thinks "set the seccomp field, get a sandboxed
  app" gets an app that runs without any seccomp restriction (or a
  bwrap launch error).
- The deny-list (ptrace, mount, unshare, bpf, …) is silently bypassed.

Suggested fix: open the cached bpf file pre-fork, pass the resulting
`OwnedFd` to `Command::pre_exec` via `dup2(fd, 10)`, and only then
emit `--seccomp 10`. Or replace the hardcoded `10` with the fd
returned from `as_raw_fd`. Either way the rendered argv has to
match an fd the spawn actually plumbs through.

Suggested test:
`tests::seccomp_some_does_not_emit_arbitrary_fd_without_plumbing` —
build a profile with `seccomp = Some("/some/path".into())`, call
`spawn_in_workspace`, assert the spawn fails fast (or the test
patches `Command::pre_exec` and asserts fd 10 is open in the child).

### C-6 — `tab_binding::adopt_toplevel` silently drops the surface when active workspace has no strips (P1)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/compositor/tab_binding.rs:45-83`

When `active_workspace().strips.is_empty()` (a real state — a user
can delete every strip; the bootstrap default has strips but the
data model permits zero), `adopt_toplevel` synthesises a `tab_id`,
returns it, and otherwise does nothing — the tab never enters the
tree.

The compositor's `XdgShellHandler::new_toplevel` (at
`state.rs:308-327`) receives the synthesised `tab_id` from
`adopt_toplevel` but **does not** correlate it with the wayland
`surface_id`. The compositor never tracks the wl_surface → no-op
adoption mapping; it just calls `surface.send_configure()`. The
client then maps a real wayland surface that the compositor will
never paint into any tile rect — the surface is alive in the
protocol layer but invisible.

Consequences:
- Buffers and shm pool memory pile up for an unreachable surface.
- The surface is never destroyed by the compositor side, so it
  outlives until the client drops it.

Suggested fix: in `adopt_toplevel`, on the no-strips branch,
return `Option<TabId>` (or even an error) so the caller can
`surface.send_close()` and tear down the toplevel cleanly.

Suggested test:
`tests::adopt_toplevel_into_empty_strips_returns_no_tab` — build a
tree with `active_workspace().strips.clear()`, call `adopt_toplevel`,
assert it returns `None` (after the signature change) or that the
returned `tab_id` is `0` and the tree is unchanged.

### C-7 — `toplevel_destroyed` doesn't drop the surface from `SurfaceTracker` (P2 latent)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/compositor/state.rs:329-337`

`XdgShellHandler::toplevel_destroyed` calls
`tab_binding::close_tab_for_destroyed_surface` to remove the foreign
tab. It does **not** call `self.surface_tracker.forget(surface_id)`.

The `SurfaceTracker` is only cleared when a `commit` hook receives
`BufferAssignment::Removed` (line 275-277). A client that drops its
toplevel without an explicit `attach NULL` first leaves a stale
slot. The slot's `Vec<u8>` (sized at the buffer's last frame)
stays allocated until the next commit on the same surface_id — but
the surface is destroyed, so that never happens.

Consequences:
- Per-surface memory leak (~`width*height*4` bytes per leaked surface).
- Slow accumulation; not exploitable directly but visible in long
  sessions with many short-lived toplevels.

Suggested fix: call `self.surface_tracker.forget(surface_id)` from
`toplevel_destroyed` immediately after the tab-binding call. Same
shape as `commit`'s `Removed` branch.

Suggested test:
`tests::toplevel_destroyed_drops_surface_tracker_slot` — synthesise a
commit (via `deposit_for_test`) for surface_id 7, call the destroy
hook, assert `tracker.take_latest(7).is_none()`.

### C-8 — Peer keypair file is written world-readable (P1 — security)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/network/behaviour.rs:67-74`
`/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/network.rs:35`

`load_or_create_keypair` writes the freshly-generated ed25519 keypair
to `/var/lib/quick-fs/peer-key` (or test path) via `std::fs::write`,
which uses the default umask permissions (0644 on most systems).
The private key is then readable by any user on the box.

Anyone with read access can use the keypair to impersonate this
peer in the libp2p mesh, hijacking gossipsub messages and accepting
inbound `RemoteInput` events on behalf of this peer.

Suggested fix: after writing, call
`std::os::unix::fs::PermissionsExt::set_permissions(&path,
PermissionsExt::from_mode(0o600))`. Same shape for the parent
directory (`/var/lib/quick-fs`) — should be `0o700`.

Suggested test:
`tests::keypair_file_is_mode_0600` — create a keypair, stat the
file, assert the mode bits are `0o600`.

### C-9 — `replica.handle` swallows `apply_changes` errors silently (P2)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/network/replica.rs:88-90`

```rust
NeighbourMsg::SyncResp { changes } => {
    let _ = self.tree.lock().unwrap().apply_changes(changes);
}
```

A malformed or corrupt `SyncResp` from a peer (e.g. a deliberately
crafted change that fails automerge's CRDT validation, or a
truncated wire byte) returns
`Err(AutomergeError::...)`. The error is discarded; the local
replica believes the sync round succeeded but the changes never
landed. The local tree silently diverges from the peer's view.

Even worse, there's no observability — nothing logs the failure,
so a user looking at telemetry sees a healthy sync round.

Suggested fix: log at `tracing::warn!` with the peer id + error,
and surface a structured `NetEvent::SyncFailed` so the shell can
optionally notify the user.

Suggested test: `tests::malformed_sync_resp_yields_warn_event` —
feed a `SyncResp` with random bytes, assert the replica returned
without panicking AND that a `NetEvent::SyncFailed` was emitted.

### C-10 — `BridgeSocket::start` accept loop has no shutdown channel; workspace deletion would leak threads + sockets (P2 latent)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/sandbox/spawn.rs:23-30, 137-154`
`/var/home/quinten/Development/quickdesktop/quick-shell/src/compositor/bridge.rs:30-105`

`BRIDGES` is a process-lifetime `OnceLock<Mutex<HashMap<u64, BridgeSocket>>>`
("intentionally leaked"). Each `BridgeSocket::start` spawns one
accept-loop thread; each accept spawns two relay threads. There's
no `stop()` method, no atomic-bool shutdown flag, and no way to
remove an entry from the map.

When a future "workspace deleted" path lands, every previously-
launched workspace's bridge keeps its threads + open file descriptors
forever. The `Drop` impl on `BridgeSocket` (line 108-114) unlinks
the socket file, but the listener fd is owned by the accept thread
which never sees the drop signal.

This is latent today (no workspace-delete path exists), but the
docstring at `spawn.rs:25-28` admits to this gap. P2 latent because
the trigger isn't wired yet; flip to P1 the moment workspace
deletion ships.

Suggested fix: switch to an `Arc<AtomicBool>` shutdown flag the
accept loop polls + a `BridgeSocket::stop()` that sets the flag and
unlinks the socket. The `HashMap` then needs a `remove(ws_id)` path.

Suggested test:
`tests::bridge_socket_stop_terminates_accept_thread` — create,
start, stop, assert the accept thread joins within a timeout.

---

## Section D — Family-walk findings (systematic pattern matches)

### D-1 — `apply_remote_input` is a stub: every accepted event is logged, none reaches the seat (P1)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/compositor/state.rs:154-171, 182-191`

The `RemoteInputSink` impl on `State` is documented as v1 ("seat
injection lands in 4.4 follow-up"). The path returns `true` from
`apply_remote_input` when the share-registry gate allows the event,
giving the network actor (and any test inspecting the boolean)
positive confirmation. But the only mutation is
`self.last_remote_input = Some((peer, surface, event))` — no
keyboard / pointer wire-up to the smithay `Seat<Self>`.

Consequence: the `read_only=false` happy path's contract ("the
peer drives the seat") is a no-op. The `read_only=true` path
correctly drops; the difference between the two outcomes is
*just the boolean*, which is what every test asserts on.

Suggested fix: until the seat wireup lands, either rename the
function to `try_apply_remote_input_and_record` or make it return
an enum `Routed | DroppedReadOnly | DroppedNoShare` so callers
can't confuse "routed" with "applied".

Suggested test: replace the boolean assertion in
`streaming::tests::remote_input_blocked_when_read_only` with an
assertion that the actual seat saw N events. Will need
`#[ignore]` until the seat plumbing lands.

### D-2 — `FrameBuilder::widget_registry` clones the entire HashMap into the measure closure every relayout (P3)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/compositor/frame.rs:141`

```rust
let measure_map = self.node_map.clone();
```

Each frame's relayout clones the entire `HashMap<NodeId, *mut dyn Widget>`
into a local for the closure passed to `compute_layout_with_measure`.
The map can hold one entry per widget (hundreds for a real shell);
the clone happens at 60 fps. Heap churn during steady-state UI.

The reason for the clone is the borrow checker — the closure needs
`&self.node_map`, while `compute_layout_with_measure` already
borrows `self.taffy` mutably. A scoped borrow split (or carrying
the map in a `Cell`/`RefCell`) avoids the clone.

P3 (smell, no concrete crash). A profiler run would show measurable
allocator pressure but no functional break.

Suggested fix: take an `&HashMap<...>` via a small `MeasureCtx`
struct, lifetime-tied to the `relayout` call, and pass `&measure_ctx`
into the closure.

Suggested test: not appropriate as a unit test; add an
`Allocator::alloc_count` benchmark if the fix lands.

### D-3 — `compositor::native::run`'s test-control socket file never gets unlinked on exit (P3)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/compositor/native.rs:64-82, 396-403`

`bind_test_control_socket` opens a `UnixListener` at the
`NIRIOS_TEST_CONTROL` path. The `Drop` for `UnixListener` closes
the fd but does **not** unlink the socket file (POSIX semantics).
At process exit (`process::exit(0)`) the file lingers.

The next compositor launch with the same path detects this and
removes it (line 67), so the practical impact is "the socket file
keeps existing after the test process dies". Not exploitable but
clutters `/tmp` between test runs.

Suggested fix: wrap the listener in a small struct whose `Drop`
unlinks the path, or register a `ctrlc` handler that does the same.

### D-4 — `BridgeSocket` accept-loop thread is detached on drop without any join (P3)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/compositor/bridge.rs:30-46, 108-114`

The `accept_thread` field is `Option<JoinHandle<()>>`. The `Drop` impl
unlinks the bridge socket file but never `take().join()`s the
thread. The thread sees the listener error on the next `accept()`
(because the file is gone), then exits — but the join handle was
dropped, which is a detach.

P3 because the thread does terminate (no leak); the smell is
"silent detach makes debugging harder". A `tracing::warn!` if the
join would have been quick would surface this.

### D-5 — `swizzle_to_rgba` `debug_assert_eq` may not run in release; mismatched-length src is UB-adjacent (P3)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/compositor/surface_readback.rs:143`

`swizzle_row_to_rgba` accepts `src: &[u8]` and asserts
`src.len() == width * 4` only under `debug_assertions`. In release
the loop uses `src.chunks_exact(4)` which truncates extras
silently — a hostile or buggy client could pass a row whose stride
includes padding bytes and have those bytes silently swallowed.

The defensive check in `buffer_to_rgba` ensures
`stride >= width * 4` (line 197) so the actual call site can't
pass a too-short row. P3 because the defence-in-depth at the
caller is correct.

### D-6 — `apply_grants` re-walks `/dev` (or `XDG_RUNTIME_DIR`) every launch (P3)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/sandbox/profile.rs:108-147`

Every call to `apply_grants` with a `Capability::Camera` re-runs
`std::fs::read_dir("/dev")` and re-adds video device binds. With
multiple grants per launch (or many launches in a session) this is
O(grants * /dev-entries) IO. Inside `apply_grants` the grants
loop calls `grant_camera` once per `Capability::Camera` grant — so
if a workspace's grant list redundantly contains the same Camera
capability twice, the binds get duplicated.

The dedup contract in `grant_filesystem_path` (line 213-216) is
correct (idempotent). The same path-dedup should apply to
`grant_camera` / `grant_microphone`. Today, two camera grants
produce two `--bind /dev/video0 /dev/video0` triples in argv —
bwrap accepts both, but the duplication is wasteful.

Suggested fix: check for duplicate `(host, sandbox)` pairs before
pushing in `grant_camera` / `grant_microphone`.

### D-7 — `shared_surfaces: HashMap<u64, u64>` value type is "the peer this is shared with" — but `Workspace` has many peers (P2)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/state.rs:392-404`

The field is documented as "one entry per (surface, peer) pair" but
the type is `HashMap<surface_id, peer_id>` — a single u64 value.
If a surface is shared with two peers simultaneously, the second
`share_surface(s, peer_b)` call overwrites the first
`share_surface(s, peer_a)` — peer A silently loses access.

The code comment at line 395-399 acknowledges this with "multi-peer
per surface is a v3 follow-up", but the function signature accepts
the call as if it succeeded. Callers that try to set up
multi-recipient streaming will have their shares clobbered without
warning.

Suggested fix: in the v2 codepath, `share_surface` should return
`Result<(), AlreadySharedWith>` when an entry exists for the same
surface_id. The current `pub fn share_surface(...)` returning unit
is a deception.

Suggested test:
`tests::share_surface_to_second_peer_returns_error_in_v2` — share
to peer A, share to peer B, assert the second call surfaces an
error rather than silently overwriting.

### D-8 — `WorkspaceTreeReplica::request_sync` `unwrap`s a `serde_json::to_vec` (P3)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/network/replica.rs:54-61`

```rust
let since = serde_json::to_vec(&heads).unwrap_or_default();
```

Wait — this is `unwrap_or_default()`, not `unwrap()`. False alarm.
But on closer read, `heads` is `Vec<ChangeHash>`. If serialisation
ever fails (it shouldn't for a finite enum) the function pushes a
`SyncReq { since: vec![] }` which the recipient interprets as
"send me every change" — that's a correctness corner: a corrupt
serialisation silently turns into a full-sync request, wasting
bandwidth but not breaking anything. P3.

### D-9 — `next_id` static counter starts at 1 even when loading persisted state with higher IDs (P2)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/state.rs:73-77`

The atomic `N` starts at 1 every fresh process. The persisted state
may contain `WorkspaceId(7)`. After a restart, `WorkspaceId::new()`
returns `WorkspaceId(1)` — colliding with whatever existed in slot 1
of the cache. Same family as C-1, but trips even without peer-sync.

The repro is harder because `bootstrap_default` exclusively uses
`WorkspaceId::new()`, so a fresh tree never has gaps in ID space.
But if the persisted state ever ends up with a non-contiguous ID
range (e.g. a deleted workspace), the next allocation collides.

Suggested fix: at load time, walk the loaded tree and seed `N` to
`max(existing_ids) + 1`. Add a `WorkspaceTree::initialise_id_counter`
helper called from both `bootstrap_default` and `load_or_default`.

### D-10 — `pump_test_control_conn` reads with no max-size guard (P3)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/compositor/native.rs:134-173`

`BufReader::read_line` reads until `\n` with no maximum. A
malicious client connecting to the test-control socket can flood
the compositor with a single never-ending line, eventually OOMing.

The test-control socket is opt-in (env-var gated) and lives at a
caller-chosen path. So the practical attack surface is "a CI
process tests against the production binary with the env var set
and a peer on the host abuses the socket". P3 because both the
opt-in and the path are under the operator's control.

Suggested fix: use `BufReader::take(64 * 1024)` and treat
oversized lines as a protocol error.

---

## Section E — Out-of-scope

These are bugs but the fix is a design change, not a localised
code edit. They warrant a separate plan / RFC:

- **C-3 (the two-tree split)** is technically a P0 *code* bug, but
  the right fix is an architectural decision: do `Chrome` and
  `ShellNetwork` and the portal grant store all share one
  `Arc<Mutex<WorkspaceTree>>`? Or do we instead build a write
  pipeline that bridges them? That's a design discussion.
- **C-1 + D-9 (next_id collision)** could be fixed locally by
  scoping the counter per-peer, but the real fix is using
  automerge's native ObjId for cross-peer identity. That's a
  data-model migration, not a single PR.

---

## Section F — Status appendix

| # | Severity | Title | Status |
|---|----------|-------|--------|
| C-1 | P0 | `next_id()` collides across peers | ❌ unfixed |
| C-2 | P0 | `persist::load_or_default` drops automerge doc | ❌ unfixed |
| C-3 | P0 | Two `Arc<Mutex<WorkspaceTree>>` clones break collab + portal | ❌ unfixed |
| C-4 | P1 | `test_control` ClickAt/Named returns Ok without executing | ❌ unfixed |
| C-5 | P1 | `SandboxProfile::seccomp = Some(_)` emits fd-10 without plumbing | ❌ unfixed |
| C-6 | P1 | `adopt_toplevel` silently drops surface when strips empty | ❌ unfixed |
| C-7 | P2 | `toplevel_destroyed` leaks SurfaceTracker slot | ❌ unfixed |
| C-8 | P1 | Peer keypair written world-readable (`0644`) | ❌ unfixed |
| C-9 | P2 | `replica.handle` swallows `apply_changes` error | ❌ unfixed |
| C-10 | P2 | BridgeSocket accept-loop has no shutdown path | ❌ unfixed |
| D-1 | P1 | `apply_remote_input` returns `true` without injecting seat | ❌ unfixed |
| D-2 | P3 | `node_map.clone()` per relayout | ❌ unfixed |
| D-3 | P3 | test_control socket file not unlinked on exit | ❌ unfixed |
| D-4 | P3 | BridgeSocket accept thread detach on drop | ❌ unfixed |
| D-5 | P3 | `swizzle_row` debug_assert only in debug | ❌ unfixed |
| D-6 | P3 | `apply_grants` no dedup for camera/mic binds | ❌ unfixed |
| D-7 | P2 | `shared_surfaces` is single-peer-per-surface, silently overwrites | ❌ unfixed |
| D-8 | P3 | `request_sync` falls back to "full sync" on serialise failure | ❌ unfixed |
| D-9 | P2 | `next_id` counter starts at 1 ignoring persisted state | ❌ unfixed |
| D-10 | P3 | `pump_test_control_conn` no max line size | ❌ unfixed |

Totals: **P0: 3 / P1: 4 / P2: 5 / P3: 7 / out-of-scope: 1**.

---

## Section G — Suggested next steps

Wave 1 — collaboration-correctness P0s (parallel-safe):
- C-1 (`next_id` cross-peer collision) — affects all of peer-sync.
- C-2 (`persist::load_or_default` drops doc) — single-line fix; high impact.
- C-3 (split trees) — bigger change; spec out as RFC first.

Wave 2 — security P1s (parallel-safe):
- C-8 (`0600` keypair) — single-line fix; high impact.
- C-5 (seccomp fd-10) — needs `pre_exec` plumbing.
- C-4 (test_control click execution) — one-line interim; full Phase 2b later.

Wave 3 — leak P2s + smells:
- C-6 / C-7 (surface tracker + adoption lifecycle).
- C-9 (replica error handling).
- D-1 (apply_remote_input deceptive boolean).
- D-7 (shared_surfaces single-peer overwrite).
