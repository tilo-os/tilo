//! A workspace folder's own history: the changes each of its files holds, as the rows
//! History lists them.
//!
//! A folder's writes are not actions. Every file in it has versions of its own, kept in
//! the tree record the folder is ([`fs_protocol::history`]), each signed by the person's
//! device and naming the agent session that wrote through it. This reads them all, one row
//! per file and writer, coalesced the way Files' own file history coalesces them, and names
//! each by the file's path as the tree names it now.

use std::collections::BTreeMap;

use automerge::AutoCommit;

use crate::history_source::AuthorDirectory;
use crate::history_source::WriterPlace;
use crate::history_source::file_changes::{
    NoTurnBoundary, QUIET, change_summary, coalesce, provenance_label, unknown_file_author,
};
use crate::time::{Duration, UNIX_EPOCH};
use crate::widget::history::{HistoryEntry, HistoryEntryId};

/// How many of a folder's changes [`entries`] lists at most: the newest. A folder can
/// hold more changes than one history pane shows, and the pane is what bounds it.
pub const LIMIT: usize = 500;

/// Every change the folder in `doc` holds, oldest first: one row per file and writer.
///
/// `authors` names the people and agents the versions record, so a row reads as a person
/// and the agent that wrote for them. A node under the trash is not at a path and is left
/// out, as it is out of the folder.
#[must_use]
pub fn entries(doc: &AutoCommit, authors: &AuthorDirectory) -> Vec<HistoryEntry> {
    let mut rows = records(doc, authors);
    // Keep the newest: the pane lists the most recent, and a long-lived folder holds more
    // than it shows.
    rows.sort_by_key(|row| (row.timestamp, row.id.0));
    if rows.len() > LIMIT {
        rows.drain(..rows.len().saturating_sub(LIMIT));
    }
    rows
}

/// Every change the folder in `doc` holds, in the order the tree lists its files: what
/// [`entries`] lists before it keeps the newest.
#[must_use]
pub fn records(doc: &AutoCommit, authors: &AuthorDirectory) -> Vec<HistoryEntry> {
    use fs_protocol::history;

    let Ok(tree) = <fs_protocol::tree::FilesTree as tilo_resource::Resource>::read(doc) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for (id, node) in &tree.nodes {
        if tree.trashed(id) {
            continue;
        }
        let Some(document_id) = node.document_id else {
            continue;
        };
        let path = fs_protocol::tree::path_of(&tree, id);
        let folder = match node.parent.as_deref() {
            Some(parent) if parent != fs_protocol::tree::ROOT_ID => {
                format!("/{}", fs_protocol::tree::path_of(&tree, parent))
            }
            _ => "/".to_owned(),
        };
        let versions: Vec<_> = history::record_versions(doc, document_id)
            .into_values()
            .collect();
        if versions.is_empty() {
            continue;
        }
        let attributed = history::record_attributions(doc, document_id);
        let attributions: BTreeMap<_, _> = attributed
            .iter()
            .map(|(version, attributed)| (*version, attributed.attribution.clone()))
            .collect();
        for change in coalesce(&versions, &attributions, QUIET, &NoTurnBoundary) {
            let version = change.last();
            let person = version.actor.person.as_bytes();
            let author = authors
                .resolve_attributed_file_actor_in(
                    person,
                    version.actor.device.as_bytes(),
                    attributions.get(&version.id),
                    WriterPlace {
                        folder: Some(&folder),
                    },
                )
                .unwrap_or_else(|| unknown_file_author(person));
            rows.push(HistoryEntry {
                id: HistoryEntryId(*version.id.0.as_bytes()),
                author,
                entry_kind: kind_of(&version.provenance),
                artifact_kind: collab_types::ArtifactKind::File,
                timestamp: UNIX_EPOCH
                    .checked_add(Duration::from_millis(version.timestamp_ms))
                    .unwrap_or(UNIX_EPOCH),
                summary: change_summary(&change, &path),
                text_delta: None,
                diff_payload: None,
                redaction: None,
                context: None,
                // What the change did and the file it did it to, as the row's own
                // elements: the file a link that opens it.
                target: Some(crate::widget::history::EntryTarget {
                    action: provenance_label(version.provenance.clone()),
                    label: path.clone(),
                    id: id.clone(),
                }),
            });
        }
    }
    rows
}

/// What a folder's change is, as History groups and marks it: what the file's newest
/// version in the change did to it.
fn kind_of(provenance: &fs_protocol::FileProvenance) -> crate::widget::EntryKind {
    use crate::widget::EntryKind;
    use fs_protocol::FileProvenance;
    match provenance {
        FileProvenance::Create | FileProvenance::Edit | FileProvenance::ConflictResolution => {
            EntryKind::Details
        }
        FileProvenance::Rename | FileProvenance::Move => EntryKind::Move,
        FileProvenance::Delete => EntryKind::Delete,
        FileProvenance::Restore { .. } => EntryKind::Restore,
    }
}

#[cfg(test)]
mod tests {
    use super::kind_of;
    use crate::widget::EntryKind;
    use fs_protocol::{FileProvenance, FileVersionId};

    /// A folder's change is listed as what it did to the file, so History groups a run of
    /// writes as edits, never as permission changes.
    #[test]
    fn a_folder_change_is_listed_as_what_it_did_to_the_file() {
        assert_eq!(kind_of(&FileProvenance::Create), EntryKind::Details);
        assert_eq!(kind_of(&FileProvenance::Edit), EntryKind::Details);
        assert_eq!(kind_of(&FileProvenance::Rename), EntryKind::Move);
        assert_eq!(kind_of(&FileProvenance::Delete), EntryKind::Delete);
        assert_eq!(
            kind_of(&FileProvenance::Restore {
                from: FileVersionId(uuid::Uuid::nil())
            }),
            EntryKind::Restore
        );
    }
}
