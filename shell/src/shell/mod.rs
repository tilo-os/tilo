//! # Visual smoke checklist
//!
//! Run `cargo run -p quick-desktop` and verify:
//!
//! - `TopBar` renders with a live clock; wheel over the center area cycles to
//!   the CPU/RAM sparkline and the calendar (200 ms cooldown).
//! - Cmd+K opens `GlobalSearch`; typing filters; Enter invokes; Escape closes.
//! - Cmd+Tab toggles the `OverviewView`; clicking a card activates the workspace.
//! - The `RightTray` gear icon opens `QuickSettings`; sliders are interactive;
//!   WiFi/BT drill-in and back work.
//! - The workspace-name button opens `WorkspaceSettings`; switching to the Roles
//!   tab lets you toggle per-app permissions via `DataTable`; the Files tab
//!   shows per-role × per-file matrix.
//! - `SidebarWheel` scrolls strips; overscrolling past the threshold shows the
//!   "New session" hint and commits a new workspace; releasing before the
//!   threshold snaps back.
//! - Right-clicking a tile opens the context menu.
//! - Hovering the workspace-name button reveals a tooltip after ~600 ms.
//! - Creating a workspace (any path) triggers a toast.
//! - A simulated remote cursor floats over the active tile.

pub mod actions;
pub mod browser_stub;
pub mod catalogue;
pub mod chrome_state;
pub mod collab_broker;
pub mod collab_broker_capnp;
pub mod collab_core;
pub mod collab_outbound;
pub mod collab_presence;
pub mod color_pipeline;
pub mod crdt_text;
pub mod cursor_interp;
pub mod desktop_backdrop;
pub mod doc_open;
pub mod editor_app;
mod file_picker_overlay;
pub mod global_search;
pub mod hdr_display;
pub mod hdr_tonemap;
pub mod host_apps;
pub mod identity_provisioning;
pub mod launcher;
pub mod media_keys;
pub mod mpris;
pub mod network;
pub mod notes_app;
pub mod notification;
pub mod osd;
pub mod overlay_host;
pub mod overview;
pub mod peer_join_toast;
pub mod peer_presence;
pub mod peer_sync_overlay;
pub mod peer_view;
pub mod peer_windows;
pub mod persist;
pub mod placement;
mod power_menu;
pub mod projection;
mod projection_overlay;
pub mod qs_actions;
pub mod qs_refresh;
pub mod quick_settings;
mod quick_settings_vpn;
mod quick_settings_wifi;
pub mod remote_cursors;
pub mod room_activity;
pub mod room_records;
pub mod search;
pub mod session_audio_policy;
pub mod sidebar_wheel;
pub mod sidebar_wheel_chrome;
pub mod sidebar_wheel_disc;
pub mod split_row;
pub mod state;
pub mod status;
pub mod system_monitor;
pub mod tab_documents;
pub mod test_support;
pub mod text_entry;
pub mod tile;
pub mod tool_intents;
pub mod topbar;
pub mod topbar_chrome;
pub mod uri_open;
pub mod window_actions;
pub mod workspace_drop;
pub mod workspace_settings;

use state::{AppKind as ShellKind, TileContent as ShellTile, WorkspaceTree};
use ui::widget::{
    AppKind as UiKind, AppWidget, Column, Container, GroupRow, Stack, StripView, TabDescriptor,
    TileContent as UiTile, Widget, WindowGroup as UiGroup, WindowWidth as UiWidth, WorkspaceView,
    context_menu::{ContextMenu, MenuItem},
    toast::ToastStack,
};

use crate::shell::browser_stub::BrowserStubWidget;
use crate::shell::editor_app::EditorApp;
use crate::shell::global_search::GlobalSearch;
use crate::shell::notes_app::NotesApp;
use crate::shell::sidebar_wheel::SidebarWheel;
use crate::shell::tile::{
    Strip as RuntimeStrip, Tab as RuntimeTab, TileContent as RuntimeTile,
    WindowGroup as RuntimeGroup, Workspace as RuntimeWorkspace, WorkspaceTree as RuntimeTree,
};
use crate::shell::topbar::TopBar;
use crate::shell::workspace_settings::WorkspaceSettings;
use file_explorer::FileExplorerWidget;
use terminal::TerminalWidget;

/// Root nirios shell handle. Owns the runtime workspace tree — the live
/// counterpart to the persisted [`state::WorkspaceTree`], holding real
/// `Box<dyn AppWidget>`s in [`tile::TileContent::App`] instead of
/// `AppPlaceholder` symbols — and the single Automerge document shared
/// across every hosted app.
// Each overlay's visibility is toggled independently (they are not
// mutually exclusive — e.g. quick settings can open over the overview),
// so collapsing them into one enum/flags field would misrepresent the
// state space rather than simplify it; 41 call sites across the crate
// read/write these individually.
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent overlay-visibility flags, not mutually exclusive states"
)]
pub struct Shell {
    tree: RuntimeTree,
    state: ui::state::State,

    topbar: TopBar,
    /// Overscroll past threshold creates a new workspace.
    sidebar: SidebarWheel,
    /// Hidden unless `search_visible`.
    search: GlobalSearch,
    ws_settings: WorkspaceSettings,
    toasts: ToastStack,
    ctx_menu: ContextMenu,

    overview_visible: bool,
    search_visible: bool,
    quick_settings_visible: bool,
    ws_settings_visible: bool,

    /// Test-only pointer to the first hosted [`TerminalWidget`], captured
    /// before coercion into `Box<dyn AppWidget>`. Stays valid because the
    /// unsizing coercion doesn't move the heap allocation. `None` if PTY
    /// spawn failed and the tile fell back to a `Stub`.
    #[cfg(any(test, debug_assertions))]
    first_terminal_ptr: Option<*const TerminalWidget>,
    /// Same allocation-stability argument as `first_terminal_ptr`. `None`
    /// when the default workspace has no file-explorer tile.
    #[cfg(any(test, debug_assertions))]
    first_file_explorer_ptr: Option<*const FileExplorerWidget>,
}

