//! The ACL-filtered catalogue of the tree's records, as the record-index projection serves it.
//!
//! A row is a document in the tree and the path it sits at now, kept only when the mount's
//! identity may see it. The catalogue is a projection of the namespace, not part of it: it is
//! built when a request reads it at the tree's and the ACL's current revisions, and reused
//! until either of them moves.
//!
//! Rebuilding it costs a walk of the whole tree (every row's path derived). So it is never
//! rebuilt per change: a folder copied in makes thousands of changes, and a walk per change
//! prices the copy by how full the workspace is instead of by the file. A change costs the
//! walk only when a request next asks for the projection, once however many changes landed
//! since. Everything below is private to [`QuickFS`]; the projection's xattrs live beside the
//! other mount projections in `fs.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fs_protocol::projection::{DOCUMENT_INDEX_PAGE_ENTRIES, DocumentIndexEntryV1};
use tilo_lock::MutexRecover as _;

use super::{AclVisibleDocumentIndex, QuickFS};
use crate::acl::{AclHandle, IdentityKey};
use crate::namespace::Namespace;

impl QuickFS {
    /// Rebuild the retained catalogue whenever the ACL is replaced, so a revocation or grant
    /// answers without waiting for a request. A namespace change needs no worker: the
    /// request that reads the projection builds it at the revision it finds.
    pub(super) fn start_document_index_acl_worker(&self) {
        let Some((acl, identity)) = self.enforcement.clone() else {
            return;
        };
        self.start_document_index_refresh_worker(acl.subscribe_revision(), (acl, identity));
    }

    fn start_document_index_refresh_worker(
        &self,
        mut generations: tokio::sync::watch::Receiver<u64>,
        enforcement: (AclHandle, IdentityKey),
    ) {
        let namespace = Arc::clone(&self.namespace);
        let retained = Arc::downgrade(&self.acl_visible_document_index);
        drop(self.hydrator.runtime_handle.spawn(async move {
            while generations.changed().await.is_ok() {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                while generations.has_changed().unwrap_or(false) {
                    let _ = generations.borrow_and_update();
                }
                let Some(retained) = retained.upgrade() else {
                    break;
                };
                // The refresh copies the catalogue and evaluates the ACL:
                // blocking work that must not occupy a runtime worker.
                let namespace = Arc::clone(&namespace);
                let enforcement = enforcement.clone();
                let refreshed = tokio::task::spawn_blocking(move || {
                    QuickFS::refresh_acl_visible_record_index_into(
                        &namespace,
                        Some(&enforcement),
                        &retained,
                    );
                })
                .await;
                if refreshed.is_err() {
                    break;
                }
            }
        }));
    }

    /// The namespace owns the record and path index. This only fingerprints its current
    /// CRDT heads plus the live ACL generation; it does not enumerate records. Domain and
    /// length framing prevent an ambiguous concatenation from producing an equivalent
    /// cursor generation.
    pub(super) fn document_catalog_revision(&self) -> String {
        let namespace = self.ns();
        let acl_revision = self
            .enforcement
            .as_ref()
            .map_or(0, |(acl, _)| acl.revision());
        Self::document_catalog_revision_from_index(
            namespace.indexed_document_revision(),
            acl_revision,
        )
    }

