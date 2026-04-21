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

### 4. Real-socket collab E2E

`quick-ui/src/collaboration.rs` supports a Unix socket, but all current
tests go through in-process `MemoryRelay`. No two-binary test exists that
confirms CRDT sync across processes.

### 5. Presence over real socket

`RemoteCursorOverlay` renders `SimulatedFeed` — a demo peer with a fake
cursor. Real peer presence never lights up a cursor today.

## Visual gaps (pointer)

See `docs/nirios/visual-gaps.md` for the full list.

**Closed** as of 2026-04-21 iteration:
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
- Earlier: shield/backdrop work

**Remaining visual gap:** #3 backdrop blur — needs Vello blur primitive
or translucent-tint fallback; does not generalise from the #1 sprite
trick (blur must track content underneath).

**In flight** (parallel subagents dispatched):
- Real-socket two-peer collab E2E (closes §3 #4 below)

**Remaining highest-priority:**
- #1 content card shadow — Blocker, needs Vello blur or pre-rendered
  radial-gradient sprites (M effort).
- #3 backdrop blur on overlays/topbar — High, needs Vello blur layer
  or translucent fallback.
- #5 full lucide icon pass — High, L one-time cost.
- #7 text weight pass — Medium, S effort (cross-cutting; needs
  `TextRenderer::draw_text` to grow a weight param).

## Process reminder

Per user's explicit direction (session 2026-04-21):
- Don't implement big changes yourself — dispatch subagents in worktrees
- Give subagents concrete instructions including distrobox test commands
- Keep changes scoped; large features get their own iteration