impl Shell {
    /// Constructs a shell hosting the default demo workspace: Terminal
    /// (real PTY, falls back to a `Stub`), Files, Notes, Editor, and a
    /// Browser stub (Servo not ported).
    pub fn new_with_default_demo_workspace() -> Self {
        let DemoTabs {
            tabs,
            #[cfg(any(test, debug_assertions))]
            first_terminal_ptr,
            #[cfg(any(test, debug_assertions))]
            first_file_explorer_ptr,
        } = default_demo_tabs();
        let group = RuntimeGroup::with_tabs(tabs, state::WindowWidth::Normal);
        let strip = RuntimeStrip::new("Main", vec![group]);
        let workspace = RuntimeWorkspace::new("Home", vec![strip]);
        let tree = RuntimeTree::new(vec![workspace]);
        // WorkspaceSettings must target a workspace id; pre-seed with the
        // active one. `open_ws_settings` rebinds it on later switches.
        let active_ws_id = tree.active;
        let mut shell = Self {
            tree,
            state: ui::state::State::new(),
            topbar: TopBar::new(),
            sidebar: SidebarWheel::new(),
            search: GlobalSearch::new(),
            ws_settings: WorkspaceSettings::new(active_ws_id),
            toasts: ToastStack::new(),
            ctx_menu: ContextMenu::new(),
            overview_visible: false,
            search_visible: false,
            quick_settings_visible: false,
            ws_settings_visible: false,
            #[cfg(any(test, debug_assertions))]
            first_terminal_ptr,
            #[cfg(any(test, debug_assertions))]
            first_file_explorer_ptr,
        };
        shell.search.seed_default_commands();
        // Prime every app against the shared doc so `update` has CRDT
        // sub-trees to work with from the first call.
        shell.sync_all_apps();
        shell
    }

    #[cfg(any(test, debug_assertions))]
    pub fn new_for_test() -> Self {
        Self::new_with_default_demo_workspace()
    }

    /// Iterate as `tree.workspaces[..].strips[..].window_groups[..].tabs[..].tile`.
    pub fn workspace_tree(&self) -> &RuntimeTree {
        &self.tree
    }

    /// Shared by the constructor and [`Self::tick_for_test`].
    fn sync_all_apps(&mut self) {
        for ws in &mut self.tree.workspaces {
            for strip in &mut ws.strips {
                for group in &mut strip.window_groups {
                    for tab in &mut group.tabs {
                        if let RuntimeTile::App(app) = &mut tab.tile {
                            app.sync(&mut self.state.doc, &self.state.root);
                        }
                    }
                }
            }
        }
    }

