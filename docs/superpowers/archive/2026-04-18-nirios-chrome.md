# Plan 2 — Nirios Shell Chrome

> **For agentic workers:** This plan is written as a strict step-by-step TDD recipe. Each task has a single, concrete deliverable and ends in a passing `cargo test -p <crate>` plus a single `git` commit. Do not skip ahead, do not combine tasks, and do not introduce placeholders / `todo!()` unless the step itself explicitly asks for one. Every code sample in this file is the literal minimum scaffold you should land — flesh out only what the TDD step demands, not more. When a parallel-safe group is marked in the Task DAG, the tasks inside it touch disjoint files and may be run concurrently by separate subagents; everything outside a group is strictly sequential.

**Spec:** `docs/superpowers/specs/2026-04-18-nirios-shell-design.md`
**Deferred tracker:** `docs/superpowers/deferred/nirios-shell.md`
**Predecessor:** `docs/superpowers/plans/2026-04-18-nirios-foundation.md` (Plan 1 of 4) — provides tokens, animation, overlay, dnd, `Tabs`/`TabBar`, `Scrollable`, extended `Widget` trait (`presence_key`, `permission_scope`, `accepts_drop`, `on_drop`, `wants_pointer_capture`, `draw(..., &PresenceCtx)`), `WorkspaceTree`/`Workspace`/`Strip`/`WindowGroup`/`Tab`/`TileContent`, and the `nirios::Shell` root widget.
**Successor:** Plan 3 (apps — terminal `AppWidget`, file-explorer port, browser stub) and Plan 4 (collab hardening).

## Goal

Land the visible shell chrome on top of Plan 1's foundation: OverviewView, GlobalSearch (built on a new `CommandPalette` toolkit primitive), QuickSettings popover, WorkspaceSettings modal (built on a new `DataTable` toolkit primitive), TopBar, SidebarWheel with overscroll-create gesture, presence-driven remote-cursor overlay, and the `ContextMenu` / `Tooltip` / `Toast` toolkit primitives — each landed with its first shell consumer so no primitive ships dead.

**Demoable end-state.** After the last task:

- `cargo run -p quick-desktop` shows the TopBar with a live clock, CPU/RAM sparkline, and calendar cycled by wheel (200 ms cooldown).
- **Cmd+Tab** toggles `OverviewView`; clicking a card activates that workspace.
- **Cmd+K** opens `GlobalSearch` with context-aware filtering and fuzzy match.
- The RightTray QuickSettings button opens a popover with interactive brightness/volume sliders and a multi-level WiFi/BT drill-in.
- The workspace-name button opens `WorkspaceSettings` with three tabs; the Roles tab edits permissions via `DataTable` and persists to `WorkspaceTree`.
- The left `SidebarWheel` switches strips on wheel and creates a new workspace on overscroll past threshold with a visible "New session" hint.
- Right-click opens a context menu; hover surfaces a tooltip after a reveal delay; a toast fires on workspace creation.
- A simulated remote cursor floats over the active tile, driven by `PresenceCtx`.

## Architecture

All new toolkit primitives (`CommandPalette`, `DataTable`, `ContextMenu`, `Tooltip`, `Toast`) live in `quick-ui/src/widget/`. All new shell chrome modules live in `quick-shell/src/shell/`. Shell chrome is composed by the existing `nirios::Shell` root widget from Plan 1 — we only add children, never rewrite the root. The `PresenceCtx` scaffold from Plan 1 is populated with a `remote_cursors: Vec<RemoteCursor>` field and is fed by a simulated feed module (`shell::remote_cursors::SimulatedFeed`) so Plan 4 can swap to the real `CollaborationManager` source without touching widgets.

Every new widget implements the extended `Widget` contract from Plan 1:

- `presence_key` returns a `PresenceKey::Widget { path }` derived from the widget's CRDT path (empty path for chrome that has no CRDT surface — OverviewView, QuickSettings).
- `permission_scope` returns `Some(PermissionScope { resource: Workspace(ws), op: Admin })` only on the WorkspaceSettings role/file tabs; everything else is unscoped.
- `accepts_drop` is `false` for Plan-2 widgets (tab drag is Plan 1).
- `wants_pointer_capture` is `true` for `SidebarWheel` while an overscroll gesture is active, and for `DataTable` column-resize drags.

## Tech stack

- No new workspace dependencies. Uses only crates already present in Plan 1 (`automerge`, `taffy`, `vello`, `cosmic-text`, `winit`, `wgpu`, `serde`, `serde_json`, `uuid`, `sysinfo` — `sysinfo` is added here for the TopBar system-monitor sparkline).
- **Sole new dep:** `sysinfo = "0.32"` added to `quick-shell/Cargo.toml` for CPU/RAM readings.
- `cargo test -p quick-ui` and `cargo test -p quick-desktop` are the two test targets; neither test ever requires a GPU (all rendering goes through a headless Vello scene buffer).

## File Structure

New files added by this plan:

```
quick-ui/src/widget/
  command_palette.rs       # CommandPalette + CommandEntry (new primitive)
  data_table.rs            # DataTable + ColumnDef + ColumnWidth (new primitive)
  context_menu.rs          # ContextMenu primitive
  tooltip.rs               # Tooltip primitive
  toast.rs                 # Toast primitive + ToastStack

quick-ui/tests/
  command_palette_test.rs
  data_table_test.rs
  context_menu_test.rs
  tooltip_test.rs
  toast_test.rs

quick-shell/src/shell/
  overview.rs              # OverviewView
  search.rs                # GlobalSearch (uses CommandPalette)
  quick_settings.rs        # QuickSettings popover
  workspace_settings.rs    # WorkspaceSettings modal (uses DataTable)
  topbar.rs                # TopBar with LeftNav/CenterWidgets/RightTray
  sidebar_wheel.rs         # SidebarWheel + overscroll-create gesture
  remote_cursors.rs        # PresenceCtx-driven overlay + SimulatedFeed
  system_monitor.rs        # sysinfo-backed CPU/RAM sparkline

quick-shell/tests/
  shell_chrome_test.rs     # shell-chrome interaction tests
```

Files modified:

```
quick-ui/src/widget/mod.rs          # pub mod + re-exports for new primitives
quick-ui/src/lib.rs                 # re-exports
quick-shell/src/shell/mod.rs  # wire new modules into nirios::Shell
quick-shell/src/shell/state.rs # add Role/RolePermissions/WorkspaceFile if Plan 1 didn't finalize
quick-shell/Cargo.toml        # add sysinfo = "0.32"
quick-ui/src/presence/mod.rs        # populate PresenceCtx with remote_cursors field
```

## Task DAG for Concurrent Execution

All task numbers below reference the numbered tasks in this plan. Tasks within a `{group}` touch disjoint files and are safe to dispatch to parallel subagents; groups themselves are strictly ordered.

```
G1  (sequential):  T1 (sysinfo dep bump, PresenceCtx field) → T2 (Role/RolePermissions/WorkspaceFile types in shell state)

G2  (parallel, toolkit primitives — disjoint widget files):
     T3  CommandPalette primitive
     T4  DataTable primitive (ColumnWidth + sort)
     T5  DataTable column resize
     T6  DataTable row virtualization
     T7  ContextMenu primitive
     T8  Tooltip primitive
     T9  Toast + ToastStack primitive

G3  (parallel, shell-side consumers — each touches a separate shell file):
     T10 system_monitor module (sysinfo sparkline)
     T11 OverviewView
     T12 GlobalSearch (uses CommandPalette from T3)
     T13 QuickSettings popover level 1 (brightness/volume sliders)
     T14 QuickSettings multi-level drill-in (WiFi/BT)
     T15 WorkspaceSettings — General tab
     T16 WorkspaceSettings — Roles tab (uses DataTable from T4)
     T17 WorkspaceSettings — Files tab (uses DataTable from T4)

G4  (sequential — TopBar composes CenterWidgets which depends on T10):
     T18 TopBar LeftNav (overview toggle, workspace up/down, ws name button)
     T19 TopBar CenterWidgets (clock + sparkline + calendar, wheel cycle, 200ms cooldown)
     T20 TopBar RightTray (collaborators + QuickSettings trigger)

G5  (sequential — SidebarWheel needs Spring from Plan 1 and the overscroll gesture is load-bearing):
     T21 SidebarWheel skeleton (static wheel, wheel-to-switch-strip)
     T22 SidebarWheel overscroll-create gesture (hint + commit + snap-back)
     T23 SidebarWheel Workspace-Management button entrypoint

G6  (sequential — remote cursors):
     T24 PresenceCtx remote_cursors population + RemoteCursor struct
     T25 SimulatedFeed driving remote cursors
     T26 remote_cursors.rs overlay widget over active tile

G7  (sequential — wire-up + integration tests + shell demo):
     T27 Wire chrome into nirios::Shell root widget
     T28 Shell-chrome interaction tests (Cmd+K, Cmd+Tab, overscroll, quick-settings)
     T29 Toast wiring: fire on workspace create + workspace rename
     T30 ContextMenu wiring: right-click on tile → default menu
     T31 Tooltip wiring: topbar ws-name button tooltip
     T32 Visual smoke: `cargo run -p quick-desktop` manual checklist doc inside module comments
     T33 Self-review checklist run-through + README snippets
```

