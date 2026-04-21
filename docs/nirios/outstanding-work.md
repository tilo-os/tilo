# Nirios Shell — Outstanding Work

Snapshot as of 2026-04-21 ralph iteration 1. Items below are not tracked in
`docs/nirios/visual-gaps.md` (which is purely visual polish) — these are
functional gaps, architectural misses, and redesign asks.

## Functional — shell wiring

### 1. Cmd+K / Cmd+Tab routing — resolved (2026-04-21)

Earlier this doc claimed the hotkeys were dead because `main.rs` never
builds a `Shell`. That was incorrect: the short-term fix was *already*
implemented in `ShellOverlayHost::handle_event`
(`quick-shell/src/shell/overlay_host.rs:951-1040`, added in commit
`e5c134d`). That pre-pass sees every `WindowEvent::KeyboardInput` because
`quick_ui::App::run` routes them through `root_widget.handle_event`
(`quick-ui/src/app.rs:575`).

Integration tests pinning all three shortcuts (Ctrl+K / Ctrl+Tab /
Escape) now live in `quick-shell/tests/e2e_shell_flows.rs` so a future
regression is caught.

The long-term pre-event-hook idea (`App::add_pre_event_filter`) is not
needed today — revisit only if the shell grows hotkeys that must fire
with zero widget-tree walking (e.g. global screen-record toggle).

### 2. Tab-drag to new window unreliable — resolved (2026-04-21)

All three root causes are addressed:
- ~~`gap_index_for` y-band check~~ — removed; release point comes from
  the in-row `TabBar` so a drop anywhere in screen space that doesn't
  land on a group is now correctly classified as a new-window gap
  (`quick-ui/src/widget/group_row.rs:107-113`).
- ~~`split_tab_into_new_group` refusal for single-tab sources~~ — removed;
  the refuse rule was costing every drag-out of a sole tab
  (`group_row.rs:383-392`).
- ~~Missing inter-group drop-zone indicator~~ — commit `f611af6`. While
  any child `TabBar` reports `drag_in_flight() == true`, `GroupRow`
  tracks the hover-gap index via `gap_index_for` on `CursorMoved` and
  paints a translucent indigo capsule (2 px radius, 4 px wide, full
  row height, `accent[5] @ 0.5`) between the two neighbouring groups.
  The indicator clears on release (so nothing lingers after a drop)
  and never paints without an active drag. Regression tests:
  `hover_gap_tracks_cursor_during_drag`, `hover_gap_clears_on_release`,
  `hover_gap_is_none_when_no_drag`.

## Redesign — left wheel as in-plane disc

User spec (verbatim, 2026-04-21): *"I want a rounded, wheel like thingy
on the left, placed on the same plane as actual windows (a strip should
be an axis going out from that middle -> basically a big circle). You
can then drag this wheel in the middle to move it to another strip of
windows."*

**Clarified later (2026-04-21):** *"No permanent sidebar, but workspaces
appear as rotated axes of the wheel disc, meaning you can go to the left
and drag it to move between workspaces."*

So: spokes = **workspaces** (not strips). The disc **replaces**
`SidebarWheelChrome` outright — no env-var gate, no fallback.

### Interpretation

- The wheel is a **circular disc** anchored mid-left, vertically centred,
  with a diameter large enough to read spoke labels — start with 240 px.
- The disc lives **on the same z-plane as windows**, not as a floating
  overlay. It occupies its own horizontal slot inside the workspace row
  (i.e. replace the floating `SidebarWheelChrome` with a disc widget that
  sits to the left of the GroupRow, not above it).
- Each **workspace** appears as a **spoke/ray** extending radially from
  the disc's centre. Each spoke's tip is labelled with the workspace
  name. Rotating the disc snaps the active workspace.
- **Interaction:** press-and-drag anywhere on the disc's body rotates the
  spoke set. Releasing snaps to the nearest spoke (that workspace becomes
  active). `|delta y| > threshold` switches workspaces. Current
  (`:732-781`) drag-to-rotate already implements a vertical-drag→angle
  mapping; reuse it.
- **Plane-level**: the disc does NOT absorb clicks meant for the topbar or
  for windows. Its bounding box is the disc itself, not a 96-px strip.

### Status — 2026-04-21

- **Widget built and wired** as the primary sidebar. Commits:
  `2a7ec2b` (widget scaffold) → `2337b21` (rename spokes to workspaces +
  mount in `build_nirios_overlay_host_with_chrome`). Superproject
  pointers: `3e5728c`, `c835ba0`.
- 240-px disc, spokes = **workspaces** (one spoke per `tree.workspaces`
  entry), drag-rotate with snap-to-nearest-spoke. Active workspace
  determines `active_idx`.