    /// One `sync` + `update` pass over every hosted app against the shared
    /// doc, without painting — advances the terminal's PTY reader and the
    /// CRDT without a GPU-backed render loop.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_for_test(&mut self) {
        for ws in &mut self.tree.workspaces {
            for strip in &mut ws.strips {
                for group in &mut strip.window_groups {
                    for tab in &mut group.tabs {
                        if let RuntimeTile::App(app) = &mut tab.tile {
                            app.sync(&mut self.state.doc, &self.state.root);
                            let _ = app.update(&mut self.state.doc, &self.state.root);
                        }
                    }
                }
            }
        }
    }

    /// First row of the first hosted terminal's grid, or `None` if no live
    /// terminal tile exists. `TerminalWidget::grid` is private to
    /// `quick-terminal`, but the PTY output is also mirrored into the
    /// shared doc under `terminal_grid` (see `TerminalPerformer`), so this
    /// reads it back from the CRDT instead.
    #[cfg(any(test, debug_assertions))]
    pub fn first_terminal_line(&self) -> Option<String> {
        use automerge::{ObjType, ReadDoc, Value};

        self.first_terminal_ptr?;

        let doc = &self.state.doc;
        let (grid_val, grid_id) = doc.get(&self.state.root, "terminal_grid").ok().flatten()?;
        if !matches!(grid_val, Value::Object(ObjType::List)) {
            return None;
        }
        if doc.length(&grid_id) == 0 {
            return Some(String::new());
        }
        let (line_val, line_id) = doc.get(&grid_id, 0).ok().flatten()?;
        if !matches!(line_val, Value::Object(ObjType::Map)) {
            return None;
        }
        let (text_val, text_id) = doc.get(&line_id, "text").ok().flatten()?;
        if !matches!(text_val, Value::Object(ObjType::Text)) {
            return None;
        }
        doc.text(&text_id).ok()
    }

    /// Entry names from the first hosted file-explorer, or empty if there
    /// is none. Unlike the terminal, the file-explorer keeps its listing in
    /// local widget state rather than the CRDT, so this reaches through the
    /// raw pointer captured at construction (same validity argument as
    /// `first_terminal_ptr`).
    #[cfg(any(test, debug_assertions))]
    pub fn first_file_explorer_entries(&self) -> Vec<String> {
        let Some(ptr) = self.first_file_explorer_ptr else {
            return Vec::new();
        };
        // SAFETY: see the doc-comment above; pointer is live for `&self`.
        let fe: &FileExplorerWidget = unsafe { &*ptr };
        fe.entries().iter().map(|e| e.name.clone()).collect()
    }

    // Trivial visibility getters so tests can assert every chrome piece
    // was constructed without exposing the fields themselves.

    pub fn topbar(&self) -> &TopBar {
        &self.topbar
    }

    pub fn sidebar_wheel(&self) -> &SidebarWheel {
        &self.sidebar
    }

    pub fn overview_visible(&self) -> bool {
        self.overview_visible
    }

    pub fn search_visible(&self) -> bool {
        self.search_visible
    }

    pub fn quick_settings_visible(&self) -> bool {
        self.quick_settings_visible
    }

    pub fn ws_settings_visible(&self) -> bool {
        self.ws_settings_visible
    }

    /// Open the global search palette and re-seed its command list so the
    /// first keystroke lands against a known state. Idempotent — calling
    /// it on an already-visible palette just refreshes the seed.
    pub fn open_search(&mut self) {
        self.search_visible = true;
    }

    pub fn close_search(&mut self) {
        self.search_visible = false;
    }

    pub fn toggle_overview(&mut self) {
        self.overview_visible = !self.overview_visible;
    }

    pub fn open_quick_settings(&mut self) {
        self.quick_settings_visible = true;
    }

    pub fn close_quick_settings(&mut self) {
        self.quick_settings_visible = false;
    }

    /// A second tray-gear click closes the popover rather than re-opening it.
    pub fn toggle_quick_settings(&mut self) {
        self.quick_settings_visible = !self.quick_settings_visible;
    }

    /// Rebinds the modal's `ws_id` so a newly-created workspace becomes the
    /// target after a switch.
    pub fn open_ws_settings(&mut self) {
        self.ws_settings = WorkspaceSettings::new(self.tree.active);
        self.ws_settings_visible = true;
    }

    pub fn close_ws_settings(&mut self) {
        self.ws_settings_visible = false;
    }

    /// Single funnel for topbar output. Tray-gear is a toggle; the
    /// workspace-name click is a plain open (the modal has its own close).
    pub fn handle_topbar_output(&mut self, out: topbar::TopBarOutput) {
        match out {
            topbar::TopBarOutput::OpenWorkspaceSettings => self.open_ws_settings(),
            topbar::TopBarOutput::OpenQuickSettings => self.toggle_quick_settings(),
        }
    }

    /// Canonical entry point for shell-level global hotkeys — both the
    /// production path ([`Shell::handle_window_event`]) and tests
    /// ([`shell::test_support::key_press`]) funnel through here. Returns
    /// `true` if the press matched a global binding, `false` if it should
    /// fall through to the focused widget tree.
    // `press` is only ever read here, but narrowing to `&KeyPress` would
    // touch `shell/tests/shell_chrome_test.rs`'s call sites, outside this
    // pass's scope (only shell/src/shell/ is ours to edit).
    #[allow(
        clippy::needless_pass_by_value,
        reason = "press is only ever borrowed here, but narrowing the signature would touch \
                  shell/tests call sites outside this pass's scope"
    )]
    pub fn dispatch_key(&mut self, press: test_support::KeyPress) -> bool {
        use test_support::Mods;
        // Normalize to lowercase so `Cmd+K` and `Cmd+k` hit the same branch.
        let logical = press.logical.as_str();
        let shortcut = settings_store::hardware::ShortcutConfig::load();
        let pressed = |action: settings_store::hardware::ShortcutAction| {
            shortcut.bindings.get(&action).is_some_and(|chord| {
                let needs_cmd =
                    chord.modifiers.contains("Super") || chord.modifiers.contains("Ctrl");
                needs_cmd == press.mods.contains(Mods::CMD)
                    && chord.modifiers.contains("Shift") == press.mods.contains(Mods::SHIFT)
                    && chord.modifiers.contains("Alt") == press.mods.contains(Mods::ALT)
                    && chord.key.eq_ignore_ascii_case(logical)
            })
        };
        if pressed(settings_store::hardware::ShortcutAction::Search) {
            self.open_search();
            return true;
        }
        if pressed(settings_store::hardware::ShortcutAction::WorkspaceOverview) {
            self.toggle_overview();
            return true;
        }
        if pressed(settings_store::hardware::ShortcutAction::QuickSettings) {
            self.toggle_quick_settings();
            return true;
        }

        // Priority matches z-order: search / overview sit above
        // quick-settings, which sits above the ws_settings modal.
        if logical.eq_ignore_ascii_case("Escape") && press.mods == Mods::NONE {
            if self.search_visible {
                self.close_search();
                return true;
            }
            if self.overview_visible {
                self.overview_visible = false;
                return true;
            }
            if self.quick_settings_visible {
                self.close_quick_settings();
                return true;
            }
            if self.ws_settings_visible {
                self.close_ws_settings();
                return true;
            }
            return false;
        }

        false
    }

    /// Projects a winit `WindowEvent::KeyboardInput` onto
    /// [`test_support::KeyPress`] and routes it through
    /// [`Self::dispatch_key`]. Tests go through `dispatch_key` directly so
    /// they don't have to reach for winit's private `KeyEventExtra` field.
    pub fn handle_window_event(
        &mut self,
        event: &winit::event::WindowEvent,
        modifiers: &winit::event::Modifiers,
    ) -> bool {
        use test_support::{KeyPress, Mods};
        use winit::event::{ElementState, KeyEvent, WindowEvent};
        use winit::keyboard::{Key, NamedKey};
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let mut mods = Mods::NONE;
        let ms = modifiers.state();
        // Super and Ctrl are treated as equivalent — Linux hardware rarely
        // has a dedicated Super key.
        if ms.super_key() || ms.control_key() {
            mods = mods | Mods::CMD;
        }
        if ms.shift_key() {
            mods = mods | Mods::SHIFT;
        }
        if ms.alt_key() {
            mods = mods | Mods::ALT;
        }
        let logical = match logical_key {
            Key::Named(NamedKey::Escape) => "Escape".to_string(),
            Key::Named(NamedKey::Tab) => "Tab".to_string(),
            Key::Named(NamedKey::Enter) => "Enter".to_string(),
            Key::Named(NamedKey::ArrowUp) => "ArrowUp".to_string(),
            Key::Named(NamedKey::ArrowDown) => "ArrowDown".to_string(),
            Key::Named(NamedKey::ArrowLeft) => "ArrowLeft".to_string(),
            Key::Named(NamedKey::ArrowRight) => "ArrowRight".to_string(),
            Key::Named(NamedKey::Backspace) => "Backspace".to_string(),
            Key::Named(NamedKey::Space) => "Space".to_string(),
            Key::Character(c) => c.as_str().to_string(),
            _ => return false,
        };
        self.dispatch_key(KeyPress { logical, mods })
    }

    /// Number of toasts currently in the stack. Lets a test assert "a toast
    /// was fired" without caring about timing.
    #[cfg(any(test, debug_assertions))]
    pub fn toast_count(&self) -> usize {
        self.toasts.len()
    }

    /// [`ToastKind`] of every active toast in push order.
    #[cfg(any(test, debug_assertions))]
    pub fn toast_kinds(&self) -> Vec<ui::widget::toast::ToastKind> {
        self.toasts.iter().map(|t| t.kind).collect()
    }

    /// Routes a +/- wheel tick into the sidebar and returns the resulting
    /// active-strip index.
    #[cfg(any(test, debug_assertions))]
    pub fn simulate_sidebar_wheel(&mut self, dir: i32) -> usize {
        // `SidebarWheel::on_wheel` reads/writes `active_strip` on
        // `state::WorkspaceTree`, not the runtime tree, so dispatch against
        // a scratch state tree and copy the result back out.
        let active_ws = self.tree.active;
        let mut scratch = state::WorkspaceTree {
            workspaces: self
                .tree
                .workspaces
                .iter()
                .map(|w| {
                    let mut s = state::Workspace::new(w.id, &w.name);
                    s.strips = w
                        .strips
                        .iter()
                        .map(|st| state::Strip::new(st.id, &st.name))
                        .collect();
                    s
                })
                .collect(),
            active: active_ws,
            active_strip: std::collections::HashMap::default(),
            active_group: std::collections::HashMap::default(),
            collaborators: vec![],
            ..Default::default()
        };
        // Seed the active-strip entry for the active workspace.
        if let Some(ws) = self.tree.workspaces.iter().find(|w| w.id == active_ws)
            && let Some(first) = ws.strips.first()
        {
            scratch.active_strip.insert(active_ws, first.id);
        }
        self.sidebar.on_wheel(dir, &mut scratch);
        // Snap back the active strip id (if changed) by index lookup.

        scratch
            .active_strip
            .get(&active_ws)
            .and_then(|id| {
                scratch
                    .workspaces
                    .iter()
                    .find(|w| w.id == active_ws)?
                    .strips
                    .iter()
                    .position(|s| s.id == *id)
            })
            .unwrap_or(0)
    }

    /// Index of the currently-active strip on the active workspace in the
    /// runtime tree. `0` when the workspace has no strips (shouldn't happen
    /// for a sanely-constructed shell, but the caller shouldn't panic in
    /// that case).
    #[cfg(any(test, debug_assertions))]
    pub fn active_strip_index(&self) -> usize {
        // No per-workspace active-strip map on the runtime tree yet (that
        // lives on `state::WorkspaceTree`) — `0` is a stable placeholder;
        // callers wanting the live selection use the `state` tree path.
        let _ = self;
        0
    }

    /// Drives the sidebar's overscroll gesture past the commit threshold;
    /// asserts the toast side of the new-workspace routing (the append
    /// itself lands on `state::WorkspaceTree`, not the runtime tree).
    #[cfg(any(test, debug_assertions))]
    pub fn simulate_overscroll_commit(&mut self) {
        use ui::widget::toast::ToastKind;
        // Scratch state tree, same reason as `simulate_sidebar_wheel`.
        let mut scratch = state::WorkspaceTree {
            workspaces: vec![state::Workspace::new(state::WorkspaceId::new(), "scratch")],
            active: state::WorkspaceId::from(0),
            active_strip: std::collections::HashMap::default(),
            active_group: std::collections::HashMap::default(),
            collaborators: vec![],
            ..Default::default()
        };
        if let Some(w) = scratch.workspaces.first() {
            scratch.active = w.id;
        }
        self.sidebar.begin_overscroll(1);
        for _ in 0..30 {
            // 300px of pull, comfortably past the 240px threshold.
            self.sidebar.drag_overscroll(1, 10.0);
        }
        self.sidebar.release_overscroll(&mut scratch);
        self.toasts.push("Workspace created", ToastKind::Success);
    }

    /// Same route as a real tray-gear click: `TopBarOutput::OpenQuickSettings`.
    #[cfg(any(test, debug_assertions))]
    pub fn simulate_tray_gear_click(&mut self) {
        let out = self.topbar.click_tray_settings();
        self.handle_topbar_output(out);
    }

    /// The button-click counterpart to [`Self::simulate_overscroll_commit`].
    #[cfg(any(test, debug_assertions))]
    pub fn simulate_new_workspace_via_management(&mut self) {
        use ui::widget::toast::ToastKind;
        // Scratch state tree, same reason as `simulate_sidebar_wheel`.
        let mut scratch = state::WorkspaceTree {
            workspaces: vec![state::Workspace::new(state::WorkspaceId::new(), "scratch")],
            active: state::WorkspaceId::from(0),
            active_strip: std::collections::HashMap::default(),
            active_group: std::collections::HashMap::default(),
            collaborators: vec![],
            ..Default::default()
        };
        if let Some(w) = scratch.workspaces.first() {
            scratch.active = w.id;
        }
        let _ = self.sidebar.click_management_new_workspace(&mut scratch);
        self.toasts.push("Workspace created", ToastKind::Success);
    }

    /// Renames via [`WorkspaceSettings::rename`], matching the production
    /// path where the General tab's name field commits a new value.
    #[cfg(any(test, debug_assertions))]
    pub fn simulate_rename_active_workspace(&mut self, new_name: &str) {
        use ui::widget::toast::ToastKind;
        // Scratch persisted workspace, same reason as the other simulators.
        let mut scratch = state::Workspace::new(
            self.tree.active,
            self.tree
                .workspaces
                .iter()
                .find(|w| w.id == self.tree.active)
                .map(|w| w.name.clone())
                .unwrap_or_default(),
        );
        self.ws_settings.rename(&mut scratch, new_name);
        // Mirror into the runtime tree so `active_workspace_name()`
        // reflects the edit.
        if let Some(ws) = self
            .tree
            .workspaces
            .iter_mut()
            .find(|w| w.id == self.tree.active)
        {
            ws.name = new_name.to_string();
        }
        self.toasts.push("Workspace renamed", ToastKind::Info);
    }

    // Default tile context menu: "Close Tab" / separator / "Split Right" /
    // "Move to New Group". Callbacks are currently no-ops.

    pub fn context_menu_is_open(&self) -> bool {
        self.ctx_menu.is_open()
    }

    /// `None` if the menu has never been opened.
    pub fn context_menu_position(&self) -> Option<[f32; 2]> {
        self.ctx_menu.position()
    }

    /// Action-item labels in order, skipping separators.
    pub fn context_menu_labels(&self) -> Vec<String> {
        self.ctx_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(std::string::ToString::to_string))
            .collect()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn simulate_right_click_on_active_tile(&mut self, position: [f32; 2]) {
        self.open_default_tile_context_menu(position);
    }

    /// Entry point for `WindowEvent::DroppedFile`: classifies the item,
    /// picks an app (Editor for code files, Files otherwise), and splices a
    /// new `Stub` tab into the active workspace's first strip/group.
    /// Returns the new tab id, or `None` if the path has no filename.
    pub fn handle_file_drop(&mut self, path: &std::path::Path) -> Option<u64> {
        workspace_drop::apply_runtime_workspace_drop_from_path(&mut self.tree, path)
    }

    #[cfg(any(test, debug_assertions))]
    pub fn simulate_file_drop(&mut self, path: &std::path::Path) -> Option<u64> {
        self.handle_file_drop(path)
    }

    /// Populate `ctx_menu` with the default tile action list and open it
    /// at `position`. Extracted so the production right-click path (wired
    /// alongside the compositor's pointer-event layer in a later task) and
    /// the test-only simulator share the same item-construction code.
    fn open_default_tile_context_menu(&mut self, position: [f32; 2]) {
        // Rebuild from scratch so repeated right-clicks don't accumulate
        // items from earlier menus (e.g. if a follow-up task adds a
        // context-specific path that seeds different entries).
        self.ctx_menu = ContextMenu::new();
        self.ctx_menu.push(MenuItem::action("Close Tab", || {}));
        self.ctx_menu.push(MenuItem::separator());
        self.ctx_menu.push(MenuItem::action("Split Right", || {}));
        self.ctx_menu
            .push(MenuItem::action("Move to New Group", || {}));
        self.ctx_menu.open_at(position);
    }
}