Subagents may take G2 in parallel (8 tasks across 8 files), and G3 in parallel (8 tasks across 8 files). G4–G7 are sequential.

---

## Tasks

### T1 — Bump deps, extend `PresenceCtx`

**Files:** `quick-shell/Cargo.toml`, `quick-ui/src/presence/mod.rs`

**Test (write first, must fail):** In `quick-ui/tests/presence_remote_cursors_test.rs`:

```rust
use quick_ui::presence::{PresenceCtx, RemoteCursor};

#[test]
fn presence_ctx_holds_remote_cursors() {
    let mut ctx = PresenceCtx::default();
    ctx.remote_cursors.push(RemoteCursor {
        user_id: "peer-1".into(),
        color: [1.0, 0.3, 0.2, 1.0],
        position: [120.0, 80.0],
        label: "Ada".into(),
    });
    assert_eq!(ctx.remote_cursors.len(), 1);
    assert_eq!(ctx.remote_cursors[0].user_id, "peer-1");
}
```

**Run:** `cargo test -p quick-ui presence_ctx_holds_remote_cursors` — expect compile error: `RemoteCursor` missing.

**Impl:** In `quick-ui/src/presence/mod.rs`, append (Plan 1 created this file with an empty `PresenceCtx`):

```rust
#[derive(Debug, Clone)]
pub struct RemoteCursor {
    pub user_id: String,
    pub color: [f32; 4],
    pub position: [f32; 2], // scene-space coords
    pub label: String,
}

#[derive(Debug, Default, Clone)]
pub struct PresenceCtx {
    pub remote_cursors: Vec<RemoteCursor>,
    // ... Plan 1 fields preserved
}
```

