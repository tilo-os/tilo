//! # Visual smoke checklist
//!
//! Run `cargo run -p tilo-desktop` and verify:
//!
//! - `TopBar` renders with a live clock; wheel over the center area cycles to
//!   the CPU/RAM sparkline and the calendar (200 ms cooldown).
//! - Cmd+K opens the search palette; typing filters; Enter invokes; Escape closes.
//! - Cmd+Tab toggles the `OverviewView`; clicking a card activates the workspace.
//! - The `RightTray` gear icon opens `QuickSettings`; sliders are interactive;
//!   WiFi/BT drill-in and back work.
//! - The workspace-name button opens `WorkspaceSettings`; switching to the Roles
//!   tab lets you toggle per-app permissions via `DataTable`; the Files tab
//!   shows per-role × per-file matrix.
//! - `SidebarWheelDisc` switches workspaces, resists with overscroll at each
//!   end, and exposes an add-workspace control on the disc band.
//! - Right-clicking a tile opens the context menu.
//! - Hovering the workspace-name button reveals a tooltip after ~600 ms.
//! - Creating a workspace (any path) triggers a toast.
//! - A simulated remote cursor floats over the active tile.

pub mod actions;
pub(crate) mod animation_diagnostics;
pub mod app_action_access;
pub mod app_action_consent;
pub mod app_close;
pub mod automation_folders;
pub mod automation_host;
pub mod automation_triggers;
pub(crate) mod broker_services;
pub mod capture_protection;
pub(crate) mod capture_surface;
pub mod catalogue;
pub mod channel_encryption;
pub mod channel_policy;
pub mod chrome_state;
pub mod closed_tabs;
pub mod codec_pack_notice;
pub mod collab_broker;
pub mod collab_broker_capnp;
pub mod collab_core;
pub mod collab_history;
pub mod collab_mls_authority;
pub mod collab_outbound;
pub mod collab_presence;
pub mod color_pipeline;
pub mod continuity_policy;
pub mod continuity_suggestion;
pub mod crdt_text;
pub mod cursor_interp;
pub mod desktop_backdrop;
pub(crate) mod desktop_commands;
#[cfg(target_os = "linux")]
pub(crate) mod desktop_continuation;
pub mod device_fabric;
pub mod device_fabric_runtime;
pub mod device_fabric_secrets;
pub mod display_bars;
pub mod display_confirm;
pub mod display_identify;
pub mod doc_open;
pub mod document_import;
pub mod editor_app;
pub mod emoji_recents;
pub mod fabric_clock;
mod file_picker_overlay;
pub mod fixed_chords;
pub mod fixture_inputs;
pub mod floating;
pub mod foreign;
pub mod frame;
pub mod fs_authority;
pub mod hdr_display;
pub mod hdr_surface;
pub mod hdr_tonemap;
pub mod history_narrative;
pub mod host_apps;
pub mod hosted_inputs;
pub mod i18n;
pub mod identity_provisioning;
pub mod input_policy;
pub mod keyboard_shortcuts;
pub mod launch_gate;
pub mod launcher;
pub mod live_presence;
pub mod magnifier;
pub mod media_keys;
pub mod mpris;
pub(crate) mod native_media;
pub(crate) mod native_workspace_owner;
pub mod nearby_transfer;
pub mod network;
pub mod notes_app;
pub mod notification;
pub mod notification_history;
pub mod now_playing;
pub mod osd;
pub mod output_scene;
pub mod overlay_host;
pub mod overview;
pub mod pad_map;
pub mod peer_actions;
pub mod peer_presence;
pub mod peer_sync_overlay;
pub mod peer_view;
pub mod peer_windows;
pub mod persist;
pub mod personal_continuity;
pub mod personal_continuity_registry;
pub mod personal_tethering;
pub mod personal_tethering_runtime;
pub mod physical_gamepad;
pub mod placement;
pub mod portal_dialog;
mod power_menu;
pub mod present;
pub mod projection;
mod projection_overlay;
pub mod quick_settings;
pub mod realtime_session;
pub mod recent_files;
pub mod record_hydration;
pub mod remote_action_link;
pub mod remote_actions;
pub mod remote_audio;
pub mod remote_capture;
pub mod remote_cursors;
pub mod render;
pub mod room_activity;
pub mod room_records;
pub mod screenshot_send;
pub mod search;
pub mod service_members;
pub mod session_audio_policy;
pub mod session_policy;
pub mod shell_narrator;
pub mod spatial_calibration;
pub mod speaker_calibration;
pub mod speaker_group;
pub mod speaker_group_audio;
pub mod speaker_group_runtime;
pub mod speaker_settings;
pub mod split_row;
pub mod state;
pub mod status;
pub mod system_monitor;
pub mod system_sounds;
pub mod tab_binding;
pub mod tab_documents;
pub mod test_control;
#[cfg(unix)]
pub mod test_control_socket;
pub mod test_support;
pub mod tile;
pub mod tool_intents;
pub mod topbar_chrome;
pub mod trusted_ui;
pub mod uri_open;
pub mod window_actions;
pub mod window_appearance;
pub mod window_previews;
pub mod window_transfer;
#[cfg(target_os = "linux")]
pub mod windows_compat;
pub mod workspace_drop;
pub mod workspace_governance;
pub mod workspace_history;

use state::{AppKind as ShellKind, TileContent as ShellTile, WorkspaceTree};
use ui::widget::context_menu::MenuItem;
#[cfg(any(test, debug_assertions))]
use ui::widget::toast::ToastStack;
use ui::widget::{
    AppKind as UiKind, AppWidget, GroupRow, TabDescriptor, TileContent as UiTile, Widget,
    WindowGroup as UiGroup, context_menu::ContextMenu,
};

use crate::shell::editor_app::EditorApp;
use browser::BrowserWidget;

use crate::shell::notes_app::NotesApp;
use crate::shell::tile::{
    Strip as RuntimeStrip, Tab as RuntimeTab, TileContent as RuntimeTile,
    WindowGroup as RuntimeGroup, Workspace as RuntimeWorkspace, WorkspaceTree as RuntimeTree,
};

use file_explorer::FileExplorerWidget;
use terminal::TerminalWidget;
pub use tilo_desktop::launchpad::{
    EMPTY_WORKSPACE_ACTIVITY, EMPTY_WORKSPACE_CHAT, EMPTY_WORKSPACE_INVITE, EMPTY_WORKSPACE_MEET,
    EMPTY_WORKSPACE_NOTES, EMPTY_WORKSPACE_TASKS,
};
use tilo_desktop::workspace_settings::WorkspaceSettings;

