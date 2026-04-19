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

/// Mutable chrome state shared across the shell widget tree.
#[derive(Debug)]
pub struct ChromeState {
    pub tree: WorkspaceTree,
    pub overlays: OverlayFlags,
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
            revision: 0,
        }))
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