- `SidebarWheelChrome::from_tree(...)` no longer mounts in the live
  shell. The module file still exists for reference; a follow-up can
  delete it once the disc is proven.
- **CRDT write-back — resolved (2026-04-21).** Commit `65d105d` added
  `SidebarWheelDisc::with_chrome(Rc<RefCell<ChromeState>>)`; on snap
  the widget calls `chrome.borrow_mut().tree.set_active(ws_id)` so the
  rest of the shell picks up the new active workspace on the next
  frame. Same commit fixed label readability — labels whose spoke
  angle lands in (90°, 270°) get an extra π rotation so glyphs stay
  upright.

### Proposed structure

New file: `quick-shell/src/shell/sidebar_wheel_disc.rs`.

- `SidebarWheelDisc { strips: Vec<SpokeLabel>, active_idx, rotation_deg,
  drag_from: Option<Point>, rect, radius_px }`.
- `build_layout`: leaf node with fixed `width = 240 px`, `flex_grow: 0`,
  `align_self: Center`. It sits between the left padding and the GroupRow
  inside the content Column (i.e. workspace row becomes: `[wheel][strips]`
  in a Row, or wheel stacked above strips via Column — TBD during impl).
- `draw`: three layers —
  1. Disc outline: rounded-stroke circle `#334155` at 1 px, centred.
  2. Active-spoke indigo glow: a soft accent arc at the current spoke's
     angle, like the existing `SidebarWheelChrome` rim arc but centred.
  3. Spoke labels: for each strip, one label at angle `offset * spoke_gap`
     where `spoke_gap = 360 / strip_count.max(3)`. Labels use the
     concept's `text-white/40` vertical-rl style (see
     `concept-features.md:399`).
- `handle_event`: press anywhere inside the disc → capture `drag_from`.
  Motion while pressed → update `rotation_deg` by `(dy / radius_px) *
  RADIANS_PER_PIXEL * 180/π`. Release → snap to nearest spoke (compute
  nearest multiple of `spoke_gap`, commit `active_idx`, reset rotation).

### Migration plan

1. Build the disc widget alongside the existing `SidebarWheelChrome` (do
   not delete it — keep as fallback until the disc is verified).
2. In `build_nirios_root_with_chrome`, swap the sidebar construction
   behind a compile-time feature flag `wheel-disc` (or runtime env var
   `NIRIOS_WHEEL=disc`). This keeps the existing Stack-based layout
   working while we validate.
3. Once visual + interaction parity is verified, flip the default and
   remove `SidebarWheelChrome`.

**Estimated effort:** L — 300-500 lines of new widget code plus extensive
tests for rotation math + spoke hit-testing.

## Integration / apps

### 3. Terminal + file-explorer live tabs — resolved (2026-04-21)

Commit `8a954dc` extended `to_ui_tile` to route `ShellKind::Terminal`
→ `TerminalWidget::spawn_default()` (fallback to stub on PTY spawn
failure) and `ShellKind::FileExplorer` → `FileExplorerWidget::new()`.
New integration test `persisted_app_placeholders.rs` asserts both
routes resolve to `UiTile::App` (or `Stub` for Terminal under
sandboxed spawn).

Known follow-up: Browser (`ShellKind::Browser`) still falls through to
the stub widget since `apps/browser` depends on `libservo` from a git
source that doesn't publish the crate (CI can't resolve it offline).
Not a blocker for the current iteration.

### 4. Real-socket collab E2E — resolved (2026-04-21)

Commit `066e112` adds `quick-ui/tests/real_socket_two_peer.rs`: a
test-only Unix-socket broker accepts two `CollaborationManager`
connections, fans each peer's `CollabRequest` to the opposite side as
the matching `CollabEvent`, and asserts `SyncIncremental` ↔
`CRDTIncremental` plus `Presence` ↔ `RemotePresenceUpdate` round-trips.
Runs in `cargo test -p quick-ui --test real_socket_two_peer` in 0.20s.

Previously all collab tests went through in-process `MemoryRelay`; the
real wire protocol (4-byte LE length prefix + JSON body) was unexercised
until this commit.

### 5. Presence over real socket — resolved (2026-04-21)

Commits `44e7053` + `ab62d41` close the loop: `PresenceCtx` gained
`upsert_remote_cursor(user_id, position, display_name)` which resolves
colours via `color_for(&UserId)` (pinned palette + FNV→HSL fallback),
and all three `CollabEvent::RemoteCursorMoved` handlers in
`quick-ui/src/app.rs` (`poll_and_apply_collab_events`,
`apply_collab_event_for_test`, `Event::AboutToWait`) now feed it. The
`CollabEvent::Context` seed also upserts once per collaborator so
display names are attached before the first cursor move. Tests:
`remote_cursor_presence::{remote_cursor_move_populates_presence_ctx,
remote_cursor_move_updates_existing_entry_in_place}`.