(If Plan 1 defined `PresenceCtx` with `#[non_exhaustive]`, keep that; we're only adding a field.)

Then add to `quick-shell/Cargo.toml` under `[dependencies]`:

```toml
sysinfo = "0.32"
```

**Run:** `cargo test -p quick-ui presence_ctx_holds_remote_cursors` — must pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-ui/src/presence/mod.rs quick-ui/tests/presence_remote_cursors_test.rs quick-shell/Cargo.toml && git -C /var/home/user/Development/quickdesktop commit -m "chore: extend PresenceCtx with remote_cursors + add sysinfo dep"`

---

### T2 — `Role`, `RolePermissions`, `WorkspaceFile` types in shell state

**File:** `quick-shell/src/shell/state.rs`

**Test (write first):** In `quick-shell/tests/workspace_settings_state_test.rs`:

```rust
use quick_desktop::shell::state::{Role, RolePermissions, WorkspaceFile, Workspace, WorkspaceId};

#[test]
fn role_permissions_round_trip() {
    let mut ws = Workspace::new(WorkspaceId::new(), "Design");
    ws.roles.push(Role {
        id: "role-1".into(),
        name: "Editor".into(),
        permissions: RolePermissions {
            can_invite: false,
            can_edit_files: true,
            per_app: Default::default(),
            per_file: Default::default(),
        },
    });
    ws.workspace_files.push(WorkspaceFile {
        id: "file-1".into(),
        name: "brief.md".into(),
        mime: "text/markdown".into(),
    });
    assert_eq!(ws.roles[0].name, "Editor");
    assert_eq!(ws.workspace_files[0].name, "brief.md");
}
```

**Run:** `cargo test -p quick-desktop role_permissions_round_trip` — fail.

**Impl:** Append to `quick-shell/src/shell/state.rs`:

```rust
use std::collections::HashMap;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Role {
    pub id: String,
    pub name: String,
    pub permissions: RolePermissions,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RolePermissions {
    pub can_invite: bool,
    pub can_edit_files: bool,
    pub per_app: HashMap<String, AppPerm>,   // key = AppKind tag
    pub per_file: HashMap<String, FilePerm>, // key = WorkspaceFile.id
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub enum AppPerm { #[default] None, View, Edit, Admin }

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub enum FilePerm { #[default] None, Read, Write }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceFile {
    pub id: String,
    pub name: String,
    pub mime: String,
}
```

Confirm `Workspace` already has `roles: Vec<Role>` and `workspace_files: Vec<WorkspaceFile>` (Plan 1 declared these in §5 of the spec). If Plan 1 left them as `Vec<()>` placeholders, replace with the concrete types here.

**Run:** `cargo test -p quick-desktop role_permissions_round_trip` — pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/state.rs quick-shell/tests/workspace_settings_state_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): Role/RolePermissions/WorkspaceFile concrete types"`

---

### T3 — `CommandPalette` primitive (fuzzy match + select)

**File:** `quick-ui/src/widget/command_palette.rs`

**Test (write first):** `quick-ui/tests/command_palette_test.rs`:

```rust
use quick_ui::widget::command_palette::{CommandPalette, CommandEntry};
use std::sync::{Arc, Mutex};

#[test]
fn command_palette_filters_by_query() {
    let mut p = CommandPalette::new();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let h = hits.clone();
    p.push(CommandEntry::new("ws.create", "Create Workspace")
        .on_select(move |_, _| h.lock().unwrap().push("create")));
    let h = hits.clone();
    p.push(CommandEntry::new("ws.rename", "Rename Workspace")
        .on_select(move |_, _| h.lock().unwrap().push("rename")));
    p.set_query("ren");
    assert_eq!(p.visible_entries().len(), 1);
    assert_eq!(p.visible_entries()[0].label, "Rename Workspace");
}

#[test]
fn command_palette_custom_filter_is_composed() {
    let mut p = CommandPalette::new();
    p.push(CommandEntry::new("file.open", "Open File"));
    p.push(CommandEntry::new("ws.create", "Create Workspace"));
    p.set_filter(Box::new(|e| e.id.starts_with("ws.")));
    p.set_query("create");
    assert_eq!(p.visible_entries().len(), 1);
    assert_eq!(p.visible_entries()[0].id, "ws.create");
}
```

**Run:** fail.

**Impl:** Create the file with `CommandPalette` holding `Vec<CommandEntry>`, a cached filtered index, and a fuzzy score (use `subsequence score = match_count - gap_penalty`, ≤ 60 lines). Implement the four public methods in the signature block. `Widget` impl: draws a popover-shaped rectangle with a text-input row on top and a vertical list of visible entries; consumes `Up`/`Down`/`Enter`/`Esc` through `handle_event`. Enter invokes `on_select(state, path)`.

Reference snippet (excerpt):

```rust
pub struct CommandPalette {
    entries: Vec<CommandEntry>,
    query: String,
    filter: Option<Box<dyn Fn(&CommandEntry) -> bool + Send + Sync>>,
    visible: Vec<usize>,
    cursor: usize,
    // render caches omitted
}

impl CommandPalette {
    pub fn new() -> Self { /* ... */ }
    pub fn push(&mut self, e: CommandEntry) { self.entries.push(e); self.rebuild(); }
    pub fn set_query(&mut self, q: &str) { self.query = q.into(); self.rebuild(); }
    pub fn set_filter(&mut self, f: Box<dyn Fn(&CommandEntry) -> bool + Send + Sync>) {
        self.filter = Some(f); self.rebuild();
    }
    pub fn visible_entries(&self) -> Vec<&CommandEntry> {
        self.visible.iter().map(|&i| &self.entries[i]).collect()
    }
    fn rebuild(&mut self) {
        self.visible.clear();
        for (i, e) in self.entries.iter().enumerate() {
            if let Some(f) = &self.filter { if !f(e) { continue; } }
            if fuzzy_score(&self.query, &e.label).is_none() { continue; }
            self.visible.push(i);
        }
    }
}
```

Include a simple `fuzzy_score(query, text) -> Option<i32>` returning `Some(0)` when `query` is empty, and `None` when not a subsequence.

**Run:** `cargo test -p quick-ui --test command_palette_test` — pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-ui/src/widget/command_palette.rs quick-ui/tests/command_palette_test.rs quick-ui/src/widget/mod.rs quick-ui/src/lib.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(quick-ui): CommandPalette primitive with fuzzy match + filter"`

---

### T4 — `DataTable` primitive skeleton + sortable columns

**File:** `quick-ui/src/widget/data_table.rs`

**Test (write first):** `quick-ui/tests/data_table_test.rs`:

```rust
use quick_ui::widget::data_table::{DataTable, ColumnDef, ColumnWidth};

#[derive(Clone)]
struct Row { name: String, size: u64 }

fn cols() -> Vec<ColumnDef<Row>> {
    vec![
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
            .sortable(true)
            .with_render(Box::new(|_r, _s, _t| {})),
        ColumnDef::new("size", "Size", ColumnWidth::Fixed(80.0))
            .sortable(true)
            .with_render(Box::new(|_r, _s, _t| {})),
    ]
}

#[test]
fn data_table_sorts_ascending_and_descending() {
    let mut t = DataTable::new(cols());
    t.set_rows(vec![
        Row { name: "c".into(), size: 30 },
        Row { name: "a".into(), size: 10 },
        Row { name: "b".into(), size: 20 },
    ]);
    t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", true);
    let v = t.view_rows();
    assert_eq!(v[0].name, "a");
    t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", false);
    let v = t.view_rows();
    assert_eq!(v[0].name, "c");
}
```

**Run:** fail.

**Impl:** Implement the struct per the locked signature in the prompt. Since `sort_by(&mut self, column_key: &str, ascending: bool)` needs per-column comparators, expose a typed helper `sort_by_with<F: Fn(&Row,&Row)->Ordering>(&mut self, cmp: F, key: &str, ascending: bool)` that stores the last sort key/direction and reapplies it when rows change.

Include:

```rust
pub enum ColumnWidth { Fixed(f32), Flex(f32), Auto }

pub struct ColumnDef<Row> {
    pub key: String,
    pub header: String,
    pub width: ColumnWidth,
    pub sortable: bool,
    pub render_cell: Box<dyn Fn(&Row, &mut vello::Scene, &quick_ui::theme::Theme) + Send + Sync>,
    pub(crate) resize_handle_px: f32, // 6.0 default, used by T5
}

impl<Row> ColumnDef<Row> {
    pub fn new(key: impl Into<String>, header: impl Into<String>, width: ColumnWidth) -> Self;
    pub fn sortable(mut self, v: bool) -> Self { self.sortable = v; self }
    pub fn with_render(
        mut self,
        f: Box<dyn Fn(&Row, &mut vello::Scene, &quick_ui::theme::Theme) + Send + Sync>,
    ) -> Self { self.render_cell = f; self }
}
```

Widget impl renders a header row (tokens from Plan 1: `Theme::tokens.surface_raised`, `Theme::tokens.border_subtle`) + body rows. Header click toggles sort direction and calls `sort_by`.

**Run:** `cargo test -p quick-ui --test data_table_test` — pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-ui/src/widget/data_table.rs quick-ui/tests/data_table_test.rs quick-ui/src/widget/mod.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(quick-ui): DataTable primitive with sortable columns"`

---

### T5 — `DataTable` resizable columns

**File:** `quick-ui/src/widget/data_table.rs`

**Test (add to `data_table_test.rs`):**

```rust
#[test]
fn data_table_column_resize_updates_width() {
    let mut t = DataTable::new(cols());
    assert_eq!(t.column_width_px("size"), Some(80.0));
    t.begin_resize("size", 80.0);
    t.drag_resize(95.0); // delta = +15
    t.end_resize();
    assert_eq!(t.column_width_px("size"), Some(95.0));
    // clamped to >= min
    t.begin_resize("size", 95.0);
    t.drag_resize(10.0);
    t.end_resize();
    assert_eq!(t.column_width_px("size"), Some(DataTable::<Row>::MIN_COL_WIDTH));
}
```

**Run:** fail.

**Impl:** Add `begin_resize`, `drag_resize`, `end_resize`, `column_width_px`, and `const MIN_COL_WIDTH: f32 = 40.0`. Store resolved per-column pixel widths in a `HashMap<String, f32>`. In `handle_event` map pointer press on the right edge (`resize_handle_px`-wide zone) to `begin_resize`, pointer move to `drag_resize`, release to `end_resize`. Return `true` from `wants_pointer_capture` while a resize is active.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-ui/src/widget/data_table.rs quick-ui/tests/data_table_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(quick-ui): DataTable resizable columns with min-width clamp"`

---

### T6 — `DataTable` row virtualization

**File:** `quick-ui/src/widget/data_table.rs`

**Test:**

```rust
#[test]
fn data_table_virtualization_windows_rows() {
    let mut t = DataTable::new(cols());
    let rows: Vec<Row> = (0..10_000).map(|i| Row { name: format!("r{i}"), size: i as u64 }).collect();
    t.set_rows(rows);
    t.set_viewport(500.0, 24.0);        // 500px tall, 24px row height
    t.set_scroll_y(4800.0);             // scrolled down 200 rows
    let (start, end) = t.visible_row_range();
    assert!(start >= 195 && start <= 205, "start={start}");
    // (viewport / row_height) + small overscan
    assert!(end - start >= 20 && end - start <= 32, "range size = {}", end - start);
}
```

**Run:** fail.

**Impl:** Add `set_viewport(h, row_h)`, `set_scroll_y(y)`, `visible_row_range() -> (usize, usize)` that returns `start = floor(scroll / row_h)` with 2-row overscan each side, `end = min(start + ceil(vp / row_h) + 4, len)`. In `draw`, loop only over `start..end`.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-ui/src/widget/data_table.rs quick-ui/tests/data_table_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(quick-ui): DataTable virtualized row rendering"`

---

### T7 — `ContextMenu` primitive

**File:** `quick-ui/src/widget/context_menu.rs`

**Test:** `quick-ui/tests/context_menu_test.rs`:

```rust
use quick_ui::widget::context_menu::{ContextMenu, MenuItem};

#[test]
fn context_menu_shows_and_invokes_action() {
    let mut m = ContextMenu::new();
    let invoked = std::sync::Arc::new(std::sync::Mutex::new(false));
    let i = invoked.clone();
    m.push(MenuItem::action("Open", move || *i.lock().unwrap() = true));
    m.push(MenuItem::separator());
    m.push(MenuItem::action("Delete", || {}));
    assert_eq!(m.items().len(), 3);
    m.open_at([10.0, 20.0]);
    assert!(m.is_open());
    assert_eq!(m.position(), Some([10.0, 20.0]));
    m.click_item(0);
    assert!(*invoked.lock().unwrap());
    assert!(!m.is_open()); // closes after action
}
```

**Run:** fail.

**Impl:** Define `MenuItem::Action { label, action: Box<dyn Fn() + Send + Sync> }` and `MenuItem::Separator`. `ContextMenu` with `open_at(p: [f32;2])`, `close()`, `is_open() -> bool`, `position() -> Option<[f32;2]>`, `click_item(idx: usize)`. Widget impl renders at stored position when `is_open`, backed by Plan 1's `overlay::Popover`.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-ui/src/widget/context_menu.rs quick-ui/tests/context_menu_test.rs quick-ui/src/widget/mod.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(quick-ui): ContextMenu primitive"`

---

### T8 — `Tooltip` primitive with reveal delay

**File:** `quick-ui/src/widget/tooltip.rs`

**Test:** `quick-ui/tests/tooltip_test.rs`:

```rust
use quick_ui::widget::tooltip::Tooltip;
use std::time::Duration;

#[test]
fn tooltip_reveals_after_delay() {
    let mut t = Tooltip::new("Open workspace settings");
    assert!(!t.is_visible());
    t.on_hover_enter(Duration::from_millis(0));
    t.tick(Duration::from_millis(200));
    assert!(!t.is_visible());
    t.tick(Duration::from_millis(500)); // cumulative 700ms > 600ms default
    assert!(t.is_visible());
    t.on_hover_leave();
    assert!(!t.is_visible());
}

#[test]
fn tooltip_custom_delay() {
    let mut t = Tooltip::new("x").with_delay(Duration::from_millis(100));
    t.on_hover_enter(Duration::from_millis(0));
    t.tick(Duration::from_millis(150));
    assert!(t.is_visible());
}
```

**Run:** fail.

**Impl:** Store `label: String`, `delay: Duration (default 600ms)`, `hover_start: Option<Instant/Duration>`, `is_visible: bool`. In `tick(elapsed)` accumulate and flip `is_visible` once crossed.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-ui/src/widget/tooltip.rs quick-ui/tests/tooltip_test.rs quick-ui/src/widget/mod.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(quick-ui): Tooltip primitive with 600ms reveal delay"`

---

### T9 — `Toast` + `ToastStack`

**File:** `quick-ui/src/widget/toast.rs`

**Test:** `quick-ui/tests/toast_test.rs`:

```rust
use quick_ui::widget::toast::{ToastStack, ToastKind};
use std::time::Duration;

#[test]
fn toast_stack_expires_entries() {
    let mut s = ToastStack::new();
    s.push("Workspace created", ToastKind::Info);
    s.push("Save failed", ToastKind::Error);
    assert_eq!(s.len(), 2);
    s.tick(Duration::from_millis(3100));   // default 3s TTL elapsed
    assert_eq!(s.len(), 0);
}

#[test]
fn toast_stack_caps_entries() {
    let mut s = ToastStack::new().with_max(3);
    for i in 0..5 { s.push(format!("t{i}"), ToastKind::Info); }
    assert_eq!(s.len(), 3);
    // oldest dropped, newest retained
    assert!(s.iter().any(|t| t.text == "t4"));
    assert!(!s.iter().any(|t| t.text == "t0"));
}
```

**Run:** fail.

**Impl:** `ToastKind { Info, Success, Warning, Error }`. `Toast { text: String, kind: ToastKind, ttl_remaining: Duration }`. `ToastStack` with `new`, `with_max(n)`, `push(text, kind)`, `tick(dt)`, `len`, `iter`. Widget impl draws stacked top-right with Plan-1 tokens (`motion::spring::snappy` for enter).

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-ui/src/widget/toast.rs quick-ui/tests/toast_test.rs quick-ui/src/widget/mod.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(quick-ui): Toast + ToastStack with TTL and cap"`

---

### T10 — `system_monitor` module (sysinfo-backed sparkline source)

**File:** `quick-shell/src/shell/system_monitor.rs`

**Test:** `quick-shell/tests/system_monitor_test.rs`:

```rust
use quick_desktop::shell::system_monitor::{SystemMonitor, SparkSample};

#[test]
fn system_monitor_collects_samples() {
    let mut m = SystemMonitor::new(60);
    m.refresh();
    m.refresh();
    assert!(m.samples().len() >= 1);
    let s: &SparkSample = m.samples().last().unwrap();
    assert!(s.cpu_pct >= 0.0 && s.cpu_pct <= 100.0);
    assert!(s.ram_pct >= 0.0 && s.ram_pct <= 100.0);
}
```

**Run:** fail.

**Impl:**

```rust
use sysinfo::System;

pub struct SparkSample { pub cpu_pct: f32, pub ram_pct: f32 }

pub struct SystemMonitor {
    sys: System,
    samples: std::collections::VecDeque<SparkSample>,
    cap: usize,
}

impl SystemMonitor {
    pub fn new(cap: usize) -> Self { Self { sys: System::new_all(), samples: Default::default(), cap } }
    pub fn refresh(&mut self) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let cpu = self.sys.global_cpu_usage();
        let ram = (self.sys.used_memory() as f32 / self.sys.total_memory() as f32) * 100.0;
        if self.samples.len() == self.cap { self.samples.pop_front(); }
        self.samples.push_back(SparkSample { cpu_pct: cpu, ram_pct: ram });
    }
    pub fn samples(&self) -> Vec<&SparkSample> { self.samples.iter().collect() }
}
```

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/system_monitor.rs quick-shell/tests/system_monitor_test.rs quick-shell/src/shell/mod.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): sysinfo-backed SystemMonitor for topbar sparkline"`

---

### T11 — `OverviewView` (vertical list of workspace mini-cards)

**File:** `quick-shell/src/shell/overview.rs`

**Test:** Add to `quick-shell/tests/shell_chrome_test.rs`:

```rust
use quick_desktop::shell::overview::OverviewView;
use quick_desktop::shell::state::{WorkspaceTree, Workspace, WorkspaceId};

#[test]
fn overview_view_click_activates_workspace() {
    let mut tree = WorkspaceTree::default();
    let a = Workspace::new(WorkspaceId::new(), "Alpha");
    let b = Workspace::new(WorkspaceId::new(), "Beta");
    let b_id = b.id;
    tree.workspaces = vec![a, b];
    tree.active = tree.workspaces[0].id;

    let mut ov = OverviewView::new();
    ov.sync(&tree);
    assert_eq!(ov.card_count(), 2);
    ov.click_card(1, &mut tree);
    assert_eq!(tree.active, b_id);
}
```

**Run:** fail.

**Impl:** `OverviewView` stores `cards: Vec<CardCache>`. `sync(&WorkspaceTree)` rebuilds cards (id + name + strip count). `card_count`, `click_card(idx, &mut WorkspaceTree)` sets `tree.active`. Widget impl: vertical `Column` of `Container`s sized per card; active card renders with accent border.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/overview.rs quick-shell/tests/shell_chrome_test.rs quick-shell/src/shell/mod.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): OverviewView vertical list of workspace cards"`

---

### T12 — `GlobalSearch` (uses `CommandPalette`, Cmd+K toggle)

**File:** `quick-shell/src/shell/search.rs`

**Test:** Add to `shell_chrome_test.rs`:

```rust
use quick_desktop::shell::search::GlobalSearch;

#[test]
fn global_search_seeds_commands_and_filters() {
    let mut s = GlobalSearch::new();
    s.seed_default_commands(); // "Create Workspace", "Rename Workspace", "Open Settings", ...
    s.palette_mut().set_query("create");
    assert!(s.palette().visible_entries().iter().any(|e| e.label.contains("Create")));
}

#[test]
fn global_search_context_filter_narrows_results() {
    let mut s = GlobalSearch::new();
    s.seed_default_commands();
    s.set_context_kind(quick_desktop::shell::search::SearchContext::Files);
    let labels: Vec<String> = s.palette().visible_entries().iter().map(|e| e.label.clone()).collect();
    assert!(labels.iter().all(|l| l.to_lowercase().contains("file") || l.to_lowercase().contains("open")));
}
```

**Run:** fail.

**Impl:** `GlobalSearch { palette: CommandPalette, context: SearchContext }`. `SearchContext { Any, Files, Workspace, App }`. `seed_default_commands` pushes at minimum: `Create Workspace` (id `ws.create`), `Rename Workspace` (`ws.rename`), `Open Settings` (`settings.open`), `Open File` (`file.open`), `Switch Workspace…` (`ws.switch`). `set_context_kind` installs a palette `set_filter` that keeps only entries whose id starts with the matching prefix (`file.` for Files, `ws.` for Workspace, etc., plus `settings.*` always visible).

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/search.rs quick-shell/tests/shell_chrome_test.rs quick-shell/src/shell/mod.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): GlobalSearch with context-aware filter"`

---

### T13 — QuickSettings level 1 (brightness + volume sliders)

**File:** `quick-shell/src/shell/quick_settings.rs`

**Test:**

```rust
#[test]
fn quick_settings_brightness_and_volume_clamp_and_mutate() {
    use quick_desktop::shell::quick_settings::QuickSettings;
    let mut q = QuickSettings::new();
    q.set_brightness(50.0);
    q.set_volume(80.0);
    assert_eq!(q.brightness(), 50.0);
    assert_eq!(q.volume(), 80.0);
    q.set_brightness(-10.0);
    q.set_volume(999.0);
    assert_eq!(q.brightness(), 0.0);
    assert_eq!(q.volume(), 100.0);
}
```

**Run:** fail.

**Impl:** `QuickSettings { brightness: f32, volume: f32, nav_stack: Vec<QsLevel> }`. Initial `nav_stack = vec![QsLevel::Root]`. `set_brightness`/`set_volume` clamp to `[0, 100]`. Widget impl renders brightness slider, volume slider, three row buttons (Settings, Camera, Power) plus WiFi/BT rows that call `push_level` (implemented in T14).

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/quick_settings.rs quick-shell/tests/shell_chrome_test.rs quick-shell/src/shell/mod.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): QuickSettings popover level 1 with clamped sliders"`

