//! Native live widget sidecars; topology lives only in qos-desktop.
use ui::widget::{AppKind as UiAppKind, AppWidget};

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