/// Builder output for the default demo workspace. Carries the tab list plus
/// test-only raw pointers to the `TerminalWidget` / `FileExplorerWidget`
/// allocations inside two of those tabs, so `Shell` can expose introspection
/// helpers without downcasting through `Box<dyn AppWidget>` (not `Any`).
struct DemoTabs {
    tabs: Vec<RuntimeTab>,
    #[cfg(any(test, debug_assertions))]
    first_terminal_ptr: Option<*const TerminalWidget>,
    #[cfg(any(test, debug_assertions))]
    first_file_explorer_ptr: Option<*const FileExplorerWidget>,
}

fn default_demo_tabs() -> DemoTabs {
    let mut tabs: Vec<RuntimeTab> = Vec::new();
    #[cfg(any(test, debug_assertions))]
    let mut first_terminal_ptr: Option<*const TerminalWidget> = None;
    // Declared up here (written unconditionally below) so the final struct
    // literal can reference it without a local scope leak.
    #[cfg(any(test, debug_assertions))]
    let first_file_explorer_ptr: Option<*const FileExplorerWidget>;

    match TerminalWidget::spawn_default() {
        Ok(t) => {
            let boxed: Box<TerminalWidget> = Box::new(t);
            #[cfg(any(test, debug_assertions))]
            {
                // Capture the pointer *before* coercing into
                // `Box<dyn AppWidget>` — the unsizing coercion preserves the
                // address, so this stays valid for the `Box`'s lifetime.
                first_terminal_ptr = Some(&raw const *boxed);
            }
            let app: Box<dyn AppWidget> = boxed;
            tabs.push(RuntimeTab::new("Terminal", RuntimeTile::App(app)));
        }
        Err(e) => {
            tracing::warn!("terminal spawn failed: {e}; falling back to stub");
            tabs.push(RuntimeTab::new(
                "Terminal",
                RuntimeTile::Stub {
                    kind: UiKind::Terminal,
                    label: format!("Terminal: {e}"),
                },
            ));
        }
    }

    // File-explorer: `new` is infallible; OS errors surface via its own
    // in-pane status bar rather than the Stub fallback.
    let fe_boxed: Box<FileExplorerWidget> = Box::new(FileExplorerWidget::new());
    #[cfg(any(test, debug_assertions))]
    {
        first_file_explorer_ptr = Some(&raw const *fe_boxed);
    }
    let fe_app: Box<dyn AppWidget> = fe_boxed;
    tabs.push(RuntimeTab::new("Files", RuntimeTile::App(fe_app)));

    // Seeds a welcome paragraph on first boot; later boots load the shared doc.
    let notes = NotesApp::with_seed(
        "Welcome to nirios.\n\n\
         These notes sync live with anyone invited to this workspace — edits \
         replicate through the same CRDT path every widget uses.\n\n\
         Ctrl+Z / Ctrl+Y undo and redo across the whole shell.",
    );
    tabs.push(RuntimeTab::new("Notes", RuntimeTile::App(Box::new(notes))));

    // Uses its own CRDT key so it doesn't stomp on the Notes body.
    let editor = EditorApp::with_default_snippet();
    tabs.push(RuntimeTab::new(
        "Editor",
        RuntimeTile::App(Box::new(editor)),
    ));

    tabs.push(RuntimeTab::new(
        "Browser",
        RuntimeTile::App(Box::new(BrowserStubWidget::new())),
    ));

    DemoTabs {
        tabs,
        #[cfg(any(test, debug_assertions))]
        first_terminal_ptr,
        #[cfg(any(test, debug_assertions))]
        first_file_explorer_ptr,
    }
}

