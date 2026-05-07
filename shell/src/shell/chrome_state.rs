//! Shared chrome state for the nirios shell root.
//!
//! Chrome widgets (TopBarChrome, SidebarWheelChrome, ShellOverlayHost)
//! live at sibling positions in the widget tree and need to mutate the
//! same set of fields — overlay visibility flags and the workspace tree
//! itself. This module packages that state into one `Rc<RefCell<_>>`
//! handle so each widget can hold a clone and read/write without routing
//! every interaction through the tree's root.
//!
//! This is the shell's equivalent of the Elm-style `Model` that widgets
//! mutate via events; keeping it out of the Automerge doc is deliberate
//! — overlay visibility is local UI state that should not sync to peers,
//! and the WorkspaceTree already has its own JSON persistence layer.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use crate::shell::quick_settings::{BtDevice, QsLevel, WifiNetwork};
use crate::shell::state::{AppId, Capability, WorkspaceTree};
// Re-export WsTab so chrome users can refer to the tab routing enum
// without reaching into `workspace_settings`. The enum itself lives
// next to the modal widget; the chrome holds a copy of the active tab
// as a typed field — see `ChromeState::ws_settings_tab`.
pub use crate::shell::workspace_settings::WsTab;

/// Snapshot of which shell overlays are currently on screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OverlayFlags {
    pub overview: bool,
    pub quick_settings: bool,
    pub ws_settings: bool,
    /// Collaborator dropdown, opened by clicking the avatar stack in the
    /// top bar's right cluster. Lists every peer by name so invites and
    /// session handoff have a visible surface; pure UI state — the roster
    /// itself lives on `WorkspaceTree::collaborators`.
    pub collaborators: bool,
}

/// Global-search scope. Mirrors the concept's "context banner" scope
/// radios; defaults to `Workspace` so ⌘K results stay scoped to the
/// active workspace unless the user explicitly widens to everything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchScope {
    #[default]
    Workspace,
    All,
}

impl SearchScope {
    /// Short label shown inside the scope pill.
    pub fn label(self) -> &'static str {
        match self {
            SearchScope::Workspace => "Workspace",
            SearchScope::All => "All",
        }
    }
}

/// Quick-settings tray values. Local UI state; does not sync to peers —
/// device settings like brightness, volume, and radio toggles are always
/// per-device, never shared.
#[derive(Clone, Debug, PartialEq)]
pub struct QuickSettingsState {
    /// Brightness level, 0.0..=1.0.
    pub brightness: f32,
    /// Volume level, 0.0..=1.0.
    pub volume: f32,
    pub wifi: bool,
    pub bluetooth: bool,
    /// Navigation stack for the popover. The renderer dispatches on
    /// `nav_stack.last()` (always at least `Root`) — pushing `Wifi` or
    /// `Bluetooth` flips the popover body to the drill-in list.
    /// Concept ref: `QuickSettings.tsx:108-128, 101-162`.
    pub nav_stack: Vec<QsLevel>,
    /// Demo WiFi networks rendered in the WiFi drill-in list. Mock data
    /// until real scanning lands; concept ref: `QuickSettings.tsx:108-128`.
    pub wifi_networks: Vec<WifiNetwork>,
    /// Demo Bluetooth devices rendered in the BT drill-in list. Mock data
    /// until real pairing lands; concept ref: `QuickSettings.tsx:140-160`.
    pub bt_devices: Vec<BtDevice>,
}

impl Default for QuickSettingsState {
    fn default() -> Self {
        Self {
            brightness: 0.6,
            volume: 0.4,
            wifi: true,
            bluetooth: false,
            nav_stack: vec![QsLevel::Root],
            wifi_networks: vec![
                WifiNetwork {
                    ssid: "Home_Network".to_string(),
                    signal: 4,
                    secure: true,
                    connected: true,
                },
                WifiNetwork {
                    ssid: "Cafe_Guest".to_string(),
                    signal: 3,
                    secure: false,
                    connected: false,
                },
                WifiNetwork {
                    ssid: "Neighbour_5G".to_string(),
                    signal: 2,
                    secure: true,
                    connected: false,
                },
                WifiNetwork {
                    ssid: "FreeWiFi".to_string(),
                    signal: 1,
                    secure: false,
                    connected: false,
                },
            ],
            bt_devices: vec![
                BtDevice {
                    name: "AirPods Pro".to_string(),
                    battery: 80,
                    connected: true,
                },
                BtDevice {
                    name: "Magic Mouse".to_string(),
                    battery: 65,
                    connected: false,
                },
                BtDevice {
                    name: "Keychron K2".to_string(),
                    battery: 40,
                    connected: false,
                },
            ],
        }
    }
}

impl QuickSettingsState {
    /// Currently visible level — the renderer's dispatch key. Defaults to
    /// `Root` if the stack is somehow empty (`new()` always seeds it).
    pub fn current_level(&self) -> &QsLevel {
        self.nav_stack.last().unwrap_or(&QsLevel::Root)
    }

    /// Push a new level onto the nav stack. Used by the toggle-card
    /// chevron-affordance click in `draw_quick_settings`.
    pub fn push_level(&mut self, level: QsLevel) {
        self.nav_stack.push(level);
    }