/// Root tilo shell handle. Owns the runtime workspace tree — the live
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

    ws_settings: WorkspaceSettings,
    #[cfg(any(test, debug_assertions))]
    toasts: ToastStack,
    ctx_menu: ContextMenu,

    overview_visible: bool,
    search_visible: bool,
    quick_settings_visible: bool,
    _hosted_inputs: hosted_inputs::HostedTileInputs,
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
    /// (null backend), Files, Notes, Editor, and a
    /// Browser chrome. Production launches the packaged WPE-backed browser;
    /// this in-process workspace mirror uses the same `BrowserWidget` contract.
    /// # Panics
    /// Panics if its explicitly temporary fixture directory cannot be created.
    pub fn new_with_default_demo_workspace() -> Self {
        Self::new_demo_workspace(None)
    }

    /// Construct the same demo workspace with an explicitly provided Terminal.
    /// The caller creates the widget with its chosen process/service; this
    /// constructor performs no process discovery or login-shell fallback.
    /// # Panics
    /// Panics if the temporary Files fixture cannot be created.
    pub fn new_with_demo_terminal(terminal: TerminalWidget) -> Self {
        Self::new_demo_workspace(Some(terminal))
    }

    #[allow(
        clippy::expect_used,
        reason = "The scratch workspace requires its private temporary Files fixture."
    )]
    fn new_demo_workspace(terminal: Option<TerminalWidget>) -> Self {
        let hosted_inputs = hosted_inputs::HostedTileInputs::fixture().expect("demo Files fixture");
        let DemoTabs {
            tabs,
            #[cfg(any(test, debug_assertions))]
            first_terminal_ptr,
            #[cfg(any(test, debug_assertions))]
            first_file_explorer_ptr,
        } = default_demo_tabs(&hosted_inputs, terminal);
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
            ws_settings: WorkspaceSettings::new(active_ws_id),
            #[cfg(any(test, debug_assertions))]
            toasts: ToastStack::new(),
            ctx_menu: ContextMenu::new(),
            overview_visible: false,
            search_visible: false,
            quick_settings_visible: false,
            _hosted_inputs: hosted_inputs,
            ws_settings_visible: false,
            #[cfg(any(test, debug_assertions))]
            first_terminal_ptr,
            #[cfg(any(test, debug_assertions))]
            first_file_explorer_ptr,
        };
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

    /// Canonical entry point for shell-level global hotkeys — both the
    /// production path ([`Shell::handle_window_event`]) and tests
    /// (`shell::test_support::key_press`) funnel through here. Returns
    /// `true` if the press matched a global binding, `false` if it should
    /// fall through to the focused widget tree.
    // `press` is only ever read here, but narrowing to `&KeyPress` would
    // touch `crates/session/shell/tests/shell_chrome_test.rs`'s call sites, outside this
    // pass's scope (only crates/session/shell/src/shell/ is ours to edit).
    #[allow(
        clippy::needless_pass_by_value,
        reason = "press is only ever borrowed here, but narrowing the signature would touch \
                  crates/session/shell/tests call sites outside this pass's scope"
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

    /// `ToastKind` of every active toast in push order.
    #[cfg(any(test, debug_assertions))]
    pub fn toast_kinds(&self) -> Vec<ui::widget::toast::ToastKind> {
        self.toasts.iter().map(|t| t.kind).collect()
    }

    /// Routes a +/- wheel tick into the sidebar and returns the resulting
    /// active-strip index.
    #[cfg(any(test, debug_assertions))]
    pub fn simulate_sidebar_wheel(&mut self, dir: i32) -> usize {
        self.tree.move_active_strip(dir).unwrap_or(0)
    }

    /// Index of the currently-active strip on the active workspace in the
    /// runtime tree. `0` when the workspace has no strips (shouldn't happen
    /// for a sanely-constructed shell, but the caller shouldn't panic in
    /// that case).
    #[cfg(any(test, debug_assertions))]
    pub fn active_strip_index(&self) -> usize {
        let active_ws = self.tree.active;
        let Some(workspace) = self.tree.workspaces.iter().find(|w| w.id == active_ws) else {
            return 0;
        };
        self.tree
            .active_strip
            .get(&active_ws)
            .and_then(|id| workspace.strips.iter().position(|strip| strip.id == *id))
            .unwrap_or(0)
    }

    #[cfg(any(test, debug_assertions))]
    pub fn append_empty_strip_for_test(&mut self, name: &str) -> usize {
        let Some(workspace) = self
            .tree
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == self.tree.active)
        else {
            return 0;
        };
        workspace.strips.push(RuntimeStrip::new(name, Vec::new()));
        workspace.strips.len().saturating_sub(1)
    }

    /// Same route as a real tray-gear click: a toggle of quick settings.
    #[cfg(any(test, debug_assertions))]
    pub fn simulate_tray_gear_click(&mut self) {
        self.toggle_quick_settings();
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
        self.toasts.push(
            crate::shell::i18n::t("shell-workspace-renamed"),
            ToastKind::Info,
        );
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

    /// Populate `ctx_menu` with the tab action list and open it at
    /// `position`. The items come from [`tab_context_menu_items`], the one
    /// list the production host (`ShellOverlayHost`) builds its menu from,
    /// so this fixture can never offer an action the desktop does not.
    #[cfg(any(test, debug_assertions))]
    fn open_default_tile_context_menu(&mut self, position: [f32; 2]) {
        // Rebuild from scratch so repeated right-clicks don't accumulate
        // items from earlier menus.
        let (menu, _actions) = build_tab_context_menu(2);
        self.ctx_menu = menu;
        self.ctx_menu.open_at(position);
    }
}

/// One action a tab's right-click menu offers. The production
/// [`overlay_host::ShellOverlayHost`] and the fixture [`Shell`] both build
/// their menu from [`tab_context_menu_items`], so the two cannot drift, and
/// every action here is reachable by pointer, keyboard (arrows + Enter in
/// the open menu) and by its [`Self::semantic_key`] through accessibility,
/// scripts and the bench — a menu row is not a capability on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabMenuAction {
    /// Close this tab through the same durable lifecycle the `✕` and the
    /// keyboard chord use (`ShellOverlayHost::close_tab_in_group`).
    CloseTab,
    /// Detach the tab into a new window group immediately to the right of
    /// its current group — the drag-out gesture, without the drag.
    SplitRight,
    /// Detach the tab into a new window group at the end of the strip.
    MoveToNewGroup,
}

impl TabMenuAction {
    /// User-facing label, from the shell's catalogue.
    pub fn label(self) -> String {
        use crate::shell::i18n::t;
        match self {
            Self::CloseTab => t("shell-tab-menu-close"),
            Self::SplitRight => t("shell-tab-menu-split-right"),
            Self::MoveToNewGroup => t("shell-tab-menu-new-group"),
        }
    }