---

### T14 — QuickSettings multi-level drill-in

**File:** `quick-shell/src/shell/quick_settings.rs`

**Test:**

```rust
#[test]
fn quick_settings_drill_in_and_back() {
    use quick_desktop::shell::quick_settings::{QuickSettings, QsLevel};
    let mut q = QuickSettings::new();
    assert_eq!(q.current_level(), &QsLevel::Root);
    q.push_level(QsLevel::Wifi);
    assert_eq!(q.current_level(), &QsLevel::Wifi);
    q.push_level(QsLevel::WifiDetails("Home-5G".into()));
    assert!(matches!(q.current_level(), QsLevel::WifiDetails(s) if s == "Home-5G"));
    q.pop_level();
    assert_eq!(q.current_level(), &QsLevel::Wifi);
    q.pop_level();
    assert_eq!(q.current_level(), &QsLevel::Root);
    q.pop_level(); // no-op at root
    assert_eq!(q.current_level(), &QsLevel::Root);
}
```

**Run:** fail.

**Impl:** Add `enum QsLevel { Root, Wifi, WifiDetails(String), Bluetooth, BluetoothDetails(String) }`. Implement `push_level`, `pop_level` (no-op at len≤1), `current_level`. Widget renders a back-chevron when `nav_stack.len() > 1`, driven by Plan 1's spring animation (slide from right).

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/quick_settings.rs quick-shell/tests/shell_chrome_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): QuickSettings multi-level WiFi/BT drill-in"`