**Follow-up (not blocking):** the legacy `State.collab.cursors`
`HashMap` is still written + read by `App::run`'s in-app cursor-arrow
paint; it can be deleted once `RemoteCursorOverlay` is the single
source of truth for every consumer.

## Visual gaps (pointer)

See `docs/nirios/visual-gaps.md` for the full list.

**Closed** as of 2026-04-21 iteration:
- Golden-image snapshot diff harness with `UPDATE_SNAPSHOTS=1` refresh
  hook — submodule commits `8c3d619` + `6a9c677`, superproject bump
  `5f0df57`. Pins `e2e_open_close_overview` and
  `sidebar_wheel_disc_renders_a_spoke_per_workspace` against committed
  PNG goldens at `quick-shell/tests/snapshots/`.
- Drag-ghost translucent preview on tab drag (source dims to 0.35, ghost
  paints at 0.6 at `last_cursor - drag_offset`) — commit `4e579ac`
- Lucide X/Maximize2/Minimize2/Search on tab decorations + maximized
  controls — commit `62c5c53`
- Real peer presence populating `PresenceCtx::remote_cursors` (shell
  overlay now sees real peers) — commits `44e7053` + `ab62d41`
- Real-socket two-peer collab E2E (`CollabEvent::CRDTIncremental` +
  `RemotePresenceUpdate` fanout over Unix socket) — commit `066e112`