    /// Stable semantic address of the menu row (`tabmenu:<slug>`): the
    /// `semantic:` target a script or the bench names, and the
    /// `perform_semantic_action` target an assistive technology activates.
    pub fn semantic_key(self) -> &'static str {
        match self {
            Self::CloseTab => "tabmenu:close",
            Self::SplitRight => "tabmenu:split-right",
            Self::MoveToNewGroup => "tabmenu:new-group",
        }
    }

    /// The action a semantic target names, if it is one of ours.
    pub fn from_semantic_key(key: &str) -> Option<Self> {
        [Self::CloseTab, Self::SplitRight, Self::MoveToNewGroup]
            .into_iter()
            .find(|action| action.semantic_key() == key)
    }
}

/// The tab menu's rows in order; `None` is a separator. A group with a
/// single tab is not offered the two split actions — detaching its only
/// tab would dissolve the group and re-create it one slot over, a no-op
/// that reads as a glitch.
pub fn tab_context_menu_items(tab_count: usize) -> Vec<Option<TabMenuAction>> {
    let mut items = vec![Some(TabMenuAction::CloseTab)];
    if tab_count >= 2 {
        items.push(None);
        items.push(Some(TabMenuAction::SplitRight));
        items.push(Some(TabMenuAction::MoveToNewGroup));
    }
    items
}

/// A closed [`ContextMenu`] holding [`tab_context_menu_items`] for a group
/// of `tab_count` tabs, plus the index-aligned action list the host reads a
/// selection back through (`ContextMenu::take_selection` yields the row
/// index; the `Arc<dyn Fn()>` callbacks are deliberately inert because the
/// host has to mutate itself and the persisted tree).
pub fn build_tab_context_menu(tab_count: usize) -> (ContextMenu, Vec<Option<TabMenuAction>>) {
    let items = tab_context_menu_items(tab_count);
    let mut menu = ContextMenu::new();
    for item in &items {
        match item {
            Some(action) => menu.push(MenuItem::action(action.label(), || {})),
            None => menu.push(MenuItem::separator()),
        }
    }
    (menu, items)
}

#[cfg(test)]
mod tab_menu_item_tests {
    use super::*;

    #[test]
    fn tab_menu_items_depend_on_group_size_and_keys_round_trip() {
        fn a_single_tab_group_is_offered_only_close() {
            let items = tab_context_menu_items(1);
            assert_eq!(items, vec![Some(TabMenuAction::CloseTab)]);
            let (menu, _) = build_tab_context_menu(1);
            assert_eq!(menu.items().len(), 1);
        }

        fn a_multi_tab_group_gets_close_then_the_split_pair_after_a_separator() {
            let items = tab_context_menu_items(3);
            assert_eq!(
                items,
                vec![
                    Some(TabMenuAction::CloseTab),
                    None,
                    Some(TabMenuAction::SplitRight),
                    Some(TabMenuAction::MoveToNewGroup),
                ]
            );
            let (menu, actions) = build_tab_context_menu(3);
            assert_eq!(menu.items().len(), actions.len(), "one menu row per item");
            let labels: Vec<_> = menu.items().iter().filter_map(|i| i.label()).collect();
            assert_eq!(labels, ["Close Tab", "Split Right", "Move to New Group"]);
        }

        fn semantic_keys_round_trip_and_are_unique() {
            for action in [
                TabMenuAction::CloseTab,
                TabMenuAction::SplitRight,
                TabMenuAction::MoveToNewGroup,
            ] {
                assert_eq!(
                    TabMenuAction::from_semantic_key(action.semantic_key()),
                    Some(action)
                );
                assert!(action.semantic_key().starts_with("tabmenu:"));
            }
            assert_eq!(TabMenuAction::from_semantic_key("tabmenu:nope"), None);
        }

        a_single_tab_group_is_offered_only_close();
        a_multi_tab_group_gets_close_then_the_split_pair_after_a_separator();
        semantic_keys_round_trip_and_are_unique();
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

#[allow(
    clippy::expect_used,
    reason = "The caller constructs these inputs using HostedTileInputs::fixture, which always supplies Files."
)]
fn default_demo_tabs(
    inputs: &hosted_inputs::HostedTileInputs,
    terminal: Option<TerminalWidget>,
) -> DemoTabs {
    let mut tabs: Vec<RuntimeTab> = Vec::new();
    #[cfg(any(test, debug_assertions))]
    let mut first_terminal_ptr: Option<*const TerminalWidget> = None;
    // Declared up here (written unconditionally below) so the final struct
    // literal can reference it without a local scope leak.
    #[cfg(any(test, debug_assertions))]
    let first_file_explorer_ptr: Option<*const FileExplorerWidget>;

    match terminal.map_or_else(TerminalWidget::spawn_headless, Ok) {
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
                    label: crate::shell::i18n::t_args(
                        "shell-terminal-unavailable",
                        &[("error", e.to_string().into())],
                    ),
                },
            ));
        }
    }

    // File-explorer: `new` is infallible; OS errors surface via its own
    // in-pane status bar rather than the Stub fallback.
    let fe_boxed: Box<FileExplorerWidget> = Box::new(FileExplorerWidget::with_startup_inputs(
        inputs.files.clone().expect("demo Files inputs"),
    ));
    #[cfg(any(test, debug_assertions))]
    {
        first_file_explorer_ptr = Some(&raw const *fe_boxed);
    }
    let fe_app: Box<dyn AppWidget> = fe_boxed;
    tabs.push(RuntimeTab::new("Files", RuntimeTile::App(fe_app)));

    // Seeds a welcome paragraph on first boot; later boots load the shared doc.
    let notes = NotesApp::with_seed(
        "Welcome to tilo.\n\n\
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
        RuntimeTile::App(Box::new(BrowserWidget::new())),
    ));

    DemoTabs {
        tabs,
        #[cfg(any(test, debug_assertions))]
        first_terminal_ptr,
        #[cfg(any(test, debug_assertions))]
        first_file_explorer_ptr,
    }
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
/// # Panics
/// Panics if its explicitly temporary fixture directory cannot be created.
#[allow(
    clippy::expect_used,
    reason = "These infallible scratch/demo entry points require a private temporary Files fixture."
)]
pub fn resolve_tile_shape_for_test(t: &ShellTile) -> &'static str {
    let inputs = hosted_inputs::HostedTileInputs::fixture().expect("tile-shape fixture");
    match to_ui_tile_with_inputs(t, &inputs) {
        UiTile::App(_) => "App",
        UiTile::Stub { .. } => "Stub",
        UiTile::Notice { .. } => "Notice",
    }
}

/// Shared by the initial tree→widget build and by
/// [`overlay_host::ShellOverlayHost`]'s per-revision resync.
pub(crate) fn tile_app_kind(t: &ShellTile) -> UiKind {
    match t {
        ShellTile::WorkspaceActivity => UiKind::Settings,
        ShellTile::Stub { kind, .. } | ShellTile::AppPlaceholder { kind, .. } => to_ui_kind(kind),
        // No `AppKind` describes "arbitrary external client", so both a
        // foreign surface and a peer window wear the catch-all `Settings`
        // glyph — and keep wearing it when a `PeerWindow` binds in and
        // becomes `Foreign`, so the pill doesn't change under the user.
        ShellTile::Foreign { .. } | ShellTile::PeerWindow { .. } | ShellTile::LocalApp { .. } => {
            UiKind::Settings
        }
    }
}

