# Nirios Concept-Fidelity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the visual + functional gaps identified in the 2026-04-28 concept-coverage audit (`docs/nirios/concept-coverage.md`) so the shipped implementation is pixel-close to `nirios-concept/src/`.

**Architecture:** No structural changes. Each task is scoped to one or two files in `quick-shell/src/shell/` or `quick-ui/src/widget/`. Independent tasks can run as parallel subagents in worktrees.

**Tech stack:** Rust + Vello + Taffy. Concept reference: React/Tailwind in `nirios-concept/src/`.

**Color palette reference (Tailwind → hex, theme tokens):**

- `indigo-400` → `#818cf8` (theme: `colors.accent[6]`)
- `indigo-500` → `#6366f1` (theme: `colors.accent[5]`, `theme.accent_primary`)
- `indigo-600` → `#4f46e5` (theme: `colors.accent[4]`)
- `emerald-400` → `#4ade80`
- `emerald-500` → `#22c55e` (sparkline at 30% alpha = `rgba(34,197,94,0.3)`)
- `blue-400` → `#60a5fa`
- `orange-400` → `#fb923c`
- `slate-300` → `#cbd5e1` (theme: `colors.neutral[7]`)
- `slate-700` → `#334155` (theme: `colors.neutral[3]`)
- `slate-800` → `#1e293b` (theme: `colors.neutral[2]`)
- `slate-900` → `#0f172a` (theme: `colors.neutral[1]`)

**TDD discipline:** Each task ships with a failing test FIRST. Run the test, confirm it fails for the right reason, then implement and confirm pass. Commit per task.

---

## Task index

1. WorkspaceSettings: 256 px sidebar nav (General / Roles & Permissions / Files / Close)
2. WorkspaceSettings General tab: invite-link field + collaborator grid
3. WorkspaceSettings Roles tab: role list + per-role app/file permission grids
4. WorkspaceSettings Files tab: file grid w/ assigned-role avatars
5. GlobalSearch: leading lucide search glyph + Cmd+K kbd chip + radius bump to 24 px
6. GlobalSearch: filter strip (scope/filetype/full-text) + footer kbd hints
7. GlobalSearch: selected-row indigo-500 fill + per-row icon container
8. QuickSettings: 3-button action row + slider card + indigo toggle cards + Power Profile
9. QuickSettings: nav_stack drill-in render for WiFi/BT lists
10. TopBar SystemMonitor: per-metric semantic colors (emerald CPU / blue RAM / orange TMP)
11. WindowFrame maximized chrome: auto-hide unless cursor.y ≤ 80
12. TileHost search-active: compose 60 px indigo bloom sprite

---

### Task 1: WorkspaceSettings — 256 px sidebar nav

**Files:**
- Modify: `quick-shell/src/shell/overlay_host.rs` (function `draw_ws_settings`, current 520×360 single-column draw)
- Modify: `quick-shell/src/shell/workspace_settings.rs` (add `WsTab` enum + `active_tab: WsTab`)
- Test: `quick-shell/src/shell/workspace_settings.rs` `#[cfg(test)] mod tests`

**Concept ref:** `nirios-concept/src/components/WorkspaceSettingsModal.tsx:43-95` — full-bleed slate-900/70 modal split: 256 px left sidebar (workspace identity card + 3 tab buttons + footer Close) + right content pane.

**Rust hooks (existing — reuse):**
- `WorkspaceSettings` struct at `quick-shell/src/shell/workspace_settings.rs` — already owns `apps_table`, `files_table`, `select_role`, `invite_link`. Add `active_tab` field.
- `ChromeState::overlays.ws_settings` — visibility toggle wired in `topbar_chrome.rs` gear-click.
- `theme.tokens.colors.neutral[1..=3]` — slate-900..700 ramp for sidebar bg + border.
- `theme.tokens.colors.accent[5]` — indigo-500 for the workspace identity square.

- [ ] **Step 1: Add `WsTab` enum + `active_tab` field with failing test pinning the tab default + switch behavior.**
  ```rust
  // In quick-shell/src/shell/workspace_settings.rs
  #[derive(Copy, Clone, Debug, PartialEq, Eq)]
  pub enum WsTab { General, Roles, Files }

  // ...add `pub active_tab: WsTab` to WorkspaceSettings, default WsTab::General

  #[cfg(test)]
  mod tab_tests {
      use super::*;
      #[test]
      fn ws_settings_default_tab_is_general() {
          let ws = WorkspaceSettings::new();
          assert_eq!(ws.active_tab, WsTab::General);
      }
      #[test]
      fn select_tab_switches_active() {
          let mut ws = WorkspaceSettings::new();
          ws.select_tab(WsTab::Roles);
          assert_eq!(ws.active_tab, WsTab::Roles);
          ws.select_tab(WsTab::Files);
          assert_eq!(ws.active_tab, WsTab::Files);
      }
  }
  ```
- [ ] **Step 2: Run test, verify FAIL with `error[E0599]: no variant or associated item named 'General' found for enum 'WsTab'` or `no field active_tab`.**
  Run: `cargo test -p quick-shell ws_settings_default_tab_is_general`
- [ ] **Step 3: Implement `WsTab` + `active_tab` field + `select_tab()` method on `WorkspaceSettings`. Then rewrite `draw_ws_settings` to lay out a 4xl×600 px modal with a 256 px left sidebar block (slate-900 bg, 1 px slate-700 right border) holding: (a) 56×56 indigo-500 rounded-2xl square w/ workspace first letter Bold 24 px white, (b) "Workspace Settings" caption text-dim 12 px Semibold below, (c) 3 nav rows (General / Roles & Permissions / Files) each `h-10 rounded-xl` — selected = indigo-500 bg + white text, idle = transparent + text-dim, hover = white/5, (d) footer "Close Settings" row at the bottom with X glyph. Right pane gets remaining width and dispatches by `ws.active_tab` to placeholder content (filled by Tasks 2–4).**
  ```rust
  // overlay_host.rs::draw_ws_settings (replacing current 520×360 single-column body)
  let modal_w = 896.0;  // max-w-4xl
  let modal_h = 600.0;
  let modal_rect = centered_rect(scene_size, modal_w, modal_h);
  // shell card
  scene.fill(Fill::NonZero, Affine::IDENTITY, slate_900_alpha(0.70),
             None, &RoundedRect::from_rect(modal_rect, 24.0));
  // sidebar
  let sidebar_w = 256.0;
  let sidebar_rect = Rect::new(modal_rect.x0, modal_rect.y0,
                                modal_rect.x0 + sidebar_w, modal_rect.y1);
  scene.fill(Fill::NonZero, Affine::IDENTITY, slate_900_alpha(0.40),
             None, &sidebar_rect);
  // identity card (56×56 indigo-500 w/ first letter)
  // 3 nav rows
  for (i, (tab, label)) in [(WsTab::General, "General"),
                              (WsTab::Roles, "Roles & Permissions"),
                              (WsTab::Files, "Files")].iter().enumerate() {
      let row_rect = Rect::new(sidebar_rect.x0 + 16.0,
                                sidebar_rect.y0 + 120.0 + i as f64 * 48.0,
                                sidebar_rect.x1 - 16.0,
                                sidebar_rect.y0 + 160.0 + i as f64 * 48.0);
      let active = ws.active_tab == *tab;
      let bg = if active { indigo_500() } else { transparent() };
      // text fill: white if active else slate-400
  }
  // dispatch right pane by ws.active_tab
  let pane_rect = Rect::new(sidebar_rect.x1, modal_rect.y0,
                             modal_rect.x1, modal_rect.y1);
  match ws.active_tab {
      WsTab::General => draw_ws_general(scene, ws, pane_rect, theme),
      WsTab::Roles   => draw_ws_roles(scene, ws, pane_rect, theme),
      WsTab::Files   => draw_ws_files(scene, ws, pane_rect, theme),
  }
  ```
  Wire click handlers in `overlay_host.rs::handle_event` to call `ws.select_tab(...)` on each nav row hit.
