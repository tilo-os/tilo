//! Which app a tile is, and which record a tab is bound to.

use std::collections::BTreeMap;

use automerge::{AutoCommit, ObjId, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

use crate::app::AppId;
use crate::ids::{GroupId, StripId, TabId};
use crate::layout::TileContent;
use crate::projection::{ROOM_CONTENTS_KEY, contents_key, tab_entries_at};

/// Key on a room document's tab entry naming the record the tab shows.
pub const TAB_RECORD_KEY: &str = "record_id";

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

/// Every tab in a room document that names the record it shows, as the
/// raw record id string. Parsing it is the reader's business. Concurrently
/// created containers are joined, as [`crate::projection::contents_of`] does.
pub fn record_routes(doc: &AutoCommit) -> BTreeMap<TabId, String> {
    record_routes_at(doc, None)
}

/// [`record_routes`] as the document stood at `heads` (`None`: now): a host
/// showing a room's history reads which record each window showed then.
pub fn record_routes_at(
    doc: &AutoCommit,
    heads: Option<&[automerge::ChangeHash]>,
) -> BTreeMap<TabId, String> {
    let mut routes = BTreeMap::new();
    for (_, _, tab, entry) in tab_entries_at(doc, heads) {
        let held = match heads {
            Some(heads) => doc.get_at(&entry, TAB_RECORD_KEY, heads),
            None => doc.get(&entry, TAB_RECORD_KEY),
        };
        if let Some((Value::Scalar(value), _)) = held.ok().flatten()
            && let ScalarValue::Str(record) = value.as_ref()
        {
            routes.insert(tab, record.to_string());
        }
    }
    routes
}

/// Why a record could not be bound to a tab.
#[derive(Debug, thiserror::Error)]
pub enum BindRecordError {
    #[error("the room document has no entry for that tab")]
    MissingTab,
    #[error("the tab is already bound to a different record")]
    AlreadyBound,
    #[error(transparent)]
    Document(#[from] automerge::AutomergeError),
}

/// Bind `record` to the tab at `strip`/`group`/`tab` in a room document.
/// A binding is permanent: rebinding the same record is a no-op, a
/// different one is refused.
///
/// # Errors
///
/// [`BindRecordError::MissingTab`] when the document carries no such tab,
/// [`BindRecordError::AlreadyBound`] when it names another record, or the
/// document's own error when the write fails.
pub fn bind_tab_record(
    doc: &mut AutoCommit,
    strip: StripId,
    group: GroupId,
    tab: TabId,
    record: &str,
) -> Result<(), BindRecordError> {
    let mut object = ROOT;
    for key in [
        ROOM_CONTENTS_KEY.to_string(),
        contents_key(strip.raw()),
        "groups".into(),
        contents_key(group),
        "tabs".into(),
        contents_key(tab),
    ] {
        object = child(doc, &object, &key).ok_or(BindRecordError::MissingTab)?;
    }
    match doc.get(&object, TAB_RECORD_KEY)? {
        Some((Value::Scalar(value), _)) if matches!(value.as_ref(), ScalarValue::Str(current) if current.as_str() == record) => {
            Ok(())
        }
        Some(_) => Err(BindRecordError::AlreadyBound),
        None => {
            doc.put(&object, TAB_RECORD_KEY, record)?;
            Ok(())
        }
    }
}

fn child(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    doc.get(parent, key).ok().flatten().map(|(_, id)| id)
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
