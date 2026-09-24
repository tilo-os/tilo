//! Which app a tile is, and which record a tab is bound to.

use std::collections::BTreeMap;

use automerge::{AutoCommit, ObjId, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

use crate::app::AppId;
use crate::ids::{GroupId, StripId, TabId};
use crate::layout::TileContent;
use crate::projection::{ROOM_CONTENTS_KEY, contents_key, contents_key_id};

/// Key on a room document's tab entry naming the record the tab shows.
pub const TAB_RECORD_KEY: &str = "record_id";

/// The exact app a tile belongs to, as grants and launchers key it:
/// built-in kinds under [`crate::APP_ID_PREFIX`], every other app by its
/// own id. `None` for a device-local surface that belongs to no app.
pub fn canonical_app_id(tile: &TileContent) -> Option<AppId> {
    match tile {
        TileContent::WorkspaceActivity => None,
        TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => {
            Some(kind.app_id())
        }
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::PeerWindow { app_id, .. } => Some(app_id.clone()),
    }
}

/// Every tab in a room document that names the record it shows, as the
/// raw record id string. Parsing it is the reader's business.
pub fn record_routes(doc: &AutoCommit) -> BTreeMap<TabId, String> {
    let mut routes = BTreeMap::new();
    let Some((_, contents)) = doc.get(&ROOT, ROOM_CONTENTS_KEY).ok().flatten() else {
        return routes;
    };
    for strip in doc.keys(&contents) {
        let Some(groups) = child(doc, &contents, &strip).and_then(|s| child(doc, &s, "groups"))
        else {
            continue;
        };
        for group in doc.keys(&groups) {
            let Some(tabs) = child(doc, &groups, &group).and_then(|g| child(doc, &g, "tabs"))
            else {
                continue;
            };
            for tab in doc.keys(&tabs) {
                let (Some(id), Some(entry)) = (contents_key_id(&tab), child(doc, &tabs, &tab))
                else {
                    continue;
                };
                if let Some((Value::Scalar(value), _)) =
                    doc.get(&entry, TAB_RECORD_KEY).ok().flatten()
                    && let ScalarValue::Str(record) = value.as_ref()
                {
                    routes.insert(id, record.to_string());
                }
            }
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
