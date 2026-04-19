//! Runtime tile/workspace tree for the nirios shell.
//!
//! The serializable, JSON-persisted shape lives in [`crate::shell::state`]:
//! it carries only symbolic placeholders (`TileContent::AppPlaceholder`,
//! `TileContent::Stub`) because live `Box<dyn AppWidget>` instances are not
//! serializable. At boot, the shell hydrates the persisted tree into the
//! runtime tree defined here, constructing the concrete `AppWidget` for each
//! `AppPlaceholder` and preserving `Stub` fallbacks verbatim.
//!
//! Introduced in Plan 3 Task 15 to wire real `quick-terminal` and
//! `file-explorer` widgets into the default demo workspace. The runtime tree
//! mirrors the field layout of [`crate::shell::state::WorkspaceTree`] so the
//! shell UI (chrome, strips, tabs) can treat both uniformly when it
//! introspects structure — only the `tile` variant differs.

use quick_ui::widget::{AppKind as UiAppKind, AppWidget};

use crate::shell::state::{GroupId, StripId, TabId, WindowWidth, WorkspaceId};

/// Content hosted inside a single tab's tile at runtime. Unlike the persisted
/// [`crate::shell::state::TileContent`], this enum carries live
/// `Box<dyn AppWidget>` handles for apps that have been successfully
/// constructed at boot.
///
/// Spec: `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` §5.
pub enum TileContent {
    /// A live in-process app hosted inside the tile. The trait object's
    /// `app_kind`/`app_title`/`app_icon` drive presentation and permission
    /// gating; the underlying `Widget` handles layout/draw/input.
    App(Box<dyn AppWidget>),
    /// A static placeholder rendered as a card — used both for "coming
    /// soon" apps (e.g. the browser) and as a fallback when an
    /// `AppWidget::new`/`spawn_default` call fails (spec §9a).
    Stub {
        kind: UiAppKind,
        label: String,
    },
}

impl TileContent {
    /// The UI-level app kind for this tile. For `App` variants this comes
    /// from the live `AppWidget`; for `Stub` variants it mirrors the
    /// placeholder's declared kind. Exposed so the shell chrome can filter
    /// tabs by kind without having to match on the variant at every call
    /// site.
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

/// A single tab inside a window-group. Carries the live
/// [`TileContent`] plus the identifiers used by the persisted state layer
/// so hydration and re-serialisation can reconcile runtime and disk forms.
pub struct Tab {
    pub id: TabId,
    pub title: String,
    pub tile: TileContent,
}

impl Tab {
    pub fn new(title: impl Into<String>, tile: TileContent) -> Self {
        Self {
            id: next_runtime_id(),
            title: title.into(),
            tile,
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
        let active_tab = tabs.first().map(|t| t.id).unwrap_or(0);
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
        let active = workspaces
            .first()
            .map(|w| w.id)
            .unwrap_or_else(WorkspaceId::new);
        Self { workspaces, active }
    }
}

/// Runtime-only id counter for tabs/groups. Starts at a high offset so it
/// never clashes with ids minted by `crate::shell::state::next_id` during
/// the same process lifetime (state's counter starts at 1; an offset well
/// above any realistic persisted value keeps them disjoint).
fn next_runtime_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1 << 32);
    N.fetch_add(1, Ordering::Relaxed)
}
