//! A workspace folder's own history: the changes each of its files holds, as the rows
//! History lists them.
//!
//! A folder's writes are not actions. Every file in it has versions of its own, kept in
//! the tree the folder is ([`fs_protocol::history`]), each signed by the person's device
//! and naming the agent session that wrote through it. A tree is kept in chunks, each a
//! record of its own ([`fs_protocol::tree`]): a file's versions live in the document of the
//! chunk its record id names, while its path comes from the whole tree. This reads them
//! all, one row per file and writer, coalesced the way Files' own file history coalesces
//! them, and names each by the file's path as the tree names it now.

use std::collections::BTreeMap;

use automerge::AutoCommit;
use fs_protocol::files::{FileDocumentId, TreeChunk};

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

/// The records a folder's versions are read from: the workspace's tree, its own record
/// (chunk 0) and each chunk of it.
pub trait FolderRecords {
    /// The whole tree as the records hold it, every chunk's nodes together: what names
    /// each file and walks its path. `None` when the tree cannot be read now.
    fn tree(&self) -> Option<fs_protocol::tree::Tree>;

    /// Read the document of chunk `chunk`, which holds the versions of the files whose
    /// record ids name it ([`FileDocumentId::chunk`]). `None` when it cannot be read now.
    fn read_chunk<T>(&self, chunk: TreeChunk, read: impl FnOnce(&AutoCommit) -> T) -> Option<T>;
}

/// Every change the folder `records` hold, oldest first: one row per file and writer.
///
/// `authors` names the people and agents the versions record, so a row reads as a person
/// and the agent that wrote for them. A node under the trash is not at a path and is left
/// out, as it is out of the folder.
#[must_use]
pub fn entries<R: FolderRecords>(records: &R, authors: &AuthorDirectory) -> Vec<HistoryEntry> {
    let mut rows = resources_of(records, authors);
    // Keep the newest: the pane lists the most recent, and a long-lived folder holds more
    // than it shows.
    rows.sort_by_key(|row| (row.timestamp, row.id.0));
    if rows.len() > LIMIT {
        rows.drain(..rows.len().saturating_sub(LIMIT));
    }
    rows
}

/// Every change one document's folder holds, oldest first: a tree whose nodes and their
/// versions are in that one document, as a tree was before it was chunked.
#[must_use]
pub fn entries_in(doc: &AutoCommit, authors: &AuthorDirectory) -> Vec<HistoryEntry> {
    entries(&OneDocument(doc), authors)
}

/// Every change the folder `records` hold, in the order the tree lists its files: what
/// [`entries`] lists before it keeps the newest.
#[must_use]
pub fn resources_of<R: FolderRecords>(records: &R, authors: &AuthorDirectory) -> Vec<HistoryEntry> {
    let Some(tree) = records.tree() else {
        return Vec::new();
    };
    // A file's versions are in the document of the chunk its record id names, so the nodes
    // are read one chunk's document at a time.
    let mut by_chunk: BTreeMap<TreeChunk, Vec<(&String, FileDocumentId)>> = BTreeMap::new();
    for (id, node) in &tree.nodes {
        if tree.trashed(id) {
            continue;
        }
        let Some(document_id) = node.document_id else {
            continue;
        };
        by_chunk
            .entry(document_id.chunk())
            .or_default()
            .push((id, document_id));
    }
    let mut rows = Vec::new();
    for (chunk, nodes) in by_chunk {
        let Some(()) = records.read_chunk(chunk, |doc| {
            for (id, document_id) in nodes {
                rows.extend(file_rows(doc, &tree, id, document_id, authors));
            }
        }) else {
            continue;
        };
    }
    rows
}

/// Every change one document's folder holds, in the order the tree lists its files: what
/// [`entries_in`] lists before it keeps the newest.
#[must_use]
pub fn records(doc: &AutoCommit, authors: &AuthorDirectory) -> Vec<HistoryEntry> {
    resources_of(&OneDocument(doc), authors)
}

/// Every change a folder kept in chunks holds, oldest first: `index` is the tree's own record
/// and `chunks` the documents of the chunks this device holds, each read on its own. Chunk
/// records are never merged into one document: one writer's changes to two chunks share its
/// actor, and one document cannot hold both.
#[must_use]
pub fn entries_in_chunks(
    index: &AutoCommit,
    chunks: &[(TreeChunk, AutoCommit)],
    authors: &AuthorDirectory,
) -> Vec<HistoryEntry> {
    entries(&Chunked { index, chunks }, authors)
}

