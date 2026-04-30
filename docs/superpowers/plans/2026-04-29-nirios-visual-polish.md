# Nirios Visual Polish — Wave 3 Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the six visible issues surfaced by the 2026-04-29 headless-screenshot pass (`/tmp/nirios-vis/*.png`). Each task addresses one observed delta between the rendered scene and what the concept expects.

**Architecture:** No structural changes. Each task is scoped to 1–3 files in `quick-shell/src/shell/` or `quick-ui/src/widget/`. Most are parallel-safe; the WS-tab refactor (Task 4) must precede the Roles geometry cleanup (Task 5).

**Tech stack:** Rust + Vello + Taffy. Verification harness: `cargo run --bin shots` headless screenshots + the existing `concept_parity_spec` ledger + `e2e_shell_flows` goldens.

**Reference:** screenshot pass at commit `f1e707f` showed:
- desktop.png — wheel disc clipped left, content card narrow, top-bar right sparse
- overview.png — only 1 workspace card (default tree has 1)
- ws-settings-files.png — "__files_tab__" literal leaked into Name input
- ws-settings-roles.png — Roles pane overlaps "Press Esc to close" + Name input from General body

---

## Dependency graph

```
T1 (wheel clamp)        ← independent
T2 (seed workspaces)    ← independent
T3 (tile width)         ← independent
T4 (typed WsTab)        ← prerequisite for T5
T5 (Roles geometry)     ← depends on T4
T6 (top-bar right)      ← depends on T2 (collaborators seeded)
```

Parallel-safe set: T1, T2, T3, T4 (4 chain heads). T5 dispatches after T4; T6 dispatches after T2 lands and the visual gap is verified to still exist.

---

## Task 1: Clamp the sidebar wheel disc to a minimum left margin

**Files:**
- Modify: `quick-shell/src/shell/sidebar_wheel_disc.rs` (the disc widget — `apply_layout` / wherever `center_x` is computed)
- Test: `quick-shell/src/shell/sidebar_wheel_disc.rs` `#[cfg(test)] mod tests`

**Concept ref:** The 240 px disc must stay fully on-screen regardless of viewport width. At narrow / window-default sizes (≤ 1200 px wide), the disc currently centers at a position that puts its left edge in negative X.

**Observed:** `desktop.png` at 1600×900 shows only ~5% of the disc's right curve.

**Rust hooks (existing — reuse):**
- Grep `sidebar_wheel_disc.rs` for `radius_px` / `center_x` / `apply_layout` / `Rect::new`. Adjust the rect computation so `rect.x0 >= MIN_LEFT_MARGIN` (suggest 16 px so the disc still feels close to the edge).
- The disc's existing geometry helpers (`spoke_angle`, `hub_rect`, etc.) all consume `self.rect` so clamping the rect cascades automatically.

- [ ] **Step 1: Write failing test pinning `rect.x0 >= 16.0` after `apply_layout` at narrow viewports**

```rust
#[test]
fn disc_rect_stays_inside_viewport_at_narrow_width() {
    let mut disc = SidebarWheelDisc::new(/* default constructor args */);
    // Drive build_layout + apply_layout via a tiny taffy tree at 800 px wide.
    let viewport_w = 800.0;
    // ... taffy setup mirroring existing tests in this file ...
    disc.apply_layout(&taffy, node, Point::ORIGIN);
    let rect = disc.rect_for_tests();
    assert!(
        rect.x0 >= 16.0,
        "disc must keep a >= 16 px left margin so its left curve never \
         clips off-screen at narrow viewport widths; observed x0 = {}",
        rect.x0
    );
}
```

If `rect_for_tests` doesn't exist, add a `#[cfg(test)] pub(crate) fn rect_for_tests(&self) -> Rect { self.rect }` accessor.

- [ ] **Step 2: Run test — verify FAIL** with the message above (expected on current code).
  ```
  cargo test -p quick-desktop --lib -- sidebar_wheel_disc::tests::disc_rect_stays_inside_viewport_at_narrow_width
  ```

- [ ] **Step 3: Implement clamp**

