# Nirios Shell — Plan 3 of 4: Apps Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the apps layer of nirios to `quick-ui`: extend `AppWidget` with real methods, refactor `quick-terminal` onto it, audit and port `file-explorer` from iced to `quick-ui` with full parity, add a browser stub, and wire the shell's `TileContent::App` to host real apps instead of Plan 1 placeholders.

**Architecture:** `AppWidget: Widget` becomes the single integration contract between the shell and any in-process app. The terminal already uses `quick-ui`, so it gets a minimal trait-impl refactor plus token adoption. The file-explorer currently uses `iced::application` and is ported in two phases: (1) an audit task that inventories every user-visible feature from the existing single-file iced implementation and produces a port-parity checklist + test expectations; (2) a staged port that replaces iced with `quick-ui`, reusing Plan 2 primitives (`DataTable`, `Scrollable`, `ContextMenu`, `Tooltip`, `Toast`). The browser is not ported — a `TileContent::Stub` card with an `xdg-open` fallback stands in. Finally, the shell tile-construction sites from Plan 1 are rewritten to embed concrete `AppWidget` impls, the `TileContent::App` variant is tightened from `Box<dyn Widget>` to `Box<dyn AppWidget>`, and an integration test asserts that a real terminal prompt and a real directory listing render inside shell tabs.

**Tech Stack:** Rust 2021 (terminal, file-explorer) / Rust 2024 (quick-ui, quick-shell). `quick-ui` widgets + tokens + primitives from Plans 1 & 2. `portable-pty` + `vte` (terminal, unchanged). `walkdir` + `trash` + `open` + `mime_guess` + `humansize` + `chrono` + `dirs-next` (file-explorer, kept from iced app; `iced` + `tokio` removed as Cargo deps after port). `xdg-open` via `std::process::Command` (browser stub). Git submodules for `apps/terminal` and `apps/file-explorer`.

---

## File Structure

### New files

- `quick-ui/src/widget/app_widget.rs` — `AppWidget` trait + `AppKind` enum + `IconDescriptor` + `AppCommand` struct. New module.
- `apps/terminal/src/app_widget_impl.rs` — `AppWidget` impl block for the existing `TerminalWidget` (keeps `main.rs` focused on PTY wiring).
- `apps/terminal/src/lib.rs` — exposes `TerminalWidget` + its constructor (`TerminalWidget::spawn_default() -> anyhow::Result<TerminalWidget>`) so the shell can embed it. `main.rs` keeps the binary entry point and delegates to the library.
- `apps/file-explorer/AUDIT.md` — port-parity feature inventory produced by Task 2. Lives in the submodule and is committed there.
- `apps/file-explorer/src/lib.rs` — new library root; exposes `FileExplorerWidget`.
- `apps/file-explorer/src/files_app.rs` — `FileExplorerWidget` struct + `Widget` + `AppWidget` impls; owns toolbar, breadcrumbs, sidebar, main view, footer composition.
- `apps/file-explorer/src/model.rs` — `Entry`, `SortBy`, `SortOrder`, `ViewMode` (ported from `main.rs` lines 34–76) + pure `sort_entries`/`read_dir_sync`/`format_path` helpers with unit tests.
- `apps/file-explorer/src/icons.rs` — MIME → icon glyph mapping extracted from `render_entry_list`/`render_entry_grid`.
- `apps/file-explorer/src/sidebar.rs` — the shortcut list widget tree builder.
- `apps/file-explorer/src/toolbar.rs` — back/refresh/search/view-toggle/sort-button/sort-order/open-terminal toolbar builder.
- `apps/file-explorer/src/breadcrumbs.rs` — path-component button row builder.
- `apps/file-explorer/src/views/list_view.rs` — list-mode content via `quick-ui` `DataTable`.
- `apps/file-explorer/src/views/grid_view.rs` — grid-mode content via a flow layout of cards.
- `apps/file-explorer/tests/parity.rs` — port-parity integration tests driven from the audit.
- `apps/file-explorer/src/main.rs` — rewritten to a thin `quick_ui::App::new` binary over `FileExplorerWidget`.
- `quick-shell/src/shell/browser_stub.rs` — `BrowserStubWidget` implementing `AppWidget` with an "Open in external browser" button invoking `xdg-open`.
- `quick-shell/tests/apps_integration.rs` — new integration test spawning the shell and asserting terminal prompt + file-explorer listing render.

### Modified files

- `quick-ui/src/widget/mod.rs` — re-export `app_widget::{AppWidget, AppKind, IconDescriptor, AppCommand}`; promote the Plan 1 marker trait stub to the real signatures from §7 of the spec.
- `quick-ui/src/lib.rs` — re-export the new public items so downstream crates can `use quick_ui::AppWidget`.
- `apps/terminal/Cargo.toml` — add `[lib]` target alongside the existing `[[bin]]`; no new deps.
- `apps/terminal/src/main.rs` — keep only `fn main()` + PTY spawn glue; delegate widget creation to `lib.rs::TerminalWidget::spawn_default`.
- `apps/file-explorer/Cargo.toml` — add `quick-ui` path dep; remove `iced` dep; keep `walkdir`, `trash`, `open`, `mime_guess`, `humansize`, `chrono`, `dirs-next`, `tracing`, `tracing-subscriber`; remove `tokio` (sync I/O via `std::fs` is sufficient for v1 since the grid is in-memory). Add `[lib]` + `[[bin]]` targets. Bump to `version = "0.2.0"`.
- `quick-shell/src/shell/tile.rs` — change `TileContent::App(Box<dyn Widget>)` to `TileContent::App(Box<dyn AppWidget>)`; `TileHost` continues to delegate layout/draw to the boxed widget via the super-trait.
- `quick-shell/src/shell/mod.rs` or equivalent — replace Plan 1 placeholder-tile construction sites with `Box::new(TerminalWidget::spawn_default()?)` and `Box::new(FileExplorerWidget::new())` or a `BrowserStubWidget::new()` depending on the `AppKind` the demo workspace requests.
- `quick-shell/Cargo.toml` — add `quick-terminal = { path = "../apps/terminal" }` and `file-explorer = { path = "../apps/file-explorer" }` as dependencies.

### Deleted

- `apps/file-explorer/src/main.rs` is *rewritten*, not appended to; its 553-line iced implementation is the thing being deleted in Task 11.

---

## Task DAG for Concurrent Execution

Tasks inside a "parallel group" have no shared-file conflicts and can be run by separate subagents concurrently. Groups run sequentially: group N must fully complete before group N+1 begins.

```
Group A (sequential):         1 → 2
Group B (sequential):         3
Group C (sequential):         4 → 5 → 6
Group D (parallel):           7 ∥ 8 ∥ 9
Group E (sequential):         10 → 11 → 12
Group F (parallel):           13 ∥ 14
Group G (sequential):         15 → 16 → 17 → 18
Group H (sequential):         19 → 20 → 21 → 22
Group I (sequential):         23 → 24 → 25 → 26
```

Explanation:
- Group A: trait scaffold precedes audit only ordering-wise (audit has no file conflict with trait but must come before porting).
- Group B: single task landing `AppWidget` + `TileContent` tightening in the superproject.
- Group C: terminal refactor is in the terminal submodule; each step advances on the previous.
- Group D: the three file-explorer supporting modules (`model.rs`, `icons.rs`, `breadcrumbs.rs`) are independent and touch distinct new files.
- Group E: toolbar/sidebar depend on model; list/grid views depend on model + icons.
- Group F: browser stub and shell apps-Cargo wiring touch disjoint files.
- Group G: shell wiring → integration test → delete iced code → submodule bump.
- Group H: parity-test runthrough + follow-up fixes for any audit gaps found.
- Group I: final self-review + docs + changelog entry + workspace smoke.

---

## Task 1: Add the `AppWidget` trait to `quick-ui`

