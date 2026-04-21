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

### 2. Tab-drag to new window unreliable — fix in flight

Root causes documented in `group_row.rs` comments after subagent ae0ea2b…
  lands:
- `gap_index_for` y-band check rejects releases outside the row's exact
  rect.
- `split_tab_into_new_group` refusal rule blocks every single-tab source.
- No inter-group drop-zone indicator exists; only intra-bar reorder zones.

## Redesign — left wheel as in-plane disc

User spec (verbatim): *"I want a rounded, wheel like thingy on the left,
placed on the same plane as actual windows (a strip should be an axis
going out from that middle -> basically a big circle). You can then drag
this wheel in the middle to move it to another strip of windows."*

### Interpretation

- The wheel is a **circular disc** anchored mid-left, vertically centred,
  with a diameter large enough to read spoke labels — start with 240 px.
- The disc lives **on the same z-plane as windows**, not as a floating
  overlay. It occupies its own horizontal slot inside the workspace row
  (i.e. replace the floating `SidebarWheelChrome` with a disc widget that
  sits to the left of the GroupRow, not above it).
- Each **strip** of the current workspace appears as a **spoke/ray**
  extending radially from the disc's centre. A strip is "axial" in the
  sense that its row of window groups is conceptually attached to that
  ray; in the 2D render we paint each strip label at its ray's tip.
- **Interaction:** press-and-drag anywhere on the disc's body rotates the
  spoke set. Releasing snaps to the nearest spoke (that strip becomes
  active). `|delta y| > threshold` switches strips. Current (`:732-781`)
  drag-to-rotate already implements a vertical-drag→angle mapping; reuse
  it.
- **Plane-level**: the disc does NOT absorb clicks meant for the topbar or
  for windows. Its bounding box is the disc itself, not a 96-px strip.

### Status — 2026-04-21

- **Widget built** (`quick-shell/src/shell/sidebar_wheel_disc.rs`, commit
  `2a7ec2b`). 240-px disc, spokes-as-strips layout, drag-rotate with
  snap-to-nearest-spoke. Snapshot test + 4 interaction tests pass.
- **Not wired** into `build_nirios_root_with_chrome` yet —
  `SidebarWheelChrome` still renders in the live shell. Next iteration:
  add a feature flag / env var and swap construction paths.

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

### 3. Terminal + file-explorer not live as shell tabs

`apps/terminal` (`quick-terminal`) and `apps/file-explorer` (`file-explorer`
crate) are workspace members but the Notes / Files / Terminal / Browser
tabs in the live shell still render stub `EditorApp`s. The
`AppPlaceholder` → real widget path needs wiring in
`shell/mod.rs::to_ui_tile`.

### 4. Real-socket collab E2E

`quick-ui/src/collaboration.rs` supports a Unix socket, but all current
tests go through in-process `MemoryRelay`. No two-binary test exists that
confirms CRDT sync across processes.

### 5. Presence over real socket

`RemoteCursorOverlay` renders `SimulatedFeed` — a demo peer with a fake
cursor. Real peer presence never lights up a cursor today.

## Visual gaps (pointer)

See `docs/nirios/visual-gaps.md` for the full list. Blockers #1, #3
(fallback), #6 and #5 (shield only) are closed as of this iteration.
Remaining: full lucide icon pass, real Gaussian blur on overlays
(vello upgrade), tab-drop DropZone animation timing.

## Process reminder

Per user's explicit direction (session 2026-04-21):
- Don't implement big changes yourself — dispatch subagents in worktrees
- Give subagents concrete instructions including distrobox test commands
- Keep changes scoped; large features get their own iteration