In `apply_layout`, find the line that sets `self.rect = Rect::new(...)`. Wrap with:
```rust
const MIN_LEFT_MARGIN: f64 = 16.0;
let raw_x0 = origin.x + l.location.x as f64;
let x0 = raw_x0.max(MIN_LEFT_MARGIN);
let dx = x0 - raw_x0;
self.rect = Rect::new(x0, ..., x0 + l.size.width as f64, ...);
// All hub / spoke geometry derives from self.rect, so no further changes.
```

- [ ] **Step 4: Run test — verify PASS**

- [ ] **Step 5: Visual verification via the shots harness**
  ```
  cargo run --bin shots -- --width 1200 --scenes desktop --out /tmp/check
  ```
  Inspect `/tmp/check/desktop.png` — the full disc should be visible.

- [ ] **Step 6: Commit**
  ```bash
  git commit -m "fix(sidebar-wheel-disc): clamp x0 to 16 px so the disc never clips off-screen"
  ```

---

## Task 2: Seed `WorkspaceTree::bootstrap_default` with 3 demo workspaces + collaborators

**Files:**
- Modify: `quick-shell/src/shell/state.rs` — `WorkspaceTree::bootstrap_default`
- Test: same file, existing `#[cfg(test)] mod tests` block

**Concept ref:** The committed e2e golden `quick-shell/tests/snapshots/e2e/e2e-open-close-overview-settled.png` shows 3 workspaces (Home / Research / Design) — that's what the live binary should also boot with. Currently `bootstrap_default` seeds only 1 workspace ("Home"), so the overview shows a single card.

**Observed:** `overview.png` shows only the Home card.

**Rust hooks (existing — reuse):**
- `WorkspaceTree::push_workspace` or whatever the existing add-workspace API is on the tree (grep `bootstrap_default` for the current shape).
- `Collaborator::new` / `with_cursor_color` / `with_role` builder chain (per `parity-status.md` §8) — use these to seed 2-3 demo collaborators on at least one workspace so Task 6's avatar stack is unblocked.

- [ ] **Step 1: Write failing test pinning the seeded count**

```rust
#[test]
fn bootstrap_default_seeds_three_demo_workspaces() {
    let tree = WorkspaceTree::bootstrap_default();
    assert!(
        tree.workspaces.len() >= 3,
        "default tree should seed at least 3 workspaces (Home / Research / \
         Design) so the overview has meaningful content; got {}",
        tree.workspaces.len()
    );
    // At least one workspace should have collaborators so the top-bar
    // avatar stack has something to render.
    assert!(
        tree.workspaces.iter().any(|w| !w.collaborators.is_empty()),
        "at least one default workspace should seed >=1 collaborator"
    );
}
```

- [ ] **Step 2: Run test — verify FAIL** with both assertions failing.

- [ ] **Step 3: Implement seeding**

