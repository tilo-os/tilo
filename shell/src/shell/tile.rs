//! Runtime tile/workspace tree for the nirios shell.
//!
//! The serializable, JSON-persisted shape lives in [`crate::shell::state`]:
//! it carries only symbolic placeholders, since live `Box<dyn AppWidget>`
//! instances aren't serializable. At boot the shell hydrates the persisted
//! tree into the runtime tree defined here, constructing the concrete
//! `AppWidget` for each placeholder. The two mirror each other's field
//! layout so shell UI can treat them uniformly — only `tile` differs.

use ui::widget::{AppKind as UiAppKind, AppWidget};

use crate::shell::state::{GroupId, StripId, TabId, WindowWidth, WorkspaceId};

/// Content hosted inside a single tab's tile at runtime. Unlike the
/// persisted [`crate::shell::state::TileContent`], this carries live
/// `Box<dyn AppWidget>` handles for apps constructed at boot.
pub enum TileContent {
    /// A live in-process app; `app_kind`/`app_title`/`app_icon` drive
    /// presentation and permission gating, the `Widget` handles the rest.
    App(Box<dyn AppWidget>),
    /// A static fallback card used when a live app cannot be constructed.
    Stub { kind: UiAppKind, label: String },
}

impl TileContent {
    /// The UI-level app kind, so chrome can filter tabs by kind without
    /// matching on the variant at every call site.
    pub fn kind(&self) -> UiAppKind {
        match self {
            TileContent::App(w) => w.app_kind(),
            TileContent::Stub { kind, .. } => *kind,
        }
    }
}

impl std::fmt::Debug for TileContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TileContent::App(w) => f
                .debug_struct("App")
                .field("kind", &w.app_kind())
                .field("title", &w.app_title())
                .finish(),
            TileContent::Stub { kind, label } => f
                .debug_struct("Stub")
                .field("kind", kind)
                .field("label", label)
                .finish(),
        }
    }
}

/// A single tab inside a window-group: the live [`TileContent`] plus the
/// identifiers the persisted state layer uses to reconcile runtime and
/// disk forms.
pub struct Tab {
    pub id: TabId,
    pub title: String,
    pub tile: TileContent,
}

impl Tab {
    pub fn new(title: impl Into<String>, tile_content: TileContent) -> Self {
        Self {
            id: next_runtime_id(),
            title: title.into(),
            tile: tile_content,
        }
    }
}

pub struct WindowGroup {
    pub id: GroupId,
    pub tabs: Vec<Tab>,
    pub active_tab: TabId,
    pub width: WindowWidth,
}

impl WindowGroup {
    pub fn with_tabs(tabs: Vec<Tab>, width: WindowWidth) -> Self {
        let active_tab = tabs.first().map_or(0, |t| t.id);
        Self {
            id: next_runtime_id(),
            tabs,
            active_tab,
            width,
        }
    }
}

pub struct Strip {
    pub id: StripId,
    pub name: String,
    pub window_groups: Vec<WindowGroup>,
}

impl Strip {
    pub fn new(name: impl Into<String>, window_groups: Vec<WindowGroup>) -> Self {
        Self {
            id: StripId::new(),
            name: name.into(),
            window_groups,
        }
    }
}

pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub strips: Vec<Strip>,
}

impl Workspace {
    pub fn new(name: impl Into<String>, strips: Vec<Strip>) -> Self {
        Self {
            id: WorkspaceId::new(),
            name: name.into(),
            strips,
        }
    }
}

pub struct WorkspaceTree {
    pub workspaces: Vec<Workspace>,
    pub active: WorkspaceId,
}

impl WorkspaceTree {
    pub fn new(workspaces: Vec<Workspace>) -> Self {
        let active = workspaces.first().map(|w| w.id).unwrap_or_default();
        Self { workspaces, active }
    }
}

/// Runtime-only id counter for tabs/groups, starting at a high offset so
/// it never clashes with `crate::shell::state::next_id` (which starts at 1).
fn next_runtime_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1 << 32);
    N.fetch_add(1, Ordering::Relaxed)
}