fn to_ui_width(w: state::WindowWidth) -> UiWidth {
    w.to_ui()
}

fn to_ui_kind(k: &ShellKind) -> UiKind {
    // `ui::AppKind` has only five variants; the shell's own `AppKind` still
    // carries `FileExplorer` and `Other(String)` for v1 compatibility.
    match k {
        ShellKind::Terminal => UiKind::Terminal,
        ShellKind::FileExplorer => UiKind::Files,
        ShellKind::Browser => UiKind::Browser,
        // Apps hosted under `Other` are all text editors; without this
        // they wore the unknown-kind settings cog instead of the editor glyph.
        ShellKind::Other(s) if is_editor_label(s) => UiKind::Editor,
        ShellKind::Settings | ShellKind::Other(_) => UiKind::Settings,
    }
}

/// Apps the shell hosts in-process under an `AppKind::Other` label, as
/// `(tab title, label)`. Two columns because the persisted tree names an
/// app by label, while a group rebuilt from widgets (a tab dragged into its
/// own window) has only the tab's title to recover it from.
const HOSTED_OTHER_APPS: &[(&str, &str)] = &[
    ("Notes", "Notes"),
    ("Editor", "Editor"),
    ("Document", "Word"),
];

fn is_editor_label(s: &str) -> bool {
    HOSTED_OTHER_APPS
        .iter()
        .any(|(_, label)| label.eq_ignore_ascii_case(s))
}