In `bootstrap_default`, after the existing Home workspace, push:
```rust
let mut research = Workspace::new(WorkspaceId(2), "Research");
research.collaborators.push(
    Collaborator::new("alice", "Alice")
        .with_cursor_color("#ec4899")
        .with_role("editor"),
);
research.collaborators.push(
    Collaborator::new("bob", "Bob")
        .with_cursor_color("#f97316")
        .with_role("viewer"),
);
tree.workspaces.push(research);

let design = Workspace::new(WorkspaceId(3), "Design");
tree.workspaces.push(design);
```
(Adjust to the actual `Workspace::new` / id type signatures present in the file. If Workspace doesn't have `new`, mirror the existing constructor pattern in the same file.)

- [ ] **Step 4: Run test — verify PASS**

- [ ] **Step 5: Audit existing tests for breakage**
  ```
  cargo test -p quick-desktop --lib 2>&1 | tee /tmp/lib-after.log
  cargo test -p quick-desktop --tests -- --test-threads 1 2>&1 | tee /tmp/tests-after.log
  ```
  Some tests likely pin `tree.workspaces.len() == 1`. For each failure, decide:
  - If the test pinned a count, update it to the new count (3) — that's a legitimate doc-of-fixture update.
  - If the test relied on a specific workspace ordering or data, see if it's still valid; rewrite if needed.

- [ ] **Step 6: Re-run e2e + concept_parity_spec — verify 21/21 + 15/15 stay green**
  ```
  cargo test -p quick-desktop --tests -- --test-threads 1
  ```
  If `e2e-open-close-overview-settled` golden invalidates because the rendered overview now differs from the pinned snapshot (it shouldn't — the golden was already 3-workspace), re-seed with `UPDATE_SNAPSHOTS=1` and verify the diff is exactly the new content.

- [ ] **Step 7: Commit**
  ```bash
  git commit -m "feat(state): seed bootstrap_default with 3 workspaces + demo collaborators"
  ```

---

## Task 3: Tile host fills available workspace width

**Files:**
- Modify: `quick-ui/src/widget/window_group.rs` — `WindowWidth` default OR
- Modify: `quick-shell/src/shell/state.rs` — `WorkspaceTree::bootstrap_default` group widths

**Concept ref:** `WindowFrame.tsx` doesn't constrain the white card's width — the tile fills its container. Our `WindowWidth::Normal` is 600 px (per CLAUDE.md), which leaves ~800 px of empty workspace at 1600 px viewport.

**Observed:** `desktop.png` shows ~625 px white card on a ~1400 px workspace area.

**Pick the lighter touch.** Two valid approaches:

**Option A (preferred):** Change the demo workspace's group `WindowWidth` from `Normal` → `Wide` so first-run looks reasonable. Keep the user's ability to choose Narrow/Normal/Wide/Custom intact. One-line change in `state.rs`'s default tree.

**Option B:** Add a new `WindowWidth::Auto` variant that fills available width up to a max (e.g. 1200 px), and make it the default. More invasive — touches every WindowGroup constructor + `WindowWidth::pixels()` resolver.

Use Option A unless inspection shows the demo tree already uses Wide and the visible narrow card comes from elsewhere.

- [ ] **Step 1: Inspect current default**
  ```
  rg 'WindowWidth::' quick-shell/src/shell/state.rs quick-ui/src/widget/window_group.rs
  ```
  Locate where `bootstrap_default`'s group sets its width.

- [ ] **Step 2: Write failing test**

```rust
// In quick-shell/src/shell/state.rs tests
#[test]
fn bootstrap_default_first_group_uses_wide_or_larger() {
    let tree = WorkspaceTree::bootstrap_default();
    let first_ws = tree.workspaces.first().expect("at least one workspace");
    let first_strip = first_ws.strips.first().expect("at least one strip");
    let first_group = first_strip.groups.first().expect("at least one group");
    assert!(
        matches!(first_group.width, WindowWidth::Wide | WindowWidth::Custom(w) if w >= 900),
        "default group should be Wide+ so the white card fills more than \
         half the viewport; got {:?}",
        first_group.width
    );
}
```

- [ ] **Step 3: Run test — verify FAIL**

- [ ] **Step 4: Implement — flip the seed value to `WindowWidth::Wide`**

- [ ] **Step 5: Run test — verify PASS**

- [ ] **Step 6: Re-render + visually verify**
  ```
  cargo run --bin shots -- --scenes desktop --out /tmp/check
  ```
  White card should fill noticeably more of the workspace area.

- [ ] **Step 7: Verify e2e + spec ledger stay green**
  ```
  cargo test -p quick-desktop --tests -- --test-threads 1
  ```
  E2E goldens may need re-seed if the rendered group is wider; that's the expected concept-correct change.

- [ ] **Step 8: Commit**
  ```bash
  git commit -m "feat(state): default group width is Wide so the tile fills the workspace"
  ```

---

## Task 4: Typed `WsTab` on chrome state — replace `editing_workspace_name` side-channel

**Files:**
- Modify: `quick-shell/src/shell/chrome_state.rs` — add `pub ws_settings_tab: WsTab` field on `ChromeState`
- Modify: `quick-shell/src/shell/overlay_host.rs::draw_ws_settings` — read `chrome.ws_settings_tab` instead of pattern-matching `editing_workspace_name` against magic strings
- Modify: `quick-shell/tests/concept_parity_spec.rs::render_with_ws_settings_tab` — set `chrome.ws_settings_tab` instead of `editing_workspace_name`
- Re-export: `quick-shell/src/shell/workspace_settings.rs` (`WsTab` enum from T1) — make sure it's accessible from chrome_state.rs (move to a shared module if needed)

**Concept ref:** `WorkspaceSettingsModal.tsx:50-95` — General/Roles/Files/Collaborators tabs are independent UI state, NOT entangled with the name-edit buffer.

**Observed:** `ws-settings-files.png` shows the Name input pre-filled with `__files_tab__` because the test side-channel set `editing_workspace_name = "__files_tab__"` and the General body's name display (`shown` variable) reads from that same field as a fallback when `editing_workspace_name.is_some()`.

**Rust hooks (existing — reuse):**
- `WsTab` enum already exists in `quick-shell/src/shell/workspace_settings.rs` per T1's commit `879220f`. May need to move to a more central location (e.g. `chrome_state.rs`) so chrome can hold it without circular import.
- `OverlayFlags::ws_settings: bool` stays as-is (open/closed gate).

- [ ] **Step 1: Move (or re-export) `WsTab` so it's available from `chrome_state.rs`**

Either move the enum from `workspace_settings.rs` to `chrome_state.rs`, or add a `pub use crate::shell::workspace_settings::WsTab;` re-export in `chrome_state.rs`. Pick whichever creates fewer circular imports.

- [ ] **Step 2: Add field to ChromeState**

```rust
// In chrome_state.rs
#[derive(Clone, Debug)]
pub struct ChromeState {
    // ... existing fields ...
    /// Current Workspace Settings tab. Independent of `editing_workspace_name`,
    /// which is only the name-input buffer.
    pub ws_settings_tab: WsTab,
}

impl Default for ChromeState {
    fn default() -> Self {
        Self {
            // ... existing defaults ...
            ws_settings_tab: WsTab::default(), // = General
        }
    }
}
```

- [ ] **Step 3: Write failing test**

```rust
// In tests/concept_parity_spec.rs
#[test]
fn ws_settings_files_tab_does_not_leak_magic_string_into_name_input() {
    let tree = WorkspaceTree::bootstrap_default();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    {
        let mut st = chrome.borrow_mut();
        st.overlays.ws_settings = true;
        st.ws_settings_tab = WsTab::Files;
        // editing_workspace_name MUST stay None — the typed field carries
        // tab routing now.
        st.editing_workspace_name = None;
    }
    // ... render via the standard scene-build helper ...
    let scene = render(&mut host, &chrome, /* w */ 1600.0, /* h */ 900.0);
    // Inspect the scene's rendered text for the literal substring; if
    // the side-channel ever leaks back, this fires.
    let rendered = collect_drawn_text(&scene); // helper next to collect_solid_colors
    assert!(
        !rendered.iter().any(|s| s.contains("__files_tab__") || s.contains("__roles_tab__")),
        "Name input must not display the tab side-channel literal; got: {:?}",
        rendered
    );
}
```

If `collect_drawn_text` doesn't exist, add it in the test support — iterate the scene's `DrawTag::TEXT` ops and pull out the strings.

- [ ] **Step 4: Run test — verify FAIL** (currently the side-channel does leak).

- [ ] **Step 5: Update `render_with_ws_settings_tab` helper**

```rust
fn render_with_ws_settings_tab(tree: &WorkspaceTree, tab: WsTab) -> (usize, Chrome) {
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(tree);
    {
        let mut st = chrome.borrow_mut();
        st.overlays.ws_settings = true;
        st.ws_settings_tab = tab;
    }
    // ... rest unchanged ...
}
```

Update the call sites that previously passed `roles: bool` to pass `WsTab::Roles` / `WsTab::Files` / `WsTab::General`.

- [ ] **Step 6: Update `draw_ws_settings` to read the typed field**

```rust
let active_tab = state.ws_settings_tab;

// Tabs strip — visual highlight follows active_tab, not editing_workspace_name.
for (i, tab) in tabs.iter().enumerate() {
    let active = match (i, active_tab) {
        (0, WsTab::General) => true,
        (1, WsTab::Roles) => true,
        (2, WsTab::Files) => true,
        (3, WsTab::Collaborators) => true,
        _ => false,
    };
    // ... pill rendering ...
}

match active_tab {
    WsTab::Roles => { draw_roles_pane(...); return; }
    WsTab::Files => { draw_files_pane(...); return; }
    WsTab::Collaborators => { /* not yet implemented — fall through */ }
    WsTab::General => {} // fall through to General body
}
```

The `if roles_active { ... return; }` branch becomes `match active_tab`. The General body ONLY runs when `active_tab == WsTab::General`.

- [ ] **Step 7: Run all spec tests — verify FAIL → PASS for the new test, all previously-passing tests stay green**

```
cargo test -p quick-desktop --test concept_parity_spec -- --test-threads 1
```

Expected: 21 + 1 (new test) = 22/22 PASS.

- [ ] **Step 8: Commit**
  ```bash
  git commit -m "refactor(chrome-state): typed WsTab field replaces __roles_tab__ side-channel"
  ```

---

## Task 5: Roles pane geometry no longer overlaps General body

**Files:**
- Modify: `quick-shell/src/shell/overlay_host.rs::draw_ws_settings`
- Test: `quick-shell/tests/concept_parity_spec.rs`

**Concept ref:** Tabs are mutually exclusive — switching to Roles fully replaces the body region. Currently the Roles render extends into General's `Press Esc to close` hint and Name input geometry.

**Observed:** `ws-settings-roles.png` shows "Press Esc to close" text bleeding into the Roles content.

**Depends on:** Task 4 (typed WsTab) must land first — the Roles arm reads from the typed field cleanly.

**Rust hooks:**
- T4's `match active_tab` block in `draw_ws_settings` — make sure the `WsTab::Roles` arm `return`s after drawing the Roles pane *and the close hint specific to Roles*, never falling through.
- Modal body bounds — `body_top` to `rect.y1 - 16` is the rect available for tab-pane content. Each per-tab pane must fully paint inside that rect and own its own close-hint placement.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn roles_tab_does_not_render_press_esc_to_close_text() {
    let tree = WorkspaceTree::bootstrap_default();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    {
        let mut st = chrome.borrow_mut();
        st.overlays.ws_settings = true;
        st.ws_settings_tab = WsTab::Roles;
    }
    let scene = render(&mut host, &chrome, 1600.0, 900.0);
    let rendered = collect_drawn_text(&scene);
    let leaked: Vec<_> = rendered
        .iter()
        .filter(|s| s.contains("Press Esc to close") || s.contains("Allowed Applications") == false && s.contains("Name"))
        .collect();
    // Roles render owns its own close hint; General's "Press Esc to close"
    // string and the "Name" label of the General Name input must NOT
    // appear when the Roles tab is active.
    assert!(
        !rendered.iter().any(|s| *s == "Name"),
        "Roles tab rendered General's Name label — tab branching is leaking"
    );
}
```

- [ ] **Step 2: Run — verify FAIL**

- [ ] **Step 3: Audit `draw_ws_settings` for unconditional drawing in the General path**

The General body block (Name input, Invite Link, Collaborators, close hint) currently runs as the fall-through path. After T4's match, ensure these only run inside the `WsTab::General` arm. The Roles arm has its own close hint per T2's commit `fdfb5dd`; verify the geometry doesn't extend past the modal's `rect.y1 - 16`.

- [ ] **Step 4: Implement — wrap the General body block in `if matches!(active_tab, WsTab::General)`**

Or, equivalently, move the General body into a new `draw_general_pane` helper called only from the `WsTab::General` arm of the match. Mirrors T2/T3/T4's structure.

- [ ] **Step 5: Run — verify PASS**

- [ ] **Step 6: Visual verification**
  ```
  cargo run --bin shots -- --scenes ws-settings-roles --out /tmp/check
  ```
  Inspect `/tmp/check/ws-settings-roles.png` — Roles content should occupy the full body without "Press Esc to close" or "Name" leaking through.

- [ ] **Step 7: Commit**
  ```bash
  git commit -m "fix(workspace-settings): Roles/Files panes fully replace General body"
  ```

---

## Task 6: Top-bar right cluster verification (avatars + 3-dot)

**Files:**
- Read: `quick-shell/src/shell/topbar_chrome.rs::draw_top_bar` (or whatever owns the right cluster)
- Probably no edits — this task is mostly verification once T2 lands.

**Concept ref:** `TopBarWidgets.tsx` shows: collaborator avatar stack (3 + overflow chip) + 3-dot quick-settings toggle, on the right.

**Observed:** `desktop.png` shows only a small wifi/cast icon on the top-bar right.

**Depends on:** Task 2 (collaborators seeded on at least one workspace).

- [ ] **Step 1: Re-render `desktop.png` after T2 lands**
  ```
  cargo run --bin shots -- --scenes desktop --out /tmp/check
  ```
  If avatars + 3-dot now show up: Task 6 is done by transitive resolution.

- [ ] **Step 2: If avatars still missing**, grep `topbar_chrome.rs` for the avatar-stack draw block. Likely gated on `collaborators.len() > 0` already, but the iteration source might be `tree.collaborators` (top-level) not `active_workspace.collaborators`. Check.

- [ ] **Step 3: If 3-dot still missing**, the small wifi icon I currently see may BE the 3-dot stylized differently. Compare the actual rendered glyph against `quick_ui::icons::Icon::More*` or whatever the lucide name is in the topbar code. If it's the wrong glyph, swap.

- [ ] **Step 4: Add a regression test**

```rust
#[test]
fn topbar_right_cluster_paints_avatar_stack_when_collaborators_present() {
    let tree = WorkspaceTree::bootstrap_default(); // post-T2 has collaborators
    let scene = render_topbar_only(&tree); // existing test helper, or write one
    // Avatar fills are colored circles in the right third of the top bar.
    // Sample a few pixels in that band and assert at least one peer-color
    // (alice = #ec4899, bob = #f97316) shows up.
    let band_rgba = pixels_in_rect(&scene, /* x */ 1100..1600, /* y */ 8..32);
    let has_peer_color = band_rgba.iter().any(|(r, g, b, _)| {
        // alice pink-500 or bob orange-500, generous tolerance.
        (r.abs_diff(0xec) <= 8 && g.abs_diff(0x48) <= 8 && b.abs_diff(0x99) <= 8)
            || (r.abs_diff(0xf9) <= 8 && g.abs_diff(0x73) <= 8 && b.abs_diff(0x16) <= 8)
    });
    assert!(has_peer_color, "topbar right cluster must render at least one peer-coloured avatar");
}
```

- [ ] **Step 5: Run — verify PASS** (post-T2). If FAIL, dig into the topbar render.

- [ ] **Step 6: Commit (only if any code change)**
  ```bash
  git commit -m "fix(topbar): right cluster renders peer avatars from active workspace"
  ```

---

## End-to-end verification recipe

After each task lands, run the full ledger to confirm no regression:

```
distrobox enter my-distrobox -- bash -c 'cd quick-shell && cargo test --lib'
distrobox enter my-distrobox -- bash -c 'cd quick-shell && cargo test --tests -- --test-threads 1'
distrobox enter my-distrobox -- bash -c 'cargo test -p quick-ui --lib'
distrobox enter my-distrobox -- bash -c 'cargo build -p quick-ui -p quick-desktop'
```

Plus a visual pass:
```
cargo run --bin shots
# read each PNG in target/nirios-shots/ to confirm the fix
```

Snapshot goldens (`tests/snapshots/e2e/*.png`) may invalidate when T2 (more workspaces) and T3 (wider tile) land. Re-seed with `UPDATE_SNAPSHOTS=1` only after confirming the diff is exactly the documented concept-correct change.

## Out of scope

- Mini-window preview tab-strip on top (audit row §1 — separate concept-coverage gap, not in this batch).
- Workspace settings Collaborators tab content (concept TSX still has 4 tabs; only General/Roles/Files have plans).
- Foggy collaborator blur on tabs (mix-blend-screen — needs design call).
- DragPreview mock-window-at-cursor (needs new overlay layer).

## Summary

- 6 tasks, 4 parallel-safe at the chain heads.
- Each task ends with a green spec test + visual verification via `cargo run --bin shots`.
- Sequence: T1, T2, T3, T4 in parallel → T5 after T4 → T6 after T2.
