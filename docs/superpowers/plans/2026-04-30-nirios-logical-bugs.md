# 2026-04-30 — nirios shell: logical-bug audit + 3-wave repair plan

Triggered by a live-binary session that surfaced 7 bugs in 10 minutes
(see report header). All 7 share a *family*: a state mutation that
isn't fully propagated, a cache that goes stale, an event that doesn't
absorb, or a draggable that has no drag-loop. This plan classifies
those 7, walks the same patterns through every other widget that
plausibly carries the same defect, and lands on a 3-wave fix tree.

Repository state at audit time:
- Worktree: `.claude/worktrees/agent-aa998aeb6b4d48d80`
- Branch: `nirios-shell`
- Quick-shell HEAD: `54de1df` ("fix(workspace-settings): grow modal …").
- Quick-ui HEAD: `3e01242` (the cross-group-drop fix — anchor bug #1+#2
  already applied here).

All findings cite `<absolute-path>:<line>`. Read-only audit; no source
edits in this commit.

---

## Section A — Bug taxonomy

Six families, with each anchor bug placed under the family that
explains it. Same family-tags reused in §B for the new bugs.

### Family P-Sync — Parallel-state desync

Two or more vectors / maps that must change together; one mutation
forgets the sibling.

- **#2 (anchor) — Drag-reorder doesn't visibly reorder.**
  `WindowGroup::handle_event` permutes `hosts` after a `take_reorder`
  but used to skip `bar.reorder(from, to)`. Already fixed
  (`window_group.rs:1029-1036`, commit `3e01242`). The pattern is the
  family signature: any vec named in lockstep with another (`hosts` ↔
  `bar.tabs`, `closing_hosts` ↔ `closing_tabs`, `node_ids` ↔ `groups`,
  `cards` ↔ `tree.workspaces`, `collaborator_initials` ↔
  `collaborator_colors`) must be mutated through a single helper or
  every site reviewed.

### Family Cache-Stale — Stale geometry/index after structure mutation

A cached `Vec<Rect>` / `HashMap<id, Rect>` populated in
`apply_layout` is read in `draw` or `handle_event` after `tabs` /
`hosts` / `groups` shrunk between passes.

- **#1 (anchor) — Cross-group drop crash.**
  `tabs.rs:1051` (now `tabs.rs:1015-1017` post-fix) — `GroupRow`
  spliced a tab into a freshly-spawned group whose `tab_rects` were
  empty; `TabBar::draw` then indexed past the end. Defensive guard
  applied. The same defensive contract is *not* applied to
  `close_rects`/`max_rects`/`search_rects`, nor to `node_ids` on
  `GroupRow` (see §B).

### Family Drag-Loop — Pointer-capture / drag-loop missing

Widget paints draggable chrome but its `handle_event` doesn't
implement the full `Pressed → CursorMoved → Released` triplet, or
its `wants_pointer_capture` hook is wrong.

- **#4 (anchor) — QuickSettings sliders don't drag.**
  `overlay_host.rs:1442-1457` — the QS slider hit-test only fires on
  `MouseInput { state: Pressed }` and snaps the value to the click x.
  No `is_dragging` flag, no `MouseMotion` follow-up, no `Released`.
  The user can click-set but not drag.

### Family Modal-Leak — Click-through / event leak through overlays

An overlay paints over content but doesn't *absorb* every pointer
event in its rect; the underlying widget still hovers/clicks.

- **#7 (anchor) — GlobalSearch palette has click-through.**
  `overlay_host.rs:1149-1660` — only `MouseInput { state: Pressed }`
  events get an "outside-rect close" + "inside-rect swallow" pass.
  `MouseInput { state: Released }` and `CursorMoved` *both* fall
  through to `self.content.handle_event` (line 1649), so a hover or
  release lands on the content beneath. Same defect for QS, WS-
  settings, collaborators dropdown.

### Family Modal-Cosmetic — Open-edge / alpha polish on overlays

An overlay opens but its first-frame state is wrong (no auto-focus,
wrong alpha, etc.). The widgets work, the polish doesn't.

