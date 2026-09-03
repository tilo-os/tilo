//! The `AppWidget` trait — the single contract between the nirios shell and
//! any in-process app hosted inside a tile. Extends the base `Widget` trait
//! with app-level metadata (kind, title, icon, permission hint, commands).
//!
//! `AppKind` is the tile-host enum re-exported from `tile_host`: one enum,
//! `Terminal, Files, Editor, Browser, Settings`.

use crate::widget::{AppKind, Widget};

/// A typed, device-local request from app content to shell chrome. Keeping
/// this on the common app contract lets both in-process tiles and future IPC
/// adapters use one destination without an app reaching into compositor
/// state or pretending a status toast completed the navigation.
#[derive(Debug, Clone, PartialEq)]
pub enum AppHostIntent {
    ShowWorkspaceActivity {
        resource_id: String,
        version_id: Option<[u8; 16]>,
        /// A factual app-native event that may not yet be part of the shell's
        /// CRDT record index. This makes the handoff exact instead of opening
        /// an empty resource lens while the durable indexes converge.
        entry: Option<crate::widget::HistoryEntry>,
    },
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

/// An [`AppCommand::invoke`] handler.
pub type AppCommandInvoke =
    Box<dyn Fn(&mut automerge::AutoCommit, &automerge::ObjId) + Send + Sync>;

/// A command palette entry contributed by an app. Invoked with a mutable
/// CRDT handle and the app's root `ObjId` so the command can mutate state
/// directly (matching `quick-ui`'s direct-mutation event model).
pub struct AppCommand {
    pub id: String,
    pub label: String,
    pub shortcut: Option<String>,
    pub invoke: AppCommandInvoke,
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

    /// Drain a one-shot request for shell-owned chrome.
    fn take_host_intent(&mut self) -> Option<AppHostIntent> {
        None
    }
}