---

### T15 — WorkspaceSettings General tab

**File:** `quick-shell/src/shell/workspace_settings.rs`

**Test:**

```rust
#[test]
fn workspace_settings_general_edits_name_and_removes_collaborator() {
    use quick_desktop::shell::workspace_settings::{WorkspaceSettings, WsTab};
    use quick_desktop::shell::state::{Workspace, WorkspaceId, Collaborator};
    let mut ws = Workspace::new(WorkspaceId::new(), "Design");
    ws.collaborators = vec![
        Collaborator { id: "u1".into(), name: "Ada".into() },
        Collaborator { id: "u2".into(), name: "Bo".into() },
    ];
    let mut modal = WorkspaceSettings::new(ws.id);
    modal.set_tab(WsTab::General);
    modal.rename(&mut ws, "Concept Room");
    assert_eq!(ws.name, "Concept Room");
    modal.remove_collaborator(&mut ws, "u1");
    assert_eq!(ws.collaborators.len(), 1);
    assert_eq!(ws.collaborators[0].id, "u2");
}
```

Add `Collaborator` to `state.rs` if not already present: `pub struct Collaborator { pub id: String, pub name: String }` plus `pub collaborators: Vec<Collaborator>` on `Workspace`.

**Run:** fail.

**Impl:** `enum WsTab { General, Roles, Files }`. `WorkspaceSettings { ws_id: WorkspaceId, active_tab: WsTab }`. Methods `set_tab`, `rename(&mut Workspace, &str)`, `remove_collaborator(&mut Workspace, &str)`, `invite_link(&Workspace) -> String` (returns `format!("quickdesktop://invite/{}", ws.id)`). Widget impl renders backed by Plan 1's `overlay::Modal` + `FocusTrap`, with three `Tab` buttons.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/workspace_settings.rs quick-shell/src/shell/state.rs quick-shell/tests/shell_chrome_test.rs quick-shell/src/shell/mod.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): WorkspaceSettings modal General tab"`

---

### T16 — WorkspaceSettings Roles tab (uses `DataTable`)

**File:** `quick-shell/src/shell/workspace_settings.rs`

**Test:**

```rust
#[test]
fn workspace_settings_roles_tab_edits_app_permissions() {
    use quick_desktop::shell::workspace_settings::{WorkspaceSettings, WsTab};
    use quick_desktop::shell::state::{Workspace, WorkspaceId, Role, RolePermissions, AppPerm};
    let mut ws = Workspace::new(WorkspaceId::new(), "Design");
    ws.roles.push(Role { id: "r1".into(), name: "Viewer".into(), permissions: RolePermissions::default() });
    let mut modal = WorkspaceSettings::new(ws.id);
    modal.set_tab(WsTab::Roles);
    modal.select_role("r1");
    modal.set_app_perm(&mut ws, "r1", "Terminal", AppPerm::View);
    modal.set_app_perm(&mut ws, "r1", "Terminal", AppPerm::Edit);
    let perm = ws.roles[0].permissions.per_app.get("Terminal").copied().unwrap_or(AppPerm::None);
    assert!(matches!(perm, AppPerm::Edit));
}
```

**Run:** fail.

**Impl:** Add `selected_role: Option<String>` + `apps_table: DataTable<AppPermRow>` + `files_table: DataTable<FilePermRow>` to `WorkspaceSettings`. `select_role(&str)` sets selection; `set_app_perm(&mut Workspace, role_id, app_name, AppPerm)` mutates `workspace.roles[r].permissions.per_app`. Build `apps_table` columns: `App` (flex), `Permission` (fixed 160px, renders a small segmented dropdown).

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/workspace_settings.rs quick-shell/tests/shell_chrome_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): WorkspaceSettings Roles tab with DataTable app perms"`

---

### T17 — WorkspaceSettings Files tab (per-role file perm matrix)

**File:** `quick-shell/src/shell/workspace_settings.rs`

**Test:**

```rust
#[test]
fn workspace_settings_files_tab_edits_file_permissions() {
    use quick_desktop::shell::workspace_settings::{WorkspaceSettings, WsTab};
    use quick_desktop::shell::state::{Workspace, WorkspaceId, Role, RolePermissions, WorkspaceFile, FilePerm};
    let mut ws = Workspace::new(WorkspaceId::new(), "Design");
    ws.roles.push(Role { id: "r1".into(), name: "Editor".into(), permissions: RolePermissions::default() });
    ws.workspace_files.push(WorkspaceFile { id: "f1".into(), name: "brief.md".into(), mime: "text/markdown".into() });
    let mut modal = WorkspaceSettings::new(ws.id);
    modal.set_tab(WsTab::Files);
    modal.set_file_perm(&mut ws, "r1", "f1", FilePerm::Write);
    let perm = ws.roles[0].permissions.per_file.get("f1").copied().unwrap_or(FilePerm::None);
    assert!(matches!(perm, FilePerm::Write));
}
```

**Run:** fail.

**Impl:** `set_file_perm(&mut Workspace, role_id, file_id, FilePerm)`. Build `files_table` columns: `File` (flex), one `Permission` column per role (dynamic from `ws.roles`); cell renders a small segmented dropdown with None/Read/Write.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/workspace_settings.rs quick-shell/tests/shell_chrome_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): WorkspaceSettings Files tab per-role permission matrix"`

---

### T18 — TopBar LeftNav (overview + ws up/down + ws name)

**File:** `quick-shell/src/shell/topbar.rs`

**Test:**

```rust
#[test]
fn topbar_leftnav_navigates_workspaces() {
    use quick_desktop::shell::topbar::{TopBar, TopBarEvent};
    use quick_desktop::shell::state::{WorkspaceTree, Workspace, WorkspaceId};
    let mut tree = WorkspaceTree::default();
    tree.workspaces = vec![
        Workspace::new(WorkspaceId::new(), "A"),
        Workspace::new(WorkspaceId::new(), "B"),
        Workspace::new(WorkspaceId::new(), "C"),
    ];
    tree.active = tree.workspaces[1].id;
    let mut tb = TopBar::new();
    tb.emit(TopBarEvent::WorkspaceUp, &mut tree);
    assert_eq!(tree.active, tree.workspaces[0].id);
    tb.emit(TopBarEvent::WorkspaceDown, &mut tree);
    tb.emit(TopBarEvent::WorkspaceDown, &mut tree);
    assert_eq!(tree.active, tree.workspaces[2].id);
    // clamped at edges
    tb.emit(TopBarEvent::WorkspaceDown, &mut tree);
    assert_eq!(tree.active, tree.workspaces[2].id);
}

#[test]
fn topbar_workspace_name_button_emits_settings_request() {
    use quick_desktop::shell::topbar::{TopBar, TopBarEvent, TopBarOutput};
    let mut tb = TopBar::new();
    let out = tb.click_workspace_name();
    assert!(matches!(out, TopBarOutput::OpenWorkspaceSettings));
}
```

**Run:** fail.

**Impl:** `TopBar` with `emit(TopBarEvent, &mut WorkspaceTree)`: `WorkspaceUp` decrements active index (clamped ≥ 0), `WorkspaceDown` increments (clamped ≤ len-1), `ToggleOverview` delegates to outer shell. `click_workspace_name() -> TopBarOutput::OpenWorkspaceSettings` (shell opens the modal).

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/topbar.rs quick-shell/tests/shell_chrome_test.rs quick-shell/src/shell/mod.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): TopBar LeftNav with clamped workspace up/down + name button"`

---

### T19 — TopBar CenterWidgets (clock + sparkline + calendar, wheel cycle, 200ms cooldown)

**File:** `quick-shell/src/shell/topbar.rs`

**Test:**

```rust
#[test]
fn topbar_center_widget_cycles_with_wheel_cooldown() {
    use quick_desktop::shell::topbar::{TopBar, CenterWidget};
    use std::time::Duration;
    let mut tb = TopBar::new();
    assert_eq!(tb.center_widget(), CenterWidget::Clock);
    tb.on_center_wheel(1, Duration::from_millis(0));
    assert_eq!(tb.center_widget(), CenterWidget::SystemMonitor);
    // second wheel within 200ms cooldown: ignored
    tb.on_center_wheel(1, Duration::from_millis(100));
    assert_eq!(tb.center_widget(), CenterWidget::SystemMonitor);
    tb.on_center_wheel(1, Duration::from_millis(300));
    assert_eq!(tb.center_widget(), CenterWidget::Calendar);
    // wrap
    tb.on_center_wheel(1, Duration::from_millis(700));
    assert_eq!(tb.center_widget(), CenterWidget::Clock);
    // negative direction
    tb.on_center_wheel(-1, Duration::from_millis(1000));
    assert_eq!(tb.center_widget(), CenterWidget::Calendar);
}
```

