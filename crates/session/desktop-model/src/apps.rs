//! Which app a tile is, and which record a tab is bound to.

use std::collections::BTreeMap;

use automerge::AutoCommit;

use crate::app::AppId;
use crate::ids::TabId;
use crate::layout::{Tab, TileContent};

/// The exact app a tile belongs to, as grants and launchers key it:
/// built-in kinds by their id, every other app by its own id.
pub fn canonical_app_id(tile: &TileContent) -> Option<AppId> {
    match tile {
        TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => {
            Some(kind.app_id())
        }
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::HostedWindow { app_id, .. } => Some(AppId::new(app_id)),
    }
}

/// Whether `tab` shows one thing of its app (a turn of a conversation, a note) rather than the
/// app as a whole: `view` names what it shows. Such a tab is the person's place of that thing,
/// and no window is ever bound into it, nor is it renamed after, or reused for, an app's window.
/// A tab whose view is the app's own default view is the app's place, and a window takes it, as
/// every open without a view does.
#[must_use]
pub fn shows_a_view(tab: &Tab) -> bool {
    matches!(&tab.tile, TileContent::LocalApp { .. }) && !tab.view.is_empty()
}

/// Whether the app a window announces is the app a request named: the id
/// itself, or the short name it ends with (`so.tilo.notes` is `notes`).
#[must_use]
pub fn app_ids_match(announced: &str, asked: &str) -> bool {
    let announced = announced.to_ascii_lowercase();
    let asked = asked.to_ascii_lowercase();
    announced == asked || asked.rsplit('.').next() == Some(announced.as_str())
}

/// Every window in a workspace document's layout that names the record it
/// shows, as the raw record id string. Parsing it is the reader's business.
pub fn document_routes(doc: &AutoCommit) -> BTreeMap<TabId, String> {
    crate::shared_resource::read(doc, None)
        .map(|desktop| desktop.records.into_iter().collect())
        .unwrap_or_default()
}

/// An application a desktop can open, as launchers and "Open \<App\>" actions
/// name it: its durable app id and display title.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenableApp {
    pub app_id: &'static str,
    pub title: &'static str,
}

/// The applications one host can open. Each data source (the browser's
/// bundled apps, a workspace host's checked record factories) implements it
/// once; everything that lists, validates or titles an open asks it.
pub trait AppCatalogue {
    fn openable(&self) -> Vec<OpenableApp>;

    /// The entry for `app`, when this host can open it.
    fn find(&self, app: &str) -> Option<OpenableApp> {
        self.openable()
            .into_iter()
            .find(|entry| entry.app_id == app)
    }
}

/// A host that opens nothing.
impl AppCatalogue for () {
    fn openable(&self) -> Vec<OpenableApp> {
        Vec::new()
    }
}