pub fn to_ui_tile(t: &ShellTile) -> UiTile {
    to_ui_tile_with_inputs(t, &hosted_inputs::HostedTileInputs::default())
}

pub(crate) fn to_ui_tile_with_inputs(
    t: &ShellTile,
    inputs: &hosted_inputs::HostedTileInputs,
) -> UiTile {
    match try_to_ui_tile(t, terminal::HostedProcessServices::default(), inputs) {
        Ok(tile) => tile,
        Err(error) => UiTile::Notice {
            title: crate::shell::i18n::t("shell-app-unavailable"),
            detail: error.to_string(),
        },
    }
}

pub(crate) fn to_ui_tile_for_tab_with_inputs(
    t: &ShellTile,
    tab: state::TabId,
    inputs: &hosted_inputs::HostedTileInputs,
) -> UiTile {
    try_to_ui_tile_for_tab(
        t,
        terminal::HostedProcessServices::default(),
        inputs,
        Some(tab),
    )
    .unwrap_or_else(|error| UiTile::Notice {
        title: crate::shell::i18n::t("shell-app-unavailable"),
        detail: error.to_string(),
    })
}

/// Construct the actual embedded app using explicit host startup inputs.
/// # Errors
/// A required service must exist before the widget or a process is constructed.
pub fn try_to_ui_tile(
    t: &ShellTile,
    services: terminal::HostedProcessServices,
    inputs: &hosted_inputs::HostedTileInputs,
) -> Result<UiTile, hosted_inputs::HostedTileError> {
    try_to_ui_tile_for_tab(t, services, inputs, None)
}

#[expect(
    clippy::too_many_lines,
    reason = "one match over every tile kind, each building its hosted tile with localized fallbacks"
)]
fn try_to_ui_tile_for_tab(
    t: &ShellTile,
    services: terminal::HostedProcessServices,
    inputs: &hosted_inputs::HostedTileInputs,
    tab: Option<state::TabId>,
) -> Result<UiTile, hosted_inputs::HostedTileError> {
    Ok(match t {
        // The retained activity controller is painted into this ordinary
        // tile by `ShellOverlayHost`, which also owns the workspace-history
        // index. The neutral body exists only for the first reconciliation
        // frame before that controller has its content rect.
        ShellTile::WorkspaceActivity => UiTile::Notice {
            title: crate::shell::i18n::t("shell-activity-title"),
            detail: crate::shell::i18n::t("shell-activity-loading"),
        },
        ShellTile::Stub { kind, label } => UiTile::Stub {
            kind: to_ui_kind(kind),
            label: label.clone(),
        },
        // Pulls the client's most recent buffer out of
        // `compositor::foreign`'s frame store and also publishes the tab's
        // rect back so the compositor can configure the client to size.
        ShellTile::Foreign { app_id, surface_id } => UiTile::App(Box::new(
            crate::shell::foreign::ForeignSurfaceWidget::new(*surface_id, app_id.clone()),
        )),
        // A window someone else has open that this machine hasn't got its
        // own instance of yet; becomes a `Foreign` above once it arrives.
        ShellTile::PeerWindow { label, note, .. } => UiTile::Notice {
            title: label.clone(),
            detail: note.clone(),
        },
        ShellTile::LocalApp { label, note, .. } => UiTile::Notice {
            title: label.clone(),
            detail: if note.is_empty() {
                crate::shell::i18n::t("shell-window-restoring")
            } else {
                note.clone()
            },
        },
        ShellTile::AppPlaceholder { kind, label } => match kind {
            ShellKind::Terminal => {
                let widget = if inputs.is_fixture() {
                    TerminalWidget::spawn_headless().map_err(|error| {
                        hosted_inputs::HostedTileError::Fixture(error.to_string())
                    })?
                } else {
                    TerminalWidget::spawn_hosted(services)
                        .map_err(hosted_inputs::HostedTileError::Terminal)?
                };
                UiTile::App(Box::new(widget))
            }
            // File-explorer — `new` is infallible; the widget surfaces any
            // directory-read errors via its own in-pane status bar.
            ShellKind::FileExplorer => {
                let tab_files = tab.and_then(|tab| inputs.files_for_tab(tab));
                let widget = if let Some(files) = tab_files.as_ref().or(inputs.files.as_ref()) {
                    if let Some(services) = inputs.file_services() {
                        FileExplorerWidget::with_startup_inputs_and_services(
                            files.clone(),
                            services.clone(),
                        )
                    } else {
                        FileExplorerWidget::with_startup_inputs(files.clone())
                    }
                } else {
                    if inputs.is_fixture() {
                        return Err(hosted_inputs::HostedTileError::RequiredServiceMissing(
                            "Files startup",
                        ));
                    }
                    #[cfg(target_os = "linux")]
                    {
                        FileExplorerWidget::new()
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        return Err(hosted_inputs::HostedTileError::RequiredServiceMissing(
                            "Files startup",
                        ));
                    }
                };
                UiTile::App(Box::new(widget))
            }
            ShellKind::Browser => UiTile::App(Box::new(BrowserWidget::new())),
            // Each opens on an empty document — prose the shell wrote would
            // be indistinguishable from prose the user wrote.
            ShellKind::Other(s) if s.eq_ignore_ascii_case("Notes") => {
                UiTile::App(Box::new(NotesApp::new()))
            }
            ShellKind::Other(s) if s.eq_ignore_ascii_case("Editor") => {
                match tab.and_then(|tab| inputs.document(tab)) {
                    Some(hosted_inputs::HostedDocumentState::Ready(document)) => {
                        UiTile::App(Box::new(EditorApp::for_file(
                            document.path,
                            document.provider_id,
                            document.text,
                        )))
                    }
                    Some(hosted_inputs::HostedDocumentState::Unavailable {
                        path, reason, ..
                    }) => UiTile::Notice {
                        title: path.file_name().map_or_else(
                            || crate::shell::i18n::t("shell-document-unavailable"),
                            |name| {
                                crate::shell::i18n::t_args(
                                    "shell-named-document-unavailable",
                                    &[("name", name.to_string_lossy().into_owned().into())],
                                )
                            },
                        ),
                        detail: reason,
                    },
                    None => UiTile::App(Box::new(EditorApp::new())),
                }
            }
            ShellKind::Other(s) if s.eq_ignore_ascii_case("Word") => {
                UiTile::App(Box::new(word_processor::WordProcessorWidget::new()))
            }
            _ => UiTile::Stub {
                kind: to_ui_kind(kind),
                label: label.clone(),
            },
        },
    })
}