**Run:** fail.

**Impl:** `enum CenterWidget { Clock, SystemMonitor, Calendar }`. `TopBar { center: CenterWidget, last_wheel: Duration, cooldown: Duration /* 200ms */ }`. `on_center_wheel(dir: i32, now: Duration)` increments index by `dir.signum()` and wraps modulo 3 *only if* `now - last_wheel >= cooldown`. Widget impl draws the active center widget; SystemMonitor variant reads samples from a `SystemMonitor` reference injected via `set_system_monitor` (refreshed once per frame in Plan 1's tick hook).

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/topbar.rs quick-shell/tests/shell_chrome_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): TopBar CenterWidgets wheel-cycle with 200ms cooldown"`

---

### T20 — TopBar RightTray (collaborator avatars + QuickSettings trigger)

**File:** `quick-shell/src/shell/topbar.rs`

**Test:**

```rust
#[test]
fn topbar_right_tray_renders_collaborators_and_opens_quick_settings() {
    use quick_desktop::shell::topbar::{TopBar, TopBarOutput};
    use quick_desktop::shell::state::{WorkspaceTree, Collaborator, Workspace, WorkspaceId};
    let mut tree = WorkspaceTree::default();
    tree.workspaces = vec![Workspace::new(WorkspaceId::new(), "A")];
    tree.active = tree.workspaces[0].id;
    tree.collaborators = vec![
        Collaborator { id: "u1".into(), name: "Ada".into() },
        Collaborator { id: "u2".into(), name: "Bo".into() },
        Collaborator { id: "u3".into(), name: "Cy".into() },
        Collaborator { id: "u4".into(), name: "Di".into() },
    ];
    let tb = TopBar::new();
    let avatars = tb.visible_avatars(&tree, 3);
    assert_eq!(avatars.len(), 3);
    assert_eq!(tb.overflow_count(&tree, 3), 1);
    let out = TopBar::new().click_tray_settings();
    assert!(matches!(out, TopBarOutput::OpenQuickSettings));
}
```

**Run:** fail.

**Impl:** `visible_avatars(&WorkspaceTree, max: usize) -> Vec<&Collaborator>` returns first N; `overflow_count` returns `len - max` (min 0). `click_tray_settings() -> TopBarOutput::OpenQuickSettings`. Widget impl draws avatars as circular initials (first char, themed colors) with a `+N` pill when overflow > 0, and a gear icon button.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/topbar.rs quick-shell/tests/shell_chrome_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): TopBar RightTray avatars + QuickSettings trigger"`

---

### T21 — `SidebarWheel` skeleton (wheel-to-switch-strip)

**File:** `quick-shell/src/shell/sidebar_wheel.rs`

**Test:**

```rust
#[test]
fn sidebar_wheel_switches_active_strip() {
    use quick_desktop::shell::sidebar_wheel::SidebarWheel;
    use quick_desktop::shell::state::{WorkspaceTree, Workspace, WorkspaceId, Strip, StripId};
    let mut ws = Workspace::new(WorkspaceId::new(), "A");
    ws.strips = vec![
        Strip::new(StripId::new(), "s1"),
        Strip::new(StripId::new(), "s2"),
        Strip::new(StripId::new(), "s3"),
    ];
    let mut tree = WorkspaceTree::default();
    let ws_id = ws.id;
    tree.workspaces = vec![ws];
    tree.active = ws_id;
    tree.active_strip.insert(ws_id, tree.workspaces[0].strips[0].id);

    let mut w = SidebarWheel::new();
    w.on_wheel(1, &mut tree);
    assert_eq!(tree.active_strip[&ws_id], tree.workspaces[0].strips[1].id);
    w.on_wheel(1, &mut tree);
    assert_eq!(tree.active_strip[&ws_id], tree.workspaces[0].strips[2].id);
    // clamped — overscroll not yet committing (T22 adds it)
    w.on_wheel(1, &mut tree);
    assert_eq!(tree.active_strip[&ws_id], tree.workspaces[0].strips[2].id);
}
```

**Run:** fail.

**Impl:** `SidebarWheel { scroll_offset: f32, overscroll: f32, gesture_active: bool }`. `on_wheel(dir, &mut tree)` looks up the current workspace's strips, advances active by `dir.signum()` clamped `[0, len-1]`. Widget impl stacks mini-strip cards vertically with Plan 1's spring animation for the "wheel" (2D stack is acceptable in v1; full 2.5D is deferred — §10 of spec).

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/sidebar_wheel.rs quick-shell/tests/shell_chrome_test.rs quick-shell/src/shell/mod.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): SidebarWheel skeleton with wheel-to-switch-strip"`

---

### T22 — SidebarWheel overscroll-create gesture

**File:** `quick-shell/src/shell/sidebar_wheel.rs`

**Test:**

```rust
#[test]
fn sidebar_wheel_overscroll_commits_new_workspace_past_threshold() {
    use quick_desktop::shell::sidebar_wheel::SidebarWheel;
    use quick_desktop::shell::state::{WorkspaceTree, Workspace, WorkspaceId, Strip, StripId};
    let mut ws = Workspace::new(WorkspaceId::new(), "A");
    ws.strips = vec![Strip::new(StripId::new(), "s1")];
    let mut tree = WorkspaceTree::default();
    let ws_id = ws.id;
    tree.workspaces = vec![ws];
    tree.active = ws_id;
    tree.active_strip.insert(ws_id, tree.workspaces[0].strips[0].id);

    let mut w = SidebarWheel::new();
    // at the top/bottom end, wheel direction +1 = downward overscroll
    w.begin_overscroll(1);
    for _ in 0..10 { w.drag_overscroll(1, 10.0); } // 100px of pull
    assert!(w.overscroll_hint_visible());
    assert!(w.overscroll_progress() > 0.0);

    // below threshold → snap back
    w.release_overscroll(&mut tree);
    assert_eq!(tree.workspaces.len(), 1);

    w.begin_overscroll(1);
    for _ in 0..30 { w.drag_overscroll(1, 10.0); } // 300px
    assert!(w.overscroll_progress() >= 1.0);
    w.release_overscroll(&mut tree);
    assert_eq!(tree.workspaces.len(), 2);
    assert_eq!(tree.active, tree.workspaces[1].id);
}

#[test]
fn sidebar_wheel_overscroll_below_threshold_snaps_back() {
    use quick_desktop::shell::sidebar_wheel::SidebarWheel;
    use quick_desktop::shell::state::{WorkspaceTree, Workspace, WorkspaceId};
    let mut tree = WorkspaceTree::default();
    tree.workspaces = vec![Workspace::new(WorkspaceId::new(), "A")];
    tree.active = tree.workspaces[0].id;
    let mut w = SidebarWheel::new();
    w.begin_overscroll(-1);
    w.drag_overscroll(-1, 50.0);
    w.release_overscroll(&mut tree);
    assert_eq!(tree.workspaces.len(), 1);
    assert_eq!(w.overscroll_progress(), 0.0);
}
```

**Run:** fail.

**Impl:** Add `const OVERSCROLL_THRESHOLD: f32 = 240.0`. Methods `begin_overscroll(dir)`, `drag_overscroll(dir, px)` accumulates `overscroll += px.abs()` (only when direction matches edge), `overscroll_hint_visible() = overscroll > 32.0`, `overscroll_progress() = (overscroll / THRESHOLD).min(1.0)`. `release_overscroll(&mut tree)`: if progress >= 1.0, push a fresh `Workspace::new(WorkspaceId::new(), "Untitled")` into `tree.workspaces` (before-or-after based on gesture dir) and set `tree.active = new.id`, then reset overscroll; else spring-animate back to 0. Return `true` from `wants_pointer_capture` while `gesture_active`.

Widget draws a hint card with label "New session" whose alpha = `overscroll_progress()`.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/sidebar_wheel.rs quick-shell/tests/shell_chrome_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): SidebarWheel overscroll-create gesture with threshold commit + snap-back"`

---

### T23 — SidebarWheel workspace-management button entrypoint

**File:** `quick-shell/src/shell/sidebar_wheel.rs`

**Test:**

```rust
#[test]
fn sidebar_wheel_management_button_creates_workspace() {
    use quick_desktop::shell::sidebar_wheel::SidebarWheel;
    use quick_desktop::shell::state::{WorkspaceTree, Workspace, WorkspaceId};
    let mut tree = WorkspaceTree::default();
    tree.workspaces = vec![Workspace::new(WorkspaceId::new(), "A")];
    tree.active = tree.workspaces[0].id;
    let w = SidebarWheel::new();
    w.click_management_new_workspace(&mut tree);
    assert_eq!(tree.workspaces.len(), 2);
    assert_eq!(tree.active, tree.workspaces[1].id);
}
```