- [ ] **Step 4: Run test, verify PASS.** Run: `cargo test -p quick-shell ws_settings_default_tab_is_general select_tab_switches_active`
- [ ] **Step 5: Commit.**
  ```bash
  git commit -m "feat(ws-settings): add 256 px sidebar nav with General/Roles/Files tabs"
  ```

---

### Task 2: WorkspaceSettings General tab — invite link + collaborator grid

**Files:**
- Modify: `quick-shell/src/shell/overlay_host.rs` (new helper `draw_ws_general`)
- Modify: `quick-shell/src/shell/workspace_settings.rs` (add `remove_collaborator`, `copy_invite_link` action stubs if missing)
- Test: `quick-shell/src/shell/workspace_settings.rs` `#[cfg(test)] mod tests`

**Concept ref:** `nirios-concept/src/components/WorkspaceSettingsModal.tsx:99-149` — `Invite Link` text input (read-only) with a Copy button on the right; below it a `Collaborators (N)` grid where each row is `avatar + name + role chip + Remove`.

**Rust hooks (existing — reuse):**
- `WorkspaceSettings::invite_link: String` at `quick-shell/src/shell/workspace_settings.rs:147` — already populated; just unused by draw.
- `Workspace::collaborators` (the active workspace's roster, accessible via `chrome.tree.active_workspace()`).
- `quick_ui::presence::user_color(peer_id)` — palette-pinned avatar fill.

- [ ] **Step 1: Write failing test pinning `remove_collaborator` mutates the active workspace roster and that `invite_link` is non-empty after `WorkspaceSettings::new`.**
  ```rust
  #[test]
  fn invite_link_is_populated_after_new() {
      let ws = WorkspaceSettings::new();
      assert!(!ws.invite_link.is_empty(), "invite_link must be auto-populated");
      assert!(ws.invite_link.starts_with("nirios://"),
              "invite_link should start with nirios:// scheme");
  }
  #[test]
  fn remove_collaborator_drops_id_from_pending_removals() {
      let mut ws = WorkspaceSettings::new();
      ws.queue_remove_collaborator("alice".into());
      assert!(ws.pending_removals.iter().any(|p| p == "alice"));
  }
  ```
- [ ] **Step 2: Run test, verify FAIL.**
  Run: `cargo test -p quick-shell invite_link_is_populated_after_new remove_collaborator_drops_id_from_pending_removals`
- [ ] **Step 3: Implement `pending_removals: Vec<String>` + `queue_remove_collaborator()`. If `invite_link` isn't auto-populated in `WorkspaceSettings::new`, set it to `"nirios://workspace/{id}"` in the constructor. Then write `draw_ws_general(scene, ws, pane_rect, theme)`:**
  ```rust
  // Title: "General" 24 px Bold white, top-left of pane with 32 px padding
  // Invite Link section
  //   label "Invite Link" 12 px Semibold uppercase tracking-wider slate-400
  //   row: rounded-xl bg-slate-800/50 border slate-700, 48 px tall
  //     left: ws.invite_link text in font_mono, slate-300
  //     right: Copy button rounded-lg bg-indigo-500 hover:bg-indigo-600,
  //            label "Copy" 14 px Medium white
  // 24 px gap
  // Collaborators (N) heading w/ count
  // Grid of rows; each row 56 px tall:
  //   - 32 px circle avatar fill = user_color(peer_id), white initial centered
  //   - name 14 px Medium white
  //   - role chip rounded-full px-2 py-0.5 bg-{role.color}/20 text-{role.color}
  //   - Remove button right-aligned, X glyph in slate-400, hover red-400
  ```
  Wire Copy button click to `ws.invite_link_clipboard_pending = true` (a flag the shell flushes to clipboard via existing wayland clipboard hook). Wire Remove → `ws.queue_remove_collaborator(peer_id)`.
- [ ] **Step 4: Run test, verify PASS.** Run: `cargo test -p quick-shell invite_link_is_populated_after_new remove_collaborator_drops_id_from_pending_removals`
- [ ] **Step 5: Commit.**
  ```bash
  git commit -m "feat(ws-settings): General tab — invite link + collaborator grid"
  ```

---

### Task 3: WorkspaceSettings Roles tab — role list + permission grids

**Files:**
- Modify: `quick-shell/src/shell/overlay_host.rs` (new helper `draw_ws_roles`)
- Modify: `quick-shell/src/shell/workspace_settings.rs` — wire existing `apps_table` + `files_table` `DataTable` state to render path, add `selected_role_id` if missing.
- Test: `quick-shell/src/shell/workspace_settings.rs` `#[cfg(test)] mod tests`

**Concept ref:** `nirios-concept/src/components/WorkspaceSettingsModal.tsx:152-254` — left column: role list, each row = colored swatch + name + isDefault chip; right column: per-role app-permissions grid (5 apps × on/off toggles) + file-permissions radio strip (none/read/write).

**Rust hooks (existing — reuse):**
- `WorkspaceSettings::apps_table: DataTable` — cells already represent `(role_id, app_id) -> bool`.
- `WorkspaceSettings::files_table: DataTable` — cells represent `(role_id, file_id) -> Permission`.
- `WorkspaceSettings::select_role(id)` at `workspace_settings.rs` (existing API).
- `Role { id, name, color, is_default }` (assumed present; if not, define in this task).

- [ ] **Step 1: Write failing test pinning role-selection updates `selected_role_id` and that toggling an apps_table cell flips the bool.**
  ```rust
  #[test]
  fn select_role_updates_selected_id() {
      let mut ws = WorkspaceSettings::with_demo_roles();  // helper if needed
      ws.select_role("editor".into());
      assert_eq!(ws.selected_role_id.as_deref(), Some("editor"));
  }
  #[test]
  fn toggle_app_permission_flips_cell() {
      let mut ws = WorkspaceSettings::with_demo_roles();
      ws.select_role("editor".into());
      let before = ws.apps_table.cell("editor", "terminal").as_bool();
      ws.toggle_app_permission("editor", "terminal");
      let after = ws.apps_table.cell("editor", "terminal").as_bool();
      assert_ne!(before, after);
  }
  ```
- [ ] **Step 2: Run test, verify FAIL.**
  Run: `cargo test -p quick-shell select_role_updates_selected_id toggle_app_permission_flips_cell`
- [ ] **Step 3: Add `selected_role_id: Option<String>` + `toggle_app_permission(role, app)` + `cycle_file_permission(role, file)` to `WorkspaceSettings`. Implement `draw_ws_roles`:**
  ```rust
  // Pane is split: left 240 px role list, right ~600 px permission grids
  // Role list rows (h-12, p-3, rounded-xl):
  //   - 12×12 rounded-full swatch fill = role.color
  //   - name 14 px Medium white
  //   - isDefault ? "Default" pill 10 px slate-400 bg-slate-800 px-2 py-0.5
  //   - selected: bg-white/10 ring-1 ring-white/20
  //
  // Right pane top: "App Permissions" heading
  //   - 5-row grid of (app_icon, app_name, toggle_pill).
  //   - Toggle pill: 36×20 rounded-full, on=indigo-500, off=slate-700.
  //     White knob 16×16 at left/right; click toggles.
  //   - Pull on/off from ws.apps_table.cell(role, app).as_bool().
  // Right pane bottom: "File Permissions" heading
  //   - per-file row: name + 3 radio chips (None / Read / Write)
  //   - selected radio fills with role.color, others slate-700 stroke.
  //   - Click cycles via ws.cycle_file_permission(role, file).
  ```
  Wire role-row click → `ws.select_role(role.id)`. Wire toggle hit → `ws.toggle_app_permission(role, app)`. Wire radio hit → `ws.cycle_file_permission(role, file)`.
- [ ] **Step 4: Run test, verify PASS.** Run: `cargo test -p quick-shell select_role_updates_selected_id toggle_app_permission_flips_cell`
- [ ] **Step 5: Commit.**
  ```bash
  git commit -m "feat(ws-settings): Roles tab — role list + app/file permission grids"
  ```

---

### Task 4: WorkspaceSettings Files tab — file grid with role assignments

**Files:**
- Modify: `quick-shell/src/shell/overlay_host.rs` (new helper `draw_ws_files`)
- Modify: `quick-shell/src/shell/workspace_settings.rs` — add `files: Vec<WsFile>` if absent, with assigned-role ids.
- Test: `quick-shell/src/shell/workspace_settings.rs` `#[cfg(test)] mod tests`

**Concept ref:** `nirios-concept/src/components/WorkspaceSettingsModal.tsx:257-303` — grid of file rows; each row = file icon + name + type badge + `assigned-roles` avatar pile (max 3 + overflow chip) + Remove button.

**Rust hooks (existing — reuse):**
- `WorkspaceSettings::files_table: DataTable` — already keys files. Iterate row-keys to enumerate files.
- `WorkspaceSettings::roles` — needed to map role_id → swatch color for the avatar pile.
- `quick_ui::icons::draw_icon` — file-type glyphs (assumed available; concept uses lucide `FileText` / `Image` / `Folder`).

- [ ] **Step 1: Write failing test pinning `queue_remove_file` adds to `pending_file_removals` and that `assigned_roles_for(file_id)` returns ids whose `files_table` cell is non-`None`.**
  ```rust
  #[test]
  fn queue_remove_file_appends_id() {
      let mut ws = WorkspaceSettings::with_demo_files();
      ws.queue_remove_file("README.md".into());
      assert!(ws.pending_file_removals.contains(&"README.md".to_string()));
  }
  #[test]
  fn assigned_roles_for_returns_role_ids_with_any_perm() {
      let mut ws = WorkspaceSettings::with_demo_files();
      ws.cycle_file_permission("editor", "README.md"); // → Read
      let roles = ws.assigned_roles_for("README.md");
      assert!(roles.iter().any(|r| r == "editor"));
  }
  ```
- [ ] **Step 2: Run test, verify FAIL.**
  Run: `cargo test -p quick-shell queue_remove_file_appends_id assigned_roles_for_returns_role_ids_with_any_perm`
- [ ] **Step 3: Add `pending_file_removals: Vec<String>` + `queue_remove_file()` + `assigned_roles_for(file_id) -> Vec<String>`. Implement `draw_ws_files`:**
  ```rust
  // Heading "Files (N)" top-left
  // Grid: each row 64 px tall, gap 8 px, p-3, rounded-xl bg-slate-800/30
  //   - file icon 24×24 (FileText / Image / Folder) slate-300
  //   - name 14 px Medium white
  //   - type badge "MD" / "PNG" / "DIR" rounded px-2 py-0.5 slate-700 bg slate-300 text
  //   - role avatars: up to 3 from assigned_roles_for(file), 24×24 circles fill = role.color,
  //     overlap by -8 px; if N > 3 trailing "+M" chip
  //   - Remove button right-aligned X glyph slate-400 hover red-400
  ```
  Wire Remove click → `ws.queue_remove_file(file_id)`. Roles avatar pile is read-only on this tab.
- [ ] **Step 4: Run test, verify PASS.** Run: `cargo test -p quick-shell queue_remove_file_appends_id assigned_roles_for_returns_role_ids_with_any_perm`
- [ ] **Step 5: Commit.**
  ```bash
  git commit -m "feat(ws-settings): Files tab — file grid with assigned-role avatars"
  ```

---

### Task 5: GlobalSearch — leading search glyph + Cmd+K kbd chip + radius bump

**Files:**
- Modify: `quick-ui/src/widget/command_palette.rs` (function `CommandPalette::draw`, around line 239 where the palette card is filled)
- Test: `quick-ui/src/widget/command_palette.rs` `#[cfg(test)] mod tests`

**Concept ref:** `nirios-concept/src/components/GlobalSearch.tsx:113-153` — `max-w-2xl rounded-3xl` card; input row: leading lucide `Search` glyph + input + trailing `⌘K` kbd chip in `bg-white/5 border border-white/10`.

**Rust hooks (existing — reuse):**
- `theme.tokens.radii` — currently `radii.md = 8`. Add `radii.xl` if absent (or hard-code `24.0` for the palette).
- `quick_ui::icons::draw_icon` — `IconKind::Search` glyph (verify exists; otherwise add a kurbo magnifier as in `tabs.rs:511-535`).
- `command_palette.rs:239` — current `RoundedRect::from_rect(rect, theme.tokens.radii.md as f64)` line.

- [ ] **Step 1: Write failing test pinning the palette card radius is 24 px and that the input row reserves a leading 32 px icon area + a trailing 56 px kbd chip area.**
  ```rust
  #[test]
  fn palette_card_radius_is_24_px() {
      let palette = CommandPalette::new();
      assert_eq!(palette.surface_radius_px(), 24.0,
                 "concept palette uses rounded-3xl = 24 px");
  }
  #[test]
  fn input_row_layout_reserves_leading_icon_and_kbd_chip() {
      let palette = CommandPalette::new();
      let layout = palette.compute_input_layout(Rect::new(0.0, 0.0, 640.0, 56.0));
      assert!(layout.icon_rect.width() >= 24.0 && layout.icon_rect.width() <= 40.0);
      assert!(layout.kbd_chip_rect.width() >= 48.0,
              "Cmd+K chip needs at least 48 px");
      assert!(layout.kbd_chip_rect.x1 < 640.0,
              "kbd chip must be inside the row");
      assert!(layout.input_rect.x0 > layout.icon_rect.x1);
      assert!(layout.input_rect.x1 < layout.kbd_chip_rect.x0);
  }
  ```
- [ ] **Step 2: Run test, verify FAIL with `no method named surface_radius_px` / `no method named compute_input_layout`.**
  Run: `cargo test -p quick-ui palette_card_radius_is_24_px input_row_layout_reserves_leading_icon_and_kbd_chip`
- [ ] **Step 3: Add `surface_radius_px(&self) -> f64 { 24.0 }` + `pub struct InputRowLayout { icon_rect, input_rect, kbd_chip_rect }` + `compute_input_layout(row_rect)` returning `icon_rect = row.x0+12..x0+12+24`, `kbd_chip_rect = row.x1-12-48..row.x1-12`, `input_rect = (icon_rect.x1+12)..(kbd_chip_rect.x0-12)`. Update `draw`:**
  ```rust
  // around command_palette.rs:239 — change radius literal
  scene.fill(Fill::NonZero, Affine::IDENTITY,
             slate_900_alpha(0.70), None,
             &RoundedRect::from_rect(rect, self.surface_radius_px()));

  // After drawing the input field bg, paint the leading icon + kbd chip
  let row = self.input_row_rect();  // existing or new helper
  let layout = self.compute_input_layout(row);
  // 1. leading lucide Search glyph (slate-400 stroke)
  draw_icon(scene, IconKind::Search, layout.icon_rect, slate_400(), 1.5);
  // 2. trailing Cmd+K kbd chip
  let chip_rr = RoundedRect::from_rect(layout.kbd_chip_rect, 6.0);
  scene.fill(Fill::NonZero, Affine::IDENTITY, white_alpha(0.05), None, &chip_rr);
  scene.stroke(&Stroke::new(1.0), Affine::IDENTITY, white_alpha(0.10), None, &chip_rr);
  text_renderer.draw(scene, "⌘K", layout.kbd_chip_rect.center(),
                     11.0, TextWeight::Semibold, slate_300());
  ```
- [ ] **Step 4: Run test, verify PASS.** Run: `cargo test -p quick-ui palette_card_radius_is_24_px input_row_layout_reserves_leading_icon_and_kbd_chip`
- [ ] **Step 5: Commit.**
  ```bash
  git commit -m "feat(palette): leading search glyph + Cmd+K chip + 24 px radius"
  ```

---

### Task 6: GlobalSearch — filter strip + footer kbd hints

**Files:**
- Modify: `quick-ui/src/widget/command_palette.rs`
- Modify: `quick-shell/src/shell/search.rs` — add `filter_state: SearchFilter` (scope/filetypes/full_text/replace).
- Test: `quick-shell/src/shell/search.rs` `#[cfg(test)] mod tests`

**Concept ref:** `nirios-concept/src/components/GlobalSearch.tsx:184-244` (filter strip — scope chips, filetype multi-select, Full-Text toggle, Replace toggle) + `:274-280` (footer with ↑↓ Navigate / Enter Select chips + scope label right).

**Rust hooks (existing — reuse):**
- `ChromeState::search_scope` — already toggled by Workspace/All pills; subsume into `SearchFilter::scope`.
- `search.rs::set_context_kind` — context-aware filtering already wired but not surfaced.

- [ ] **Step 1: Write failing test pinning that toggling `full_text` and adding a `Filetype::Rust` filter both round-trip through `SearchFilter`.**
  ```rust
  #[test]
  fn search_filter_full_text_toggles() {
      let mut f = SearchFilter::default();
      assert!(!f.full_text);
      f.toggle_full_text();
      assert!(f.full_text);
      f.toggle_full_text();
      assert!(!f.full_text);
  }
  #[test]
  fn search_filter_filetypes_multi_select() {
      let mut f = SearchFilter::default();
      f.toggle_filetype(Filetype::Rust);
      f.toggle_filetype(Filetype::Markdown);
      assert!(f.filetypes.contains(&Filetype::Rust));
      assert!(f.filetypes.contains(&Filetype::Markdown));
      f.toggle_filetype(Filetype::Rust);
      assert!(!f.filetypes.contains(&Filetype::Rust));
  }
  ```
- [ ] **Step 2: Run test, verify FAIL.**
  Run: `cargo test -p quick-shell search_filter_full_text_toggles search_filter_filetypes_multi_select`
- [ ] **Step 3: Add `pub struct SearchFilter { scope: SearchScope, filetypes: HashSet<Filetype>, full_text: bool, replace: bool }` + `Default` + `toggle_full_text()` + `toggle_filetype()` to `search.rs`. In `command_palette.rs::draw`, after the input row, paint a 40 px filter strip:**
  ```rust
  // Filter strip — only when palette is wide enough OR scope != Workspace
  // Layout: scope chips (left, ~120 px), filetype chips (middle, flex), toggles (right ~160 px)
  // Each chip: rounded-full px-3 py-1, on=indigo-500/20 text-indigo-400, off=white/5 text-slate-400
  for (kind, label) in [(Scope::Workspace, "Workspace"), (Scope::All, "All Workspaces")] {
      // ...draw chip
  }
  // Filetype chips: Rust / TS / MD / IMG (subset) — 4 toggleable rounded-full
  // Full-Text toggle: a 36×20 pill, on=emerald-500
  // Replace toggle: same, on=orange-400
  ```
  After the results list, paint a 36 px footer band (slate-900/40 bg, 1 px slate-700 top border):
  ```rust
  // Footer:
  //   left: kbd chips for ↑↓ Navigate, Enter Select (each `bg-white/5 border-white/10
  //         px-1.5 py-0.5 rounded text-[10px] font-mono`)
  //   right: scope label "in Workspace" / "in All Workspaces" 11 px slate-400
  ```
  Wire chip clicks to `filter_state.toggle_*`.
- [ ] **Step 4: Run test, verify PASS.** Run: `cargo test -p quick-shell search_filter_full_text_toggles search_filter_filetypes_multi_select`
- [ ] **Step 5: Commit.**
  ```bash
  git commit -m "feat(palette): filter strip + footer kbd hints"
  ```

---

### Task 7: GlobalSearch — selected-row indigo-500 fill + per-row icon container

**Files:**
- Modify: `quick-ui/src/widget/command_palette.rs` (the result-row draw block, around lines 308-316 per audit)
- Test: `quick-ui/src/widget/command_palette.rs` `#[cfg(test)] mod tests`

**Concept ref:** `nirios-concept/src/components/GlobalSearch.tsx:251-258` — `${idx === selectedIndex ? 'bg-indigo-500 text-white' : 'hover:bg-white/5 text-slate-300'}`. Each row also has a leading icon container `p-2 rounded-xl bg-white/20` on selected, `bg-white/5` otherwise.

**Rust hooks (existing — reuse):**
- `theme.tokens.colors.accent[5]` — indigo-500 = `#6366f1`.
- Existing `result_row_rect(idx)` helper in `command_palette.rs` (or compute inline).
- `quick_ui::icons::draw_icon` for the per-row icon (kind from `SearchResult::kind`).

- [ ] **Step 1: Write failing test pinning that `result_row_fill(selected=true, theme)` returns `theme.tokens.colors.accent[5]` and not `theme.tokens.colors.neutral[4]`.**
  ```rust
  #[test]
  fn selected_row_fill_is_indigo_500_not_neutral_4() {
      let theme = Theme::default();
      let palette = CommandPalette::new();
      let fill_selected = palette.result_row_fill(true, &theme);
      let fill_idle     = palette.result_row_fill(false, &theme);
      assert_eq!(fill_selected, theme.tokens.colors.accent[5],
                 "selected row must be indigo-500 per concept");
      assert_ne!(fill_selected, theme.tokens.colors.neutral[4],
                 "must NOT use mid-grey neutral[4]");
      // idle is transparent or white/5 — anything but accent[5]
      assert_ne!(fill_idle, theme.tokens.colors.accent[5]);
  }
  ```
- [ ] **Step 2: Run test, verify FAIL with `no method named result_row_fill` or with the assertion equality failure (current code returns neutral[4]).**
  Run: `cargo test -p quick-ui selected_row_fill_is_indigo_500_not_neutral_4`
- [ ] **Step 3: Add `pub fn result_row_fill(&self, selected: bool, theme: &Theme) -> Color { if selected { theme.tokens.colors.accent[5] } else { transparent() } }`. In the result-row draw (around `command_palette.rs:308-316`), replace the existing `neutral[4]` fill with `self.result_row_fill(selected, theme)`. Then paint the leading icon container per row:**
  ```rust
  // For each result row:
  let row_rr = RoundedRect::from_rect(row_rect, 12.0);  // rounded-xl
  scene.fill(Fill::NonZero, Affine::IDENTITY,
             self.result_row_fill(selected, theme), None, &row_rr);
  // Icon container: 32×32 inset 8 px from row top-left
  let icon_box = Rect::new(row_rect.x0 + 12.0, row_rect.y0 + 8.0,
                            row_rect.x0 + 12.0 + 32.0, row_rect.y0 + 8.0 + 32.0);
  let icon_bg = if selected { white_alpha(0.20) } else { white_alpha(0.05) };
  scene.fill(Fill::NonZero, Affine::IDENTITY, icon_bg, None,
             &RoundedRect::from_rect(icon_box, 12.0));
  draw_icon(scene, result.icon_kind(), icon_box.inset(-6.0),
            if selected { white() } else { slate_300() }, 1.5);
  // Title (16 px Semibold) + subtitle (12 px slate-400) shifted 56 px from row.x0
  let text_color = if selected { white() } else { slate_300() };
  ```
- [ ] **Step 4: Run test, verify PASS.** Run: `cargo test -p quick-ui selected_row_fill_is_indigo_500_not_neutral_4`
- [ ] **Step 5: Commit.**
  ```bash
  git commit -m "feat(palette): selected-row indigo-500 fill + per-row icon container"
  ```

---

### Task 8: QuickSettings — action row + slider card + indigo toggle cards + Power Profile

**Files:**
- Modify: `quick-shell/src/shell/overlay_host.rs` (function `draw_quick_settings`, current lines ~2907-2926 per audit)
- Modify: `quick-shell/src/shell/quick_settings.rs` — add `power_profile: PowerProfile` enum + `cycle_power_profile()`; ensure `wifi_connection: Option<String>` and `bluetooth_device: Option<String>` exist for the sub-line text.
- Test: `quick-shell/src/shell/quick_settings.rs` `#[cfg(test)] mod tests`

**Concept ref:** `nirios-concept/src/components/QuickSettings.tsx:80-310` — popover `w-80 rounded-3xl` (24 px radius) bg `slate-900/80 backdrop-blur-md`, padding 16 px, gap 16 px between sections. Sections top→bottom:
1. **Action row** (`:181-201`): 3 equal-width buttons `flex-1 bg-white/10` — Settings, Camera, Power (Power tinted red).
2. **Slider card** (`:204-215`): `bg-white/5 p-3 rounded-2xl` containing brightness + volume sliders, each `h-1.5 rounded-full` thin bar, `bg-white` fill, leading Sun/Volume2 icons.
3. **Toggle card grid** (`:218-246`): 2-col grid; each card `bg-indigo-500 hover:bg-indigo-600 rounded-2xl p-4`, contents = Wi-Fi/Bluetooth glyph + chevron + label ("Wi-Fi"/"Bluetooth") + connection name sub-line.
4. **Power Profile row** (`:249-258`): single row `bg-white/5 p-3 rounded-2xl` w/ Battery icon + label + cycle chevron showing current profile.

**Rust hooks (existing — reuse):**
- `QuickSettings::brightness: f32`, `QuickSettings::volume: f32` (sliders) — already in `quick_settings.rs`.
- `QuickSettings::wifi_on: bool`, `QuickSettings::bluetooth_on: bool` — already wired toggles.
- `theme.tokens.radii.lg` (12 px now) — replace with literal 24.0 for popover, 16.0 for cards.

- [ ] **Step 1: Write failing test pinning `cycle_power_profile` cycles `PowerProfile::Saver → Balanced → Performance → Saver` and that `wifi_connection_label()` returns `"Not connected"` when disabled.**
  ```rust
  #[test]
  fn cycle_power_profile_cycles_through_three_states() {
      let mut q = QuickSettings::new();
      assert_eq!(q.power_profile, PowerProfile::Balanced);
      q.cycle_power_profile();
      assert_eq!(q.power_profile, PowerProfile::Performance);
      q.cycle_power_profile();
      assert_eq!(q.power_profile, PowerProfile::Saver);
      q.cycle_power_profile();
      assert_eq!(q.power_profile, PowerProfile::Balanced);
  }
  #[test]
  fn wifi_connection_label_is_empty_when_off() {
      let mut q = QuickSettings::new();
      q.wifi_on = false;
      assert_eq!(q.wifi_connection_label(), "Not connected");
  }
  ```
- [ ] **Step 2: Run test, verify FAIL.**
  Run: `cargo test -p quick-shell cycle_power_profile_cycles_through_three_states wifi_connection_label_is_empty_when_off`
- [ ] **Step 3: Add `PowerProfile { Saver, Balanced, Performance }` + `power_profile` field (default `Balanced`) + `cycle_power_profile()` + `wifi_connection_label()` returning the SSID or `"Not connected"`. Rewrite `draw_quick_settings`:**
  ```rust
  let popover_w = 320.0;  // w-80
  let popover_rect = anchor_below(quick_settings_btn_rect, popover_w);
  // Popover bg
  scene.fill(Fill::NonZero, Affine::IDENTITY, slate_900_alpha(0.80), None,
             &RoundedRect::from_rect(popover_rect, 24.0));
  scene.stroke(&Stroke::new(1.0), Affine::IDENTITY, slate_700(), None,
               &RoundedRect::from_rect(popover_rect, 24.0));
  let mut cursor_y = popover_rect.y0 + 16.0;
  // Section 1: 3-button action row, each 88×56 rounded-2xl bg-white/10
  let row_rect = Rect::new(popover_rect.x0 + 16.0, cursor_y,
                            popover_rect.x1 - 16.0, cursor_y + 56.0);
  let btn_w = (row_rect.width() - 16.0) / 3.0;  // 8 px gap
  for (i, (icon, tint)) in [(IconKind::Settings, white()),
                              (IconKind::Camera,  white()),
                              (IconKind::Power,   red_400())].iter().enumerate() {
      let x = row_rect.x0 + i as f64 * (btn_w + 8.0);
      let btn = Rect::new(x, row_rect.y0, x + btn_w, row_rect.y1);
      scene.fill(Fill::NonZero, Affine::IDENTITY, white_alpha(0.10), None,
                 &RoundedRect::from_rect(btn, 16.0));
      draw_icon(scene, *icon, btn.inset(-18.0), *tint, 1.5);
  }
  cursor_y += 56.0 + 16.0;
  // Section 2: slider card
  let slider_card = Rect::new(popover_rect.x0 + 16.0, cursor_y,
                                popover_rect.x1 - 16.0, cursor_y + 92.0);
  scene.fill(Fill::NonZero, Affine::IDENTITY, white_alpha(0.05), None,
             &RoundedRect::from_rect(slider_card, 16.0));
  // brightness row (Sun icon + h-1.5 bar)
  draw_slider_with_icon(scene, slider_card.with_y_range(top..top+36),
                         IconKind::Sun, q.brightness);
  // volume row (Volume2 icon)
  draw_slider_with_icon(scene, slider_card.with_y_range(top+44..top+80),
                         IconKind::Volume2, q.volume);
  cursor_y += 92.0 + 16.0;
  // Section 3: 2-col toggle card grid
  let grid_w = popover_rect.width() - 32.0;
  let card_w = (grid_w - 8.0) / 2.0;
  let card_h = 88.0;
  for (i, (kind, label, sub, on)) in [
      (IconKind::Wifi,      "Wi-Fi",     q.wifi_connection_label(),       q.wifi_on),
      (IconKind::Bluetooth, "Bluetooth", q.bluetooth_connection_label(), q.bluetooth_on),
  ].iter().enumerate() {
      let x = popover_rect.x0 + 16.0 + i as f64 * (card_w + 8.0);
      let card = Rect::new(x, cursor_y, x + card_w, cursor_y + card_h);
      let bg = if *on { indigo_500() } else { white_alpha(0.05) };
      scene.fill(Fill::NonZero, Affine::IDENTITY, bg, None,
                 &RoundedRect::from_rect(card, 16.0));
      // Glyph top-left, chevron top-right, label + sub-line bottom-left
      draw_icon(scene, *kind, card.with_topleft_inset(12.0, 12.0, 24.0), white(), 1.5);
      draw_icon(scene, IconKind::ChevronRight,
                card.with_topright_inset(12.0, 12.0, 16.0),
                white_alpha(0.60), 1.5);
      text_renderer.draw(scene, label, card.bottomleft_inset(12.0, 32.0),
                         13.0, TextWeight::Semibold, white());
      text_renderer.draw(scene, sub, card.bottomleft_inset(12.0, 12.0),
                         11.0, TextWeight::Regular, white_alpha(0.70));
  }
  cursor_y += card_h + 16.0;
  // Section 4: Power Profile row
  let pp_rect = Rect::new(popover_rect.x0 + 16.0, cursor_y,
                            popover_rect.x1 - 16.0, cursor_y + 48.0);
  scene.fill(Fill::NonZero, Affine::IDENTITY, white_alpha(0.05), None,
             &RoundedRect::from_rect(pp_rect, 16.0));
  draw_icon(scene, IconKind::Battery, pp_rect.left_inset(12.0, 24.0), white(), 1.5);
  text_renderer.draw(scene, "Power Profile", pp_rect.left_inset(48.0, mid),
                     13.0, TextWeight::Semibold, white());
  text_renderer.draw(scene, q.power_profile.label(),
                     pp_rect.right_inset(36.0, mid),
                     12.0, TextWeight::Medium, white_alpha(0.70));
  draw_icon(scene, IconKind::ChevronRight, pp_rect.right_inset(12.0, 16.0),
            white_alpha(0.60), 1.5);
  ```
  Wire toggle-card click → `q.wifi_on = !q.wifi_on` (or `q.toggle_wifi()`); chevron click on toggle card pushes `nav_stack` (Task 9). Power profile row click → `q.cycle_power_profile()`.
- [ ] **Step 4: Run test, verify PASS.** Run: `cargo test -p quick-shell cycle_power_profile_cycles_through_three_states wifi_connection_label_is_empty_when_off`
- [ ] **Step 5: Commit.**
  ```bash
  git commit -m "feat(quick-settings): action row + slider card + indigo toggle cards + power profile"
  ```

---

### Task 9: QuickSettings — nav_stack drill-in render for WiFi/BT lists

**Files:**
- Modify: `quick-shell/src/shell/overlay_host.rs` (extend `draw_quick_settings` to dispatch on `q.nav_stack.last()`)
- Modify: `quick-shell/src/shell/quick_settings.rs` — confirm `nav_stack: Vec<QsRoute>` exists; add `QsRoute::WifiList`/`QsRoute::BluetoothList` if missing; add demo `wifi_networks` + `bt_devices`.
- Test: `quick-shell/src/shell/quick_settings.rs` `#[cfg(test)] mod tests`

**Concept ref:** `nirios-concept/src/components/QuickSettings.tsx:101-162` — drill-in views: header with `← Back` chevron + title; list of network/device rows with name + signal/icon + check-mark on the connected one.

**Rust hooks (existing — reuse):**
- `QuickSettings::nav_stack` (per audit, already exists as field).
- `quick_ui::icons::draw_icon` IconKind::ChevronLeft / Wifi / Bluetooth / Check.

- [ ] **Step 1: Write failing test pinning that `push_route` / `pop_route` mutate the stack and that `current_route()` returns top.**
  ```rust
  #[test]
  fn push_pop_route_updates_current() {
      let mut q = QuickSettings::new();
      assert_eq!(q.current_route(), QsRoute::Root);
      q.push_route(QsRoute::WifiList);
      assert_eq!(q.current_route(), QsRoute::WifiList);
      q.pop_route();
      assert_eq!(q.current_route(), QsRoute::Root);
      // popping Root is a no-op
      q.pop_route();
      assert_eq!(q.current_route(), QsRoute::Root);
  }
  ```
- [ ] **Step 2: Run test, verify FAIL.**
  Run: `cargo test -p quick-shell push_pop_route_updates_current`
- [ ] **Step 3: Add `QsRoute { Root, WifiList, BluetoothList }` + `push_route` + `pop_route` + `current_route()` returning `Root` if empty. Add demo data `wifi_networks: Vec<WifiNetwork { ssid, signal, secure }>` and `bt_devices: Vec<BtDevice { name, connected }>`. In `draw_quick_settings`:**
  ```rust
  match q.current_route() {
      QsRoute::Root => { /* existing root layout from Task 8 */ }
      QsRoute::WifiList => draw_qs_wifi_list(scene, q, popover_rect, theme),
      QsRoute::BluetoothList => draw_qs_bt_list(scene, q, popover_rect, theme),
  }

  fn draw_qs_wifi_list(scene, q, rect, theme) {
      // Header row 48 px:
      //   ChevronLeft (24×24) at x0+12 — click pops route
      //   "Wi-Fi" label 14 px Semibold white centered
      // List rows 56 px each:
      //   Wifi icon (signal strength) + ssid 14 px Medium white + Lock icon if secure
      //   trailing Check icon if connected
      //   bg slate-800/50 first, transparent rest, hover white/5
  }
  fn draw_qs_bt_list(...) { /* same shape with Bluetooth + connected check */ }
  ```
  Wire chevron-back click in `handle_event` → `q.pop_route()`. Wire Wifi/BT toggle-card chevron click in Task 8 → `q.push_route(QsRoute::WifiList | BluetoothList)`.
- [ ] **Step 4: Run test, verify PASS.** Run: `cargo test -p quick-shell push_pop_route_updates_current`
- [ ] **Step 5: Commit.**
  ```bash
  git commit -m "feat(quick-settings): drill-in WiFi/BT lists with back chevron"
  ```

---

### Task 10: TopBar SystemMonitor — per-metric semantic colors

**Files:**
- Modify: `quick-shell/src/shell/topbar_chrome.rs` (around lines 632-711 per audit; the SystemMonitor draw branch + sparkline draw)
- Test: `quick-shell/src/shell/topbar_chrome.rs` `#[cfg(test)] mod tests`

**Concept ref:** `nirios-concept/src/components/TopBarWidgets.tsx:60-110` (label colors) + `:120-124` (sparkline color):
- CPU label `text-emerald-400` = `#4ade80`
- RAM label `text-blue-400` = `#60a5fa`
- TMP label `text-orange-400` = `#fb923c`
- Sparkline bars `bg-emerald-500/30` = `rgba(34,197,94,0.3)` (= `#22c55e` at 30% alpha — note: only behind CPU; per concept the sparkline switches color per active metric).

**Rust hooks (existing — reuse):**
- `topbar_chrome.rs::SystemMonitor` carousel slot — already wired with per-metric labels; only color is wrong.
- `kurbo::Color` constructors (or `ColorRgba8::new(R,G,B,A)`).

- [ ] **Step 1: Write failing test pinning `metric_label_color(MetricKind::Cpu)` returns emerald-400, RAM = blue-400, Tmp = orange-400 — NOT indigo.**
  ```rust
  #[test]
  fn system_monitor_metric_label_colors_are_semantic() {
      let chrome = TopBarChrome::new();
      let cpu  = chrome.metric_label_color(MetricKind::Cpu);
      let ram  = chrome.metric_label_color(MetricKind::Ram);
      let tmp  = chrome.metric_label_color(MetricKind::Tmp);
      // emerald-400
      assert_eq!(cpu, Color::rgba8(0x4a, 0xde, 0x80, 0xff));
      // blue-400
      assert_eq!(ram, Color::rgba8(0x60, 0xa5, 0xfa, 0xff));
      // orange-400
      assert_eq!(tmp, Color::rgba8(0xfb, 0x92, 0x3c, 0xff));
      // None of them are indigo-500
      let indigo = Color::rgba8(0x63, 0x66, 0xf1, 0xff);
      assert_ne!(cpu, indigo);
      assert_ne!(ram, indigo);
      assert_ne!(tmp, indigo);
  }
  #[test]
  fn sparkline_fill_is_emerald_500_at_30_percent_alpha() {
      let chrome = TopBarChrome::new();
      let fill = chrome.sparkline_fill(MetricKind::Cpu);
      assert_eq!(fill, Color::rgba8(0x22, 0xc5, 0x5e, 0x4d));  // 0x4d ≈ 0.30 * 255
  }
  ```
- [ ] **Step 2: Run test, verify FAIL with the assertion mismatch (current returns indigo).**
  Run: `cargo test -p quick-shell system_monitor_metric_label_colors_are_semantic sparkline_fill_is_emerald_500_at_30_percent_alpha`
- [ ] **Step 3: Add `pub fn metric_label_color(&self, k: MetricKind) -> Color` returning the three semantic colors. Add `pub fn sparkline_fill(&self, k: MetricKind) -> Color` returning the metric's semantic at 30% alpha (or always emerald-500/30 per the concept's CPU-first reading). Replace the inline color literals/`accent_primary` references in the SystemMonitor draw branch (`topbar_chrome.rs:632-711`) with these helpers.**
  ```rust
  pub fn metric_label_color(&self, k: MetricKind) -> Color {
      match k {
          MetricKind::Cpu => Color::rgba8(0x4a, 0xde, 0x80, 0xff),  // emerald-400
          MetricKind::Ram => Color::rgba8(0x60, 0xa5, 0xfa, 0xff),  // blue-400
          MetricKind::Tmp => Color::rgba8(0xfb, 0x92, 0x3c, 0xff),  // orange-400
      }
  }
  pub fn sparkline_fill(&self, _k: MetricKind) -> Color {
      // concept paints emerald-500/30 behind whichever metric is active
      Color::rgba8(0x22, 0xc5, 0x5e, 0x4d)
  }
  ```
  In the SystemMonitor draw block, swap the existing `theme.accent_primary` (and any `accent[5]`) used for CPU/RAM labels and the sparkline fill to call these helpers.