/// Falls through to `title` itself for anything the shell does not host.
pub(crate) fn hosted_label_for_title(title: &str) -> String {
    HOSTED_OTHER_APPS
        .iter()
        .find(|(t, _)| t.eq_ignore_ascii_case(title))
        .map_or_else(|| title.to_string(), |(_, label)| (*label).to_string())
}

/// Probe into [`to_ui_tile`] for tests that don't want the full `UiTile`
/// type (and its boxed `AppWidget`) on the public surface.
#[cfg(any(test, debug_assertions))]
pub fn resolve_tile_shape_for_test(t: &ShellTile) -> &'static str {
    match to_ui_tile(t) {
        UiTile::App(_) => "App",
        UiTile::Stub { .. } => "Stub",
        UiTile::Notice { .. } => "Notice",
    }
}

/// Shared by the initial tree→widget build and by
/// [`overlay_host::ShellOverlayHost`]'s per-revision resync.
pub(crate) fn tile_app_kind(t: &ShellTile) -> UiKind {
    match t {
        ShellTile::Stub { kind, .. } | ShellTile::AppPlaceholder { kind, .. } => to_ui_kind(kind),
        // No `AppKind` describes "arbitrary external client", so both a
        // foreign surface and a peer window wear the catch-all `Settings`
        // glyph — and keep wearing it when a `PeerWindow` binds in and
        // becomes `Foreign`, so the pill doesn't change under the user.
        ShellTile::Foreign { .. } | ShellTile::PeerWindow { .. } => UiKind::Settings,
    }
}

pub(crate) fn to_ui_tile(t: &ShellTile) -> UiTile {
    match t {
        ShellTile::Stub { kind, label } => UiTile::Stub {
            kind: to_ui_kind(kind),
            label: label.clone(),
        },
        // Pulls the client's most recent buffer out of
        // `compositor::foreign`'s frame store and also publishes the tab's
        // rect back so the compositor can configure the client to size.
        ShellTile::Foreign { app_id, surface_id } => UiTile::App(Box::new(
            crate::compositor::foreign::ForeignSurfaceWidget::new(*surface_id, app_id.clone()),
        )),
        // A window someone else has open that this machine hasn't got its
        // own instance of yet; becomes a `Foreign` above once it arrives.
        ShellTile::PeerWindow { label, note, .. } => UiTile::Notice {
            title: label.clone(),
            detail: note.clone(),
        },
        ShellTile::AppPlaceholder { kind, label } => match kind {
            ShellKind::Terminal => match TerminalWidget::spawn_default() {
                Ok(w) => UiTile::App(Box::new(w)),
                Err(e) => {
                    tracing::warn!("terminal spawn failed: {e}; falling back to stub");
                    UiTile::Stub {
                        kind: to_ui_kind(kind),
                        label: format!("Terminal: {e}"),
                    }
                }
            },
            // File-explorer — `new` is infallible; the widget surfaces any
            // directory-read errors via its own in-pane status bar.
            ShellKind::FileExplorer => UiTile::App(Box::new(FileExplorerWidget::new())),
            // Each opens on an empty document — prose the shell wrote would
            // be indistinguishable from prose the user wrote.
            ShellKind::Other(s) if s.eq_ignore_ascii_case("Notes") => {
                UiTile::App(Box::new(NotesApp::new()))
            }
            ShellKind::Other(s) if s.eq_ignore_ascii_case("Editor") => {
                UiTile::App(Box::new(EditorApp::new()))
            }
            ShellKind::Other(s) if s.eq_ignore_ascii_case("Word") => {
                UiTile::App(Box::new(word_processor::WordProcessorWidget::new()))
            }
            _ => UiTile::Stub {
                kind: to_ui_kind(kind),
                label: label.clone(),
            },
        },
    }
}

/// v1 renders the active workspace's active strip as a `StripView` overlay
/// above the active `WorkspaceView`, with placeholder groups laid out
/// horizontally.
pub fn build_nirios_root(tree: &WorkspaceTree) -> Box<dyn Widget> {
    let (root, _chrome) = build_nirios_root_with_chrome(tree);
    root
}

/// Lets the caller flip the peer-sync palette commands on (`/invite`,
/// `/connect`), mirroring `--enable-peer-sync`. [`build_nirios_root`] keeps
/// the zero-arg shape so call-sites that don't care don't have to thread it.
pub fn build_nirios_root_with_options(
    tree: &WorkspaceTree,
    enable_peer_sync: bool,
) -> Box<dyn Widget> {
    let (host, _chrome) = build_nirios_overlay_host_with_options(tree, enable_peer_sync);
    Box::new(host) as Box<dyn Widget>
}

/// Also returns the shared chrome handle, so integration tests can observe
/// overlay toggles without drilling through the widget tree.
pub fn build_nirios_root_with_chrome(
    tree: &WorkspaceTree,
) -> (Box<dyn Widget>, chrome_state::Chrome) {
    let (host, chrome) = build_nirios_overlay_host_with_chrome(tree);
    (Box::new(host) as Box<dyn Widget>, chrome)
}

/// Returns the concrete [`overlay_host::ShellOverlayHost`] instead of a
/// boxed trait object, so tests can assert on host-local state like
/// `palette_visible()`. Production wiring goes through `build_nirios_root`.
pub fn build_nirios_overlay_host_with_chrome(
    tree: &WorkspaceTree,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    build_nirios_overlay_host_with_options(tree, false)
}

pub fn build_nirios_overlay_host_with_options(
    tree: &WorkspaceTree,
    enable_peer_sync: bool,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    build_nirios_overlay_host_with_handle(tree.clone().shared(), enable_peer_sync)
}

/// Extracted so [`ShellOverlayHost::resync_strips_from_tree`] can reach for
/// the same conversion when splicing freshly-added groups into a live
/// [`GroupRow`].
pub fn to_ui_window_group(g: &state::WindowGroup, records: &ui::record::RecordStore) -> UiGroup {
    let tabs_with_content: Vec<(TabDescriptor, UiTile)> = g
        .tabs
        .iter()
        .map(|t| {
            let kind = tile_app_kind(&t.tile);
            (
                TabDescriptor {
                    id: t.id,
                    title: t.title.clone(),
                    app_kind: Some(kind),
                },
                to_ui_tile(&t.tile),
            )
        })
        .collect();
    let mut group = UiGroup::new(g.id, tabs_with_content, g.active_tab, to_ui_width(g.width))
        .with_maximized(g.maximized);
    // Every tab gets its own document before it is ever synced, so no
    // hosted app writes a keystroke into the shell's own state.
    tab_documents::bind_group_records(&mut group, records);
    group
}