    /// Pop the top level. No-op when only `Root` remains so the popover
    /// can't be emptied.
    pub fn pop_level(&mut self) {
        if self.nav_stack.len() > 1 {
            self.nav_stack.pop();
        }
    }
}

/// Mutable chrome state shared across the shell widget tree.
#[derive(Debug)]
pub struct ChromeState {
    pub tree: WorkspaceTree,
    pub overlays: OverlayFlags,
    pub quick_settings: QuickSettingsState,
    /// When `Some`, the workspace-settings modal's Name input is in text-
    /// entry mode. The string is the edit buffer (may diverge from the
    /// active workspace's persisted name until the user commits with
    /// Enter). Cleared on commit or Esc.
    pub editing_workspace_name: Option<String>,
    /// Active global-search scope. The overlay host paints a scope-pill
    /// banner above the palette, and clicking a pill rotates through the
    /// variants of [`SearchScope`].
    pub search_scope: SearchScope,
    /// Currently-active tab inside the Workspace Settings modal.
    /// Independent of [`Self::editing_workspace_name`] (which is only
    /// the name-input edit buffer). `Default::default()` is
    /// [`WsTab::General`]; the renderer dispatches its modal-body
    /// content on this field.
    ///
    /// Concept ref: `WorkspaceSettingsModal.tsx:50-95` — the tabs are
    /// independent UI state, never entangled with the name-edit buffer.
    pub ws_settings_tab: WsTab,
    /// Monotonic counter bumped every time the tree is mutated. Widgets
    /// poll this in `update` to re-sync their caches; cheaper than running
    /// diff logic and easier than an event bus.
    pub revision: u64,
    /// Queue of pending capability grants the user must respond to.
    /// Phase 4 Task 4.3 (the portal D-Bus backend) pushes onto the back
    /// of this queue when a sandboxed app calls one of the portal's
    /// request methods; the overlay host renders a [`PermissionPrompt`]
    /// for `front()` and drains the entry once the user clicks an
    /// action button. v1 stores pairs of `(app_id, capability)` —
    /// the capability's `scope` is set by the portal layer before
    /// pushing (typically `GrantScope::Once`, since the prompt
    /// itself escalates to `Session`/`Always` based on the user's
    /// click). Empty by default.
    pub pending_grants: VecDeque<(AppId, Capability)>,
}

impl ChromeState {
    pub fn new(tree: WorkspaceTree) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            tree,
            overlays: OverlayFlags::default(),
            quick_settings: QuickSettingsState::default(),
            editing_workspace_name: None,
            search_scope: SearchScope::default(),
            ws_settings_tab: WsTab::default(),
            revision: 0,
            pending_grants: VecDeque::new(),
        }))
    }

    /// Commit the current `editing_workspace_name` buffer (if any) to the
    /// active workspace's name and clear the buffer. Empty buffers are
    /// treated as a cancel — we never overwrite the name with "".
    pub fn commit_workspace_name(&mut self) {
        if let Some(buf) = self.editing_workspace_name.take() {
            let trimmed = buf.trim();
            if !trimmed.is_empty() {
                if let Some(ws) = self.tree.active_workspace_mut() {
                    ws.name = trimmed.to_string();
                }
            }
            self.bump();
        }
    }

    pub fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Route a winit `WindowEvent::DroppedFile` payload into the active
    /// workspace's first strip's first window group via
    /// [`super::workspace_drop::apply_workspace_drop_from_path`]. Bumps
    /// the revision counter so widgets re-sync on the next frame.
    /// Returns the new tab id, or `None` if the path has no filename.
    pub fn handle_file_drop(&mut self, path: &std::path::Path) -> Option<u64> {
        let tab_id = super::workspace_drop::apply_workspace_drop_from_path(&mut self.tree, path);
        if tab_id.is_some() {
            self.bump();
        }
        tab_id
    }

    /// Append a new empty workspace to `tree.workspaces`, activate it,
    /// and bump the revision counter so widgets re-sync on the next
    /// frame. Returns the new workspace's id.
    ///
    /// This is the plain "add workspace" entrypoint — the wheel-disc's
    /// "+" slot (`sidebar_wheel_disc::SidebarWheelDisc::plus_rect`) calls
    /// it directly on press. The concept's sidebar `+` button has the
    /// same semantics: it creates a fresh workspace and switches to it
    /// without opening any settings modal. Closes the concept's
    /// `## Known gaps` #4 ("Plus button") in
    /// `docs/nirios/wheel-disc-design.md`.
    pub fn add_workspace(&mut self, name: impl Into<String>) -> crate::shell::state::WorkspaceId {
        let id = self.tree.append_empty_workspace(&name.into());
        self.tree.set_active(id);
        self.bump();
        id
    }

    /// Switch to the workspace at the given direction (±1). Returns true
    /// if the active workspace changed.
    pub fn switch_workspace(&mut self, dir: i32) -> bool {
        let changed = match dir.signum() {
            -1 => self.tree.switch_up(),
            1 => self.tree.switch_down(),
            _ => false,
        };
        if changed {
            self.bump();
        }
        changed
    }
}

pub type Chrome = Rc<RefCell<ChromeState>>;