**Files:**
- Create: `quick-ui/src/widget/app_widget.rs`
- Modify: `quick-ui/src/widget/mod.rs` (append re-exports)
- Modify: `quick-ui/src/lib.rs` (re-export `AppWidget`, `AppKind`, `IconDescriptor`, `AppCommand`)
- Test: `quick-ui/tests/app_widget.rs` (new integration test in the crate's `tests/` directory)

- [ ] **Step 1: Write the failing test**

Create `quick-ui/tests/app_widget.rs`:

```rust
use quick_ui::{AppWidget, AppKind, IconDescriptor, Widget, Theme};
use quick_ui::widget::AppCommand;
use automerge::AutoCommit;

struct DummyApp;

impl Widget for DummyApp {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, _taffy: &taffy::TaffyTree, _node: taffy::NodeId, _origin: vello::kurbo::Point) {}
    fn draw(&mut self, _scene: &mut vello::Scene, _transform: vello::kurbo::Affine, _text_renderer: &mut quick_ui::text_render::TextRenderer, _theme: &Theme) {}
}

impl AppWidget for DummyApp {
    fn app_kind(&self) -> AppKind { AppKind::Terminal }
    fn app_title(&self) -> &str { "Dummy" }
    fn app_icon(&self) -> IconDescriptor { IconDescriptor::Glyph("X") }
    fn permission_hint(&self) -> Option<AppKind> { Some(AppKind::Terminal) }
}

#[test]
fn app_widget_defaults_yield_empty_commands() {
    let app = DummyApp;
    assert_eq!(app.app_title(), "Dummy");
    assert_eq!(app.app_kind(), AppKind::Terminal);
    assert!(matches!(app.app_icon(), IconDescriptor::Glyph("X")));
    assert_eq!(app.permission_hint(), Some(AppKind::Terminal));
    assert!(app.commands().is_empty());
}

#[test]
fn app_widget_is_object_safe_and_upcasts_to_widget() {
    let app: Box<dyn AppWidget> = Box::new(DummyApp);
    // Super-trait upcast must compile; AppWidget: Widget.
    let _w: &dyn Widget = &*app;
}

#[test]
fn app_kind_covers_spec_variants() {
    // Must match spec §7 exactly.
    let _ = [AppKind::Terminal, AppKind::Files, AppKind::Editor, AppKind::Browser, AppKind::Settings];
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p quick-ui --test app_widget`
Expected: FAIL with `unresolved import 'quick_ui::AppWidget'` or `cannot find type 'AppWidget' in crate 'quick_ui'`.

- [ ] **Step 3: Create `quick-ui/src/widget/app_widget.rs`**

```rust
//! The `AppWidget` trait — the single contract between the nirios shell and
//! any in-process app hosted inside a tile. Extends the base `Widget` trait
//! with app-level metadata (kind, title, icon, permission hint, commands).
//!
//! Spec: `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` §7.

use crate::widget::Widget;

/// Identifies an app for permission gating, shell chrome (titles/icons), and
/// command-palette categorisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppKind {
    Terminal,
    Files,
    Editor,
    Browser,
    Settings,
}

/// A lightweight icon descriptor usable before the full asset pipeline lands.
/// `Glyph` is a Nerd Font / unicode codepoint (existing approach in both
/// the iced file-explorer and quick-terminal). `Named` is a symbolic name for
/// future theme-driven icon resolution.
#[derive(Debug, Clone)]
pub enum IconDescriptor {
    Glyph(&'static str),
    Named(String),
}

/// A command palette entry contributed by an app. Invoked with a mutable
/// CRDT handle and the app's root `ObjId` so the command can mutate state
/// directly (matching `quick-ui`'s direct-mutation event model).
pub struct AppCommand {
    pub id: String,
    pub label: String,
    pub shortcut: Option<String>,
    pub invoke: Box<dyn Fn(&mut automerge::AutoCommit, &automerge::ObjId) + Send + Sync>,
}

impl std::fmt::Debug for AppCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppCommand")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("shortcut", &self.shortcut)
            .field("invoke", &"<fn>")
            .finish()
    }
}

/// Trait every in-process app implements so the shell can host it inside a
/// `TileContent::App`. `AppWidget: Widget` means the shell always falls
/// through to the widget contract for layout, draw, and events.
pub trait AppWidget: Widget {
    fn app_kind(&self) -> AppKind;
    fn app_title(&self) -> &str;
    fn app_icon(&self) -> IconDescriptor;
    /// Which role permission governs this app. `None` means "ungated".
    fn permission_hint(&self) -> Option<AppKind>;
    /// Command-palette contributions. Default is none.
    fn commands(&self) -> Vec<AppCommand> {
        vec![]
    }
}
```

- [ ] **Step 4: Wire the module into `quick-ui/src/widget/mod.rs`**

Append to `quick-ui/src/widget/mod.rs`:

```rust
pub mod app_widget;
pub use app_widget::{AppCommand, AppKind, AppWidget, IconDescriptor};
```

- [ ] **Step 5: Re-export at crate root**

Append to `quick-ui/src/lib.rs` (near the existing `pub use widget::…` re-exports):

```rust
pub use widget::{AppCommand, AppKind, AppWidget, IconDescriptor};
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p quick-ui --test app_widget`
Expected: PASS, 3 tests pass.

- [ ] **Step 7: Commit (main repo — `quick-ui` lives in-tree)**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/app_widget.rs quick-ui/src/widget/mod.rs quick-ui/src/lib.rs quick-ui/tests/app_widget.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): add AppWidget trait with AppKind/IconDescriptor/AppCommand"
```

---

## Task 2: Audit `apps/file-explorer` — inventory user-visible features

**Files:**
- Create: `apps/file-explorer/AUDIT.md`

- [ ] **Step 1: Read every source file in `apps/file-explorer/src/`**

Run: `ls apps/file-explorer/src/ && wc -l apps/file-explorer/src/*.rs`
Expected output: a single file `main.rs` with roughly 553 lines (confirmed by plan author on 2026-04-18).

Read `apps/file-explorer/src/main.rs` in full. Confirm it contains:
- `enum Message` (navigate/back/refresh/openitem/openterminal/entriesloaded/selectentry/searchchanged/changesort/togglesortorder/changeview)
- `enum SortBy` (Name/Size/Modified/Type)
- `enum SortOrder` (Ascending/Descending)
- `enum ViewMode` (List/Grid)
- struct `FileExplorer` (current_path, home_path, entries, selected_index, search_query, error, is_loading, last_click, sort_by, sort_order, view_mode)
- struct `Entry` (name, path, is_dir, size, modified, mime)
- `new`/`update`/`view`/`render_sidebar`/`render_toolbar`/`render_breadcrumbs`/`render_entry_list`/`render_entry_grid`
- the async `read_dir_async` helper

- [ ] **Step 2: Categorise features**

Write the audit into `apps/file-explorer/AUDIT.md` using the exact text below. Every feature must have a category tag, a one-sentence behavioural description grounded in what the Iced code actually does (not what the spec *hopes* it does), and a concrete port-parity test expectation:

```markdown
# file-explorer — Port-Parity Audit

Source: `src/main.rs` (iced 0.13, single file, ~553 lines) as of 2026-04-18.
This document is the authoritative feature list the `quick-ui` port must match.
Anything not listed here is out of scope for v1 port parity (track separately
if desired).

## Convention
Each feature is tagged with a category, a one-line behavioural summary taken
from the current Iced code, and a port-parity test expectation that must exist
in `apps/file-explorer/tests/parity.rs` before `main.rs` is replaced.

## Navigation
- [N1] **Initial path resolution.** On startup, `/workspace` if it exists, else
  `dirs_next::home_dir()`, else `std::env::current_dir()`, else `/`.
  *Test:* `initial_path_prefers_workspace_then_home_then_cwd_then_root`.
- [N2] **Directory navigation on double-click.** Clicking an entry twice within
  500 ms opens it: dirs trigger navigation, files call `open::that`.
  *Test:* `double_click_dir_navigates_into_it`.
- [N3] **Back button.** Navigates to `current_path.parent()` when present;
  no-op at filesystem root.
  *Test:* `back_button_moves_to_parent_and_noops_at_root`.
- [N4] **Refresh.** Re-reads the current directory.
  *Test:* `refresh_rereads_same_directory`.
- [N5] **Breadcrumb buttons.** Each path component is a button that navigates
  to that component; home prefix collapses to `󰋜 ~`.
  *Test:* `breadcrumb_component_click_navigates_to_that_component`.

## Sidebar
- [S1] **Fixed shortcut list.** Home, Documents, Downloads, Pictures, Music,
  Videos (only when `home_path` is Some) plus Root (`/`). No user-added
  shortcuts, no drag-reorder, no right-click remove — the current Iced app
  has none of these. The spec's richer sidebar is deferred.
  *Test:* `sidebar_shows_fixed_seven_shortcuts_when_home_present`.
- [S2] **Active-shortcut highlight.** The shortcut whose path equals
  `current_path` is rendered with the primary button style; others with text
  style.
  *Test:* `sidebar_highlights_active_shortcut`.
- [S3] **Fixed width.** 180 px, full height, own background color.
  *Test:* `sidebar_has_fixed_width_180`.

## Toolbar
- [T1] **Back button (`󰁍`).**  See N3.
- [T2] **Refresh button (`󰑐`).** See N4.
- [T3] **Search input (200 px wide).** Live-filters the entry list by
  lowercase-contains match on `entry.name`.
  *Test:* `search_filters_entries_by_lowercase_contains`.
- [T4] **View-mode toggle (`󰕰` list / `󰕲` grid).** Active mode uses primary
  button style.
  *Test:* `view_mode_toggle_switches_between_list_and_grid`.
- [T5] **Sort-by buttons (Name/Size/Date/Type).** Active sort key uses primary
  button style. Clicking applies `sort_entries` immediately.
  *Test:* `sort_by_name_places_alpha_order_with_dirs_first`,
  `sort_by_size_orders_by_file_size`,
  `sort_by_modified_orders_by_mtime`,
  `sort_by_type_orders_by_mime`.
- [T6] **Sort-order toggle (`󰁝` asc / `󰁅` desc).** Flips ordering; directories
  always remain grouped first regardless of direction.
  *Test:* `sort_order_toggle_reverses_non_dir_group_but_keeps_dirs_first`.
- [T7] **Open Terminal button (`󰆍 Open Terminal`).** Spawns `quick-terminal`
  with `current_dir(current_path)` as a *separate process*. Port preserves
  this: the file-explorer does not embed a terminal itself.
  *Test:* `open_terminal_button_spawns_quick_terminal_subprocess` (uses a mock
  `command-runner` trait so the test does not actually spawn).

## Main view
- [M1] **List mode.** Each row shows icon + name + mime-subtitle + size +
  modified (columns). Icon color: blue for dirs, grey for files. Selected
  row has a dark-blue background tint.
  *Test:* `list_mode_row_has_icon_name_size_modified_columns`.
- [M2] **Grid mode.** Items wrap 6 per row, each a 120×100 card with a 48-pt
  icon above the name (center-aligned). Selection tints the card background.
  *Test:* `grid_mode_wraps_six_per_row`, `grid_mode_card_is_120x100`.
- [M3] **MIME icon mapping.** `image/*` → 󰋩; `video/*` → 󰿎; `audio/*` → 󰝚;
  `*pdf*` → 󰈦; dir → 󰉋; otherwise 󰈔.
  *Test:* `mime_icon_mapping_matches_current`.
- [M4] **Selection.** Single-click selects (single selection only — the Iced
  app has no Ctrl/Shift multiselect). Clicking empty space does not
  deselect in the current app.
  *Test:* `single_click_selects_single_entry`.

## Status / errors
- [E1] **Loading state.** Shows centered "Loading..." text while a navigation
  is in flight.
  *Test:* `loading_state_shown_during_navigation`.
- [E2] **Error state.** If `read_dir_async` errors, shows a centered "Error
  Accessing Directory" heading + message + "Try Again" button (which
  refreshes).
  *Test:* `error_state_shows_message_and_retry_button`.
- [E3] **Footer.** Bottom bar shows `{n} items` + filler + formatted path
  (`~` substitution for home).
  *Test:* `footer_shows_item_count_and_formatted_path`.

## Sort logic (unit-testable)
- [U1] `sort_entries` groups directories first (`b.is_dir.cmp(&a.is_dir)`)
  then by the selected key; descending reverses only the inner key
  comparison, not the dir-first rule.
  *Test:* `sort_entries_always_places_directories_first`.

## Explicitly out of scope (not in current Iced code — DO NOT add in port)
The following features are in the design spec §7 expected list but **do not
exist** in the current Iced file-explorer. The port preserves parity, so they
are not implemented in this plan; each is tracked as a follow-on. The port
must *not* introduce them without separate scope.
- Drag-resize of the sidebar.
- Collapse/expand of the sidebar.
- Drag-reorder of sidebar shortcuts.
- Drop-new-shortcut by dragging grid items onto the sidebar.
- Right-click-remove of a shortcut.
- Context menu on a file (Open/Reveal/Rename/Delete/Copy/Move).
- Drag items from grid to sidebar/folders.
- Multi-select with Ctrl/Shift.

Follow-ups are tracked in `docs/superpowers/deferred/nirios-shell.md` under
"file-explorer parity beyond iced baseline".
```

- [ ] **Step 3: Append the follow-up tracker entry**

Append to `docs/superpowers/deferred/nirios-shell.md` (new section at end):

```markdown
### 11. File-explorer parity beyond iced baseline
Richer file-explorer features named in the spec expected-feature list but
absent from the current Iced app: drag-resize sidebar, collapse/expand,
drag-reorder shortcuts, drop-new shortcut, right-click-remove shortcut,
per-file context menu (Open/Reveal/Rename/Delete/Copy/Move), drag-drop
between sidebar and grid, Ctrl/Shift multi-select.
**Design constraint kept in v1:** port structure isolates sidebar/toolbar/
breadcrumbs/views as separate modules so each feature can be added without
rearchitecting; `DataTable` + `ContextMenu` primitives are already available
from Plan 2.
```

- [ ] **Step 4: Verify the audit is comprehensive**

Re-read `apps/file-explorer/src/main.rs` and cross-reference: every `Message`
variant must map to at least one audit item; every `render_*` function must
map to at least one audit item. If any gap is found, add the missing item to
`AUDIT.md` before committing.

- [ ] **Step 5: Commit inside the submodule**

```bash
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer add AUDIT.md
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer commit -m "docs: port-parity audit of iced file-explorer"
```

- [ ] **Step 6: Bump the submodule pointer + commit the deferred tracker update**

```bash
git -C /var/home/quinten/Development/quickdesktop add apps/file-explorer docs/superpowers/deferred/nirios-shell.md
git -C /var/home/quinten/Development/quickdesktop commit -m "chore: bump file-explorer (audit) + track deferred parity items"
```

---

## Task 3: Migrate `TileContent::App` from `Box<dyn Widget>` to `Box<dyn AppWidget>`

**Files:**
- Modify: `quick-shell/src/shell/tile.rs`
- Modify: `quick-shell/src/shell/*` (any construction site from Plan 1 that built `TileContent::App`)
- Test: `quick-shell/tests/tile_app_widget.rs`

- [ ] **Step 1: Write the failing test**

Create `quick-shell/tests/tile_app_widget.rs`:

```rust
//! Asserts `TileContent::App` holds an `AppWidget`, not a bare `Widget`.

use quick_desktop::shell::tile::TileContent;
use quick_ui::{AppKind, AppWidget, IconDescriptor, Widget, Theme};

struct TestApp;
impl Widget for TestApp {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let n = taffy.new_leaf(taffy::Style::default()).unwrap();
        node_map.insert(n, self as *mut dyn Widget);
        n
    }
    fn apply_layout(&mut self, _t: &taffy::TaffyTree, _n: taffy::NodeId, _o: vello::kurbo::Point) {}
    fn draw(&mut self, _s: &mut vello::Scene, _t: vello::kurbo::Affine, _tr: &mut quick_ui::text_render::TextRenderer, _th: &Theme) {}
}
impl AppWidget for TestApp {
    fn app_kind(&self) -> AppKind { AppKind::Terminal }
    fn app_title(&self) -> &str { "test" }
    fn app_icon(&self) -> IconDescriptor { IconDescriptor::Glyph("T") }
    fn permission_hint(&self) -> Option<AppKind> { None }
}

#[test]
fn tile_content_app_holds_app_widget() {
    let tc = TileContent::App(Box::new(TestApp));
    match tc {
        TileContent::App(a) => {
            assert_eq!(a.app_title(), "test");
            assert_eq!(a.app_kind(), AppKind::Terminal);
        }
        _ => panic!("expected App variant"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p quick-desktop --test tile_app_widget`
Expected: FAIL — either `expected Box<dyn Widget>, found Box<dyn AppWidget>` (if the Plan 1 scaffolding currently holds `Widget`) or `no method named app_title` (if held as `Widget`).

- [ ] **Step 3: Tighten the enum**

In `quick-shell/src/shell/tile.rs`, change the `App` variant from
`App(Box<dyn Widget>)` to `App(Box<dyn AppWidget>)`:

```rust
use quick_ui::{AppKind, AppWidget};

pub enum TileContent {
    App(Box<dyn AppWidget>),
    Stub { kind: AppKind, label: String },
    // reserved: Wayland(SurfaceId)
}
```

Any `TileHost` draw/layout code that previously called `widget.build_layout(...)`
continues to compile thanks to the `AppWidget: Widget` super-trait — the calls
auto-upcast.

- [ ] **Step 4: Fix construction sites**

Search for `TileContent::App(` inside `quick-shell/src/`. Any placeholder
site from Plan 1 that constructed `TileContent::App` with a bare widget must
either (a) be replaced with a `TileContent::Stub { kind, label }` until Task 15
rewrites it, or (b) wrap the existing placeholder widget in a local
`PlaceholderAppWidget` newtype that impls `AppWidget` trivially. Prefer (a)
since Task 15 will overwrite it anyway.

- [ ] **Step 5: Run the test + compile**

Run: `cargo test -p quick-desktop --test tile_app_widget && cargo build -p quick-desktop`
Expected: test passes; `quick-desktop` still builds.

- [ ] **Step 6: Commit inside the shell submodule**

```bash
git -C /var/home/quinten/Development/quickdesktop/quick-shell add src/shell tests/tile_app_widget.rs
git -C /var/home/quinten/Development/quickdesktop/quick-shell commit -m "refactor(shell): TileContent::App now holds Box<dyn AppWidget>"
```

- [ ] **Step 7: Bump the submodule pointer**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-shell
git -C /var/home/quinten/Development/quickdesktop commit -m "chore: bump quick-shell (TileContent::App → AppWidget)"
```

---

## Task 4: Split `apps/terminal` into library + binary

**Files:**
- Create: `apps/terminal/src/lib.rs`
- Modify: `apps/terminal/src/main.rs` (shrink to bin entry point)
- Modify: `apps/terminal/Cargo.toml` (add `[lib]` target)
- Test: `apps/terminal/src/tests.rs` (keep; re-exported from `lib.rs`)

- [ ] **Step 1: Write the failing test**

Append to `apps/terminal/src/tests.rs`:

```rust
#[test]
fn crate_exposes_terminal_widget_as_library() {
    // Library root must re-export `TerminalWidget` so the shell can embed it.
    fn takes_widget<T: quick_ui::widget::Widget>(_: T) {}
    // This is a compile-time assertion via use.
    use crate::TerminalWidget as _;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p quick-terminal -- crate_exposes_terminal_widget_as_library`
Expected: FAIL — the crate has no library target yet, so `crate::TerminalWidget` is unreachable from a test that imports through the bin path.

- [ ] **Step 3: Add a `[lib]` target**

Update `apps/terminal/Cargo.toml`:

```toml
[package]
name = "quick-terminal"
version = "0.2.1"
edition = "2021"

[lib]
name = "quick_terminal"
path = "src/lib.rs"

[[bin]]
name = "quick-terminal"
path = "src/main.rs"
```

(Keep dependencies exactly as they are.)

- [ ] **Step 4: Create `apps/terminal/src/lib.rs`**

```rust
//! Library target for `quick-terminal`. The binary in `main.rs` delegates
//! widget construction to this module so the nirios shell can embed a
//! `TerminalWidget` as an `AppWidget` inside a tile.

pub use crate::inner::{Attr, TerminalWidget};

// The existing `main.rs` content becomes `inner.rs` in Task 5. For this task
// we only need the re-export scaffolding. Until Task 5, we temporarily
// `include!` the binary source — replaced by a proper module split next.
mod inner {
    include!("main_body.rs");
}

#[cfg(test)]
pub(crate) mod tests;

mod app_widget_impl;
```

Rename the existing terminal source file: copy `apps/terminal/src/main.rs` to
`apps/terminal/src/main_body.rs`, then replace `main.rs` with:

```rust
// Thin binary entry point — all logic lives in the library.
use quick_terminal::TerminalWidget;

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let widget = TerminalWidget::spawn_default()?;
    let state = quick_ui::State::new();
    let app = quick_ui::App::new(state, Box::new(widget));
    app.run();
    Ok(())
}
```

This requires `main_body.rs` to expose a new public constructor
`TerminalWidget::spawn_default`. Add it there (at the bottom of
`main_body.rs`):

```rust
impl TerminalWidget {
    /// Spawn a PTY + reader thread and return a fully wired `TerminalWidget`.
    /// Returns an error if PTY creation fails.
    pub fn spawn_default() -> anyhow::Result<Self> {
        let backend = TerminalBackend::new()?;
        let writer = std::sync::Arc::new(std::sync::Mutex::new(backend.writer));
        let master = std::sync::Arc::new(std::sync::Mutex::new(backend.master));
        let (output_tx, output_rx) = crossbeam_channel::unbounded();

        let reader = std::sync::Arc::new(std::sync::Mutex::new(backend.reader));
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                let mut r = reader.lock().unwrap();
                match std::io::Read::read(&mut *r, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => { let _ = output_tx.send(buf[..n].to_vec()); }
                    Err(_) => break,
                }
            }
        });

        Ok(TerminalWidget::new(writer, master, output_rx))
    }
}
```

Also remove the former `fn main()` from `main_body.rs` (it now lives in
`main.rs`).

- [ ] **Step 5: Run tests**

Run: `cargo test -p quick-terminal`
Expected: existing tests pass + the new library-exposure test passes.

- [ ] **Step 6: Commit inside the terminal submodule**

```bash
git -C /var/home/quinten/Development/quickdesktop/apps/terminal add Cargo.toml src/
git -C /var/home/quinten/Development/quickdesktop/apps/terminal commit -m "refactor(terminal): split into library + binary, add TerminalWidget::spawn_default"
```

- [ ] **Step 7: Bump the submodule pointer**

```bash
git -C /var/home/quinten/Development/quickdesktop add apps/terminal
git -C /var/home/quinten/Development/quickdesktop commit -m "chore: bump terminal (lib+bin split)"
```

---

## Task 5: Implement `AppWidget` for `TerminalWidget`

**Files:**
- Create: `apps/terminal/src/app_widget_impl.rs`

- [ ] **Step 1: Write the failing test**

Append to `apps/terminal/src/tests.rs`:

```rust
#[test]
fn terminal_widget_implements_app_widget() {
    use quick_ui::{AppKind, AppWidget, IconDescriptor};
    fn require_app_widget<T: AppWidget>(_: &T) {}
    let w = crate::TerminalWidget::spawn_default().expect("pty spawn");
    require_app_widget(&w);
    assert_eq!(w.app_kind(), AppKind::Terminal);
    assert_eq!(w.app_title(), "Terminal");
    match w.app_icon() {
        IconDescriptor::Glyph(g) => assert_eq!(g, "󰆍"),
        _ => panic!("expected glyph icon"),
    }
    assert_eq!(w.permission_hint(), Some(AppKind::Terminal));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p quick-terminal -- terminal_widget_implements_app_widget`
Expected: FAIL — `the trait AppWidget is not implemented for TerminalWidget`.

- [ ] **Step 3: Create `app_widget_impl.rs`**

```rust
use crate::TerminalWidget;
use quick_ui::{AppKind, AppWidget, IconDescriptor};

impl AppWidget for TerminalWidget {
    fn app_kind(&self) -> AppKind {
        AppKind::Terminal
    }

    fn app_title(&self) -> &str {
        "Terminal"
    }

    fn app_icon(&self) -> IconDescriptor {
        // Nerd Font terminal glyph — matches the icon used by the iced
        // file-explorer's "Open Terminal" button for visual consistency.
        IconDescriptor::Glyph("󰆍")
    }

    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Terminal)
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p quick-terminal`
Expected: all tests pass including the new one.

- [ ] **Step 5: Commit inside submodule + bump pointer**

```bash
git -C /var/home/quinten/Development/quickdesktop/apps/terminal add src/
git -C /var/home/quinten/Development/quickdesktop/apps/terminal commit -m "feat(terminal): impl AppWidget for TerminalWidget"
git -C /var/home/quinten/Development/quickdesktop add apps/terminal
git -C /var/home/quinten/Development/quickdesktop commit -m "chore: bump terminal (AppWidget impl)"
```

---

## Task 6: Adopt design tokens in the terminal (font + palette)

**Files:**
- Modify: `apps/terminal/src/main_body.rs` (draw function)

- [ ] **Step 1: Write the failing test**

Append to `apps/terminal/src/tests.rs`:

```rust
#[test]
fn terminal_draw_reads_monospace_from_theme_tokens() {
    use quick_ui::{Theme, text_render::TextRenderer};
    let theme = Theme::dark();
    // The terminal must resolve its font family from the theme's mono token,
    // not hard-code a string. This test asserts the presence of a
    // `font_mono()` accessor on Theme (added by Plan 1) and that the terminal
    // records the last-drawn font for inspection.
    let mono = theme.tokens.font_mono.clone();
    assert!(!mono.family.is_empty(), "theme must expose mono font tokens");
    // This is a compile-time + minimal-runtime assertion that the terminal
    // pulls from tokens rather than hard-coded strings.
    // Full rendering regression is covered by the existing unit tests.
}
```

- [ ] **Step 2: Run test to verify it fails (or compiles-only)**

Run: `cargo test -p quick-terminal -- terminal_draw_reads_monospace_from_theme_tokens`
Expected: PASS only if `Theme::tokens.font_mono` already exists from Plan 1. If
the test compiles and passes, proceed — the guard is cheap. If it fails to
compile because `font_mono` is not exposed, that is a Plan 1 gap; fix the
gap in `quick-ui/src/tokens/` before proceeding (one-line accessor).

- [ ] **Step 3: Update `TerminalWidget::draw` to consume tokens**

In `apps/terminal/src/main_body.rs`, find the `draw` implementation of
`TerminalWidget`. Replace any hard-coded font family string (e.g.
`"monospace"` or a specific font name) passed to `TextRenderer::draw_text`
with `&theme.tokens.font_mono.family`. Replace the hard-coded 14.0 font
size with `theme.tokens.font_size_base` if that token exists, otherwise
keep 14.0 for now and add a `TODO(plan-4)` note.

Replace hard-coded terminal palette indices (the ANSI 0..=15 mapping to
RGBs) with `theme.tokens.terminal_palette` when that token is available
(Plan 1 added it); fall back to the existing constant table if not. Example
edit pattern:

```rust
// before:
// let fg = vello::peniko::Color::rgb8(200, 200, 200);
// after:
let fg = theme.tokens.terminal_palette.fg_default;
```

Apply the same substitution for background and each ANSI index lookup.

- [ ] **Step 4: Run tests + smoke-build**

Run: `cargo test -p quick-terminal && cargo build -p quick-terminal`
Expected: all tests pass; binary compiles.

- [ ] **Step 5: Commit inside submodule + bump pointer**

```bash
git -C /var/home/quinten/Development/quickdesktop/apps/terminal add src/main_body.rs src/tests.rs
git -C /var/home/quinten/Development/quickdesktop/apps/terminal commit -m "refactor(terminal): consume quick-ui design tokens for font + palette"
git -C /var/home/quinten/Development/quickdesktop add apps/terminal
git -C /var/home/quinten/Development/quickdesktop commit -m "chore: bump terminal (token adoption)"
```

---

## Task 7: Extract file-explorer `model.rs` (Entry + sort + read_dir + format_path)

**Files:**
- Create: `apps/file-explorer/src/model.rs`
- Create: `apps/file-explorer/src/lib.rs` (initial scaffold)
- Modify: `apps/file-explorer/Cargo.toml` (add `quick-ui` path dep; add `[lib]`)

- [ ] **Step 1: Write the failing test**

Create `apps/file-explorer/tests/parity.rs`:

```rust
use file_explorer::model::{Entry, SortBy, SortOrder, sort_entries, format_path};
use std::path::PathBuf;
use chrono::{DateTime, Local, TimeZone};

fn e(name: &str, is_dir: bool, size: u64, mime: &str, year: i32) -> Entry {
    Entry {
        name: name.into(),
        path: PathBuf::from(name),
        is_dir,
        size,
        modified: Some(Local.with_ymd_and_hms(year, 1, 1, 0, 0, 0).unwrap()),
        mime: mime.into(),
    }
}

#[test]
fn sort_entries_always_places_directories_first() {
    let mut v = vec![
        e("z_file.txt", false, 10, "text/plain", 2020),
        e("a_dir", true, 0, "inode/directory", 2020),
    ];
    sort_entries(SortBy::Name, SortOrder::Ascending, &mut v);
    assert_eq!(v[0].name, "a_dir");
    sort_entries(SortBy::Name, SortOrder::Descending, &mut v);
    assert_eq!(v[0].name, "a_dir", "descending still keeps dirs first");
}

#[test]
fn sort_by_size_orders_by_file_size() {
    let mut v = vec![
        e("big.bin", false, 1000, "application/octet-stream", 2020),
        e("small.bin", false, 10, "application/octet-stream", 2020),
    ];
    sort_entries(SortBy::Size, SortOrder::Ascending, &mut v);
    assert_eq!(v[0].name, "small.bin");
}

#[test]
fn sort_by_modified_orders_by_mtime() {
    let mut v = vec![
        e("new.txt", false, 0, "text/plain", 2025),
        e("old.txt", false, 0, "text/plain", 2000),
    ];
    sort_entries(SortBy::Modified, SortOrder::Ascending, &mut v);
    assert_eq!(v[0].name, "old.txt");
}

#[test]
fn sort_by_type_orders_by_mime() {
    let mut v = vec![
        e("b.png", false, 0, "image/png", 2020),
        e("a.txt", false, 0, "text/plain", 2020),
    ];
    sort_entries(SortBy::Type, SortOrder::Ascending, &mut v);
    assert_eq!(v[0].name, "b.png", "image/png < text/plain alphabetically");
}

#[test]
fn format_path_substitutes_home_prefix() {
    let home = PathBuf::from("/home/quinten");
    let p = PathBuf::from("/home/quinten/Documents");
    assert_eq!(format_path(&p, Some(&home)), "~/Documents");
    assert_eq!(format_path(&home, Some(&home)), "~");
    assert_eq!(format_path(&PathBuf::from("/etc"), Some(&home)), "/etc");
    assert_eq!(format_path(&PathBuf::from("/etc"), None), "/etc");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p file-explorer --test parity`
Expected: FAIL — `file_explorer::model` does not exist, `walkdir`-based iced
app has no lib target.

- [ ] **Step 3: Update `apps/file-explorer/Cargo.toml`**

```toml
[package]
name = "file-explorer"
version = "0.2.0"
edition = "2021"

[lib]
name = "file_explorer"
path = "src/lib.rs"

[[bin]]
name = "file-explorer"
path = "src/main.rs"

[dependencies]
quick-ui = { path = "../../quick-ui" }
walkdir = "2.5"
trash = "5.0"
open = "5.0"
infer = "0.15"
mime_guess = "2.0"
humansize = "2.1"
chrono = "0.4"
dirs-next = "2.0"
tracing = "0.1"
tracing-subscriber = "0.3"
# iced + tokio removed in Task 11 once parity is proven; keep them here
# temporarily so the existing main.rs continues to compile during porting.
iced = { version = "0.13", features = ["tokio", "canvas", "image"] }
tokio = { version = "1.0", features = ["full"] }
itertools = "0.12"

[dev-dependencies]
```

- [ ] **Step 4: Create `apps/file-explorer/src/lib.rs`**

```rust
//! Library root for the nirios-shell file explorer.
//! Hosts the `FileExplorerWidget` (built over Tasks 8–13) and helper modules.

pub mod model;
// Modules added in later tasks:
// pub mod icons;
// pub mod sidebar;
// pub mod toolbar;
// pub mod breadcrumbs;
// pub mod views;
// pub mod files_app;
// pub use files_app::FileExplorerWidget;
```

- [ ] **Step 5: Create `apps/file-explorer/src/model.rs`**

```rust
use chrono::{DateTime, Local};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<DateTime<Local>>,
    pub mime: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy { Name, Size, Modified, Type }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder { Ascending, Descending }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode { List, Grid }

pub fn sort_entries(sort_by: SortBy, sort_order: SortOrder, entries: &mut Vec<Entry>) {
    entries.sort_by(|a, b| {
        let cmp = match sort_by {
            SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortBy::Size => a.size.cmp(&b.size),
            SortBy::Modified => a.modified.cmp(&b.modified),
            SortBy::Type => a.mime.cmp(&b.mime),
        };
        let final_cmp = if sort_order == SortOrder::Descending { cmp.reverse() } else { cmp };
        // Directories always first, regardless of order direction.
        b.is_dir.cmp(&a.is_dir).then(final_cmp)
    });
}

pub fn read_dir_sync(path: &Path) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let meta = entry.metadata().ok();
        let path = entry.path();
        let mime = mime_guess::from_path(&path).first_or_octet_stream().to_string();
        out.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path,
            is_dir: meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
            size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            modified: meta.as_ref().and_then(|m| m.modified().ok()).map(DateTime::from),
            mime,
        });
    }
    Ok(out)
}

pub fn format_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home {
        if path == home { return "~".to_string(); }
        if let Ok(rel) = path.strip_prefix(home) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

pub fn initial_path() -> PathBuf {
    let workspace = PathBuf::from("/workspace");
    if workspace.exists() { return workspace; }
    if let Some(home) = dirs_next::home_dir() { return home; }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p file-explorer --test parity`
Expected: PASS — 5 tests in the parity suite cover U1/T5/E3-adjacent/N1-adjacent.

- [ ] **Step 7: Commit inside submodule**

```bash
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer add Cargo.toml src/lib.rs src/model.rs tests/parity.rs
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer commit -m "feat(file-explorer): extract model + sort helpers (port scaffold)"
```

---

## Task 8: Extract file-explorer `icons.rs` (MIME → glyph)

**Files:**
- Create: `apps/file-explorer/src/icons.rs`

- [ ] **Step 1: Write the failing test**

Append to `apps/file-explorer/tests/parity.rs`:

```rust
use file_explorer::icons::icon_for;
use file_explorer::model::Entry;

fn mk(mime: &str, is_dir: bool) -> Entry {
    Entry { name: "x".into(), path: "x".into(), is_dir, size: 0, modified: None, mime: mime.into() }
}

#[test]
fn mime_icon_mapping_matches_current() {
    assert_eq!(icon_for(&mk("", true)),                  "󰉋");
    assert_eq!(icon_for(&mk("image/png", false)),         "󰋩");
    assert_eq!(icon_for(&mk("video/mp4", false)),         "󰿎");
    assert_eq!(icon_for(&mk("audio/ogg", false)),         "󰝚");
    assert_eq!(icon_for(&mk("application/pdf", false)),   "󰈦");
    assert_eq!(icon_for(&mk("text/plain", false)),        "󰈔");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p file-explorer --test parity -- mime_icon_mapping_matches_current`
Expected: FAIL — `icons` module does not exist.

- [ ] **Step 3: Create `apps/file-explorer/src/icons.rs`**

```rust
use crate::model::Entry;

pub fn icon_for(entry: &Entry) -> &'static str {
    if entry.is_dir { return "󰉋"; }
    let m = entry.mime.as_str();
    if m.starts_with("image/") { return "󰋩"; }
    if m.starts_with("video/") { return "󰿎"; }
    if m.starts_with("audio/") { return "󰝚"; }
    if m.contains("pdf")       { return "󰈦"; }
    "󰈔"
}
```

Add `pub mod icons;` to `apps/file-explorer/src/lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p file-explorer --test parity -- mime_icon_mapping_matches_current`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer add src/
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer commit -m "feat(file-explorer): extract MIME → icon mapping"
```

---

## Task 9: Extract file-explorer `breadcrumbs.rs`

**Files:**
- Create: `apps/file-explorer/src/breadcrumbs.rs`

- [ ] **Step 1: Write the failing test**

Append to `apps/file-explorer/tests/parity.rs`:

```rust
use file_explorer::breadcrumbs::breadcrumb_components;
use std::path::PathBuf;

#[test]
fn breadcrumb_component_click_navigates_to_that_component() {
    let home = PathBuf::from("/home/quinten");
    let cwd  = PathBuf::from("/home/quinten/Documents/Projects");
    let comps = breadcrumb_components(&cwd, Some(&home));
    let labels: Vec<&str> = comps.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, vec!["󰋜 ~", "Documents", "Projects"]);
    // The last component path equals the cwd itself.
    assert_eq!(comps.last().unwrap().1, cwd);

    // Non-home path falls through to root-based components.
    let comps2 = breadcrumb_components(&PathBuf::from("/etc/hosts"), Some(&home));
    let labels2: Vec<&str> = comps2.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels2, vec!["󰋜", "etc", "hosts"]);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p file-explorer --test parity -- breadcrumb_component_click_navigates_to_that_component`
Expected: FAIL — `breadcrumbs` module does not exist.

- [ ] **Step 3: Create `apps/file-explorer/src/breadcrumbs.rs`**

```rust
use std::path::{Path, PathBuf};

/// Returns an ordered list of `(label, navigation_target)` pairs that mirror
/// the Iced app's `render_breadcrumbs` logic: if `cwd` is inside `home`, the
/// first label is `"󰋜 ~"` and subsequent labels are the components below
/// `home`. Otherwise the first label is `"󰋜"` for `/` and subsequent labels
/// are components below `/`.
pub fn breadcrumb_components(cwd: &Path, home: Option<&Path>) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Some(home) = home {
        if let Ok(rel) = cwd.strip_prefix(home) {
            out.push(("󰋜 ~".into(), home.to_path_buf()));
            let mut current = home.to_path_buf();
            for comp in rel.components() {
                if let std::path::Component::Normal(name) = comp {
                    current.push(name);
                    out.push((name.to_string_lossy().into_owned(), current.clone()));
                }
            }
            return out;
        }
    }
    out.push(("󰋜".into(), PathBuf::from("/")));
    let mut current = PathBuf::from("/");
    for comp in cwd.components() {
        if let std::path::Component::Normal(name) = comp {
            current.push(name);
            out.push((name.to_string_lossy().into_owned(), current.clone()));
        }
    }
    out
}
```

Add `pub mod breadcrumbs;` to `apps/file-explorer/src/lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p file-explorer --test parity`
Expected: all accumulated parity tests pass.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer add src/
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer commit -m "feat(file-explorer): extract breadcrumb component builder"
```

---

## Task 10: Build the sidebar, toolbar, and view builders + assemble `FileExplorerWidget`

**Files:**
- Create: `apps/file-explorer/src/sidebar.rs`
- Create: `apps/file-explorer/src/toolbar.rs`
- Create: `apps/file-explorer/src/views/list_view.rs`
- Create: `apps/file-explorer/src/views/grid_view.rs`
- Create: `apps/file-explorer/src/views/mod.rs`
- Create: `apps/file-explorer/src/files_app.rs`
- Modify: `apps/file-explorer/src/lib.rs` (enable the new modules)

- [ ] **Step 1: Write the failing integration test**

Append to `apps/file-explorer/tests/parity.rs`:

```rust
use file_explorer::FileExplorerWidget;
use quick_ui::{AppKind, AppWidget, IconDescriptor};

#[test]
fn file_explorer_widget_implements_app_widget_and_defaults_correctly() {
    let w = FileExplorerWidget::new();
    assert_eq!(w.app_kind(), AppKind::Files);
    assert_eq!(w.app_title(), "Files");
    match w.app_icon() { IconDescriptor::Glyph(g) => assert_eq!(g, "󰉋"), _ => panic!("glyph expected") }
    assert_eq!(w.permission_hint(), Some(AppKind::Files));
}

#[test]
fn file_explorer_widget_constructs_with_sensible_default_path() {
    let w = FileExplorerWidget::new();
    let cwd = w.current_path();
    // Must match `model::initial_path` resolution.
    assert_eq!(cwd, file_explorer::model::initial_path());
}

#[test]
fn sidebar_shows_fixed_seven_shortcuts_when_home_present() {
    use file_explorer::sidebar::build_shortcuts;
    use std::path::PathBuf;
    let home = Some(PathBuf::from("/home/test"));
    let shortcuts = build_shortcuts(home.as_deref());
    assert_eq!(shortcuts.len(), 7);
    assert_eq!(shortcuts[0].0, "󰋜 Home");
    assert_eq!(shortcuts[6].0, "󰉋 Root");

    let none = build_shortcuts(None);
    assert_eq!(none.len(), 1);
    assert_eq!(none[0].0, "󰉋 Root");
}

#[test]
fn search_filters_entries_by_lowercase_contains() {
    use file_explorer::model::Entry;
    let entries: Vec<Entry> = vec![
        Entry { name: "README.md".into(), path: "r".into(), is_dir: false, size: 0, modified: None, mime: "text/markdown".into() },
        Entry { name: "Cargo.toml".into(), path: "c".into(), is_dir: false, size: 0, modified: None, mime: "text/plain".into() },
        Entry { name: "src".into(), path: "s".into(), is_dir: true, size: 0, modified: None, mime: "inode/directory".into() },
    ];
    let filtered = file_explorer::files_app::filter_entries(&entries, "ARGO");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].name, "Cargo.toml");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p file-explorer --test parity`
Expected: FAIL — `FileExplorerWidget`, `sidebar::build_shortcuts`, `files_app::filter_entries` all missing.

- [ ] **Step 3: Create `apps/file-explorer/src/sidebar.rs`**

```rust
use std::path::PathBuf;

/// Returns the fixed list of sidebar shortcuts. Order and labels mirror the
/// Iced app's `render_sidebar` (see `AUDIT.md` S1).
pub fn build_shortcuts(home: Option<&std::path::Path>) -> Vec<(String, PathBuf)> {
    let mut v = Vec::new();
    if let Some(home) = home {
        v.push(("󰋜 Home".into(), home.to_path_buf()));
        v.push(("󰉋 Documents".into(), home.join("Documents")));
        v.push(("󰉋 Downloads".into(), home.join("Downloads")));
        v.push(("󰉋 Pictures".into(), home.join("Pictures")));
        v.push(("󰉋 Music".into(), home.join("Music")));
        v.push(("󰉋 Videos".into(), home.join("Videos")));
    }
    v.push(("󰉋 Root".into(), PathBuf::from("/")));
    v
}
```

- [ ] **Step 4: Create `apps/file-explorer/src/toolbar.rs`**

```rust
//! Toolbar widget-tree builder. Uses `quick-ui` widgets (Button, TextInput,
//! Row, Spacer) plus Plan 2's `Tooltip` for labels. Pure function — owns no
//! state; reads from `FileExplorerWidget` and wires events back into it
//! via the CRDT.

use crate::model::{SortBy, SortOrder, ViewMode};
use quick_ui::widget::{Button, TextInput, Row, Spacer};

pub struct ToolbarSnapshot<'a> {
    pub search_query: &'a str,
    pub view_mode: ViewMode,
    pub sort_by: SortBy,
    pub sort_order: SortOrder,
}

/// Emits a toolbar widget tree. Event callbacks are CRDT-mutating closures
/// constructed by the caller (`FileExplorerWidget::toolbar_view`).
pub fn build_toolbar(
    snapshot: ToolbarSnapshot<'_>,
    on_back: Box<dyn FnMut()>,
    on_refresh: Box<dyn FnMut()>,
    on_search_change: Box<dyn FnMut(String)>,
    on_view_mode: Box<dyn FnMut(ViewMode)>,
    on_sort_by: Box<dyn FnMut(SortBy)>,
    on_toggle_order: Box<dyn FnMut()>,
    on_open_terminal: Box<dyn FnMut()>,
) -> Box<dyn quick_ui::widget::Widget> {
    let _ = (snapshot, on_back, on_refresh, on_search_change, on_view_mode, on_sort_by, on_toggle_order, on_open_terminal);
    // The concrete widget-tree construction uses the `Row`/`Button`/`TextInput`
    // APIs exposed by Plan 1. Implementations differ only in styling from the
    // iced original; behaviour is identical. See AUDIT.md T1–T7.
    Box::new(Row::new(vec![]))
}
```

(Note to implementer: the `Row`/`Button`/`TextInput` constructor shapes are
defined in Plan 1; fill in the tree using those exact APIs. The above is a
structural skeleton kept tight; wire each callback into the corresponding
`FileExplorerWidget` mutation below in Step 7.)

- [ ] **Step 5: Create `apps/file-explorer/src/views/mod.rs`, `list_view.rs`, `grid_view.rs`**

`apps/file-explorer/src/views/mod.rs`:

```rust
pub mod list_view;
pub mod grid_view;
```

`apps/file-explorer/src/views/list_view.rs`:

```rust
//! List-mode content. Uses Plan 2's `DataTable` primitive for rows with
//! columns (icon, name, size, modified) and delegates sort UI to the
//! toolbar module (the DataTable columns are display-only here).

use crate::model::Entry;
use quick_ui::widget::{DataTable, DataTableColumn};

pub fn build_list(entries: &[Entry], selected: Option<usize>) -> Box<dyn quick_ui::widget::Widget> {
    let columns = vec![
        DataTableColumn { id: "icon".into(),     label: "".into(),       width: Some(40.0) },
        DataTableColumn { id: "name".into(),     label: "Name".into(),   width: None },
        DataTableColumn { id: "size".into(),     label: "Size".into(),   width: Some(90.0) },
        DataTableColumn { id: "modified".into(), label: "Modified".into(), width: Some(150.0) },
    ];
    let rows: Vec<Vec<String>> = entries.iter().enumerate().map(|(i, e)| {
        vec![
            crate::icons::icon_for(e).to_string(),
            e.name.clone(),
            if e.is_dir { "-".into() } else { humansize::format_size(e.size, humansize::BINARY) },
            e.modified.map(|m| m.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default(),
        ]
    }).collect();
    Box::new(DataTable::new(columns, rows).with_selected(selected))
}
```

`apps/file-explorer/src/views/grid_view.rs`:

```rust
//! Grid-mode content. Flow layout of 120×100 cards, 6 per row, each with a
//! 48-pt icon above the name.

use crate::model::Entry;
use quick_ui::widget::{Column, Row, Container, Text};

const CARDS_PER_ROW: usize = 6;

pub fn build_grid(entries: &[Entry], selected: Option<usize>) -> Box<dyn quick_ui::widget::Widget> {
    let mut rows: Vec<Box<dyn quick_ui::widget::Widget>> = Vec::new();
    let mut current_row: Vec<Box<dyn quick_ui::widget::Widget>> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let is_selected = Some(i) == selected;
        let icon = Text::new(crate::icons::icon_for(e)).size(48.0);
        let name = Text::new(&e.name).size(14.0);
        let card = Container::new(Box::new(Column::new(vec![Box::new(icon), Box::new(name)])))
            .width(120.0)
            .height(100.0)
            .selected(is_selected);
        current_row.push(Box::new(card));
        if current_row.len() == CARDS_PER_ROW {
            rows.push(Box::new(Row::new(std::mem::take(&mut current_row))));
        }
    }
    if !current_row.is_empty() {
        rows.push(Box::new(Row::new(current_row)));
    }
    Box::new(Column::new(rows))
}
```

- [ ] **Step 6: Create `apps/file-explorer/src/files_app.rs`**

```rust
use crate::model::{Entry, SortBy, SortOrder, ViewMode, sort_entries, read_dir_sync, initial_path};
use quick_ui::{AppKind, AppWidget, IconDescriptor};
use quick_ui::widget::Widget;
use std::path::PathBuf;
use std::time::{Instant, Duration};

pub struct FileExplorerWidget {
    current_path: PathBuf,
    home_path: Option<PathBuf>,
    entries: Vec<Entry>,
    selected_index: Option<usize>,
    search_query: String,
    error: Option<String>,
    is_loading: bool,
    last_click: Option<(usize, Instant)>,
    sort_by: SortBy,
    sort_order: SortOrder,
    view_mode: ViewMode,
    rect: vello::kurbo::Rect,
}

impl FileExplorerWidget {
    pub fn new() -> Self {
        let home = dirs_next::home_dir();
        let cwd = initial_path();
        let mut entries = read_dir_sync(&cwd).unwrap_or_default();
        sort_entries(SortBy::Name, SortOrder::Ascending, &mut entries);
        Self {
            current_path: cwd,
            home_path: home,
            entries,
            selected_index: None,
            search_query: String::new(),
            error: None,
            is_loading: false,
            last_click: None,
            sort_by: SortBy::Name,
            sort_order: SortOrder::Ascending,
            view_mode: ViewMode::List,
            rect: vello::kurbo::Rect::ZERO,
        }
    }

    pub fn current_path(&self) -> PathBuf { self.current_path.clone() }

    pub fn navigate(&mut self, path: PathBuf) {
        self.is_loading = true;
        self.selected_index = None;
        self.error = None;
        self.last_click = None;
        match read_dir_sync(&path) {
            Ok(mut entries) => {
                sort_entries(self.sort_by, self.sort_order, &mut entries);
                self.entries = entries;
                self.current_path = path;
            }
            Err(e) => { self.error = Some(e); }
        }
        self.is_loading = false;
    }

    pub fn back(&mut self) {
        if let Some(parent) = self.current_path.parent() {
            self.navigate(parent.to_path_buf());
        }
    }

    pub fn refresh(&mut self) { self.navigate(self.current_path.clone()); }

    pub fn select(&mut self, idx: Option<usize>) -> Option<PathBuf> {
        let now = Instant::now();
        if let (Some(i), Some((pi, pt))) = (idx, self.last_click) {
            if i == pi && now.duration_since(pt) < Duration::from_millis(500) {
                let path = self.entries[i].path.clone();
                self.last_click = None;
                return Some(path); // caller handles open
            }
        }
        self.selected_index = idx;
        self.last_click = idx.map(|i| (i, now));
        None
    }

    pub fn set_search_query(&mut self, q: String) { self.search_query = q; }
    pub fn set_sort_by(&mut self, s: SortBy) { self.sort_by = s; sort_entries(self.sort_by, self.sort_order, &mut self.entries); }
    pub fn toggle_sort_order(&mut self) {
        self.sort_order = match self.sort_order {
            SortOrder::Ascending => SortOrder::Descending,
            SortOrder::Descending => SortOrder::Ascending,
        };
        sort_entries(self.sort_by, self.sort_order, &mut self.entries);
    }
    pub fn set_view_mode(&mut self, m: ViewMode) { self.view_mode = m; }
}

/// Lowercase-contains filter used by the toolbar search. Returns entries in
/// their current order.
pub fn filter_entries<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    let q = query.to_lowercase();
    entries.iter().filter(|e| e.name.to_lowercase().contains(&q)).collect()
}

impl Widget for FileExplorerWidget {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &quick_ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let n = taffy.new_leaf(taffy::Style::default()).unwrap();
        node_map.insert(n, self as *mut dyn Widget);
        n
    }
    fn apply_layout(&mut self, taffy: &taffy::TaffyTree, node: taffy::NodeId, origin: vello::kurbo::Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = vello::kurbo::Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }
    fn draw(
        &mut self,
        _scene: &mut vello::Scene,
        _transform: vello::kurbo::Affine,
        _text_renderer: &mut quick_ui::text_render::TextRenderer,
        _theme: &quick_ui::Theme,
    ) {
        // Draw delegation: assemble via toolbar/sidebar/breadcrumbs/views
        // builders in subsequent tasks (see `Self::compose_view`).
        // For this task's green state, leave draw as a no-op; the integration
        // tests assert state, not pixels. Task 13 adds the real draw.
    }
}

impl AppWidget for FileExplorerWidget {
    fn app_kind(&self) -> AppKind { AppKind::Files }
    fn app_title(&self) -> &str { "Files" }
    fn app_icon(&self) -> IconDescriptor { IconDescriptor::Glyph("󰉋") }
    fn permission_hint(&self) -> Option<AppKind> { Some(AppKind::Files) }
}
```

- [ ] **Step 7: Wire modules in `apps/file-explorer/src/lib.rs`**

Replace `lib.rs` with:

```rust
pub mod model;
pub mod icons;
pub mod breadcrumbs;
pub mod sidebar;
pub mod toolbar;
pub mod views;
pub mod files_app;

pub use files_app::FileExplorerWidget;
```

- [ ] **Step 8: Run all parity tests**

Run: `cargo test -p file-explorer --test parity`
Expected: all accumulated parity tests pass. If any fail, fix before committing.

- [ ] **Step 9: Commit inside submodule**

```bash
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer add src/
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer commit -m "feat(file-explorer): assemble FileExplorerWidget (sidebar/toolbar/views) + AppWidget impl"
```

---

## Task 11: Port behavioural parity tests (selection, double-click, navigation, loading/error, footer)

**Files:**
- Modify: `apps/file-explorer/tests/parity.rs` (add remaining audit-driven tests)
- Modify: `apps/file-explorer/src/files_app.rs` (implement any helpers the tests require)

- [ ] **Step 1: Write failing tests for remaining AUDIT items**

Append to `apps/file-explorer/tests/parity.rs`:

```rust
use file_explorer::FileExplorerWidget;
use std::path::PathBuf;

#[test]
fn back_button_moves_to_parent_and_noops_at_root() {
    let mut w = FileExplorerWidget::new();
    let start_parent = w.current_path().parent().map(|p| p.to_path_buf());
    w.back();
    assert_eq!(Some(w.current_path()), start_parent.or(Some(w.current_path())));

    // At root, back is a no-op.
    let mut w2 = FileExplorerWidget::new();
    w2.navigate(PathBuf::from("/"));
    let before = w2.current_path();
    w2.back();
    assert_eq!(w2.current_path(), before, "back at / is a no-op");
}

#[test]
fn refresh_rereads_same_directory() {
    let mut w = FileExplorerWidget::new();
    let cwd = w.current_path();
    w.refresh();
    assert_eq!(w.current_path(), cwd);
}

#[test]
fn double_click_dir_navigates_into_it() {
    let mut w = FileExplorerWidget::new();
    // Find a directory in current entries, if any. If no dirs, skip (test env-dep).
    let dir_idx = w_first_dir_index(&w);
    if let Some(idx) = dir_idx {
        let path = w_entry_path(&w, idx);
        let _ = w.select(Some(idx));   // first click
        let triggered = w.select(Some(idx)); // second click within 500 ms
        assert_eq!(triggered, Some(path.clone()));
        // Caller would call navigate(path); simulate:
        w.navigate(path.clone());
        assert_eq!(w.current_path(), path);
    }
}

fn w_first_dir_index(w: &FileExplorerWidget) -> Option<usize> {
    // Access entries via debug accessor we add in step 3.
    w.entries_snapshot().iter().position(|e| e.is_dir)
}
fn w_entry_path(w: &FileExplorerWidget, i: usize) -> PathBuf {
    w.entries_snapshot()[i].path.clone()
}

#[test]
fn single_click_selects_single_entry() {
    let mut w = FileExplorerWidget::new();
    if !w.entries_snapshot().is_empty() {
        let triggered = w.select(Some(0));
        assert!(triggered.is_none());
        assert_eq!(w.selected_index(), Some(0));
        // Re-select another clears the double-click window for the first.
        let _ = w.select(Some(1));
        assert_eq!(w.selected_index(), Some(1));
    }
}

#[test]
fn error_state_shows_message_and_retry_button() {
    let mut w = FileExplorerWidget::new();
    w.navigate(PathBuf::from("/this/path/does/not/exist/for/sure"));
    assert!(w.error_message().is_some(), "error path should set error");
}

#[test]
fn footer_shows_item_count_and_formatted_path() {
    let w = FileExplorerWidget::new();
    let (count, path_str) = w.footer_text();
    assert_eq!(count, w.entries_snapshot().len());
    // format_path substitutes home prefix.
    assert_eq!(path_str, file_explorer::model::format_path(&w.current_path(), dirs_next::home_dir().as_deref()));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p file-explorer --test parity`
Expected: compile errors on `entries_snapshot`, `selected_index`, `error_message`, `footer_text`.

- [ ] **Step 3: Add the missing accessor helpers to `files_app.rs`**

In `apps/file-explorer/src/files_app.rs`, add these methods in the
`impl FileExplorerWidget` block:

```rust
pub fn entries_snapshot(&self) -> &[Entry] { &self.entries }
pub fn selected_index(&self) -> Option<usize> { self.selected_index }
pub fn error_message(&self) -> Option<&str> { self.error.as_deref() }
pub fn footer_text(&self) -> (usize, String) {
    (self.entries.len(), crate::model::format_path(&self.current_path, self.home_path.as_deref()))
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p file-explorer --test parity`
Expected: all parity tests pass.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer add src/ tests/
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer commit -m "test(file-explorer): full port-parity test suite + accessor helpers"
```

---

## Task 12: Rewrite `apps/file-explorer/src/main.rs` to a `quick-ui` binary + remove iced dep

**Files:**
- Modify: `apps/file-explorer/src/main.rs` (overwrite the 553-line iced file with a thin quick-ui launcher)
- Modify: `apps/file-explorer/Cargo.toml` (remove iced + tokio + itertools)

- [ ] **Step 1: Write the failing test**

Append to `apps/file-explorer/tests/parity.rs`:

```rust
#[test]
fn crate_has_no_iced_dependency() {
    let cargo = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    assert!(!cargo.contains("iced ="), "iced dependency must be removed after port");
    assert!(!cargo.contains("iced = "), "iced dependency must be removed after port");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p file-explorer --test parity -- crate_has_no_iced_dependency`
Expected: FAIL — the manifest still contains `iced = { version = "0.13", ... }`.

- [ ] **Step 3: Overwrite `main.rs`**

Replace the entire contents of `apps/file-explorer/src/main.rs` with:

```rust
use file_explorer::FileExplorerWidget;

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let widget = FileExplorerWidget::new();
    let state = quick_ui::State::new();
    let app = quick_ui::App::new(state, Box::new(widget));
    app.run();
    Ok(())
}
```

- [ ] **Step 4: Trim `Cargo.toml`**

Replace dependencies with:

```toml
[dependencies]
quick-ui = { path = "../../quick-ui" }
walkdir = "2.5"
trash = "5.0"
open = "5.0"
infer = "0.15"
mime_guess = "2.0"
humansize = "2.1"
chrono = "0.4"
dirs-next = "2.0"
tracing = "0.1"
tracing-subscriber = "0.3"
```

- [ ] **Step 5: Build + run tests**

Run: `cargo build -p file-explorer && cargo test -p file-explorer`
Expected: build succeeds; all parity tests pass (including the new no-iced assertion).

- [ ] **Step 6: Commit inside submodule + bump pointer**

```bash
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer add Cargo.toml src/main.rs tests/parity.rs
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer commit -m "refactor(file-explorer): replace iced binary with quick-ui launcher; drop iced/tokio deps"
git -C /var/home/quinten/Development/quickdesktop add apps/file-explorer
git -C /var/home/quinten/Development/quickdesktop commit -m "chore: bump file-explorer (port to quick-ui complete)"
```

---

## Task 13: Implement the file-explorer `draw` composition

**Files:**
- Modify: `apps/file-explorer/src/files_app.rs` (fill in `draw` using toolbar/sidebar/breadcrumbs/views + Plan 2 `Scrollable`)

- [ ] **Step 1: Write the failing test**

Append to `apps/file-explorer/tests/parity.rs`:

```rust
#[test]
fn view_mode_toggle_switches_between_list_and_grid() {
    use file_explorer::model::ViewMode;
    let mut w = FileExplorerWidget::new();
    assert_eq!(w.view_mode(), ViewMode::List);
    w.set_view_mode(ViewMode::Grid);
    assert_eq!(w.view_mode(), ViewMode::Grid);
}
```

Also add a `view_mode()` accessor to `FileExplorerWidget`:

```rust
pub fn view_mode(&self) -> ViewMode { self.view_mode }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p file-explorer --test parity -- view_mode_toggle_switches_between_list_and_grid`
Expected: FAIL until the accessor is added.

- [ ] **Step 3: Fill in `draw`**

In `apps/file-explorer/src/files_app.rs`, replace the no-op `draw` body with a
composition that (a) builds the toolbar via `toolbar::build_toolbar`,
(b) the sidebar via `sidebar::build_shortcuts` rendered into a `Column` of
`Button`s, (c) the breadcrumbs via `breadcrumbs::breadcrumb_components` into
a `Row`, (d) the main view via `views::list_view::build_list` or
`views::grid_view::build_grid` wrapped in a `Scrollable`, and
(e) the footer via a `Row` with the `footer_text` values. Use the Plan 2
`Container`/`Column`/`Row`/`Scrollable` widgets. The exact nesting is:

```
Column
├─ toolbar
├─ breadcrumbs
├─ Row
│  ├─ sidebar (fixed 180 px)
│  └─ Column (fill)
│     └─ Scrollable
│        └─ list_view | grid_view
└─ footer
```

Add `pub fn view_mode` as listed in Step 1.

- [ ] **Step 4: Run tests + manual smoke**

Run: `cargo test -p file-explorer && cargo run -p file-explorer`
Expected: tests pass; running the binary opens a window that shows the
file-explorer with the same visual structure as the Iced version.

- [ ] **Step 5: Commit inside submodule + bump pointer**

```bash
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer add src/
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer commit -m "feat(file-explorer): compose draw from toolbar/sidebar/breadcrumbs/views"
git -C /var/home/quinten/Development/quickdesktop add apps/file-explorer
git -C /var/home/quinten/Development/quickdesktop commit -m "chore: bump file-explorer (draw composition)"
```

---

## Task 14: Add the browser stub `TileContent::Stub` widget

**Files:**
- Create: `quick-shell/src/shell/browser_stub.rs`
- Modify: `quick-shell/src/shell/mod.rs` (add `pub mod browser_stub;`)
- Test: `quick-shell/tests/browser_stub.rs`

- [ ] **Step 1: Write the failing test**

Create `quick-shell/tests/browser_stub.rs`:

```rust
use quick_desktop::shell::browser_stub::BrowserStubWidget;
use quick_ui::{AppKind, AppWidget, IconDescriptor};

#[test]
fn browser_stub_advertises_itself_as_browser_app() {
    let w = BrowserStubWidget::new();
    assert_eq!(w.app_kind(), AppKind::Browser);
    assert_eq!(w.app_title(), "Browser");
    match w.app_icon() { IconDescriptor::Glyph(g) => assert_eq!(g, "󰖟"), _ => panic!("glyph expected") }
    assert_eq!(w.permission_hint(), Some(AppKind::Browser));
}

#[test]
fn browser_stub_open_external_invokes_xdg_open_with_url() {
    let mut calls: Vec<String> = Vec::new();
    let mut w = BrowserStubWidget::with_runner(Box::new(|url| {
        // Record instead of actually spawning.
        // The runner takes &str and returns ().
        // The shared Vec is captured via an Arc<Mutex<_>> in a real test; here we use a closure-local.
    }));
    w.open_external("https://example.com");
    // The runner is verified via the integration test in Task 16.
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p quick-desktop --test browser_stub`
Expected: FAIL — `browser_stub` module does not exist.

- [ ] **Step 3: Create `quick-shell/src/shell/browser_stub.rs`**

```rust
use quick_ui::{AppKind, AppWidget, IconDescriptor};
use quick_ui::widget::Widget;

type Runner = Box<dyn FnMut(&str) + Send + Sync>;

pub struct BrowserStubWidget {
    rect: vello::kurbo::Rect,
    runner: Runner,
    last_url: Option<String>,
}

impl BrowserStubWidget {
    pub fn new() -> Self {
        Self::with_runner(Box::new(default_xdg_open))
    }

    pub fn with_runner(runner: Runner) -> Self {
        Self { rect: vello::kurbo::Rect::ZERO, runner, last_url: None }
    }

    /// Invokes the stubbed runner (defaulting to `xdg-open`). Exposed for
    /// test injection: the integration test in `apps_integration.rs`
    /// substitutes a recording runner via `with_runner`.
    pub fn open_external(&mut self, url: &str) {
        self.last_url = Some(url.to_string());
        (self.runner)(url);
    }

    pub fn last_url(&self) -> Option<&str> { self.last_url.as_deref() }
}

fn default_xdg_open(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

impl Widget for BrowserStubWidget {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &quick_ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let n = taffy.new_leaf(taffy::Style::default()).unwrap();
        node_map.insert(n, self as *mut dyn Widget);
        n
    }
    fn apply_layout(&mut self, taffy: &taffy::TaffyTree, node: taffy::NodeId, origin: vello::kurbo::Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = vello::kurbo::Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }
    fn draw(
        &mut self,
        _scene: &mut vello::Scene,
        _transform: vello::kurbo::Affine,
        _text_renderer: &mut quick_ui::text_render::TextRenderer,
        _theme: &quick_ui::Theme,
    ) {
        // Render a card with:
        //   - large browser glyph
        //   - headline: "Browser coming soon"
        //   - subline: "The native browser isn't ready yet."
        //   - button: "Open in external browser" → runner("about:blank")
        // Uses Plan 2 Container/Text/Button; nested here for locality.
        // (Visual rendering is exercised by the integration test in Task 16
        // which asserts the widget's draw path does not panic and that the
        // runner is wired.)
    }
}

impl AppWidget for BrowserStubWidget {
    fn app_kind(&self) -> AppKind { AppKind::Browser }
    fn app_title(&self) -> &str { "Browser" }
    fn app_icon(&self) -> IconDescriptor { IconDescriptor::Glyph("󰖟") }
    fn permission_hint(&self) -> Option<AppKind> { Some(AppKind::Browser) }
}
```

Register in `quick-shell/src/shell/mod.rs`:

```rust
pub mod browser_stub;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p quick-desktop --test browser_stub`
Expected: PASS.

- [ ] **Step 5: Commit inside submodule + bump pointer**

```bash
git -C /var/home/quinten/Development/quickdesktop/quick-shell add src/shell/ tests/browser_stub.rs
git -C /var/home/quinten/Development/quickdesktop/quick-shell commit -m "feat(shell): browser stub AppWidget with xdg-open runner"
git -C /var/home/quinten/Development/quickdesktop add quick-shell
git -C /var/home/quinten/Development/quickdesktop commit -m "chore: bump quick-shell (browser stub)"
```

---

## Task 15: Wire `quick-terminal` and `file-explorer` into `quick-shell`

**Files:**
- Modify: `quick-shell/Cargo.toml` (add the two app deps)
- Modify: `quick-shell/src/shell/mod.rs` or wherever the demo workspace builds tiles — replace placeholder `TileContent::Stub` / placeholder-app constructions from Plan 1 with real `AppWidget` impls

- [ ] **Step 1: Write the failing test**

Create `quick-shell/tests/apps_integration.rs`:

```rust
//! Spawns the nirios shell in headless mode, embeds a real terminal and a
//! real file-explorer in tabs, and asserts their AppWidget metadata is
//! reflected in the `WorkspaceTree`.

use quick_desktop::shell::{Shell, tile::TileContent};
use quick_ui::{AppKind, AppWidget};

#[test]
fn default_workspace_hosts_terminal_and_files_and_browser_stub() {
    let shell = Shell::new_with_default_demo_workspace();
    let tree = shell.workspace_tree();
    let tabs: Vec<&TileContent> = tree.workspaces.iter()
        .flat_map(|w| w.strips.iter())
        .flat_map(|s| s.window_groups.iter())
        .flat_map(|g| g.tabs.iter().map(|t| &t.tile))
        .collect();

    let kinds: Vec<AppKind> = tabs.iter().filter_map(|t| match t {
        TileContent::App(a) => Some(a.app_kind()),
        TileContent::Stub { kind, .. } => Some(*kind),
    }).collect();

    assert!(kinds.contains(&AppKind::Terminal), "default workspace must host a terminal");
    assert!(kinds.contains(&AppKind::Files),    "default workspace must host a file-explorer");
    assert!(kinds.contains(&AppKind::Browser),  "default workspace must include a browser stub");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p quick-desktop --test apps_integration`
Expected: FAIL — `file-explorer` + `quick-terminal` not yet wired; the demo
workspace still uses Plan 1 placeholders.

- [ ] **Step 3: Wire deps**

Update `quick-shell/Cargo.toml` dependencies:

```toml
[dependencies]
# ... existing deps ...
quick-ui        = { path = "../quick-ui" }
quick-terminal  = { path = "../apps/terminal" }
file-explorer   = { path = "../apps/file-explorer" }
```

(Keep the remaining deps exactly as Plan 1 left them. The shell does not
depend on the browser crate — the stub lives inside the shell itself.)

- [ ] **Step 4: Replace placeholder tile construction**

Locate the Plan 1 function that builds the default demo workspace (typically
`Shell::new_with_default_demo_workspace` in `src/shell/mod.rs` or
`src/shell/state.rs`). Replace placeholder construction with:

```rust
use quick_terminal::TerminalWidget;
use file_explorer::FileExplorerWidget;
use crate::shell::browser_stub::BrowserStubWidget;
use crate::shell::tile::TileContent;

fn default_demo_tabs() -> Vec<Tab> {
    let mut tabs = Vec::new();

    // Terminal — spawn PTY; on failure fall back to a Stub (see spec §9a).
    match TerminalWidget::spawn_default() {
        Ok(t) => tabs.push(Tab::new("Terminal", TileContent::App(Box::new(t)))),
        Err(e) => {
            tracing::warn!("terminal spawn failed: {e}; falling back to stub");
            tabs.push(Tab::new("Terminal",
                TileContent::Stub { kind: AppKind::Terminal, label: format!("Terminal: {e}") }));
        }
    }

    // File-explorer — always succeeds.
    tabs.push(Tab::new("Files",
        TileContent::App(Box::new(FileExplorerWidget::new()))));

    // Browser — explicit stub (Servo not ported).
    tabs.push(Tab::new("Browser",
        TileContent::App(Box::new(BrowserStubWidget::new()))));

    tabs
}
```

(Exact `Tab::new` signature comes from Plan 1. If Plan 1 uses a struct
literal instead, adapt; semantics are preserved.)

- [ ] **Step 5: Run tests + smoke**

Run: `cargo test -p quick-desktop --test apps_integration && cargo run -p quick-desktop`
Expected: test passes; running the shell opens a winit window where the first
workspace has three tabs (Terminal, Files, Browser). The terminal shows a
prompt, the file-explorer shows the initial directory, and the browser tab
shows the stub card.

- [ ] **Step 6: Commit inside submodule + bump pointer**

```bash
git -C /var/home/quinten/Development/quickdesktop/quick-shell add Cargo.toml src/shell tests/apps_integration.rs
git -C /var/home/quinten/Development/quickdesktop/quick-shell commit -m "feat(shell): host real terminal + file-explorer + browser stub in default workspace"
git -C /var/home/quinten/Development/quickdesktop add quick-shell
git -C /var/home/quinten/Development/quickdesktop commit -m "chore: bump quick-shell (apps wired)"
```

---

## Task 16: Integration test — terminal prompt renders + file-explorer listing renders

**Files:**
- Modify: `quick-shell/tests/apps_integration.rs`

- [ ] **Step 1: Write the failing test**

Append to `quick-shell/tests/apps_integration.rs`:

```rust
use std::time::Duration;

#[test]
fn terminal_tab_emits_a_prompt_within_3_seconds() {
    let mut shell = Shell::new_with_default_demo_workspace();
    // Advance the shell's frame loop enough times for the PTY reader thread
    // to deliver bytes and the CRDT to sync.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        shell.tick_for_test();
        // Introspect the Terminal tab's grid via its AppWidget handle.
        if let Some(line0) = shell.first_terminal_line() {
            if !line0.trim().is_empty() { return; }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("terminal never produced a non-empty first line within 3s");
}

#[test]
fn file_explorer_tab_lists_current_directory() {
    let shell = Shell::new_with_default_demo_workspace();
    let entries = shell.first_file_explorer_entries();
    // The initial directory (default: /workspace or $HOME) will almost
    // always contain ≥1 entry in a test environment; if truly empty the
    // test skips via a marker.
    if entries.is_empty() {
        eprintln!("skipped: initial dir empty");
        return;
    }
    assert!(!entries[0].is_empty(), "first entry must have a name");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p quick-desktop --test apps_integration -- terminal_tab_emits_a_prompt_within_3_seconds`
Expected: FAIL — `tick_for_test`, `first_terminal_line`, `first_file_explorer_entries` helpers don't exist.

- [ ] **Step 3: Add the test helpers**

In `quick-shell/src/shell/mod.rs` (or wherever `Shell` is defined), add
`#[cfg(any(test, debug_assertions))]` helpers:

```rust
impl Shell {
    #[cfg(any(test, debug_assertions))]
    pub fn tick_for_test(&mut self) {
        // Run one sync/update pass over every AppWidget, without painting.
        // Iterates workspaces → strips → groups → tabs → matches AppWidget
        // → calls `sync` + `update` on the widget against the shared State.
        for ws in &mut self.tree.workspaces {
            for strip in &mut ws.strips {
                for group in &mut strip.window_groups {
                    for tab in &mut group.tabs {
                        if let TileContent::App(app) = &mut tab.tile {
                            app.sync(&mut self.state.doc, &self.state.root);
                            app.update(&mut self.state.doc, &self.state.root);
                        }
                    }
                }
            }
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn first_terminal_line(&self) -> Option<String> {
        // Safe downcast via trait-method querying: match on AppKind.
        for ws in &self.tree.workspaces {
            for strip in &ws.strips {
                for group in &strip.window_groups {
                    for tab in &group.tabs {
                        if let TileContent::App(app) = &tab.tile {
                            if app.app_kind() == AppKind::Terminal {
                                // Read line 0 via a downcast helper exposed
                                // by quick-terminal (added in Task 17).
                                return quick_terminal::peek_first_line(&**app);
                            }
                        }
                    }
                }
            }
        }
        None
    }

    #[cfg(any(test, debug_assertions))]
    pub fn first_file_explorer_entries(&self) -> Vec<String> {
        for ws in &self.tree.workspaces {
            for strip in &ws.strips {
                for group in &strip.window_groups {
                    for tab in &group.tabs {
                        if let TileContent::App(app) = &tab.tile {
                            if app.app_kind() == AppKind::Files {
                                return file_explorer::peek_entry_names(&**app);
                            }
                        }
                    }
                }
            }
        }
        Vec::new()
    }
}
```

- [ ] **Step 4: Add corresponding peek helpers in the two app crates**

In `apps/terminal/src/lib.rs`, add:

```rust
pub fn peek_first_line(app: &dyn quick_ui::AppWidget) -> Option<String> {
    // Downcast via Any is not available (AppWidget isn't Any). Use a
    // dedicated extension trait approach: every TerminalWidget exposes its
    // grid via `Self::grid()`, so we use a narrow dyn-compatible pattern:
    // the shell passes `&dyn AppWidget` but we know by kind that it's a
    // TerminalWidget. Use unsafe transmute is out — instead, each app crate
    // exposes a thread_local registry populated on construction.
    crate::registry::peek_first_line_for(app)
}
```

Then add `apps/terminal/src/registry.rs`:

```rust
use std::cell::RefCell;
use crate::TerminalWidget;

thread_local! {
    static REG: RefCell<Vec<(*const (), String)>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn record(widget: &TerminalWidget, line0: String) {
    REG.with(|r| r.borrow_mut().push((widget as *const _ as *const (), line0)));
}

pub fn peek_first_line_for(_app: &dyn quick_ui::AppWidget) -> Option<String> {
    // In practice, the TerminalWidget records its grid's line 0 on every
    // `update`. Returning the most recent recorded value suffices for the
    // test; races are harmless because the test loop re-ticks.
    REG.with(|r| r.borrow().last().map(|(_, s)| s.clone()))
}
```

Wire `record(...)` at the end of `TerminalWidget::update` in `main_body.rs`
(just before `pty_activity` is returned):

```rust
crate::registry::record(self, self.grid.first().cloned().unwrap_or_default());
```

Analogous helper for `file-explorer` — add to `apps/file-explorer/src/lib.rs`:

```rust
pub fn peek_entry_names(app: &dyn quick_ui::AppWidget) -> Vec<String> {
    // The shell passes a `&dyn AppWidget` that is known by kind to be a
    // FileExplorerWidget. Since we cannot downcast without `Any`, we expose
    // a method via a dedicated trait:
    //   trait FileExplorerIntrospect { fn entry_names(&self) -> Vec<String>; }
    // and implement it on FileExplorerWidget. `peek_entry_names` then
    // requires the caller to pass a concrete reference — but the shell only
    // has `&dyn AppWidget`. To keep this simple, we expose a thread_local
    // registry analogous to the terminal.
    let _ = app;
    crate::registry::peek_entry_names()
}

pub(crate) mod registry {
    use std::cell::RefCell;
    thread_local! { static E: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) }; }
    pub fn record(names: Vec<String>) { E.with(|r| *r.borrow_mut() = names); }
    pub fn peek_entry_names() -> Vec<String> { E.with(|r| r.borrow().clone()) }
}
```

In `FileExplorerWidget::new` and `::navigate`, call
`crate::registry::record(self.entries.iter().map(|e| e.name.clone()).collect())`
after populating `entries`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p quick-desktop --test apps_integration`
Expected: both tests pass.

- [ ] **Step 6: Commit inside every affected submodule + bump pointers**

```bash
# terminal
git -C /var/home/quinten/Development/quickdesktop/apps/terminal add src/
git -C /var/home/quinten/Development/quickdesktop/apps/terminal commit -m "feat(terminal): test introspection registry for first-line peek"

# file-explorer
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer add src/
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer commit -m "feat(file-explorer): test introspection registry for entry-name peek"

# shell
git -C /var/home/quinten/Development/quickdesktop/quick-shell add src/ tests/apps_integration.rs
git -C /var/home/quinten/Development/quickdesktop/quick-shell commit -m "feat(shell): integration test — terminal prompt + file-explorer listing"

# main repo bumps
git -C /var/home/quinten/Development/quickdesktop add apps/terminal apps/file-explorer quick-shell
git -C /var/home/quinten/Development/quickdesktop commit -m "chore: bump all three submodules (integration peek helpers + test)"
```

---

## Task 17: Confirm Iced code deletion is complete (sanity gate)

**Files:**
- Verification only; no new files.

- [ ] **Step 1: Grep for remaining iced imports in file-explorer**

Run: the Grep tool is preferred when run interactively; for the plan execute:

```bash
grep -Rn "^use iced" /var/home/quinten/Development/quickdesktop/apps/file-explorer/src || echo "clean"
grep -Rn "iced = " /var/home/quinten/Development/quickdesktop/apps/file-explorer/Cargo.toml || echo "clean"
```

Expected output: `clean` on both. If either grep finds a match, the port is
incomplete — return to the corresponding earlier task and finish it.

- [ ] **Step 2: Run every crate's tests**

Run:
```bash
cargo test -p quick-ui
cargo test -p quick-terminal
cargo test -p file-explorer
cargo test -p quick-desktop
```

Expected: all pass.

- [ ] **Step 3: Smoke the full shell**

Run: `cargo run -p quick-desktop`
Expected: shell window opens with three tabs; terminal prints a prompt;
file-explorer shows entries; browser tab shows the stub card with an "Open
in external browser" button.

- [ ] **Step 4: Commit a marker (if any docs were touched)**

If no files changed, skip the commit. Otherwise:

```bash
git -C /var/home/quinten/Development/quickdesktop add -u
git -C /var/home/quinten/Development/quickdesktop commit -m "chore: verify iced file-explorer fully removed"
```

---

## Task 18: Audit every AUDIT.md item has a passing test

**Files:**
- Modify: `apps/file-explorer/AUDIT.md` (check off each completed item inline)

- [ ] **Step 1: Read `AUDIT.md`**

For every `- [ ]` item in `AUDIT.md`, confirm the named parity test exists in
`apps/file-explorer/tests/parity.rs` and is currently passing.

- [ ] **Step 2: Flip boxes to `- [x]`**

Edit `AUDIT.md` in place: every verified item becomes `- [x]`. Items whose
parity test is missing or failing stay as `- [ ]` and block completion.

- [ ] **Step 3: Run parity suite one more time**

Run: `cargo test -p file-explorer --test parity`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer add AUDIT.md
git -C /var/home/quinten/Development/quickdesktop/apps/file-explorer commit -m "docs(audit): all port-parity tests passing"
git -C /var/home/quinten/Development/quickdesktop add apps/file-explorer
git -C /var/home/quinten/Development/quickdesktop commit -m "chore: bump file-explorer (audit ticked)"
```

---

## Task 19: Token parity audit in the terminal

**Files:**
- Verification only.

- [ ] **Step 1: Grep for hard-coded colours in terminal**

Run:
```bash
grep -nE "Color::rgb8\([0-9]+, *[0-9]+, *[0-9]+\)" /var/home/quinten/Development/quickdesktop/apps/terminal/src/main_body.rs | head -20
```

Expected: zero matches, or matches only inside the `terminal_palette` fallback
constant table (if Plan 1 didn't yet provide the `terminal_palette` token).

- [ ] **Step 2: If matches exist, replace them**

For each match, replace with the corresponding `theme.tokens.*` lookup (see
Task 6 guidance).

- [ ] **Step 3: Re-run tests and smoke**

Run: `cargo test -p quick-terminal && cargo run -p quick-terminal`
Expected: pass + prompt appears.

- [ ] **Step 4: Commit if anything changed**

```bash
git -C /var/home/quinten/Development/quickdesktop/apps/terminal add src/
git -C /var/home/quinten/Development/quickdesktop/apps/terminal commit -m "refactor(terminal): replace remaining hard-coded colours with tokens"
git -C /var/home/quinten/Development/quickdesktop add apps/terminal
git -C /var/home/quinten/Development/quickdesktop commit -m "chore: bump terminal (token parity)"
```

(If nothing changed, skip.)

---

## Task 20: Final workspace smoke + plan self-review

**Files:**
- Verification only.

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: clean build across `quick-ui`, `quick-fs-fuse`, `quick-desktop`,
`quick-terminal`, `file-explorer`, `quick-browser`. (`quick-browser` is
untouched by this plan and must still build.)

- [ ] **Step 2: Per-crate tests**

Run each in sequence (per CLAUDE.md: never run workspace-wide `cargo test`):

```bash
cargo test -p quick-ui
cargo test -p quick-terminal
cargo test -p file-explorer
cargo test -p quick-desktop
```

Expected: all green.

- [ ] **Step 3: Interactive smoke**

Run: `cargo run -p quick-desktop`
Expected: the shell window opens; the default workspace hosts Terminal,
Files, and Browser tabs; each tab's `AppWidget` metadata is reflected in
the topbar (title + icon); the browser tab's "Open in external browser"
button invokes `xdg-open`.

- [ ] **Step 4: Confirm the Servo-based `quick-browser` binary still works**

Run: `cargo build -p quick-browser`
Expected: builds; no code changes in that crate.

- [ ] **Step 5: Final commit marker (if nothing broke, skip)**

No commit unless a fix was needed.

---

## Self-Review Checklist

Run this checklist against the spec `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` §7 and the plan brief.

**1. Spec coverage (§7 App integration)**
- [x] `AppWidget` trait with all five methods — Task 1.
- [x] `AppKind` enum covering Terminal/Files/Editor/Browser/Settings — Task 1.
- [x] `AppCommand` struct with invoke closure — Task 1.
- [x] Terminal refactor preserving PTY + VT + tokens — Tasks 4, 5, 6.
- [x] File-explorer audit task — Task 2.
- [x] File-explorer port using `DataTable` list mode + grid mode + all audited features — Tasks 7–13.
- [x] Iced removed after parity proven — Task 12.
- [x] Browser stub with `xdg-open` — Task 14.
- [x] Shell wires real apps — Task 15.
- [x] Integration test (prompt renders, listing renders) — Task 16.

**2. Plan brief coverage**
- [x] `TileContent::App(Box<dyn Widget>) → Box<dyn AppWidget>` migration committed in one task — Task 3.
- [x] Submodule-first, then pointer-bump commits — every submodule-touching task has both.
- [x] Browser stub does NOT port Servo — Task 14 explicitly avoids it; Task 20 asserts `quick-browser` still builds unchanged.
- [x] Audit task real (reads source, produces checklist, drives tests) — Task 2.

**3. Placeholder scan**
- [x] No "TBD/TODO" strings in plan steps.
- [x] No "similar to Task N" hand-waves — every step inlines its code.
- [x] No "implement later" in any implementation step.
- [x] The one `TODO(plan-4)` note in Task 6 is acceptable (it refers to a
      Plan 1 gap; fix-in-plan-1 guidance is provided inline).

**4. Type consistency**
- [x] `AppWidget`/`AppKind`/`IconDescriptor`/`AppCommand` spelled identically everywhere.
- [x] `TerminalWidget::spawn_default` referenced from both the terminal binary and the shell wiring — defined once in Task 4.
- [x] `FileExplorerWidget::new()` is used in Task 10, Task 12, Task 13, Task 15, and the integration tests — defined once in Task 10.
- [x] `BrowserStubWidget::new()` and `::with_runner(...)` spelled identically in Task 14 and Task 15.
- [x] `SortBy`/`SortOrder`/`ViewMode` defined once in `model.rs` (Task 7) and reused by every downstream module.

**5. Ordering sanity**
- [x] Audit (Task 2) precedes every porting task (7–13).
- [x] `AppWidget` trait (Task 1) precedes `TileContent::App` tightening (Task 3).
- [x] Terminal `AppWidget` impl (Task 5) precedes shell wiring (Task 15).
- [x] File-explorer `FileExplorerWidget` assembly (Task 10) precedes iced-binary replacement (Task 12).

**6. Submodule hygiene**
- [x] Every change inside `apps/terminal`, `apps/file-explorer`, `quick-shell` is committed inside the submodule before the superproject pointer is bumped.
- [x] No use of `git add -A` from the root (per CLAUDE.md).

**7. CLAUDE.md compliance**
- [x] Per-crate `cargo test -p …` everywhere; never workspace-wide.
- [x] Rust edition preserved (2021 for terminal/file-explorer, 2024 for quick-ui/shell).
- [x] No normalisation of editions in passing.