/// Wires the hosted file explorer's document activation to
/// [`doc_open::open_in_workspace`] — the explorer asks, the compositor
/// answers. Named so a test can install it against its own chrome too.
pub fn install_document_opener(chrome: &chrome_state::Chrome) {
    let chrome = chrome.clone();
    file_explorer::files_app::set_document_opener(move |path| {
        doc_open::open_in_workspace(path, &chrome)
            .map(|_| ())
            .map_err(|e| e.user_message())
    });
}

/// The production entry point: threads the *shared* [`WorkspaceTreeHandle`]
/// (the same `Arc<RwLock<_>>` the portal's `GrantStoreHandle` and
/// peer-sync's `WorkspaceTreeReplica` hold) so chrome mutations land on the
/// tree other subsystems read from. Reads it once for the initial widget
/// tree; later rebuilds run `sync_from_tree` on each chrome widget.
pub fn build_nirios_overlay_host_with_handle(
    tree_handle: state::WorkspaceTreeHandle,
    enable_peer_sync: bool,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    // In-memory records: a host built without being told where to keep them
    // is a test, a probe or a `--shots` run, none of which may leave a
    // user's tabs on disk. A session passes its own store through
    // [`build_nirios_overlay_host_with_records`].
    build_nirios_overlay_host_with_records(
        tree_handle,
        enable_peer_sync,
        ui::record::RecordStore::in_memory(),
    )
}

/// A scratch tab belongs to the compositor, not to any installed app, so
/// its records live under this package in the compositor's own state dir.
pub const SHELL_RECORDS_PACKAGE: &str = "org.nirios.Shell";

pub fn session_record_store() -> ui::record::RecordStore {
    ui::record::RecordStore::open(SHELL_RECORDS_PACKAGE)
}

/// Backdrop, topbar, and one [`GroupRow`] per strip of the tree's active
/// workspace, plus the per-strip pointer map the host's reconcilers harvest
/// through. Extracted so the host can rebuild the content when the active
/// workspace changes — the strip widgets are a projection of ONE workspace.
pub fn build_workspace_root(
    tree: &WorkspaceTree,
    records: &ui::record::RecordStore,
    chrome: &chrome_state::Chrome,
) -> (
    Box<dyn Widget>,
    std::collections::HashMap<state::StripId, *mut GroupRow>,
) {
    let strips_for_view: Vec<String> = tree
        .active_workspace()
        .map(|w| w.strips.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default();
    let strip_view: Box<dyn Widget> = Box::new(StripView::new(strips_for_view, 0));
    let workspace_view: Box<dyn Widget> = Box::new(WorkspaceView::new(
        tree.workspaces.len(),
        tree.workspaces
            .iter()
            .position(|w| w.id == tree.active)
            .unwrap_or(0),
    ));

    let topbar: Box<dyn Widget> =
        Box::new(topbar_chrome::TopBarChrome::from_tree(tree).with_chrome(chrome.clone()));

    let mut content_col = Column::new()
        // Fills the viewport height remaining AFTER the topbar's 40px
        // sibling in `foreground_col`; without it the column's
        // `percent(1.0)` height overflowed the viewport by 40px and apps
        // painted right up to the absolute bottom.
        .with_fill_remaining_height()
        // 8px is the minimum keeping the active-tile bottom from clipping
        // the windowed DE's edge — the tile-host's bloom shadow paints up
        // to 120px past `self.rect.y1` (`TileHost::draw_steady_state`),
        // and at 4px it visibly bled past the window edge.
        .with_vertical_padding(8.0)
        // No horizontal padding: `GroupRow::leading_inset` reserves
        // canvas-x [0, SIDEBAR_WIDTH] for the wheel disc, and wheel/strip
        // pan as one unit — a gutter here would split that canvas in two.
        .with_horizontal_padding(0.0, 0.0)
        .push(workspace_view)
        .push(strip_view);

    // Per-strip raw pointers to the live `GroupRow` widgets, captured so
    // the host's per-frame `reconcile_active_group_off_closing` hook can
    // harvest `WindowGroup::is_closing()` ids without re-walking a
    // `&dyn Widget` tree. SAFETY contract on `ShellOverlayHost::group_row_ptrs`.
    let mut group_row_ptrs: std::collections::HashMap<state::StripId, *mut GroupRow> =
        std::collections::HashMap::new();

    if let Some(ws) = tree.active_workspace() {
        for strip in &ws.strips {
            // GroupRow routes cross-group tab drag-drops; each WindowGroup
            // still handles its own close/reorder/maximize.
            let groups: Vec<UiGroup> = strip
                .window_groups
                .iter()
                .map(|g| to_ui_window_group(g, records))
                .collect();
            let mut strip_row: Box<GroupRow> = Box::new(
                GroupRow::new(groups)
                    // Reserves canvas-x [0, SIDEBAR_WIDTH] for the wheel
                    // disc (which lives in the overlay layer) so wheel and
                    // strip share the same canvas and pan together.
                    .with_leading_inset(f64::from(sidebar_wheel_chrome::SIDEBAR_WIDTH_PX)),
            );
            // Captured before unsizing into `Box<dyn Widget>`; the
            // coercion preserves the address, valid for the host's lifetime.
            let row_ptr: *mut GroupRow = &raw mut *strip_row;
            group_row_ptrs.insert(strip.id, row_ptr);
            content_col = content_col.push(strip_row as Box<dyn Widget>);
        }
    }

    // Full-screen siblings-underneath layer so the translucent topbar
    // (alpha 204) picks up the gradient tint behind it — a Vello stand-in
    // for backdrop-blur.
    let backdrop: Box<dyn Widget> = Box::new(desktop_backdrop::DesktopBackdrop::new());
    let foreground_col = Column::new()
        .with_no_padding()
        .push(topbar)
        .push(Box::new(content_col));
    let stack = Stack::new().push(backdrop).push(Box::new(foreground_col));

    let root: Box<dyn Widget> = Box::new(
        Container::new()
            .with_no_padding()
            .with_child(Box::new(stack))
            .filling(),
    );
    (root, group_row_ptrs)
}

/// As [`build_nirios_overlay_host_with_handle`], but with the store the
/// open tabs' documents live in named by the caller.
// `tree_handle` is only ever read here (`.clone()`/`.read()`), never
// moved, so `&WorkspaceTreeHandle` would be the tighter signature — but
// its callers outside this module (`compositor::frame`,
// `compositor::hosted`) already hold their own owned handle at the call
// site, so narrowing here is not this pass's call to make.
#[allow(
    clippy::needless_pass_by_value,
    reason = "tree_handle is only ever borrowed here, but narrowing the signature would touch \
              call sites in shell::compositor, outside this pass's scope"
)]
pub fn build_nirios_overlay_host_with_records(
    tree_handle: state::WorkspaceTreeHandle,
    enable_peer_sync: bool,
    records: ui::record::RecordStore,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    let chrome = chrome_state::ChromeState::new_with_handle(tree_handle.clone());
    chrome.borrow_mut().set_records(records);
    let records = chrome.borrow().records().clone();
    // Points every hosted file explorer at the desktop's one open path, so
    // activating a row resolves the owning app and focuses an already-open
    // document exactly as a palette hit does. Installed once here and
    // picked up by explorers this host creates later without threading a
    // handle through each constructor.
    install_document_opener(&chrome);
    // Snapshot to walk without holding the read lock across widget
    // construction; feeds only the *initial* layout, the live chrome
    // handle drives subsequent updates.
    let tree: WorkspaceTree = tree_handle.read().clone();

    let (root, group_row_ptrs) = build_workspace_root(&tree, &records, &chrome);
    // Each workspace is a spoke; dragging rotates them, releasing snaps to
    // the nearest one.
    let spokes: Vec<sidebar_wheel_disc::SpokeLabel> = tree
        .workspaces
        .iter()
        .map(|w| sidebar_wheel_disc::SpokeLabel {
            text: w.name.clone(),
            workspace_id: Some(w.id.0),
        })
        .collect();
    let active_idx = tree
        .workspaces
        .iter()
        .position(|w| w.id == tree.active)
        .unwrap_or(0);
    let sidebar: Box<dyn Widget> = Box::new(
        sidebar_wheel_disc::SidebarWheelDisc::new(spokes)
            .with_active_idx(active_idx)
            .with_chrome(chrome.clone()),
    );

    // The slot's top is pinned below the topbar so the sidebar's
    // click-guard doesn't overlap the overview-toggle icon. With padding
    // off on the outer Container/Column the topbar lands at y=0, so its
    // bottom is exactly `TOPBAR_HEIGHT_PX`.
    let sidebar_top = topbar_chrome::TOPBAR_HEIGHT_PX;
    let host = overlay_host::ShellOverlayHost::new(root)
        .with_chrome(chrome.clone())
        .with_sidebar(sidebar)
        .with_sidebar_top(sidebar_top)
        .with_group_row_ptrs(group_row_ptrs)
        .with_content_workspace(tree.active.raw())
        .with_peer_sync_enabled(enable_peer_sync);
    (host, chrome)
}