/// v1 renders the active workspace's active strip as a `StripView` overlay
/// above the active `WorkspaceView`, with placeholder groups laid out
/// horizontally.
pub fn build_tilo_root(tree: &WorkspaceTree) -> Box<dyn Widget> {
    let (root, _chrome) = build_tilo_root_with_chrome(tree);
    root
}

/// Lets the caller expose the peer-sync palette commands (`/invite`,
/// `/connect`) for a network-enabled session. [`build_tilo_root`] keeps
/// the zero-arg shape so call-sites that don't care don't have to thread it.
pub fn build_tilo_root_with_options(
    tree: &WorkspaceTree,
    enable_peer_sync: bool,
) -> Box<dyn Widget> {
    let (host, _chrome) = build_tilo_overlay_host_with_options(tree, enable_peer_sync);
    Box::new(host) as Box<dyn Widget>
}

/// Also returns the shared chrome handle, so integration tests can observe
/// overlay toggles without drilling through the widget tree.
pub fn build_tilo_root_with_chrome(
    tree: &WorkspaceTree,
) -> (Box<dyn Widget>, chrome_state::Chrome) {
    let (host, chrome) = build_tilo_overlay_host_with_chrome(tree);
    (Box::new(host) as Box<dyn Widget>, chrome)
}

/// Returns the concrete [`overlay_host::ShellOverlayHost`] instead of a
/// boxed trait object, so tests can assert on host-local state like
/// `palette_visible()`. Production wiring goes through `build_tilo_root`.
pub fn build_tilo_overlay_host_with_chrome(
    tree: &WorkspaceTree,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    build_tilo_overlay_host_with_options(tree, false)
}

pub fn build_tilo_overlay_host_with_options(
    tree: &WorkspaceTree,
    enable_peer_sync: bool,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    build_tilo_overlay_host_with_handle(tree.clone().shared(), enable_peer_sync)
}

/// Extracted so `ShellOverlayHost::resync_strips_from_tree` can reach for
/// the same conversion when splicing freshly-added groups into a live
/// [`GroupRow`].
pub fn to_ui_window_group(g: &state::WindowGroup, records: &ui::record::RecordStore) -> UiGroup {
    to_ui_window_group_with_inputs(g, records, &hosted_inputs::HostedTileInputs::default())
}

pub fn to_ui_window_group_with_inputs(
    g: &state::WindowGroup,
    records: &ui::record::RecordStore,
    inputs: &hosted_inputs::HostedTileInputs,
) -> UiGroup {
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
                    icon: crate::app_icons::for_tile(&t.tile),
                },
                to_ui_tile_for_tab_with_inputs(&t.tile, t.id, inputs),
            )
        })
        .collect();
    let mut group =
        UiGroup::new(g.id, tabs_with_content, g.active_tab, g.width).with_maximized(g.maximized);
    // Every tab gets its own document before it is ever synced, so no
    // hosted app writes a keystroke into the shell's own state.
    tab_documents::bind_group_records(&mut group, records);
    group
}

/// Wires the hosted file explorer's document activation to
/// [`doc_open::open_in_workspace`] — the explorer asks, the compositor
/// answers. Named so a test can install it against its own chrome too.
pub fn install_document_opener(chrome: &chrome_state::Chrome) {
    file_explorer::open_with::set_open_with_route(std::rc::Rc::new(ShellOpenWithRoute {
        chrome: chrome.clone(),
    }));
    let chrome = chrome.clone();
    file_explorer::files_app::set_document_opener(move |path| {
        open_legacy_document_from_files(&chrome, path)
    });
}

/// Preserve the standalone synchronous opener for legacy embeddings. It runs
/// inside the Files event path, where an admission preflight would re-enter
/// the tile's record loan. Hosted native Files use the deferred owner route
/// below instead.
fn open_legacy_document_from_files(
    chrome: &chrome_state::Chrome,
    path: &std::path::Path,
) -> Result<(), String> {
    let native_app = { chrome.borrow().hosted_inputs.native_app_for_path(path) };
    if let Some(app) = native_app {
        let path = path.canonicalize().map_err(|error| {
            crate::shell::i18n::t_args(
                "shell-path-open-failed",
                &[
                    ("path", path.display().to_string().into()),
                    ("error", error.to_string().into()),
                ],
            )
        })?;
        let text = std::fs::read_to_string(&path).map_err(|error| {
            crate::shell::i18n::t_args(
                "shell-path-read-failed",
                &[
                    ("path", path.display().to_string().into()),
                    ("error", error.to_string().into()),
                ],
            )
        })?;
        let title = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        window_actions::open_hosted_after_setup(
            &mut chrome.borrow_mut(),
            app.kind,
            title,
            window_actions::Destination::Here,
            |state, tab| {
                {
                    let mut tree = state.tree_mut();
                    tree.native_document_tabs.insert(
                        tab,
                        state::NativeDocumentReference {
                            provider_id: app.package_id.to_string(),
                            path: path.clone(),
                        },
                    );
                    tree.flush_to_disk();
                }
                state.hosted_inputs.bind_document(
                    tab,
                    hosted_inputs::HostedDocument {
                        provider_id: app.package_id,
                        path,
                        text,
                    },
                );
            },
        );
        return Ok(());
    }
    doc_open::open_in_workspace(path, chrome)
        .map(|_| ())
        .map_err(|error| error.user_message())
}