- **#5 (anchor) — Palette transparency too strong.**
  `overlay_host.rs:194-205` — `MODAL_SURFACE_ALPHA = 0.70`,
  `MODAL_SCRIM_ALPHA = 0.20`. Concept §4.1 specifies
  `bg-slate-900/70` *plus* `bg-black/30 backdrop-blur-[1px]`; without
  the blur, the surface alpha alone leaves the desktop under it
  legible. Visible in `/tmp/audit-shots/palette.png` — Notes app text
  reads through the palette card.
- **#6 (anchor) — Palette doesn't focus on open.**
  `overlay_host.rs:466-468` — `toggle_palette` flips
  `palette_visible` but never sets `*focus_target` for the palette's
  input or calls anything to focus the input field. Coupled with the
  fact that `CommandPalette::handle_event` (line 859) early-returns
  unless `is_focused`, AND has no character-handling path at all
  (only Arrow/Enter/Esc — see §B B-3), so even if the user *did*
  click in, typing wouldn't update the query.

### Family Layout-Overlap — Visual rect overlap / draw-order issue

Two widgets paint into overlapping rects with no z-clear; or a hit-
rect doesn't match the paint rect; or the cluster layout pushes
elements past their cluster bounds.

- **#3 (anchor) — Top-right contains weird visual artifacts.**
  See `/tmp/audit-shots/desktop.png` zoomed: a single bright pink "A"
  avatar abuts the dark tray pill, with no visible separation. The
  bug isn't the avatar position (math at `topbar_chrome.rs:906-1012`
  checks out: ~10 px gap to the tray pill). It's that the lucide
  Battery icon at `i=2` paints at `dx = rx - 12, w = 14` with center
  at `rx - 12` and extent `[rx - 19, rx - 5]` — but the tray pill
  ends at `rx`. The 5 px gap between battery-right and pill-right is
  visually correct, but the icons ride flush against the pill edge
  while wifi at `i=0` has 5 px on the LEFT (pill starts at
  `rx - 52`, icon center `rx - 40`, extent `[rx - 47, rx - 33]`,
  pill-left to icon-left is also 5 px). The "weird artifact" the
  user perceived is the avatar's saturated `#ec4899` reading next to
  the dim slate tray pill with no separator gap — the avatar's right
  edge is 3 px away from the tray pill (line 906: `rx -= tray_w + s3`
  with `s3 = 12`). Family classification: visual overlap *of
  contrast*, not geometry.

---

## Section B — Newly-found bugs

Ten new bugs of the same families, each with file:line + family +
severity. Severity scale: **P0** = panic / data loss; **P1** =
visible breakage / unusable feature; **P2** = polish / correctness.

### B-1 — Palette query never updates from typing (Family Modal-Cosmetic + Drag-Loop)

`/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/command_palette.rs:847-894`

The palette's `handle_event` only handles `KeyboardInput` for
ArrowDown/ArrowUp/Enter/Escape. There is **no `Key::Character` or
`NamedKey::Backspace` handling**, so typing into the palette never
calls `self.set_query(...)`. Cross-checked: `overlay_host.rs:779`
calls `self.palette.update`, which only reflows the visible list
based on the *current* `self.query` — but `self.query` is only ever
written from `set_query`, which is never called from any input
handler. The palette is unfilterable. **P1.** Pairs with #6 (no
focus on open) — even if focus worked, typing wouldn't filter.

