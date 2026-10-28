//! Which app a tile is, and which record a tab is bound to.

use std::collections::BTreeMap;

use automerge::ReadDoc;

use crate::app::AppId;
use crate::ids::TabId;
use crate::layout::TileContent;

/// The exact app a tile belongs to, as grants and launchers key it:
/// built-in kinds by their package id, every other app by its
/// own id.
pub fn canonical_app_id(tile: &TileContent) -> Option<AppId> {
    match tile {
        TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => {
            Some(kind.app_id())
        }
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::PeerWindow { app_id, .. }
        | TileContent::HostedWindow { app_id, .. } => Some(app_id.clone()),
    }
}

/// Every window in a room document's layout that names the record it
/// shows, as the raw record id string. Parsing it is the reader's business.
pub fn record_routes(doc: &impl ReadDoc) -> BTreeMap<TabId, String> {
    crate::shared_record::read(doc, None)
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