/// Admit an exact native document record before publishing its tab/input pair.
/// This is called only by the owning Files open route after its record loan has
/// ended, so `TileHost` can retain its existing-only record-follow contract.
pub(crate) fn open_document_from_files(
    chrome: &chrome_state::Chrome,
    path: &std::path::Path,
) -> Result<(), String> {
    let native_app = { chrome.borrow().hosted_inputs.native_app_for_path(path) };
    if let Some(app) = native_app {
        let path = path
            .canonicalize()
            .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let checkout = ui::record::RecordCheckout::new(
            ui::record::RecordId::for_file(&path),
            ui::record::RecordKind::new("text-document"),
        );
        let records = { chrome.borrow().records().clone() };
        match records.try_open_record(&checkout) {
            Ok(_) => {}
            Err(ui::record::RecordOpenError::MissingRecord) => {
                records.create_record(&checkout).map_err(|error| {
                    format!(
                        "cannot open {}: document storage unavailable: {error}",
                        path.display()
                    )
                })?;
                records.try_open_record(&checkout).map_err(|error| {
                    format!(
                        "cannot open {}: document storage unavailable: {error}",
                        path.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "cannot open {}: document storage unavailable: {error}",
                    path.display()
                ));
            }
        }
        let title = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        window_actions::open_hosted_after_setup(
            &mut chrome.borrow_mut(),
            app.kind,
            title,
            window_actions::Destination::Here,
            |state, tab| {
                {
                    let mut tree = state.tree_mut();
                    tree.native_document_tabs.insert(
                        tab,
                        state::NativeDocumentReference {
                            provider_id: app.package_id.to_string(),
                            path: path.clone(),
                        },
                    );
                    tree.flush_to_disk();
                }
                state.hosted_inputs.bind_document(
                    tab,
                    hosted_inputs::HostedDocument {
                        provider_id: app.package_id,
                        path,
                        text,
                    },
                );
            },
        );
        return Ok(());
    }
    doc_open::open_in_workspace(path, chrome)
        .map(|_| ())
        .map_err(|error| error.user_message())
}

/// Files' "Open With": candidates, launches and defaults all come from
/// [`doc_open`], under the active workspace's policy and the local person.
struct ShellOpenWithRoute {
    chrome: chrome_state::Chrome,
}

impl file_explorer::open_with::OpenWithRoute for ShellOpenWithRoute {
    fn candidates(&self, path: &std::path::Path) -> file_explorer::open_with::OpenWithCandidates {
        let apps = {
            let state = self.chrome.borrow();
            let tree = state.tree();
            doc_open::open_with_choices(
                path,
                &state.local_user_id,
                tree.desktop
                    .active_workspace()
                    .map(|workspace| &workspace.default_apps),
            )
        };
        file_explorer::open_with::OpenWithCandidates {
            type_label: doc_open::type_label(path),
            apps: apps
                .into_iter()
                .map(|choice| file_explorer::open_with::OpenWithApp {
                    id: choice.id,
                    name: choice.name,
                    is_default: choice.is_default,
                })
                .collect(),
        }
    }

    fn open_with(&self, path: &std::path::Path, app_id: &str) -> Result<(), String> {
        doc_open::open_with_app(path, &self.chrome, app_id)
            .map(|_| ())
            .map_err(|error| error.user_message())
    }

    fn set_default(&self, path: &std::path::Path, app_id: &str) -> Result<(), String> {
        doc_open::set_personal_default(path, app_id)
    }
}

/// The production entry point: threads the *shared* [`WorkspaceTreeHandle`](crate::shell::state::WorkspaceTreeHandle)
/// (the same `Arc<RwLock<_>>` the portal's `GrantStoreHandle` and
/// peer-sync's `WorkspaceTreeReplica` hold) so chrome mutations land on the
/// tree other subsystems read from. Reads it once for the initial widget
/// tree; later rebuilds run `sync_from_tree` on each chrome widget.
/// # Panics
/// Panics if its explicitly temporary fixture directory cannot be created.
#[allow(
    clippy::expect_used,
    reason = "These infallible scratch/demo entry points require a private temporary Files fixture."
)]
pub fn build_tilo_overlay_host_with_handle(
    tree_handle: state::WorkspaceTreeHandle,
    enable_peer_sync: bool,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    // In-memory records: a host built without being told where to keep them
    // is a test, a probe or a `--shots` run, none of which may leave a
    // user's tabs on disk. A session passes its own store through
    // [`build_tilo_overlay_host_with_records`].
    build_tilo_overlay_host_with_inputs(
        tree_handle,
        enable_peer_sync,
        ui::record::RecordStore::in_memory(),
        hosted_inputs::HostedTileInputs::fixture().expect("scratch Files fixture"),
    )
}

/// A scratch tab belongs to the compositor, not to any installed app, so
/// its records live under this package in the compositor's own state dir.
pub const SHELL_RECORDS_PACKAGE: &str = "org.tilo.Shell";

pub fn session_record_store() -> ui::record::RecordStore {
    ui::record::RecordStore::open(SHELL_RECORDS_PACKAGE)
}

/// Host services for the shared desktop surface: native tile content, the
/// session's record-creation refusals, and foreign-surface readiness.
pub(crate) struct NativeTileResolver<'a> {
    pub inputs: &'a hosted_inputs::HostedTileInputs,
    pub failures: &'a std::collections::BTreeMap<u64, String>,
}

impl tilo_desktop::resolver::TileResolver for NativeTileResolver<'_> {
    fn resolve(
        &mut self,
        request: tilo_desktop::resolver::TileRequest<'_>,
    ) -> tilo_desktop::resolver::TileResolution {
        tilo_desktop::resolver::TileResolution::Content(to_ui_tile_for_tab_with_inputs(
            &request.tab.tile,
            request.tab.id,
            self.inputs,
        ))
    }
    fn creation_refusal(&self, tab: &state::Tab) -> Option<String> {
        self.failures.get(&tab.id).cloned()
    }
    fn tab_icon(&self, tab: &state::Tab) -> Option<ui::icons::AppImage> {
        crate::app_icons::for_tile(&tab.tile)
    }
}

/// The native chrome the shared desktop surface is built with: the session's
/// top bar and wallpaper, the collaboration launchpad for an empty
/// workspace, and a leading inset for the sidebar wheel.
pub(crate) fn workspace_surface_chrome(
    tree: &WorkspaceTree,
    chrome: &chrome_state::Chrome,
) -> tilo_desktop::composition::SurfaceChrome {
    let local = chrome
        .borrow()
        .explicit_local_user()
        .map_or_else(crate::shell::state::local_user_id, str::to_owned);
    let topbar = topbar_chrome::TopBarChrome::from_tree_with_local_user(&tree.desktop, local)
        .with_chrome(topbar_chrome::native_source(chrome.clone()));
    // Full-screen layer under the translucent top bar, so the bar picks up
    // the gradient tint behind it.
    let backdrop: Box<dyn Widget> = tree.desktop.active_workspace().map_or_else(
        || Box::new(desktop_backdrop::DesktopBackdrop::new()) as Box<dyn Widget>,
        |workspace| {
            let workspace_id = workspace.id.0.to_string();
            // Scratch scenes and captures host their own files and carry no
            // wallpaper source.
            let backdrop = if chrome.borrow().hosted_inputs.file_services().is_some() {
                desktop_backdrop::DesktopBackdrop::for_workspace(workspace_id)
            } else {
                desktop_backdrop::native_backdrop(workspace_id)
            };
            Box::new(backdrop) as Box<dyn Widget>
        },
    );
    tilo_desktop::composition::SurfaceChrome {
        navigation: tilo_desktop::composition::NavigationChrome::InSurface,
        // Window surfaces meet the strip boundary: moving apps inward to keep
        // a clipped shadow would leave a visible shell-owned gutter.
        work_area: tilo_desktop::composition::WorkArea::Flush { top: 12.0 },
        topbar: Box::new(topbar),
        backdrop,
        empty_workspace: Some(Box::new(
            tilo_desktop::launchpad::CollaborationLaunchpad::new(
                overlay_host::workspace_rail_clearance_for_viewport,
            ),
        )),
        // Compact top bars of the displays other than the primary.
        display_chrome: Some(Box::new(display_bars::DisplayBars::new(chrome.clone()))),
        // Reserves canvas-x [0, SIDEBAR_RESERVE_PX] for the wheel disc, which
        // lives in the overlay layer, so wheel and strip pan together.
        leading_inset: f64::from(tilo_desktop::sidebar_wheel_disc::SIDEBAR_RESERVE_PX),
        notify: None,
    }
}