- [ ] **Step 4: Run test, verify PASS.** Run: `cargo test -p quick-shell system_monitor_metric_label_colors_are_semantic sparkline_fill_is_emerald_500_at_30_percent_alpha`
- [ ] **Step 5: Commit.**
  ```bash
  git commit -m "feat(top-bar): SystemMonitor per-metric emerald/blue/orange colors"
  ```

---

### Task 11: WindowFrame maximized chrome — auto-hide unless cursor.y ≤ 80

**Files:**
- Modify: `quick-ui/src/widget/window_group.rs` — the maximized-chrome paint branch (the floating title pill + restore + close cluster).
- Test: `quick-ui/src/widget/window_group.rs` `#[cfg(test)] mod tests`

**Concept ref:** `nirios-concept/src/components/WindowFrame.tsx:108-123` — `useEffect` watches `clientY ≤ 80` and toggles a `chromeVisible` boolean; the floating chrome only renders when true (with a 200 ms fade).

**Rust hooks (existing — reuse):**
- `WindowGroup::is_maximized: bool` and the existing maximized-chrome draw path (per parity-status §4 row "Maximized state").
- The widget is already passed pointer position via `handle_event`; capture it on `WindowEvent::CursorMoved`.

- [ ] **Step 1: Write failing test pinning that `should_show_maximized_chrome(cursor_y)` returns true for `y ≤ 80` and false for `y > 80`.**
  ```rust
  #[test]
  fn maximized_chrome_visible_when_cursor_y_le_80() {
      let mut wg = WindowGroup::new();
      wg.set_maximized(true);
      assert!(wg.should_show_maximized_chrome(Some(40.0)));
      assert!(wg.should_show_maximized_chrome(Some(80.0)));
      assert!(!wg.should_show_maximized_chrome(Some(81.0)));
      assert!(!wg.should_show_maximized_chrome(Some(400.0)));
      // cursor outside window entirely — hidden
      assert!(!wg.should_show_maximized_chrome(None));
  }
  #[test]
  fn maximized_chrome_always_hidden_when_not_maximized() {
      let mut wg = WindowGroup::new();
      wg.set_maximized(false);
      assert!(!wg.should_show_maximized_chrome(Some(20.0)));
  }
  ```