Fix sketch: in `CommandPalette::handle_event`, add character / space
/ backspace branches that mutate `self.query` and call
`self.rebuild()`, mirroring `overlay_host.rs:1264-1310` (the
workspace-name editor's character path).

### B-2 — `MouseInput::Released` and `CursorMoved` leak through every overlay (Family Modal-Leak)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/overlay_host.rs:1317-1518`

The "outside-click close + inside-rect swallow" gate runs only for
`MouseInput { state: Pressed }`. `Released` and `CursorMoved` events
both fall through to `self.content.handle_event` at line 1649,
unconditionally — even when the cursor is over the open palette /
QS / WS-settings / collaborators rect. Consequences:

- A hover over the palette's rect still lights up tab pills,
  Notes-app text affordances, and the wheel-disc rim under the
  modal.
- A `Released` matched with a press inside the palette can release
  on a tab close button below, closing a tab the user never
  intended to close.
- Drag gestures on the wheel disc continue receiving CursorMoved
  while the palette is open.

**P1.** Family parallel: same gate is missing for QS, WS-settings,
collaborators. The fix is to lift the "is the cursor over an open
overlay's rect?" predicate above the *event-type* check and absorb
*all* pointer events for that frame.

Fix sketch: extract a helper `cursor_over_open_overlay(p) -> bool`
that ORs the open-overlay rects, and at the top of `handle_event`
short-circuit any `MouseInput`/`MouseWheel`/`CursorMoved` whose
cached `last_cursor` (or extracted `position`) is contained.

### B-3 — Sidebar wheel disc captures pointer on hover (Family Drag-Loop)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/sidebar_wheel_disc.rs:540-557, 591-593`

`SidebarWheelDisc::wants_pointer_capture` returns
`self.drag_from.is_some()`. But `drag_from` is set inside
`CursorMoved` on line 549 whenever the cursor *hovers* the disc
without a press, as a "potential anchor for the next press". Result:
just hovering over the disc grabs pointer capture from siblings —
including the topbar's tray pill / avatar stack underneath the disc's
rim band when the screen is short. **P1.** Conflated semantics: the
field doubles as both "drag in flight" and "hover-anchor cache".

Fix sketch: split the field. Keep `drag_from: Option<Point>` for an
*active* drag (set on Pressed, cleared on Released). Move the hover-
anchor cache to a separate `hover_anchor: Option<Point>`. Update
`wants_pointer_capture` to check the active-drag field only.

### B-4 — Cross-group drop on `closing` sibling silently loses the dragged tab (Family Cache-Stale + P-Sync)

`/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/group_row.rs:445-466`

When the cross-group drop hit-test runs at line 449, sibling groups
with `is_closing()` are *filtered out*. The downstream branch (484)
catches the case where the release landed on a closing sibling's bar
rect and falls through to `split_tab_into_new_group`. But there's a
gap: if the sibling is closing AND the release point is over its
*content area* (below the bar), neither branch matches; the
`gap_index_for(release_point)` branch returns `None` (release is
"inside a group"); and the dragged tab is silently lost — the source
group already detached it via `take_tab_detach`. **P0** (data loss).

Fix sketch: after both target-resolution branches fail, if `detach`
was already drained, *re-insert* the descriptor at the source's tail
to undo the detach. Equivalent to "no valid drop target → cancel".
Add a regression test: closing group + drop into its content area =
tab still present in source.

### B-5 — `node_ids` not pruned with `groups` after group-close sweep (Family P-Sync)

`/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/group_row.rs:391-404`

`GroupRow::handle_event` removes pruned groups from `self.groups`
in-place (line 401) after `prune.sort_unstable_by(|a, b| b.cmp(a))`,
but does **not** mirror the removal on `self.node_ids`. The sibling
flag `self.structure_dirty = true` is set, which lets `App` rebuild
the taffy tree on the next tick — **but only after** the rest of
this `handle_event` finishes. Lines 421 and 461 read
`self.groups[…]` indices and the post-prune sweep at 419-509 still
runs. None of those code paths re-index into `node_ids` directly,
so the immediate panic risk is low — but a future maintainer adding
a `node_ids[i]` lookup to the post-prune sweep would be greeted by
an IndexOutOfBounds. **P2** today, **P0 latent**.

Fix sketch: zip the prune so `node_ids` and `groups` shrink
together, OR add a debug-assert that `node_ids.len() ==
groups.len()` at the top of `apply_layout`. The latter at minimum
documents the invariant.

### B-6 — `TabBar` press-time `tab_rects[idx]` not guarded (Family Cache-Stale)

`/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/tabs.rs:1433-1445`

On `MouseInput::Pressed`, line 1434 indexes `self.tab_rects[idx]`
where `idx = self.hover_idx`. `hover_idx` was populated by the
preceding `CursorMoved` against the previous frame's `tab_rects`.
If a `start_closing` / `remove_tab` shrunk `tabs` (and hence
`tab_rects`) between the move and the press — possible when a
neighbouring widget's `handle_event` mutates the tree — the index
panics. The defensive guard at the top of `draw` (1015-1017) covers
the draw path but not the event path. **P0 latent.** Same family as
anchor #1.

Fix sketch: replace `self.tab_rects[idx]` with `self.tab_rects.get(idx)`
+ early return; or check `idx < self.tab_rects.len()` before deref.

### B-7 — `WS-Settings` tab pills are visual-only; clicks don't switch tabs (Family Modal-Leak / WS-feature)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/overlay_host.rs:1481-1515, 2780-2803`

`draw_ws_settings` paints four pills: General / Roles / Files /
Collaborators. Pill rects are computed inside `draw` but **never
stored** in `self`, so `handle_event`'s click-inside-modal branch
(1495-1515) only routes the `name_rect` hit-test. Clicking on
"Roles" or "Files" does nothing — the user has to drive
`chrome.ws_settings_tab` via `set_tab` from elsewhere (which only
the test harness does today). Same Modal-Leak family as #7 — events
"leak" because the modal didn't publish the rects to absorb them.
**P1.**

Fix sketch: cache the four pill rects on `ShellOverlayHost` (parallel
to `tray_pill_rect` on topbar), populate them in `draw_ws_settings`,
hit-test them in the WS-settings click branch and call
`chrome.ws_settings_tab = WsTab::…; chrome.bump()`.

### B-8 — `closing_hosts` retains entries for tabs detached cross-group (Family P-Sync)

`/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/window_group.rs:252-264`

`detach_tab` removes from `self.hosts` and from `self.bar.tabs` (via
`bar.remove_tab`), but does **not** remove the tab id from
`self.closing_hosts` if it was mid-close. Practically, a user can't
drag a tab whose pill is fading out (the source pill is in
`closing_tabs`, not `tabs`). But the inverse path — a sibling group
detaches, the source group then closes the *original* tab on the
same id — leaves a stale `closing_hosts` entry whose timer never
expires (no `hosts` entry to remove on sweep). The expired-sweep at
line 968-974 will `closing_hosts.remove(&id)` and then `self.hosts
.iter().position(...)` returns `None`, so the host removal is a
no-op; the bar's `finalize_close(id)` then does nothing because the
pill never entered `closing_tabs`. Net: leak in `closing_hosts`
that resolves itself, but **#needs_animation_frame** keeps pumping
redraws while the entry exists. **P2.**

Fix sketch: in `detach_tab`, also `self.closing_hosts.remove(&tab_id)`
and `self.bar.finalize_close(tab_id)` to clear any in-flight fade
state for the moved tab.

### B-9 — Slider drag breaks the moment cursor leaves the slider rect (Family Drag-Loop)

`/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/slider.rs:136-172`

`Slider::handle_event` does *not* implement `wants_pointer_capture`
(default `false`). When `is_dragging == true` and the cursor moves
outside the slider's rect, the parent's pointer routing (see
`overlay_host.rs:1621-1648` for the sidebar; the App's main router
elsewhere) stops dispatching CursorMoved to this slider — the drag
"sticks" at the position where the cursor exited. Worse: the
press-to-drag gate at line 159 requires `self.is_hovered` (set in
the same CursorMoved frame), so a press on an already-hovered slider
still works, but a press at the same instant as a CursorMoved that
hasn't fired (winit doesn't guarantee CursorMoved precedes
MouseInput) misses the press. **P1.** This is a separate Slider
widget from the QS sliders (anchor #4), but symptoms compose —
neither slider variant is draggable past the rect.

Fix sketch: add `fn wants_pointer_capture(&self) -> bool { self.is_dragging }`.
In Pressed, hit-test against `self.rect.contains(p)` directly using
`self.last_cursor` (and add a `last_cursor` field) instead of relying
on `is_hovered`. Treat clicks on the track (anywhere in the rect)
as "value jumps + drag begins".

### B-10 — `parallel rosters`: `topbar_chrome.collaborator_*` and `sidebar_wheel_chrome.cards[*].collab_count` can disagree (Family P-Sync)

`/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/topbar_chrome.rs:213-232` vs
`/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/sidebar_wheel_chrome.rs:165-202`

Both compute "how many collaborators on the active workspace?" but
through *separate* fallback rules:

- Topbar: prefer ws.collaborators (filtering out `id == "me"`); else
  `tree.collaborators` (also filtering "me").
- Wheel-disc: prefer `w.collaborators.len()` (no filter); else
  `tree.collaborators.len()` *only for active workspace* (no
  filter).

If a workspace's `collaborators` includes "me", the topbar shows
N-1 avatars while the wheel card paints N dots. **P2** (cosmetic
mismatch, hard to spot at a glance).

Fix sketch: extract a single
`active_workspace_peer_count(tree) -> (count: usize, peers:
Vec<&Collaborator>)` helper in `state.rs` and call it from both
chromes; settle the "filter `me`?" question in one place.

---

## Section C — Fix waves

Three waves, each with N parallel-safe units. Units in the same wave
touch disjoint files / tests and can be dispatched as `/batch`.
Order across waves is **wave 1 → 2 → 3** — wave 2 depends on the
shared overlay-absorb helper introduced in 1-B, wave 3 depends on
the WS-settings rect cache introduced in 2-A.

### Wave 1 — P0 / data-loss bugs (parallel-safe)

#### Unit 1-A — `quick-ui::tabs` press-time `tab_rects` guard (B-6)

File: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/tabs.rs`

TDD steps:
1. Add a regression test in
   `widget/tabs.rs::tests::press_with_stale_hover_idx_does_not_panic`
   that seeds `hover_idx = Some(7)` then calls `handle_event` with a
   Pressed event when `tab_rects.len() == 0`.
2. Make the test pass: replace
   ```rust
   let origin = self.tab_rects[idx].center();
   ```
   with
   ```rust
   let Some(rect) = self.tab_rects.get(idx).copied() else {
       self.hover_idx = None;
       return;
   };
   let origin = rect.center();
   ```
   and apply the same `get` guard to the `pill = self.tab_rects[idx]`
   line below.

#### Unit 1-B — `quick-shell::overlay_host` overlay event-absorb helper (#7, B-2)

File: `/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/overlay_host.rs`

TDD steps:
1. Add `tests::overlay_absorbs_cursor_moved_when_palette_open`,
   `tests::overlay_absorbs_released_inside_qs`,
   `tests::overlay_absorbs_cursor_moved_inside_ws_settings`. Each
   asserts `*handled == true` after `handle_event` for a CursorMoved
   / Released event whose position is inside the relevant overlay
   rect, AND that `self.content.handle_event` (instrumented via a
   recording mock content) was *not* called.
2. Make the tests pass: extract
   ```rust
   fn cursor_over_open_overlay(&self, p: Point) -> bool {
       let f = self.overlay_flags();
       (self.palette_visible && self.palette.get_rect().contains(p))
           || (f.collaborators && self.collaborators_rect().contains(p))
           || (f.quick_settings && self.quick_settings_rect().contains(p))
           || (f.ws_settings && self.ws_settings_rect().contains(p))
   }
   ```
   and at the top of `handle_event` (after the CursorMoved cache
   update), if the event is a pointer event and
   `cursor_over_open_overlay(self.last_cursor.unwrap_or_default())`
   is true, set `*handled = true` and `return` for any pointer event
   that wasn't already handled by the existing branches.

#### Unit 1-C — `quick-ui::group_row` cross-group-drop tab-loss guard (B-4)

File: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/group_row.rs`

TDD steps:
1. Add `tests::cross_group_drop_into_closing_content_area_keeps_tab_in_source`:
   set up two groups, latch one's `closing_at`, simulate the
   detach + release inside the closing group's content area, assert
   the source group still contains the tab.
2. Make the test pass: in the post-detach loop (line 427-509), after
   both `target_idx`-bearing branches fail and the `gap_idx_for`
   call returns `None`, re-insert the detached pair into the source
   group via `self.groups[src_idx].insert_detached_tab(0, desc, host)`
   (or stash and reinsert via a new helper).

#### Unit 1-D — `quick-ui::window_group` `closing_hosts` cleanup on detach (B-8)

File: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/window_group.rs`

TDD steps:
1. Add `tests::detach_tab_clears_closing_state_for_moved_tab`: seed a
   group with one tab mid-close (`closing_hosts` contains its id),
   call `detach_tab`, assert `closing_hosts` no longer contains the
   id and `bar.is_closing(id) == false`.
2. Make the test pass: in `detach_tab` (line 252-264), before the
   final `Some((desc, host))` return, call
   `self.closing_hosts.remove(&tab_id);` and
   `self.bar.finalize_close(tab_id);`.

### Wave 2 — P1 / unusable-feature bugs (parallel-safe)

#### Unit 2-A — `quick-shell::overlay_host` palette focus-on-open + WS-settings tab clicks (#6, B-7)

File: `overlay_host.rs`

TDD steps:
1. Add `tests::toggle_palette_focuses_input` — call `toggle_palette()`,
   assert `self.palette.is_focused() == true`. Add
   `tests::ws_settings_pill_click_switches_tab` — synthesise a click
   at the Files-pill rect (use `ws_settings_tab_pill_rect(WsTab::Files)`,
   a new helper), assert `chrome.ws_settings_tab == WsTab::Files`.
2. Make the tests pass:
   - In `toggle_palette`, when transitioning false→true, call
     `self.palette.set_focus(true)` and clear the query.
   - Add `ws_settings_tab_pill_rect(tab) -> Rect` mirroring the
     geometry in `draw_ws_settings:2780-2803`. Add a click branch in
     the WS-settings inside-modal path (`overlay_host.rs:1495-1515`)
     that hit-tests the four pills and writes to
     `chrome.ws_settings_tab`.

#### Unit 2-B — `quick-ui::command_palette` typing handler (B-1)

File: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/command_palette.rs`

TDD steps:
1. Add `tests::typing_filters_visible_entries` — set `is_focused = true`,
   feed `Key::Character("h")`, assert
   `palette.visible_entries().len() < palette.entries.len()` and
   `palette.query() == "h"`.
2. Make the test pass: in `handle_event` (line 866), add
   - `Key::Named(NamedKey::Backspace)` → `self.query.pop();
     self.rebuild();`
   - `Key::Named(NamedKey::Space)` → push ' ', rebuild
   - `Key::Character(s)` → push non-control chars from `s.chars()`,
     rebuild
   Same shape as `overlay_host.rs:1264-1310`.

#### Unit 2-C — `quick-ui::slider` drag-loop completeness (B-9)

File: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/slider.rs`

TDD steps:
1. Add `tests::drag_continues_after_cursor_leaves_rect` — press inside,
   move outside, assert value still updates. Add
   `tests::press_on_track_jumps_value` — press inside the rect at
   x=0.8 of width, assert value ≈ 0.8 (bypass needing prior hover).
2. Make the tests pass:
   - Add `last_cursor: Point` field; update from CursorMoved.
   - Implement `wants_pointer_capture(&self) -> bool { self.is_dragging }`.
   - Replace `if self.is_hovered && Pressed` with
     `if self.rect.contains(self.last_cursor) && Pressed` and on
     entry compute the value from `last_cursor.x` immediately
     (track jump).

#### Unit 2-D — `quick-ui::window_group` and `quick-ui::group_row` parallel-vec invariants (anchor #2 hardening + B-5)

File: `window_group.rs`, `group_row.rs`

TDD steps:
1. Add `tests::reorder_keeps_hosts_and_bar_tabs_aligned` — drive a
   reorder via `take_reorder` for several from/to combinations,
   assert `bar.tabs[i].id == hosts[i].0` for every i after each.
   Add `tests::group_prune_keeps_node_ids_aligned_in_debug` — gate on
   `cfg(debug_assertions)`, prune two groups, assert
   `node_ids.len() == groups.len()` (or that subsequent
   `apply_layout` rebuilds them via `structure_dirty`).
2. Make the tests pass:
   - In `WindowGroup::handle_event` at line 1031, replace the
     `if from < self.hosts.len() && to < self.hosts.len()` guard
     with a `debug_assert_eq!(self.bar.tabs().len(), self.hosts.len())`
     before, and the actual remove/insert.
   - In `GroupRow::handle_event` after the prune loop (line 401), add
     `debug_assert_eq!(self.node_ids.len(), self.groups.len() + prune.len()_pre_loop)`
     OR remove the corresponding `node_ids` slots in lockstep.

### Wave 3 — P1 / cosmetic + reconciliation (parallel-safe)

#### Unit 3-A — `quick-shell::overlay_host` palette + WS-settings alpha (#5)

File: `overlay_host.rs`

TDD steps:
1. Add `tests::palette_scrim_alpha_matches_concept` and
   `tests::palette_surface_alpha_lifts_off_desktop` — assert
   `MODAL_SCRIM_ALPHA >= 0.45` and `MODAL_SURFACE_ALPHA <= 0.85`,
   with rationale comment citing concept §4.1.
2. Make the tests pass: bump
   - `MODAL_SCRIM_ALPHA` from 0.20 → 0.45 (compensates for missing
     blur — concept's `bg-black/30 backdrop-blur-[1px]` reads as
     ~0.45 with no blur).
   - Leave `MODAL_SURFACE_ALPHA` at 0.70 (concept-correct) but
     ensure the scrim alpha bump above kills the bleed-through.

#### Unit 3-B — `quick-shell::sidebar_wheel_disc` separate `drag_from` from hover-anchor (B-3)

File: `sidebar_wheel_disc.rs`

TDD steps:
1. Add `tests::hover_does_not_capture_pointer` — hover into the disc
   without pressing, assert `wants_pointer_capture() == false`.
   Add `tests::press_then_release_clears_pointer_capture` — press
   inside, release, assert `wants_pointer_capture() == false`.
2. Make the tests pass: rename current `drag_from` to
   `hover_anchor`. Add a new `drag_from: Option<Point>` only set in
   the Pressed branch. Update `wants_pointer_capture` to
   `self.drag_from.is_some()`. Update CursorMoved to read
   `drag_from` for "rotate-while-pressed" and `hover_anchor` for
   "remember last hover for next press".

#### Unit 3-C — `quick-shell::state` shared peer-count helper (B-10)

File: `state.rs` (new pub fn) + `topbar_chrome.rs` + `sidebar_wheel_chrome.rs`

TDD steps:
1. Add `tests::topbar_and_wheel_disc_agree_on_active_collab_count` —
   construct a tree with 2 explicit collaborators (one is "me") on
   the active workspace, assert
   `TopBarChrome::collaborator_initials().len() ==
   SidebarWheelChrome::card_collab_count(active_idx)`.
2. Make the test pass: add
   ```rust
   pub fn active_workspace_peer_count(tree: &WorkspaceTree) -> usize {
       let active = tree.active_workspace();
       let per_ws = active.map(|w|
           w.collaborators.iter().filter(|c| c.id != "me").count()
       ).unwrap_or(0);
       if per_ws > 0 { return per_ws; }
       tree.collaborators.iter().filter(|c| c.id != "me").count()
   }
   ```
   Replace both call sites with this helper. The wheel-disc card-
   count loop has to be reshaped slightly — non-active workspaces
   keep their own per-ws filtered count (no fallback).

#### Unit 3-D — `quick-shell::topbar_chrome` right-cluster separation polish (#3)

File: `topbar_chrome.rs`

TDD steps:
1. Add `tests::avatar_stack_has_breathing_room_to_tray_pill` —
   render one collaborator + tray pill, assert the avatar's right
   edge is ≥ 8 px from `tray_pill_rect.x0`. Today it's ~3 px.
2. Make the test pass: change line 906
   `rx -= tray_w + tokens.spacing.s3 as f64;`
   to
   `rx -= tray_w + tokens.spacing.s4 as f64; // s4 = 16, ≥ 8 px after avatar radius`.
   Update any test asserting on the old gap.

---

## Section D — Verification

For every unit, the *test that pins the fix*. Visual verification
through the `shots` bin where the fix has a draw-side signal.

| Unit | Test entry point | Visual scene to re-render |
|------|------------------|---------------------------|
| 1-A  | `tabs::tests::press_with_stale_hover_idx_does_not_panic` | n/a (panic regression) |
| 1-B  | `overlay_host::tests::overlay_absorbs_*` (×3) | `palette.png`, `quick-settings.png` — no underlying-widget hover ring |
| 1-C  | `group_row::tests::cross_group_drop_into_closing_content_area_keeps_tab_in_source` | n/a (logic) |
| 1-D  | `window_group::tests::detach_tab_clears_closing_state_for_moved_tab` | n/a (logic) |
| 2-A  | `overlay_host::tests::toggle_palette_focuses_input`, `…ws_settings_pill_click_switches_tab` | `palette.png` (focus ring on input), `ws-settings-roles.png` (clicked tab is active) |
| 2-B  | `command_palette::tests::typing_filters_visible_entries` | `palette.png` after a synthetic "h" press → fewer rows |
| 2-C  | `slider::tests::drag_continues_after_cursor_leaves_rect`, `…press_on_track_jumps_value` | n/a (interaction) |
| 2-D  | `window_group::tests::reorder_keeps_hosts_and_bar_tabs_aligned`, `group_row::tests::group_prune_keeps_node_ids_aligned_in_debug` | n/a |
| 3-A  | `overlay_host::tests::palette_scrim_alpha_matches_concept` | `palette.png` — desktop content under the palette is *not* legible |
| 3-B  | `sidebar_wheel_disc::tests::hover_does_not_capture_pointer`, `…press_then_release_clears_pointer_capture` | n/a |
| 3-C  | `state::tests::topbar_and_wheel_disc_agree_on_active_collab_count` | `desktop.png` — same number of dots on the active card and avatars in topbar |
| 3-D  | `topbar_chrome::tests::avatar_stack_has_breathing_room_to_tray_pill` | `desktop.png` zoom on top-right (`/tmp/audit-shots/top-right-crop.png`) — visible gap |

### Verification mantra (per the user)

*"Test that you actually MADE the fix, don't trust counts."* — every
unit's test must call the production code path that exercises the
bug, not a lookalike helper. For example, 1-B's test must call
`overlay_host.handle_event(...)` with a real `WindowEvent::CursorMoved`,
not just assert on `cursor_over_open_overlay()` directly.

### Re-run the audit harness after each wave

```
distrobox enter my-distrobox -- bash -c 'cd /var/home/quinten/Development/quickdesktop && \
    cargo run -p quick-desktop --bin shots -- \
    --width 1920 --height 1080 --out /tmp/audit-shots'
```

Diff against the baseline at `/tmp/audit-shots/*.png` — the visual
deltas listed above are the success signal.

---

## Notes for the executing agent

- All file paths are absolute (matches the Claude harness convention
  in this worktree).
- Each unit lists its TDD step **before** the implementation step.
  Tests must fail first, then pass — that's the proof the fix is
  load-bearing.
- The `quick-ui` units must be committed inside the submodule first;
  the parent repo bumps the submodule pointer in a follow-up commit
  (`CLAUDE.md` § Submodule workflow).
- This plan does **not** touch the OS image or FUSE driver — those
  layers don't carry the bug families we audited.