/// Registers each `Collaborator` with `active_tab: Some(tab_id)` under
/// `PresenceKey::Tab { id: format!("{:x}", tab_id) }`, the key
/// `TabBar::draw` reads to paint presence dots beneath a tab pill.
/// Additive — call once per frame before draw; reset `PresenceCtx::here`
/// first if the roster needs clearing.
pub fn sync_tab_presence_from_tree(tree: &WorkspaceTree, ctx: &mut ui::presence::PresenceCtx) {
    use ui::permissions::UserId;
    use ui::presence::PresenceKey;
    if let Some(ws) = tree.active_workspace() {
        for c in &ws.collaborators {
            // Colour overrides first: later write wins for the same user
            // id, but ordering doesn't matter since this only walks the
            // active workspace.
            if let Some(hex) = &c.cursor_color {
                ctx.set_color_override(UserId(c.id.clone()), hex);
            }
            if let Some(tab_id) = c.active_tab {
                let key = PresenceKey::Tab {
                    id: format!("{tab_id:x}"),
                };
                ctx.register_presence(&key, UserId(c.id.clone()));
            }
        }
    }
    // Tree-level roster too (top-bar avatar stack); only the colour
    // override applies since tab attribution lives on the workspace.
    for c in &tree.collaborators {
        if let Some(hex) = &c.cursor_color {
            ctx.set_color_override(UserId(c.id.clone()), hex);
        }
    }
}

#[cfg(test)]
mod hosted_app_tests {
    use super::*;

    /// Drag a tab into its own window and the group rebuilds from widgets,
    /// which knows only the tab's title — if the recovered label isn't
    /// what the hydration match arms name, the tab comes back as a
    /// "coming soon" card. This is the failure this table exists to prevent.
    #[test]
    fn every_hosted_app_round_trips_from_its_tab_title_into_a_live_app() {
        for (title, label) in HOSTED_OTHER_APPS {
            let recovered = hosted_label_for_title(title);
            assert_eq!(
                &recovered, label,
                "a tab titled {title:?} must recover the label {label:?}",
            );

            let tile = ShellTile::AppPlaceholder {
                kind: ShellKind::Other(recovered.clone()),
                label: (*title).to_string(),
            };
            assert_eq!(
                resolve_tile_shape_for_test(&tile),
                "App",
                "{title:?} hydrated into a placeholder card instead of the \
                 app it names",
            );
            assert_eq!(
                to_ui_kind(&ShellKind::Other(recovered)),
                UiKind::Editor,
                "{title:?} must wear an editor glyph, not the unknown-kind \
                 settings cog",
            );
        }
    }

    /// A title the shell does not host carries through untouched, so a
    /// foreign or future app is not silently renamed into one of ours.
    #[test]
    fn an_unhosted_title_is_carried_through_unchanged() {
        assert_eq!(hosted_label_for_title("Inkscape"), "Inkscape");
        assert_eq!(hosted_label_for_title("report.odt"), "report.odt");
    }
}