**Run:** fail.

**Impl:** Add `click_management_new_workspace(&self, &mut WorkspaceTree)` which does the same append-then-activate as the overscroll commit branch (factor the insert into a private helper so both call it).

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/sidebar_wheel.rs quick-shell/tests/shell_chrome_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): SidebarWheel management-button path reuses overscroll commit helper"`

---

### T24 — `RemoteCursor` population + mapping helper

**File:** `quick-ui/src/presence/mod.rs` (extend), `quick-shell/src/shell/remote_cursors.rs`

**Test:** `quick-shell/tests/remote_cursors_test.rs`:

```rust
use quick_ui::presence::{PresenceCtx, RemoteCursor};
use quick_desktop::shell::remote_cursors::{RemoteCursorOverlay, cursor_in_bounds};

#[test]
fn remote_cursor_is_clipped_to_active_tile_bounds() {
    let mut ctx = PresenceCtx::default();
    ctx.remote_cursors.push(RemoteCursor {
        user_id: "u".into(), color: [0.0;4], position: [50.0, 50.0], label: "Ada".into(),
    });
    ctx.remote_cursors.push(RemoteCursor {
        user_id: "v".into(), color: [0.0;4], position: [999.0, 999.0], label: "Bo".into(),
    });
    let bounds = [0.0_f32, 0.0, 400.0, 300.0];
    let visible: Vec<_> = ctx.remote_cursors.iter()
        .filter(|c| cursor_in_bounds(c.position, bounds))
        .collect();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].user_id, "u");
}
```

**Run:** fail.

**Impl:** `cursor_in_bounds(pos, [x, y, w, h])` returns true when `pos` is inside. `RemoteCursorOverlay { active_tile_bounds: [f32;4] }` stub for T26.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/remote_cursors.rs quick-shell/tests/remote_cursors_test.rs quick-shell/src/shell/mod.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): remote_cursors bounds helper"`

---

### T25 — `SimulatedFeed` driving remote cursors

**File:** `quick-shell/src/shell/remote_cursors.rs`

**Test:**

```rust
#[test]
fn simulated_feed_moves_cursor_deterministically() {
    use quick_desktop::shell::remote_cursors::SimulatedFeed;
    use quick_ui::presence::PresenceCtx;
    use std::time::Duration;

    let mut feed = SimulatedFeed::new_with_seed(42);
    let mut ctx = PresenceCtx::default();
    feed.tick(Duration::from_millis(16), &mut ctx);
    assert_eq!(ctx.remote_cursors.len(), 1);
    let p0 = ctx.remote_cursors[0].position;
    feed.tick(Duration::from_millis(100), &mut ctx);
    let p1 = ctx.remote_cursors[0].position;
    assert!(p0 != p1, "cursor should have moved");
}
```

**Run:** fail.

**Impl:** `SimulatedFeed { t: Duration, seed: u64, user: String, color: [f32;4] }`. `new_with_seed(seed)` initializes a single peer "Ada" with seeded random color. `tick(dt, &mut PresenceCtx)` accumulates `t` and writes a single-cursor vec: `position = [200.0 + 80.0 * (t_secs).sin(), 150.0 + 60.0 * (t_secs * 0.7).cos()]`.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/remote_cursors.rs quick-shell/tests/remote_cursors_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): SimulatedFeed drives remote cursors deterministically"`

---

### T26 — `RemoteCursorOverlay` widget draws over active tile

**File:** `quick-shell/src/shell/remote_cursors.rs`

**Test:**

```rust
#[test]
fn remote_cursor_overlay_filters_to_active_tile() {
    use quick_desktop::shell::remote_cursors::RemoteCursorOverlay;
    use quick_ui::presence::{PresenceCtx, RemoteCursor};
    let mut ctx = PresenceCtx::default();
    ctx.remote_cursors = vec![
        RemoteCursor { user_id: "in".into(), color: [0.0;4], position: [50.0, 50.0], label: "I".into() },
        RemoteCursor { user_id: "out".into(), color: [0.0;4], position: [500.0, 500.0], label: "O".into() },
    ];
    let mut ov = RemoteCursorOverlay::new();
    ov.set_active_tile_bounds([0.0, 0.0, 200.0, 200.0]);
    let drawn = ov.cursors_to_draw(&ctx);
    assert_eq!(drawn.len(), 1);
    assert_eq!(drawn[0].user_id, "in");
}
```

**Run:** fail.

**Impl:** Implement `RemoteCursorOverlay::set_active_tile_bounds`, `cursors_to_draw(&PresenceCtx) -> Vec<&RemoteCursor>` (filters via `cursor_in_bounds`), and a `Widget` impl that draws a small caret triangle + label chip at each cursor's position.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/remote_cursors.rs quick-shell/tests/remote_cursors_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): RemoteCursorOverlay widget scoped to active tile"`

---

### T27 — Wire chrome into `nirios::Shell` root widget

**File:** `quick-shell/src/shell/mod.rs`

**Test:** In `shell_chrome_test.rs`:

```rust
#[test]
fn shell_root_wires_all_chrome() {
    use quick_desktop::shell::Shell;
    let shell = Shell::new_for_test();
    assert!(shell.has_topbar());
    assert!(shell.has_sidebar_wheel());
    assert!(shell.has_remote_cursor_overlay());
    assert!(shell.has_toast_stack());
    assert!(shell.has_context_menu());
    assert!(shell.has_tooltip());
    // overview hidden by default
    assert!(!shell.overview_visible());
}
```

**Run:** fail.

**Impl:** Add fields to `Shell`: `topbar: TopBar`, `sidebar: SidebarWheel`, `overview: OverviewView`, `search: GlobalSearch`, `quick_settings: QuickSettings`, `ws_settings: WorkspaceSettings`, `remote_overlay: RemoteCursorOverlay`, `toasts: ToastStack`, `ctx_menu: ContextMenu`, `tooltip: Tooltip`, `system_monitor: SystemMonitor`, `cursor_feed: SimulatedFeed`, `overview_visible: bool`, `search_visible: bool`, `quick_settings_visible: bool`, `ws_settings_visible: bool`. Add accessor predicates for the test (gated `#[cfg(test)]` or via a `has_*` trivial getter). In the root `draw`, compose in z-order: topbar → (sidebar + workspace_view | overview) → remote_overlay → popovers → toast stack.

Handle global hotkeys in root `handle_event`: `Cmd+K` toggles `search_visible`; `Cmd+Tab` toggles `overview_visible`; `Cmd+Up`/`Cmd+Down` call `topbar.emit(WorkspaceUp/Down, &mut tree)`.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/mod.rs quick-shell/tests/shell_chrome_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): nirios::Shell root wires all chrome + global hotkeys"`

---

### T28 — Shell-chrome interaction tests

**File:** `quick-shell/tests/shell_chrome_test.rs`

**Test (add):**

```rust
#[test]
fn cmd_k_opens_and_escape_closes_search() {
    use quick_desktop::shell::Shell;
    use quick_desktop::shell::test_support::{key_press, Mods};
    let mut shell = Shell::new_for_test();
    shell.dispatch_key(key_press("K", Mods::CMD));
    assert!(shell.search_visible());
    shell.dispatch_key(key_press("Escape", Mods::NONE));
    assert!(!shell.search_visible());
}

#[test]
fn cmd_tab_toggles_overview() {
    use quick_desktop::shell::Shell;
    use quick_desktop::shell::test_support::{key_press, Mods};
    let mut shell = Shell::new_for_test();
    shell.dispatch_key(key_press("Tab", Mods::CMD));
    assert!(shell.overview_visible());
    shell.dispatch_key(key_press("Tab", Mods::CMD));
    assert!(!shell.overview_visible());
}

#[test]
fn overscroll_commit_fires_toast() {
    use quick_desktop::shell::Shell;
    let mut shell = Shell::new_for_test();
    let before = shell.toast_count();
    shell.simulate_overscroll_commit();
    assert!(shell.toast_count() >= before + 1);
}
```

**Run:** fail.