/// The shared desktop surface for the tree's active workspace, plus the
/// per-strip row pointers the host's reconcilers harvest through. The host
/// rebuilds both together when the active workspace changes.
pub fn build_workspace_root(
    tree: &WorkspaceTree,
    records: &ui::record::RecordStore,
    chrome: &chrome_state::Chrome,
) -> (
    Box<dyn Widget>,
    std::collections::HashMap<state::StripId, *mut GroupRow>,
) {
    let surface_chrome = workspace_surface_chrome(tree, chrome);
    let state = chrome.borrow();
    let failures = state.record_creation_errors(tree);
    let mut resolver = NativeTileResolver {
        inputs: &state.hosted_inputs,
        failures: &failures,
    };
    let mut surface = Box::new(tilo_desktop::composition::DesktopSurface::build(
        &tree.desktop,
        records,
        &mut resolver,
        surface_chrome,
    ));
    for id in surface.strip_ids() {
        if let Some(row) = surface.row_mut(id) {
            for group in &mut row.groups {
                tab_documents::apply_creation_failures(group, &failures);
            }
        }
    }
    let rows = surface_row_ptrs(&mut surface);
    (surface, rows)
}

/// Stable addresses of the surface's strip rows. Each row is boxed inside
/// the surface, so its address holds until the surface is rebuilt; the host
/// replaces this map whenever that happens.
pub(crate) fn surface_row_ptrs(
    surface: &mut tilo_desktop::composition::DesktopSurface,
) -> std::collections::HashMap<state::StripId, *mut GroupRow> {
    surface
        .strip_ids()
        .into_iter()
        .filter_map(|id| surface.row_mut(id).map(|row| (id, std::ptr::from_mut(row))))
        .collect()
}

/// As [`build_tilo_overlay_host_with_handle`], but with the store the
/// open tabs' documents live in named by the caller.
///
/// # Panics
/// In unit tests, panics if the isolated Files fixture cannot be created.
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
pub fn build_tilo_overlay_host_with_records(
    tree_handle: state::WorkspaceTreeHandle,
    enable_peer_sync: bool,
    records: ui::record::RecordStore,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    #[cfg(test)]
    let inputs = hosted_inputs::HostedTileInputs::fixture().expect("test Files fixture");
    #[cfg(not(test))]
    let inputs = hosted_inputs::HostedTileInputs::default();
    build_tilo_overlay_host_with_inputs(tree_handle, enable_peer_sync, records, inputs)
}

/// Build the shell tree with host-provided app startup data, retained for tile rebuilds.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Keep the shared-tree construction signature aligned with the existing owned-handle public entry points."
)]
pub fn build_tilo_overlay_host_with_inputs(
    tree_handle: state::WorkspaceTreeHandle,
    enable_peer_sync: bool,
    records: ui::record::RecordStore,
    inputs: hosted_inputs::HostedTileInputs,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    let chrome = chrome_state::ChromeState::new_with_handle(tree_handle.clone());
    chrome.borrow_mut().hosted_inputs = inputs;
    chrome.borrow_mut().set_records(records);
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
    for room in tree.rooms() {
        launcher::set_trusted_identity_projection(room, &tree.trusted_identity_projection(room));
    }

    build_overlay_host_from_chrome(&tree, chrome, enable_peer_sync, false)
}

pub(crate) fn build_tilo_overlay_host_with_fixture_inputs(
    tree: state::WorkspaceTreeHandle,
    inputs: hosted_inputs::HostedTileInputs,
) -> Result<(overlay_host::ShellOverlayHost, chrome_state::Chrome), fixture_inputs::ShellFixtureError>
{
    fixture_inputs::validate(&tree.read(), &inputs)?;
    let snapshot = tree.read().clone();
    let chrome = chrome_state::ChromeState::new_with_fixture_handle(tree, inputs);
    Ok(build_overlay_host_from_chrome(
        &snapshot, chrome, false, true,
    ))
}