- [ ] **Step 2: Run test, verify FAIL with `no method named should_show_maximized_chrome`.**
  Run: `cargo test -p quick-ui maximized_chrome_visible_when_cursor_y_le_80 maximized_chrome_always_hidden_when_not_maximized`
- [ ] **Step 3: Add `last_cursor_y: Option<f64>` field on `WindowGroup` (set in `handle_event` on `CursorMoved`, cleared on `CursorLeft`). Add:**
  ```rust
  pub fn should_show_maximized_chrome(&self, cursor_y: Option<f64>) -> bool {
      self.is_maximized && cursor_y.map_or(false, |y| y <= 80.0)
  }
  ```
  In the existing maximized-chrome draw branch, gate every paint with:
  ```rust
  if !self.should_show_maximized_chrome(self.last_cursor_y) { return; }
  // existing pill + restore + close paint
  ```
  Optional polish: 200 ms ease-out fade — track `chrome_alpha: f32` ramping toward `if visible {1.0} else {0.0}` per redraw; multiply paint alphas by it. Bubble `needs_animation_frame()` while ramping.
- [ ] **Step 4: Run test, verify PASS.** Run: `cargo test -p quick-ui maximized_chrome_visible_when_cursor_y_le_80 maximized_chrome_always_hidden_when_not_maximized`
- [ ] **Step 5: Commit.**
  ```bash
  git commit -m "feat(window-group): auto-hide maximized chrome unless cursor.y <= 80"
  ```

