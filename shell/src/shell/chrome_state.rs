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
use std::rc::Rc;

use crate::shell::state::WorkspaceTree;

/// Snapshot of which shell overlays are currently on screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OverlayFlags {
    pub overview: bool,
    pub quick_settings: bool,
    pub ws_settings: bool,
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuickSettingsState {
    /// Brightness level, 0.0..=1.0.
    pub brightness: f32,
    /// Volume level, 0.0..=1.0.
    pub volume: f32,
    pub wifi: bool,
    pub bluetooth: bool,
}

impl Default for QuickSettingsState {
    fn default() -> Self {
        Self {
            brightness: 0.6,
            volume: 0.4,
            wifi: true,
            bluetooth: false,
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
    /// Monotonic counter bumped every time the tree is mutated. Widgets
    /// poll this in `update` to re-sync their caches; cheaper than running
    /// diff logic and easier than an event bus.
    pub revision: u64,
}

impl ChromeState {
    pub fn new(tree: WorkspaceTree) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            tree,
            overlays: OverlayFlags::default(),
            quick_settings: QuickSettingsState::default(),
            editing_workspace_name: None,
            search_scope: SearchScope::default(),
            revision: 0,
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