/// A folder held as the tree's own record and the chunk documents beside it.
struct Chunked<'a> {
    index: &'a AutoCommit,
    chunks: &'a [(TreeChunk, AutoCommit)],
}

impl FolderRecords for Chunked<'_> {
    fn tree(&self) -> Option<fs_protocol::tree::Tree> {
        let mut tree = fs_protocol::tree::TreeChunks::default();
        tree.read_index(self.index);
        for (chunk, doc) in self.chunks {
            tree.read_chunk(*chunk, doc);
        }
        Some(tree.tree())
    }

    fn read_chunk<T>(&self, chunk: TreeChunk, read: impl FnOnce(&AutoCommit) -> T) -> Option<T> {
        self.chunks
            .iter()
            .find(|(held, _)| *held == chunk)
            .map(|(_, doc)| read(doc))
    }
}

/// A folder held in one document: the nodes and their versions together, as a tree was
/// before it was chunked.
struct OneDocument<'a>(&'a AutoCommit);

impl FolderRecords for OneDocument<'_> {
    fn tree(&self) -> Option<fs_protocol::tree::Tree> {
        <fs_protocol::tree::FilesTree as tilo_resource::Resource>::read(self.0).ok()
    }

    fn read_chunk<T>(&self, _chunk: TreeChunk, read: impl FnOnce(&AutoCommit) -> T) -> Option<T> {
        Some(read(self.0))
    }
}