**Impl:** Add `shell::test_support::{Mods, key_press}` helper that constructs a `WindowEvent::KeyboardInput` suitable for `dispatch_key`. Add `Shell::simulate_overscroll_commit` convenience that calls into `SidebarWheel::release_overscroll` past threshold and routes the resulting toast push through `toasts`.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/mod.rs quick-shell/tests/shell_chrome_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "test(shell): Cmd+K / Cmd+Tab / overscroll-toast interaction tests"`

---

### T29 — Toast wiring: workspace create + rename

**File:** `quick-shell/src/shell/mod.rs`

**Test (add):**

```rust
#[test]
fn toast_fires_on_rename_and_create() {
    use quick_desktop::shell::Shell;
    use quick_ui::widget::toast::ToastKind;
    let mut shell = Shell::new_for_test();
    let b0 = shell.toast_count();
    shell.simulate_overscroll_commit();          // "Workspace created"
    shell.simulate_rename_active_workspace("New Name");
    let kinds: Vec<ToastKind> = shell.toast_kinds();
    assert!(kinds.iter().any(|k| matches!(k, ToastKind::Info | ToastKind::Success)));
    assert!(shell.toast_count() >= b0 + 2);
}
```

**Run:** fail.

**Impl:** In `Shell`, whenever a workspace is created via any path (overscroll, management button, palette), call `self.toasts.push("Workspace created", ToastKind::Success)`. When `WorkspaceSettings::rename` runs, push `"Workspace renamed"`. Expose `simulate_rename_active_workspace(new_name)` test helper.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/mod.rs quick-shell/src/shell/workspace_settings.rs quick-shell/tests/shell_chrome_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): toasts fire on workspace create + rename"`

---

### T30 — ContextMenu wiring: right-click on tile

**File:** `quick-shell/src/shell/mod.rs`

**Test:**

```rust
#[test]
fn right_click_on_tile_opens_default_context_menu() {
    use quick_desktop::shell::Shell;
    let mut shell = Shell::new_for_test();
    assert!(!shell.context_menu_is_open());
    shell.simulate_right_click_on_active_tile([120.0, 200.0]);
    assert!(shell.context_menu_is_open());
    assert_eq!(shell.context_menu_position(), Some([120.0, 200.0]));
    let labels = shell.context_menu_labels();
    assert!(labels.iter().any(|l| l == "Close Tab"));
    assert!(labels.iter().any(|l| l == "Split Right"));
    assert!(labels.iter().any(|l| l == "Move to New Group"));
}
```

**Run:** fail.

**Impl:** Add `simulate_right_click_on_active_tile`, `context_menu_is_open`, `context_menu_position`, `context_menu_labels`. On right-click, clear `ctx_menu`, push default items ("Close Tab", `MenuItem::Separator`, "Split Right", "Move to New Group"), `open_at`.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/mod.rs quick-shell/tests/shell_chrome_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): default tile context menu wired on right-click"`

---

### T31 — Tooltip wiring: topbar workspace-name button

**File:** `quick-shell/src/shell/topbar.rs`

**Test:**

```rust
#[test]
fn topbar_ws_name_tooltip_reveals_after_delay() {
    use quick_desktop::shell::topbar::TopBar;
    use std::time::Duration;
    let mut tb = TopBar::new();
    tb.hover_ws_name(true);
    tb.tick(Duration::from_millis(700));
    assert!(tb.ws_name_tooltip_visible());
    tb.hover_ws_name(false);
    assert!(!tb.ws_name_tooltip_visible());
}
```

**Run:** fail.

**Impl:** Attach a `Tooltip::new("Open workspace settings")` to TopBar; on hover-enter call `tooltip.on_hover_enter(now)`, on leave `tooltip.on_hover_leave()`. Expose `ws_name_tooltip_visible` and `tick`.

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/topbar.rs quick-shell/tests/shell_chrome_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "feat(shell): workspace-name tooltip after 600ms hover"`

---

### T32 — Visual smoke checklist (doc-comment)

**File:** `quick-shell/src/shell/mod.rs`

**Test (compile-only):** Not a runtime test — a crate doc-comment. Still do TDD: add a compile-fail test asserting the doc block exists.

```rust
// at top of quick-shell/tests/shell_chrome_test.rs:
#[test]
fn visual_smoke_checklist_is_documented() {
    let src = include_str!("../src/shell/mod.rs");
    assert!(src.contains("# Visual smoke checklist"), "visual smoke checklist missing");
    for marker in ["Cmd+K", "Cmd+Tab", "overscroll", "QuickSettings", "remote cursor"] {
        assert!(src.contains(marker), "missing smoke item: {marker}");
    }
}
```

**Run:** fail.

**Impl:** At the top of `src/shell/mod.rs` add:

```rust
//! # Visual smoke checklist
//!
//! Run `cargo run -p quick-desktop` and verify:
//!
//! - TopBar renders with a live clock; wheel over the center area cycles to
//!   the CPU/RAM sparkline and the calendar (200 ms cooldown).
//! - Cmd+K opens GlobalSearch; typing filters; Enter invokes; Escape closes.
//! - Cmd+Tab toggles the OverviewView; clicking a card activates the workspace.
//! - The RightTray gear icon opens QuickSettings; sliders are interactive;
//!   WiFi/BT drill-in and back work.
//! - The workspace-name button opens WorkspaceSettings; switching to the Roles
//!   tab lets you toggle per-app permissions via DataTable; the Files tab
//!   shows per-role × per-file matrix.
//! - SidebarWheel scrolls strips; overscrolling past the threshold shows the
//!   "New session" hint and commits a new workspace; releasing before the
//!   threshold snaps back.
//! - Right-clicking a tile opens the context menu.
//! - Hovering the workspace-name button reveals a tooltip after ~600 ms.
//! - Creating a workspace (any path) triggers a toast.
//! - A simulated remote cursor floats over the active tile.
```

**Run:** pass.

**Commit:** `git -C /var/home/user/Development/quickdesktop add quick-shell/src/shell/mod.rs quick-shell/tests/shell_chrome_test.rs && git -C /var/home/user/Development/quickdesktop commit -m "docs(shell): visual smoke checklist for nirios chrome"`

---

### T33 — Final self-review pass

**File:** (this plan file) + run a clean cargo check.

**Test:** Run in sequence:

- `cargo test -p quick-ui` — all new primitive tests green.
- `cargo test -p quick-desktop` — all new shell tests green.
- `cargo run -p quick-desktop` — visual smoke (no crash; the checklist items from T32 match behavior).
- `git -C /var/home/user/Development/quickdesktop status` — clean.

**Impl:** None — verification only. If any drift is found (API mismatch with Plan 1, naming collision), fix it in a new TDD-style mini-step and commit with prefix `fix(shell)` or `fix(quick-ui)` as appropriate. Do *not* amend previous commits.

**Commit:** No commit if nothing changes.

---

## Self-Review Checklist

Before handing off to Plan 3:

- [ ] Every new public item (structs, enums, trait impls, free functions) is exercised by at least one assertion in `cargo test -p quick-ui` or `cargo test -p quick-desktop`.
- [ ] No `todo!()` / `unimplemented!()` / `panic!()` bodies remain in Plan-2 code except the deliberate `TileContent::WaylandSurface` one defined in Plan 1.
- [ ] No workspace-wide `cargo test` invocation appears in any task; `-p <crate>` is used throughout.
- [ ] Every widget in this plan renders from `Theme::tokens`; no literal color / size constants leak into `draw` bodies (audit task T33).
- [ ] `PresenceCtx` is only mutated from `SimulatedFeed::tick` or the shell's presence update path; widgets never push cursors directly.
- [ ] `PermissionScope` is returned from `WorkspaceSettings` Roles/Files tabs; Plan 1's `PermissionContext` denies mutation when set to `View`-only (covered by integration test in Plan 4 — just confirm the hook compiles).
- [ ] All git commits use `git -C /var/home/user/Development/quickdesktop ...` form.
- [ ] Task DAG parallel groups (G2 and G3) touch disjoint files, confirmed by re-reading each task's **File:** header.
- [ ] Signatures for `CommandPalette` and `DataTable` match the prompt-locked signatures exactly (public API surface — `new`, `push`, `set_query`, `set_filter` for CommandPalette; `new`, `set_rows`, `sort_by` for DataTable). The `sort_by_with` typed helper on DataTable is additive.
- [ ] `cargo run -p quick-desktop` reproduces every item of the T32 visual smoke checklist.
- [ ] Deferred items ( 2.5D/3D wheel geometry, real `CollaborationManager` feed, wire-side permission enforcement, persistence-backed workspace tree) are **not** implemented here, matching the spec §10 and the deferred tracker.
- [ ] Files added: `command_palette.rs`, `data_table.rs`, `context_menu.rs`, `tooltip.rs`, `toast.rs` in `quick-ui/src/widget/`; `overview.rs`, `search.rs`, `quick_settings.rs`, `workspace_settings.rs`, `topbar.rs`, `sidebar_wheel.rs`, `remote_cursors.rs`, `system_monitor.rs` in `quick-shell/src/shell/`. Count matches File Structure section. No other module was created.
- [ ] No new workspace dependency beyond `sysinfo = "0.32"`.
- [ ] Each task commit is small (< 400 LoC diff); if any grew larger during execution, it was split as the plan prescribes.