    fn document_catalog_revision_from_index(index_revision: &str, acl_revision: u64) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo.fs.record-index.v2.revision\\0");
        let index_revision = index_revision.as_bytes();
        hasher.update(&(index_revision.len() as u64).to_be_bytes());
        hasher.update(index_revision);
        hasher.update(b"acl-revision\\0");
        hasher.update(&acl_revision.to_be_bytes());
        hasher.finalize().to_hex().to_string()
    }

    /// The catalogue at the tree's and the ACL's current revisions, building it now when
    /// either moved since it was taken.
    ///
    /// A caller asking for this is reading the tree's documents page by page, so the walk
    /// is the work their pages add up to; it happens once per revision asked for, however
    /// many changes made that revision. A concurrent mutation that lands while it is built
    /// leaves the answer empty rather than serving a snapshot that is already old; the
    /// caller's next ask builds the next revision.
    pub(super) fn acl_visible_document_index(&self) -> Option<Arc<AclVisibleDocumentIndex>> {
        if let Some(index) = self.retained_acl_visible_document_index() {
            return Some(index);
        }
        // One build serves every request that arrives while the tree is at this revision;
        // the others wait for it and share what it published.
        {
            let _building = self.acl_index_build.lock_recover();
            Self::refresh_acl_visible_record_index_into(
                &self.namespace,
                self.enforcement.as_ref(),
                &self.acl_visible_document_index,
            );
        }
        self.retained_acl_visible_document_index()
    }

    /// The catalogue kept now, when it was taken at the tree's and the ACL's current
    /// revisions.
    fn retained_acl_visible_document_index(&self) -> Option<Arc<AclVisibleDocumentIndex>> {
        let source_revision = self.document_catalog_revision();
        let acl_revision = self
            .enforcement
            .as_ref()
            .map_or(0, |(acl, _)| acl.revision());
        self.acl_visible_document_index
            .lock_recover()
            .as_ref()
            .filter(|index| {
                index.source_revision == source_revision && index.acl_revision == acl_revision
            })
            .cloned()
    }

    /// Build the catalogue once, at mount, so the first projection request needs no walk.
    pub(super) fn refresh_acl_visible_document_index(&self) {
        Self::refresh_acl_visible_record_index_into(
            &self.namespace,
            self.enforcement.as_ref(),
            &self.acl_visible_document_index,
        );
    }

    /// Rebuild the retained view when the namespace catalogue or the ACL moved. The
    /// Namespace lock covers only copying the catalogue rows; the ACL is evaluated after it
    /// is released, and under an unchanged ACL revision only for rows whose path the
    /// previous view did not judge.
    fn refresh_acl_visible_record_index_into(
        namespace: &Arc<Mutex<Namespace>>,
        enforcement: Option<&(AclHandle, IdentityKey)>,
        retained: &Arc<Mutex<Option<Arc<AclVisibleDocumentIndex>>>>,
    ) {
        let acl_revision = enforcement.map_or(0, |(acl, _)| acl.revision());
        let previous = retained.lock_recover().clone();
        let (source_revision, rows) = {
            let namespace = namespace.lock_recover();
            let source_revision = Self::document_catalog_revision_from_index(
                namespace.indexed_document_revision(),
                acl_revision,
            );
            if previous.as_ref().is_some_and(|index| {
                index.source_revision == source_revision && index.acl_revision == acl_revision
            }) {
                return;
            }
            let mut rows = Vec::with_capacity(namespace.indexed_document_count());
            loop {
                let page = namespace.indexed_document_page(rows.len(), DOCUMENT_INDEX_PAGE_ENTRIES);
                if page.is_empty() {
                    break;
                }
                // Each row's path is derived here, where the tree is held, for the rows one
                // page shows; opening a workspace built no path at all.
                let paths: Vec<Option<String>> = page
                    .iter()
                    .map(|entry| namespace.path_for_document(entry))
                    .collect();
                rows.extend(page.into_iter().zip(paths));
            }
            drop(namespace);
            (source_revision, rows)
        };
        let reusable = previous.filter(|index| index.acl_revision == acl_revision);
        #[cfg(test)]
        let mut acl_evaluations = 0_usize;
        let mut hidden = HashMap::new();
        let mut records = Vec::with_capacity(rows.len());
        for (entry, path) in rows {
            let Some(path) = path else { continue };
            let visible = reusable
                .as_deref()
                .and_then(|index| index.verdict(entry.document_id, &path))
                .unwrap_or_else(|| {
                    #[cfg(test)]
                    {
                        acl_evaluations = acl_evaluations.saturating_add(1);
                    }
                    enforcement.is_none_or(|(acl, identity)| acl.visible(identity, &path))
                });
            if visible {
                records.push(DocumentIndexEntryV1 {
                    document_id: entry.document_id,
                    path,
                    is_dir: entry.is_dir,
                });
            } else {
                hidden.insert(entry.document_id, path);
            }
        }

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo.fs.record-index.v3.visible-catalogue\\0");
        hasher.update(&acl_revision.to_be_bytes());
        hasher.update(&(records.len() as u64).to_be_bytes());
        for record in &records {
            hasher.update(record.document_id.0.as_bytes());
            hasher.update(&(record.path.len() as u64).to_be_bytes());
            hasher.update(record.path.as_bytes());
            hasher.update(&[u8::from(record.is_dir)]);
        }
        let revision = hasher.finalize().to_hex().to_string();
        let by_id = records
            .iter()
            .cloned()
            .map(|record| (record.document_id, record))
            .collect();
        let index = Arc::new(AclVisibleDocumentIndex {
            source_revision: source_revision.clone(),
            acl_revision,
            revision,
            records,
            by_id,
            hidden,
            #[cfg(test)]
            acl_evaluations,
        });
        // Do not publish a coherent but old snapshot if a concurrent mutation landed after
        // releasing the namespace lock above. Its reader builds the next generation.
        let current_source = {
            let namespace = namespace.lock_recover();
            Self::document_catalog_revision_from_index(
                namespace.indexed_document_revision(),
                acl_revision,
            )
        };
        // Nor one whose verdicts may mix two ACL generations: the revision bump has its own
        // refresh coming.
        if current_source != source_revision
            || enforcement.map_or(0, |(acl, _)| acl.revision()) != acl_revision
        {
            return;
        }
        let mut retained = retained.lock_recover();
        *retained = Some(index);
    }
}