fn build_overlay_host_from_chrome(
    tree: &WorkspaceTree,
    chrome: chrome_state::Chrome,
    enable_peer_sync: bool,
    fixture: bool,
) -> (overlay_host::ShellOverlayHost, chrome_state::Chrome) {
    let records = chrome.borrow().records().clone();
    let (root, group_row_ptrs) = build_workspace_root(tree, &records, &chrome);
    // Each workspace is a spoke; dragging rotates them, releasing snaps to
    // the nearest one.
    let spokes: Vec<tilo_desktop::sidebar_wheel_disc::SpokeLabel> = tree
        .desktop
        .workspaces
        .iter()
        .map(|w| tilo_desktop::sidebar_wheel_disc::SpokeLabel {
            text: w.name.clone(),
            workspace_id: Some(w.id.0),
        })
        .collect();
    let active_idx = tree
        .desktop
        .workspaces
        .iter()
        .position(|w| w.id == tree.desktop.active)
        .unwrap_or(0);
    let sidebar: Box<dyn Widget> = Box::new(
        tilo_desktop::sidebar_wheel_disc::SidebarWheelDisc::new(spokes)
            .with_active_idx(active_idx)
            .with_host(std::rc::Rc::new(chrome_state::ChromeWheel(chrome.clone()))),
    );

    // The slot's top is pinned below the topbar so the sidebar's
    // click-guard doesn't overlap the overview-toggle icon. With padding
    // off on the outer Container/Column the topbar lands at y=0, so its
    // bottom is exactly `TOPBAR_HEIGHT_PX`.
    let sidebar_top = topbar_chrome::TOPBAR_HEIGHT_PX;
    let host = if fixture {
        overlay_host::ShellOverlayHost::with_fixture_content(root)
    } else {
        overlay_host::ShellOverlayHost::new(root)
    }
    .with_chrome(chrome.clone())
    .with_sidebar(sidebar)
    .with_sidebar_top(sidebar_top)
    .with_group_row_ptrs(group_row_ptrs)
    .with_content_workspace(tree.desktop.active.raw())
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
    if let Some(ws) = tree.desktop.active_workspace() {
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
    for c in &tree.desktop.collaborators {
        if let Some(hex) = &c.cursor_color {
            ctx.set_color_override(UserId(c.id.clone()), hex);
        }
    }
}

#[cfg(test)]
mod hosted_app_tests {
    use super::*;

    #[test]
    fn hosted_app_titles_round_trip_into_live_apps_or_pass_through() {
        /// Drag a tab into its own window and the group rebuilds from widgets,
        /// which knows only the tab's title — if the recovered label isn't
        /// what the hydration match arms name, the tab comes back as a
        /// "coming soon" card. This is the failure this table exists to prevent.
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
        fn an_unhosted_title_is_carried_through_unchanged() {
            assert_eq!(hosted_label_for_title("Inkscape"), "Inkscape");
            assert_eq!(hosted_label_for_title("report.odt"), "report.odt");
        }

        every_hosted_app_round_trips_from_its_tab_title_into_a_live_app();
        an_unhosted_title_is_carried_through_unchanged();
    }
}

#[cfg(test)]
mod legacy_files_opener_regressions {
    use super::*;

    struct ClearDocumentOpener;

    impl Drop for ClearDocumentOpener {
        fn drop(&mut self) {
            file_explorer::files_app::clear_document_opener();
        }
    }

    struct MultiClickInterval(std::time::Duration);

    impl MultiClickInterval {
        fn extend_for_fixture() -> Self {
            let previous = ui::gesture::multi_click_interval();
            ui::gesture::set_multi_click_interval(std::time::Duration::from_secs(60));
            Self(previous)
        }
    }

    impl Drop for MultiClickInterval {
        fn drop(&mut self) {
            ui::gesture::set_multi_click_interval(self.0);
        }
    }

    fn legacy_chrome(
        root: &std::path::Path,
        records: ui::record::RecordStore,
    ) -> chrome_state::Chrome {
        let tree = std::sync::Arc::new(parking_lot::RwLock::new(
            state::WorkspaceTree::bootstrap_demo(),
        ));
        let chrome = chrome_state::ChromeState::new_with_handle(tree);
        chrome.borrow_mut().hosted_inputs =
            hosted_inputs::HostedTileInputs::for_host_directory(root).expect("owned Files root");
        chrome.borrow_mut().set_records(records);
        install_document_opener(&chrome);
        chrome
    }

    fn find_semantic_bounds(
        node: &ui::headless::InspectionNode,
        name: &str,
    ) -> Option<vello::kurbo::Rect> {
        fn in_access(node: &ui::a11y::AccessNode, name: &str) -> Option<vello::kurbo::Rect> {
            if node.name.as_deref() == Some(name) {
                return node.bounds.map(ui::AccessBounds::to_rect);
            }
            node.children
                .iter()
                .find_map(|child| in_access(child, name))
        }

        node.semantics
            .as_ref()
            .and_then(|access| in_access(access, name))
            .or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| find_semantic_bounds(child, name))
            })
    }

    fn semantic_selected(node: &ui::headless::InspectionNode, name: &str) -> Option<bool> {
        fn in_access(node: &ui::a11y::AccessNode, name: &str) -> Option<bool> {
            if node.name.as_deref() == Some(name) {
                return Some(node.state.selected);
            }
            node.children
                .iter()
                .find_map(|child| in_access(child, name))
        }

        node.semantics
            .as_ref()
            .and_then(|access| in_access(access, name))
            .or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| semantic_selected(child, name))
            })
    }

    #[test]
    #[serial_test::serial]
    fn legacy_files_tile_activation_does_not_reenter_the_mounted_record_loan() {
        let directory = tempfile::tempdir().expect("temporary Files root");
        let document = directory.path().join("legacy.txt");
        std::fs::write(&document, "legacy contents\n").expect("fixture document");
        let document = document.canonicalize().expect("canonical fixture document");
        let records = ui::record::RecordStore::in_memory();
        let chrome = legacy_chrome(directory.path(), records.clone());
        let _clear = ClearDocumentOpener;
        let _multi_click_interval = MultiClickInterval::extend_for_fixture();

        let mut tile = ui::widget::TileHost::new(ui::widget::TileContent::App(Box::new(
            file_explorer::FileExplorerWidget::with_startup_inputs(file_explorer::StartupInputs {
                initial_dir: directory.path().to_path_buf(),
                workspace_roots: Vec::new(),
                record_roots: Vec::new(),
                remote_accounts: Vec::new(),
                trusted_humans: file_explorer::identity_directory::TrustedHumanDirectory::default(),
            }),
        )));
        let files_checkout = ui::record::RecordCheckout::new(
            ui::record::RecordId::for_tab(700),
            ui::record::RecordKind::new("files-view-state"),
        );
        tile.bind_or_create_record(records, files_checkout);
        let mut driver =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(tile), (1000, 700));
        let row = find_semantic_bounds(&driver.inspection().root, "legacy.txt")
            .expect("legacy File row reaches the TileHost semantic tree");
        assert!(row.area() > 0.0, "legacy File row has physical bounds");
        let center = row.center();

        driver.click_at(center.x, center.y);
        assert_eq!(
            semantic_selected(&driver.inspection().root, "legacy.txt"),
            Some(true),
            "the first physical click must select the mounted Files row before activation"
        );
        assert!(
            chrome
                .borrow()
                .hosted_inputs
                .native_app_for_path(&document)
                .is_some(),
            "the configured shell owner must recognize the selected native document"
        );
        driver.click_at(center.x, center.y);

        assert!(
            chrome
                .borrow()
                .tree()
                .native_document_tabs
                .values()
                .any(|reference| reference.path == document),
            "the legacy double-click opener must finish while TileHost retains the Files record loan"
        );
    }

    #[test]
    #[serial_test::serial]
    fn deferred_native_file_admission_preserves_existing_file_record_heads() {
        use automerge::transaction::Transactable;

        let directory = tempfile::tempdir().expect("temporary Files root");
        let document = directory.path().join("existing.txt");
        std::fs::write(&document, "existing contents\n").expect("fixture document");
        let document = document.canonicalize().expect("canonical fixture document");
        let mut owners = ui::record::RecordOwners::new(directory.path().join("profile"));
        let records = owners
            .open(SHELL_RECORDS_PACKAGE, "deferred-file-admission")
            .expect("canonical profile record store");
        let checkout = ui::record::RecordCheckout::new(
            ui::record::RecordId::for_file(&document),
            ui::record::RecordKind::new("text-document"),
        );
        records
            .create_record(&checkout)
            .expect("existing document record");
        let before = records
            .with(&checkout.id, |doc| {
                doc.put(automerge::ROOT, "body", "preserved existing body")
                    .expect("seed existing document");
                doc.get_heads()
            })
            .expect("existing document writable");
        let chrome = legacy_chrome(directory.path(), records.clone());
        let _clear = ClearDocumentOpener;

        open_document_from_files(&chrome, &document).expect("deferred native admission");

        assert_eq!(
            records
                .with(&checkout.id, automerge::AutoCommit::get_heads)
                .expect("existing document remains available"),
            before,
            "deferred admission must not replace or mutate an already-open canonical file record"
        );
    }
}