---

### Task 12: TileHost search-active — compose 60 px indigo bloom sprite

**Files:**
- Modify: `quick-ui/src/widget/tile_host.rs` (around lines 100-103 for the sprite handle, and the `draw` path that currently composes the sprite only on `TileHost::focused` per audit spot-check #5)
- Test: `quick-ui/src/widget/tile_host.rs` `#[cfg(test)] mod tests`

**Concept ref:** `nirios-concept/src/components/WindowFrame.tsx:359-360` — when `isSearching` (the search-active state), the focused card receives `ring-4 ring-indigo-500/50 shadow-[0_0_60px_rgba(99,102,241,0.6)]`. The 60 px bloom is the headline visual.

**Rust hooks (existing — reuse):**
- `TileHost::indigo_bloom_sprite` precomputed at `tile_host.rs:100-103` — the Vello image-handle of a radial bloom.
- `TileHost::is_search_active: bool` (or equivalent flag, set from `ChromeState::search_active_tile_id`).
- Existing `draw_focused_glow` helper (or inline composition in `draw`) — currently uses two strokes for search-active and the sprite for focused.

- [ ] **Step 1: Write failing test pinning that `compose_search_active_glow(scene, ...)` is a no-op when `is_search_active = false` and pushes a sprite when true. Use a counting test wrapper.**
  ```rust
  #[test]
  fn search_active_glow_uses_bloom_sprite() {
      let mut tile = TileHost::test_fixture();
      tile.set_search_active(true);
      let count = tile.bloom_sprite_compose_count_for_test(/*search_active=*/true);
      assert_eq!(count, 1, "search-active path must compose bloom sprite once");
      let count_idle = tile.bloom_sprite_compose_count_for_test(/*search_active=*/false);
      assert_eq!(count_idle, 0, "idle path must not compose bloom");
  }
  #[test]
  fn search_active_glow_radius_is_60_px() {
      // The bloom sprite extends 60 px past the card on each side.
      let tile = TileHost::test_fixture();
      let card = Rect::new(100.0, 100.0, 300.0, 200.0);
      let bloom_rect = tile.search_active_bloom_rect(card);
      assert_eq!(bloom_rect.x0, card.x0 - 60.0);
      assert_eq!(bloom_rect.x1, card.x1 + 60.0);
      assert_eq!(bloom_rect.y0, card.y0 - 60.0);
      assert_eq!(bloom_rect.y1, card.y1 + 60.0);
  }
  ```
- [ ] **Step 2: Run test, verify FAIL with `no method named bloom_sprite_compose_count_for_test` / `no method named search_active_bloom_rect`.**
  Run: `cargo test -p quick-ui search_active_glow_uses_bloom_sprite search_active_glow_radius_is_60_px`
- [ ] **Step 3: Add `pub fn search_active_bloom_rect(&self, card: Rect) -> Rect { card.inset(-60.0) }`. Add a test-only counter `#[cfg(test)] bloom_sprite_compose_calls: Cell<u32>` and an instrumented compose helper. In `draw`, when `self.is_search_active`, call:**
  ```rust
  let bloom_rect = self.search_active_bloom_rect(card_rect);
  // The sprite's intrinsic radial alpha already ramps 1.0 → 0.0 at the edge,
  // so we just blit it scaled to bloom_rect with 0.6 multiplier (concept α).
  let xform = Affine::translate((bloom_rect.x0, bloom_rect.y0)) *
              Affine::scale_non_uniform(
                  bloom_rect.width()  / self.indigo_bloom_sprite_size.width,
                  bloom_rect.height() / self.indigo_bloom_sprite_size.height);
  scene.draw_image(&self.indigo_bloom_sprite, xform);  // existing alpha = 0.6 baked in
  // Then the existing two-stroke ring (8 px @ 24% outer, 4 px @ 50% inner)
  // remains for definition.
  #[cfg(test)] self.bloom_sprite_compose_calls.set(
      self.bloom_sprite_compose_calls.get() + 1);
  ```
  Keep the focused-tile compose path as-is. The two paths can both compose the sprite — they're distinct visual states (focused vs search-active) but both want the bloom.
- [ ] **Step 4: Run test, verify PASS.** Run: `cargo test -p quick-ui search_active_glow_uses_bloom_sprite search_active_glow_radius_is_60_px`
- [ ] **Step 5: Commit.**
  ```bash
  git commit -m "feat(tile-host): compose 60 px indigo bloom sprite on search-active"
  ```

---

## Self-review checklist

Before marking the plan complete, the executing engineer should:

- [ ] All 12 tasks have a failing-test step with concrete `assert_*` calls.
- [ ] Every concept reference cites a file:line range from `nirios-concept/src/components/`.
- [ ] Every Rust hook cites a file:line from `quick-shell/src/shell/` or `quick-ui/src/widget/`.
- [ ] Color literals use exact hex (no "indigo-ish" hand-waves).
- [ ] No task contains `TBD`, `implement appropriately`, or `tests for the above`.
- [ ] Commits are scoped per task; never `git add -A` from the workspace root (catches submodule pointer changes).

## Parallelism map

Tasks 1–4 (WorkspaceSettings) are sequential — Task 1 introduces the tab dispatch that Tasks 2–4 hang content off of. Tasks 2, 3, 4 can run as parallel subagents AFTER Task 1 lands.

Task 8 introduces the `QsRoute` push points used by Task 9. Task 9 must follow Task 8.

Tasks 5, 6, 7 all touch `command_palette.rs`. They're small enough to ship sequentially in one pass (Task 5 → 6 → 7), but the file-conflict risk is real if dispatched in parallel — treat as sequential.

Tasks 10, 11, 12 are independent and can run in parallel with each other and with Tasks 5–9.

Task dependency edges (must-precede):
- Task 1 → Tasks 2, 3, 4
- Task 8 → Task 9
- Task 5 → Task 6 → Task 7 (file conflict)

All other tasks are free to parallelise.