/// The changes of the file `id` of `tree`, whose versions are in `doc`: one row per change
/// of it, coalesced as Files' own file history coalesces them.
fn file_rows(
    doc: &AutoCommit,
    tree: &fs_protocol::tree::Tree,
    id: &str,
    document_id: FileDocumentId,
    authors: &AuthorDirectory,
) -> Vec<HistoryEntry> {
    use fs_protocol::history;

    let Some(node) = tree.node(id) else {
        return Vec::new();
    };
    let path = fs_protocol::tree::path_of(tree, id);
    let folder = match node.parent.as_deref() {
        Some(parent) if parent != fs_protocol::tree::ROOT_ID => {
            format!("/{}", fs_protocol::tree::path_of(tree, parent))
        }
        _ => "/".to_owned(),
    };
    let versions: Vec<_> = history::document_versions(doc, document_id)
        .into_values()
        .collect();
    if versions.is_empty() {
        return Vec::new();
    }
    let attributed = history::record_attributions(doc, document_id);
    let attributions: BTreeMap<_, _> = attributed
        .iter()
        .map(|(version, attributed)| (*version, attributed.attribution.clone()))
        .collect();
    let mut rows = Vec::new();
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
            // What the change did and the file it did it to, as the row's own elements: the
            // file a link that opens it.
            target: Some(crate::widget::history::EntryTarget {
                action: provenance_label(version.provenance.clone()),
                label: path.clone(),
                id: id.to_owned(),
            }),
        });
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

    mod chunks {
        use std::collections::BTreeMap;

        use automerge::transaction::Transactable as _;
        use automerge::{AutoCommit, ObjType, ReadDoc as _, Value};
        use fs_protocol::files::{FileDocumentId, TreeChunk};
        use fs_protocol::tree::{Placing, ROOT_ID, Tree};
        use fs_protocol::{
            ContentHash, Ed25519PublicKey, FileActor, FileProvenance, FileVersionId, FileVersionV1,
        };

        use super::super::{FolderRecords, entries, resources_of};
        use crate::history_source::AuthorDirectory;

        /// A tree kept in two chunks, each a document of its own, as a page holds them.
        struct TwoChunks {
            chunks: BTreeMap<TreeChunk, AutoCommit>,
            tree: Tree,
        }

        impl FolderRecords for TwoChunks {
            fn tree(&self) -> Option<Tree> {
                Some(self.tree.clone())
            }

            fn read_chunk<T>(
                &self,
                chunk: TreeChunk,
                read: impl FnOnce(&AutoCommit) -> T,
            ) -> Option<T> {
                self.chunks.get(&chunk).map(read)
            }
        }

        /// Write `version` where the filesystem keeps it, in the document of its file's chunk.
        fn write_version(doc: &mut AutoCommit, version: &FileVersionV1) {
            let root = match doc.get(automerge::ROOT, "history") {
                Ok(Some((Value::Object(ObjType::Map), id))) => id,
                _ => doc
                    .put_object(automerge::ROOT, "history", ObjType::Map)
                    .unwrap(),
            };
            let record = match doc.get(&root, version.document_id.0.to_string()) {
                Ok(Some((Value::Object(ObjType::Map), id))) => id,
                _ => doc
                    .put_object(&root, version.document_id.0.to_string(), ObjType::Map)
                    .unwrap(),
            };
            let versions = match doc.get(&record, "versions") {
                Ok(Some((Value::Object(ObjType::Map), id))) => id,
                _ => doc.put_object(&record, "versions", ObjType::Map).unwrap(),
            };
            doc.put(
                &versions,
                version.id.0.to_string(),
                collab_types::canonical_bytes(version).unwrap(),
            )
            .unwrap();
            doc.commit();
        }

        fn version(record: FileDocumentId, id: u128, timestamp_ms: u64) -> FileVersionV1 {
            FileVersionV1 {
                id: FileVersionId(uuid::Uuid::from_u128(id)),
                document_id: record,
                parents: Vec::new(),
                actor: FileActor {
                    person: Ed25519PublicKey([7; 32]),
                    device: Ed25519PublicKey([9; 32]),
                },
                timestamp_ms,
                content_hash: None,
                metadata_hash: ContentHash([3; 32]),
                provenance: FileProvenance::Edit,
            }
        }

        /// A folder in the first chunk holding a file, and a file at the root of the second:
        /// each file's versions in its own chunk's document.
        fn two_chunks() -> TwoChunks {
            let mut first = AutoCommit::new();
            let mut second = AutoCommit::new();
            let mut tree = Tree::default();
            let docs = fs_protocol::tree::place(
                (&mut first, 1),
                &mut tree,
                ROOT_ID,
                "docs",
                Placing::Folder,
                (1, "ada"),
            )
            .unwrap();
            let hash = "a".repeat(64);
            let file = Placing::File {
                hash: &hash,
                size: 1,
            };
            let plan = fs_protocol::tree::place(
                (&mut second, 2),
                &mut tree,
                &docs.id,
                "plan.md",
                file,
                (2, "ada"),
            )
            .unwrap();
            let notes = fs_protocol::tree::place(
                (&mut second, 2),
                &mut tree,
                ROOT_ID,
                "notes.txt",
                file,
                (3, "ada"),
            )
            .unwrap();
            let plan_record = tree.node(&plan.id).and_then(|node| node.document_id).unwrap();
            write_version(&mut second, &version(plan_record, 1, 2_000));
            let notes_document = tree
                .node(&notes.id)
                .and_then(|node| node.document_id)
                .unwrap();
            write_version(&mut second, &version(notes_document, 2, 3_000));
            TwoChunks {
                chunks: [(1, first), (2, second)].into_iter().collect(),
                tree,
            }
        }

        /// A tree spread over two chunks lists every file's versions, each named by the path
        /// the whole tree gives it, whatever chunk its versions are in.
        #[test]
        fn a_folder_in_two_chunks_lists_each_files_versions_by_its_whole_path() {
            let two = two_chunks();
            let authors = AuthorDirectory::new();
            let summaries: Vec<String> = entries(&two, &authors)
                .into_iter()
                .map(|row| row.summary)
                .collect();
            assert_eq!(
                summaries,
                ["Edited docs/plan.md", "Edited notes.txt"],
                "oldest first, each file by its whole path"
            );
            assert_eq!(
                resources_of(&two, &authors).len(),
                2,
                "one row per file, read from its own chunk"
            );
        }

        /// One writer's changes to two chunks share its actor, so the two documents cannot be
        /// merged into one; the folder's rows are read from each beside the tree's own record.
        #[test]
        fn one_writers_chunks_are_read_side_by_side_not_merged() {
            let actor = automerge::ActorId::from(b"one writer".as_slice());
            let mut two = two_chunks();
            for doc in two.chunks.values_mut() {
                doc.set_actor(actor.clone());
                doc.put(automerge::ROOT, "touched", true).unwrap();
                doc.commit();
            }
            let mut index = AutoCommit::new().with_actor(actor);
            fs_protocol::tree::list_chunk(&mut index, 1).unwrap();
            fs_protocol::tree::list_chunk(&mut index, 2).unwrap();
            let chunks: Vec<(TreeChunk, AutoCommit)> = two.chunks.into_iter().collect();
            let mut merged = chunks[0].1.clone();
            assert!(
                merged.merge(&mut chunks[1].1.clone()).is_err(),
                "one actor's first change in each chunk clashes in one document"
            );
            let summaries: Vec<String> =
                super::super::entries_in_chunks(&index, &chunks, &AuthorDirectory::new())
                    .into_iter()
                    .map(|row| row.summary)
                    .collect();
            assert_eq!(summaries, ["Edited docs/plan.md", "Edited notes.txt"]);
        }
    }
}