- #1 content card drop shadow (radial sprite) — commit `5837eee`
- #2 tab contrast — commit `4d6df4c`
- #4 overview active-row halo (4-ring fade) — commit `4b12846`
- #5 lucide icon module + chrome swap — commits `e2fad05` + `59be5eb`
- #6 sidebar card fill — superseded by wheel disc
- #7 TextWeight plumbing + chrome adoption — commits `7dd9fa1` + `9a018e3`
- #8 topbar clock active-dot contrast — commit `3b786c2`
- #9 per-peer collaborator avatar colors — commit `730e6c4`
- #10 desktop noise overlay — commit `722ad07`
- #11 permissions shield (closed via gap #5) — commit `59be5eb`
- #12 overview header typography (≥32 px bold) — commit `687e7ee`
- #13 overview scrim slate-950 — commit `687e7ee`
- Wheel-disc Plus button (closes wheel-disc-design known-gap #4) — commit `dc8956a`
- Wheel-disc per-spoke presence dots (closes wheel-disc-design known-gap #3) — commit `4ffb786`
- #3 backdrop-blur translucent-tint fallback (topbar 0.80 / popovers 0.80 /
  palette+modal 0.70 / scrim 0.20) — submodule commit `faf4e7c`
- Inter-group tab drop-zone indicator (closes §1 #2 third subitem) — commit `f611af6`
- Overview header Bold weight + clock/ws-name tier pins — submodule `255e9bb`
- TileHost spawn fade+scale-in animation (180 ms ease-out-cubic, α 0→1,
  scale 0.95→1.0) — widget `9fa86fd`, caller wire-up in
  `WindowGroup::insert_detached_tab` (covers both `move_tab_between_groups`
  and the drop-release path) — commit `f290b89`. `split_tab_into_new_group`
  deliberately unchanged (group-level fade already plays, double-fade is
  wrong). Cold-boot `WindowGroup::new` also unchanged (launch would blink).
- Tab-close fade-out animation (180 ms, α 1→0, scale 1→0.95, y 0→-20
  matching concept's `exit={{ opacity: 0, scale: 0.95, y: -20 }}`) with
  deferred host removal, neighbour-active-shift, and idempotent mid-fade
  re-click — commit `6ad564a`.
- `State::undo`/`redo`/`undo_for_author` now preserve current focus +
  widget metadata on the opposite-stack push; Ctrl+Shift+Z round-trip
  keeps the caret where it was pre-gesture. Caller in `App::run` uses
  `root_widget.get_state()` (non-mutating) + `focusable_order` index
  lookup — commit `b6aa0d0` (submodule `df4f653`).
- Group-close fade-out mirror: when `WindowGroup` loses its last tab
  (sweep drains `hosts` + `closing_hosts`), `start_closing()` latches a
  180 ms α 1→0 + scale 1→0.95 fade. `GroupRow` runs a per-tick prune
  sweep that drops groups whose `close_complete()` returns true and
  flags `structure_dirty`. Fills the tab-close-empties-group prune gap
  that previously left empty shells in place — commit `3c733e6`.
  Maximized-close bypass + drop-onto-closing-group edge cases resolved
  in `0c9a55d` (maximized-close routes through same deferred flow;
  cross-group drop onto `is_closing()` target falls through to new-group
  creation).
- Close-button red-500/80 hover tint + full-white glyph promotion on
  both per-tab close pill and maximized floating close chip — matches
  concept's `hover:bg-red-500/80` + `hover:text-white` (WindowFrame.tsx:
  249 + :334). Uses existing `theme.tokens.colors.semantic.danger`
  token with `with_alpha_factor(0.8)` — no hard-coded literals —
  commit `ee7f27f`.
- Earlier: shield/backdrop work

**Remaining visual gap:** none open — all visual-gaps.md items are either
shipped or parked pending a Vello blur primitive (#3 alpha-tint half already
shipped; Gaussian convolution deferred).

**In flight** (parallel subagents dispatched):
- (none — waiting on next dispatch decision)

**Known follow-ups from group-close agent (`3c733e6`)**:
- ~~Maximized-close bypass~~ — resolved in `0c9a55d`. Maximized-close
  now routes through `bar.start_closing` + `closing_hosts` insert so
  the 180 ms tab fade + group-close fade play the same as a normal
  close.
- ~~Tab drop-back onto closing group~~ — resolved in `0c9a55d`.
  `GroupRow` drop sweep filters out `is_closing()` siblings and
  routes the drop through `split_tab_into_new_group` at the closing
  group's slot (tab lands where the user aimed, closing group shifts
  right).
- Workspace active-group pointer may still target a closing group for
  ~180 ms; should nudge to surviving sibling at `start_closing()`.
  Cross-submodule fix (shell-layer workspace pointer), deferred.
- `GroupRow::transfer_tab` public API does not check `is_closing()`
  on the destination. No production callers today; harden only if a
  programmatic caller ever lands.

**Remaining highest-priority (structural / functional):**
- Tab/window decoration polish — lucide X/Maximize2/Minimize2/Search
  shipped in commit `62c5c53`; drag-ghost preview shipped in commit
  `4e579ac`; tab-close fade-out shipped in `6ad564a`; tile-spawn fade
  wired in `f290b89`; group-close fade-out shipped in `3c733e6`; red
  close-hover tint shipped in `ee7f27f`. Still pending:
  window-minimize animation + the remaining cross-submodule follow-up
  (workspace active-group pointer nudge).
- Concept-matched animation pass (overview zoom, tile spawn/despawn,
  wheel snap spring — spring already partial via `7916273`).
- CRDT-driven undo/redo coverage across every widget.
  - **Text input (editor typing): working** — 1 entry per char, verified
    in `e2e_undo_redo_round_trip_restores_golden` + the redo golden
    (submodule `02e7105`, superproject `a9e2129`).
  - **Window-management gestures (close / reorder / maximize): not
    persisted in the CRDT at all.** `TabBar::sync` is empty,
    `WindowGroup::handle_event` uses `Vec::remove` + `bar.remove_tab`
    with no `state.doc` mutation, so `commit_action()` finds no ops and
    `undo_stack` stays empty. Fixing this is a structural change —
    either promote tab structure into the CRDT (preferred; aligns with
    "collaborative by default") or extend the undo system to snapshot
    widget state. Neither is trivial.
  - ~~**`State::undo` drops `widget_metadata` on redo_stack push**~~ —
    resolved in commit `b6aa0d0`. `State::undo`/`redo`/`undo_for_author`
    now accept `current_focused_idx` + `current_metadata` and push them
    onto the opposite stack. `App::run` keyboard handler pulls them
    from `root_widget.get_state()` + focusable_order.
    `e2e_redo_after_undo_rehydrates_golden` still sends `NamedKey::End`
    — but for a different reason: the harness drives `State::redo`
    directly and does NOT call `apply_undo_entry`, so the metadata is
    present on the returned entry but never applied in that codepath.
    Not a plumbing gap — a test-driver limitation.
- Full automated E2E navigation tests w/ screenshot diffing for the
  golden-path flows (open overview → switch workspace → drag tab to new
  window → collab join → undo). **Harness in place** (commits `8c3d619`
  + `6a9c677` + `5f0df57`): `SnapshotAssertion` with `UPDATE_SNAPSHOTS=1`
  refresh, golden PNGs committed under `quick-shell/tests/snapshots/`.
  Pinned so far: overview settled, wheel-disc three-spokes, tab-drag
  reorder settled, maximized-tile floating controls (submodule
  `7b47b42`, superproject `3be9db2`), undo-restored + redo-rehydrated
  (submodule `02e7105`, superproject `a9e2129`). Remaining:
  quick-settings (needs clock mock or text-mask region), collab join.

## Process reminder

Per user's explicit direction (session 2026-04-21):
- Don't implement big changes yourself — dispatch subagents in worktrees
- Give subagents concrete instructions including distrobox test commands
- Keep changes scoped; large features get their own iteration
