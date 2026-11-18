//! The content versions of each file, held in the tree document: the content a device
//! keeps, frees and asks for is bound to one. A version records one content a file had:
//! its content hash, its metadata hash, and which versions it descends from. Versions are
//! keyed by [`FileDocumentId`], never by path, and live in the document of the chunk the
//! file's node is in, so they sync with the tree. What a change of a file was and who made
//! it is the tree's own History, read from its changes.

use std::collections::BTreeMap;

use automerge::{ObjType, Value};
use fs_protocol::{FileDocumentError, FileDocumentId, FileVersionId, FileVersionV1};
use thiserror::Error;

use super::{Namespace, Obj, hash_from_hex, read_u64, str_at};
use fs_protocol::TreeChunk;

pub(crate) use fs_protocol::history::{HISTORY_KEY, VERSIONS_KEY};

/// What a version's metadata digest covers: the facts a restore must find
/// unchanged besides the bytes themselves.
#[derive(serde::Serialize)]
pub(crate) struct VersionMetadataDigest {
    pub path: Option<String>,
    pub kind: u8,
    pub byte_len: u64,
    pub deleted: bool,
}

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error(transparent)]
    Document(#[from] automerge::AutomergeError),
    #[error(transparent)]
    Record(#[from] FileDocumentError),
    #[error(transparent)]
    Canonical(#[from] collab_types::CanonicalError),
    #[error("version id {0:?} is already bound to different content")]
    VersionIdCollision(FileVersionId),
    #[error("record {0:?} has no history")]
    UnknownDocument(FileDocumentId),
    #[error("the record has no version {0:?}")]
    UnknownVersion(FileVersionId),
    #[error("the record repository is at capacity: {0}")]
    CapacityExhausted(String),
    #[error("the version could not be published: {0}")]
    Publication(String),
}

fn object(value: Option<(Value<'_>, Obj)>) -> Option<Obj> {
    match value {
        Some((Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    }
}

impl Namespace {
    /// The document a record's history lives in: its node's chunk's.
    fn history_doc(&self, document_id: FileDocumentId) -> Option<&automerge::AutoCommit> {
        self.doc.doc(document_id.chunk()).map(|doc| &**doc)
    }

    /// The map of histories of chunk `chunk`, made (as every replica makes it) when its
    /// document has none. The tree's own record keeps the restores' receipts.
    pub(crate) fn ensure_history_root(&mut self, chunk: TreeChunk) -> Result<Obj, HistoryError> {
        if chunk != 0 {
            self.doc.chunk_or_new(chunk, true)?;
        }
        let doc = self
            .doc
            .doc_mut(chunk)
            .ok_or(HistoryError::Document(automerge::AutomergeError::Fail))?;
        let id = fs_protocol::tree::root_map(doc, HISTORY_KEY)?;
        Ok(Obj { chunk, id })
    }

    pub fn document_versions(
        &self,
        document_id: FileDocumentId,
    ) -> BTreeMap<FileVersionId, FileVersionV1> {
        self.history_doc(document_id)
            .map(|doc| fs_protocol::history::document_versions(doc, document_id))
            .unwrap_or_default()
    }

    fn history_slot(&mut self, document_id: FileDocumentId) -> Result<Obj, HistoryError> {
        let root = self.ensure_history_root(document_id.chunk())?;
        let key = document_id.0.to_string();
        if let Some(record) = object(self.doc.get(&root, &key)?) {
            return Ok(record);
        }
        Ok(self.doc.put_object(&root, key, ObjType::Map)?)
    }

    fn sub_map(&mut self, record: &Obj, key: &str) -> Result<Obj, HistoryError> {
        if let Some(map) = object(self.doc.get(record, key)?) {
            return Ok(map);
        }
        Ok(self.doc.put_object(record, key, ObjType::Map)?)
    }

    fn write_version(
        &mut self,
        document_id: FileDocumentId,
        version: &FileVersionV1,
    ) -> Result<(), HistoryError> {
        let record = self.history_slot(document_id)?;
        let versions = self.sub_map(&record, VERSIONS_KEY)?;
        let bytes = collab_types::canonical_bytes(version)?;
        self.doc.put(&versions, version.id.0.to_string(), bytes)?;
        Ok(())
    }

    pub fn document_history_heads(&self, document_id: FileDocumentId) -> Vec<FileVersionId> {
        fs_protocol::history::computed_heads(&self.document_versions(document_id))
            .into_iter()
            .collect()
    }

    pub fn document_version(
        &self,
        document_id: FileDocumentId,
        version_id: FileVersionId,
    ) -> (Option<FileVersionV1>, Vec<FileVersionId>) {
        let versions = self.document_versions(document_id);
        let heads = fs_protocol::history::computed_heads(&versions);
        (
            versions.get(&version_id).cloned(),
            heads.into_iter().collect(),
        )
    }

    /// Check a batch of versions against the record's history without writing it; the
    /// same checks [`Self::append_versions`] applies.
    pub fn check_versions(&self, versions: &[FileVersionV1]) -> Result<(), HistoryError> {
        let mut known: BTreeMap<FileDocumentId, BTreeMap<FileVersionId, FileVersionV1>> =
            BTreeMap::new();
        for version in versions {
            let current = known
                .entry(version.document_id)
                .or_insert_with(|| self.document_versions(version.document_id));
            check_version(current, version)?;
            current.insert(version.id, version.clone());
        }
        Ok(())
    }

    /// Append versions to their records. A version the record already holds is a
    /// replay and appends nothing.
    ///
    /// Versions arrive in any order: parents may reach this device later than the
    /// version itself, so a child that arrives first has no head until its parent does.
    pub fn append_versions(
        &mut self,
        versions: &[FileVersionV1],
    ) -> Result<Vec<bool>, HistoryError> {
        let mut known: BTreeMap<FileDocumentId, BTreeMap<FileVersionId, FileVersionV1>> =
            BTreeMap::new();
        let mut appended = Vec::with_capacity(versions.len());
        let mut accepted = Vec::new();
        for version in versions {
            let current = known
                .entry(version.document_id)
                .or_insert_with(|| self.document_versions(version.document_id));
            if check_version(current, version)? {
                current.insert(version.id, version.clone());
                accepted.push(version.clone());
                appended.push(true);
            } else {
                appended.push(false);
            }
        }
        for version in &accepted {
            self.write_version(version.document_id, version)?;
        }
        Ok(appended)
    }

    /// Append one version, returning whether the record gained it.
    pub fn append_version(&mut self, version: FileVersionV1) -> Result<bool, HistoryError> {
        Ok(self
            .append_versions(&[version])?
            .first()
            .copied()
            .unwrap_or(false))
    }

    /// `ino`'s content as a revision: the one the document records, or, for a file
    /// whose content was written without one, the version that wrote the bytes the
    /// node holds now. The answer is derived from the document, never installed beside it.
    pub fn observed_content_revision(&self, ino: u64) -> Option<fs_protocol::ContentRevisionV1> {
        if let Some(revision) = self.content_revision_for(ino) {
            return Some(revision);
        }
        let id = self.ino_to_id.get(&ino)?;
        let document_id = self.document_id_for_id(id)?;
        let obj = self.node(id)?;
        let content_hash = str_at(&self.doc, &obj, "hash")
            .and_then(|hash| hash_from_hex(&hash))
            .map(|hash| fs_protocol::ContentHash(*hash.as_bytes()));
        let byte_len = read_u64(&self.doc, &obj, "size");
        let versions = self.document_versions(document_id);
        let heads = fs_protocol::history::computed_heads(&versions);
        let version = versions
            .values()
            .filter(|version| version.content_hash == content_hash)
            .max_by_key(|version| {
                (
                    heads.contains(&version.id),
                    version.timestamp_ms,
                    version.id,
                )
            })?;
        Some(fs_protocol::ContentRevisionV1 {
            schema_version: fs_protocol::CONTENT_REVISION_VERSION,
            document_id,
            version_id: version.id,
            content_hash,
            byte_len,
            metadata_hash: version.metadata_hash,
        })
    }

    pub fn has_history(&self, document_id: FileDocumentId) -> bool {
        !self.document_versions(document_id).is_empty()
    }
}

fn check_version(
    current: &BTreeMap<FileVersionId, FileVersionV1>,
    version: &FileVersionV1,
) -> Result<bool, HistoryError> {
    version.validate()?;
    if let Some(existing) = current.get(&version.id) {
        if existing == version {
            return Ok(false);
        }
        return Err(HistoryError::VersionIdCollision(version.id));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
    use fs_protocol::{ContentHash, Ed25519PublicKey, FileActor, FileProvenance, FileVersionId};

    use super::*;

    fn actor() -> FileActor {
        FileActor {
            person: Ed25519PublicKey([1; 32]),
            device: Ed25519PublicKey([2; 32]),
        }
    }

    fn version(
        document_id: FileDocumentId,
        parents: Vec<FileVersionId>,
        provenance: FileProvenance,
        timestamp_ms: u64,
    ) -> FileVersionV1 {
        FileVersionV1 {
            id: FileVersionId::new(),
            document_id,
            parents,
            actor: actor(),
            timestamp_ms,
            content_hash: Some(ContentHash([3; 32])),
            metadata_hash: ContentHash([4; 32]),
            provenance,
        }
    }

    fn history() -> Namespace {
        Namespace::new("history").unwrap()
    }

    /// The versions `record` holds, by their id.
    fn held(namespace: &Namespace, record: FileDocumentId) -> Vec<FileVersionV1> {
        namespace.document_versions(record).into_values().collect()
    }

    fn by_id(mut versions: Vec<FileVersionV1>) -> Vec<FileVersionV1> {
        versions.sort_by_key(|version| version.id);
        versions
    }

    tilo_test_env::scenario! {
        fn the_version_history_keeps_every_version_once;

        phase versions_append_once_and_heads_follow_the_graph;
        {
            let mut namespace = history();
            let record = FileDocumentId::new();
            let root = version(record, vec![], FileProvenance::Create, 1);
            assert!(namespace.append_version(root.clone()).unwrap());
            assert!(
                !namespace.append_version(root.clone()).unwrap(),
                "exact replay"
            );
            let left = version(record, vec![root.id], FileProvenance::Edit, 2);
            let right = version(record, vec![root.id], FileProvenance::Edit, 3);
            assert_eq!(
                namespace
                    .append_versions(&[left.clone(), right.clone()])
                    .unwrap(),
                vec![true, true]
            );

            let snapshot = held(&namespace, record);
            assert_eq!(
                snapshot,
                by_id(vec![root.clone(), left.clone(), right.clone()])
            );
            let mut expected_heads = vec![left.id, right.id];
            expected_heads.sort();
            assert_eq!(
                namespace.document_history_heads(record),
                expected_heads,
                "a version no other supersedes heads the record"
            );

            // The same id bound to different content is refused, and the record
            // keeps what it had.
            let mut collision = version(record, vec![], FileProvenance::Edit, 9);
            collision.id = root.id;
            let error = namespace.append_version(collision).unwrap_err();
            assert!(
                matches!(error, HistoryError::VersionIdCollision(_)),
                "{error:?}"
            );
            assert_eq!(held(&namespace, record), snapshot);

            let other = FileDocumentId::new();
            let good = version(other, vec![], FileProvenance::Create, 1);
            let mut collision = version(other, vec![], FileProvenance::Edit, 2);
            collision.id = good.id;
            assert!(matches!(
                namespace.append_versions(&[good, collision]),
                Err(HistoryError::VersionIdCollision(_))
            ));
            assert!(held(&namespace, other).is_empty());
        }

        phase a_version_arriving_before_its_parent_is_kept;
        {
            let mut namespace = history();
            let record = FileDocumentId::new();
            let root = version(record, vec![], FileProvenance::Create, 1);
            let child = version(record, vec![root.id], FileProvenance::Edit, 2);
            assert!(namespace.append_version(child.clone()).unwrap());
            assert_eq!(namespace.document_history_heads(record), vec![child.id]);
            assert!(namespace.append_version(root.clone()).unwrap());
            assert_eq!(namespace.document_history_heads(record), vec![child.id]);
            assert_eq!(
                held(&namespace, record),
                by_id(vec![root, child])
            );
        }

        phase a_peer_that_takes_the_document_changes_has_the_versions;
        {
            let mut here = history();
            let record = FileDocumentId::new();
            let root = version(record, vec![], FileProvenance::Create, 1);
            let edit = version(record, vec![root.id], FileProvenance::Edit, 2);
            here.append_versions(&[root.clone(), edit.clone()]).unwrap();

            let mut there = history();
            there
                .apply_incremental(&here.changes_since(&[]))
                .expect("the versions travel as the document's changes");
            assert_eq!(
                held(&there, record),
                by_id(vec![root.clone(), edit.clone()])
            );
            assert_eq!(there.document_history_heads(record), vec![edit.id]);

            let (found, heads) = there.document_version(record, root.id);
            assert_eq!(found, Some(root));
            assert_eq!(heads, vec![edit.id]);
        }
    }
}
