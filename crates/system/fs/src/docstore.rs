//! Content-addressed CRDT change-history storage + versioning for office documents.
//!
//! Office files use a canonical `tilo-doc` automerge document rather than an
//! opaque blob on each write; the document is persisted here as change history —
//! each change stored once as a content-addressed blob in [`ContentStore`],
//! and a *version* a small [`VersionManifest`] of change hashes chained to
//! the previous version (so a save appends only new changes, and dedups).
//! [`DocStore::checkout`] replays a manifest for exact time-travel;
//! [`DocStore::diff_versions`] runs [`tilo_doc::diff`] over two checkouts —
//! `.odt` and `.docx` diff the same way, sharing one model.

use anyhow::{Result, anyhow};
use automerge::{AutoCommit, Change};
use bytes::Bytes;
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};
use tilo_automerge::ApplyIncoming;

use crate::store::ContentStore;

/// A persisted version: ordered content hashes of its automerge changes, plus metadata and a link to the prior version. Tiny — 32 bytes/change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionManifest {
    /// Monotonic version index (0 = first commit).
    pub version: u64,
    /// Content (blake3/iroh) hashes of each automerge change, in causal order.
    pub change_hashes: Vec<[u8; 32]>,
    /// The automerge head change hashes at this version (hex) — the document's content identity, independent of storage layout.
    pub heads: Vec<String>,
    pub author: String,
    pub time_ms: i64,
    /// Content hash of the previous version's manifest (the version chain).
    pub parent: Option<[u8; 32]>,
}

impl VersionManifest {
    /// # Errors
    ///
    /// Returns an error if postcard serialization fails.
    pub fn encode(&self) -> Result<Vec<u8>> {
        Ok(postcard::to_allocvec(self)?)
    }

    /// # Errors
    ///
    /// Returns an error if `bytes` is not a valid postcard-encoded [`VersionManifest`].
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(postcard::from_bytes(bytes)?)
    }
}

/// A lightweight handle to one committed version (durable form is the content-addressed [`VersionManifest`]).
#[derive(Debug, Clone)]
pub struct VersionRef {
    pub version: u64,
    /// Content hash of this version's manifest — the durable, shareable version id.
    pub manifest_hash: Hash,
    pub heads: Vec<String>,
    pub time_ms: i64,
    pub author: String,
    pub change_count: usize,
}

/// Persists + versions office documents as CRDT change history over a [`ContentStore`], with an in-memory index of committed versions.
pub struct DocStore<'a> {
    store: &'a ContentStore,
    history: Vec<VersionRef>,
}

impl<'a> DocStore<'a> {
    pub fn new(store: &'a ContentStore) -> Self {
        Self {
            store,
            history: Vec::new(),
        }
    }

    /// The versions committed so far (oldest first).
    pub fn versions(&self) -> &[VersionRef] {
        &self.history
    }

    pub fn latest(&self) -> Option<&VersionRef> {
        self.history.last()
    }

    /// Persist the current state of `doc` as a new version: each automerge
    /// change is ingested as a content-addressed blob (already-stored changes dedup), chained to the previous version via a [`VersionManifest`].
    ///
    /// # Errors
    ///
    /// Returns an error if a change or the manifest fails to ingest, or the manifest fails to serialize.
    pub async fn commit(&mut self, doc: &mut AutoCommit, author: &str) -> Result<VersionRef> {
        let changes = doc.get_changes(&[]);
        let mut change_hashes = Vec::with_capacity(changes.len());
        for ch in &changes {
            let h = self
                .store
                .ingest(Bytes::copy_from_slice(ch.raw_bytes()))
                .await?;
            change_hashes.push(*h.as_bytes());
        }
        let heads: Vec<String> = doc
            .get_heads()
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        let parent = self.latest().map(|v| *v.manifest_hash.as_bytes());
        let version = self.history.len() as u64;
        let time_ms = tilo_clock::unix_now_ms_i64();
        let manifest = VersionManifest {
            version,
            change_hashes,
            heads: heads.clone(),
            author: author.to_string(),
            time_ms,
            parent,
        };
        let change_count = manifest.change_hashes.len();
        let manifest_hash = self.store.ingest(Bytes::from(manifest.encode()?)).await?;
        let vref = VersionRef {
            version,
            manifest_hash,
            heads,
            time_ms,
            author: author.to_string(),
            change_count,
        };
        self.history.push(vref.clone());
        Ok(vref)
    }

    /// Convenience: commit a `tilo_doc::Document` (the ergonomic office-doc handle).
    ///
    /// # Errors
    ///
    /// Returns an error if the document fails to reload as an automerge document, or [`Self::commit`] fails.
    pub async fn commit_document(
        &mut self,
        doc: &mut tilo_doc::Document,
        author: &str,
    ) -> Result<VersionRef> {
        let mut ac = AutoCommit::load(&doc.save()).map_err(|e| anyhow!("load document: {e}"))?;
        self.commit(&mut ac, author).await
    }

    /// Convenience: commit a `tilo_doc::Workbook` — also an automerge document, so `.xlsx`/`.ods` version the same way `.docx`/`.odt` do.
    ///
    /// # Errors
    ///
    /// Returns an error if the workbook fails to reload as an automerge document, or [`Self::commit`] fails.
    pub async fn commit_workbook(
        &mut self,
        wb: &mut tilo_doc::Workbook,
        author: &str,
    ) -> Result<VersionRef> {
        let mut ac = AutoCommit::load(&wb.save()).map_err(|e| anyhow!("load workbook: {e}"))?;
        self.commit(&mut ac, author).await
    }

    /// # Errors
    ///
    /// Returns an error if the manifest is not stored locally or fails to decode.
    pub async fn load_manifest(&self, manifest_hash: Hash) -> Result<VersionManifest> {
        let bytes = self
            .store
            .local(manifest_hash)
            .await
            .ok_or_else(|| anyhow!("version manifest not stored locally"))?;
        VersionManifest::decode(&bytes)
    }

    /// Reconstruct the document at a stored manifest by replaying its change blobs (exact time-travel).
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest or any of its change blobs can't be loaded, or a change is corrupt.
    pub async fn checkout(&self, manifest_hash: Hash) -> Result<AutoCommit> {
        let manifest = self.load_manifest(manifest_hash).await?;
        self.replay(&manifest).await
    }

    /// # Errors
    ///
    /// Returns an error if `version` is out of range, or [`Self::checkout`] fails.
    pub async fn checkout_version(&self, version: usize) -> Result<AutoCommit> {
        let vref = self
            .history
            .get(version)
            .ok_or_else(|| anyhow!("no such version {version}"))?;
        self.checkout(vref.manifest_hash).await
    }

    /// As [`Self::checkout_version`], wrapped into the ergonomic `tilo_doc::Document` handle.
    ///
    /// # Errors
    ///
    /// Returns an error if [`Self::checkout_version`] fails or the replayed document can't be rewrapped.
    pub async fn checkout_document(&self, version: usize) -> Result<tilo_doc::Document> {
        let mut ac = self.checkout_version(version).await?;
        let bytes = ac.save();
        tilo_doc::Document::load(&bytes).map_err(|e| anyhow!("rewrap document: {e}"))
    }

    /// As [`Self::checkout_version`], wrapped into the ergonomic `tilo_doc::Workbook` handle.
    ///
    /// # Errors
    ///
    /// Returns an error if [`Self::checkout_version`] fails or the replayed document can't be rewrapped.
    pub async fn checkout_workbook(&self, version: usize) -> Result<tilo_doc::Workbook> {
        let mut ac = self.checkout_version(version).await?;
        let bytes = ac.save();
        tilo_doc::Workbook::load(&bytes).map_err(|e| anyhow!("rewrap workbook: {e}"))
    }

    async fn replay(&self, manifest: &VersionManifest) -> Result<AutoCommit> {
        let mut changes = Vec::with_capacity(manifest.change_hashes.len());
        for h in &manifest.change_hashes {
            let bytes = self
                .store
                .local(Hash::from(*h))
                .await
                .ok_or_else(|| anyhow!("change blob missing from content store"))?;
            let ch = Change::from_bytes(bytes.to_vec())
                .map_err(|e| anyhow!("corrupt change blob: {e}"))?;
            changes.push(ch);
        }
        let mut doc = AutoCommit::new();
        doc.apply_incoming(changes)
            .map_err(|e| anyhow!("replay changes: {e}"))?;
        Ok(doc)
    }

    /// Structural diff between two committed versions — both share block identities, so the diff aligns by id (see [`tilo_doc::diff`]).
    ///
    /// # Errors
    ///
    /// Returns an error if either version fails to check out or open as a document.
    pub async fn diff_versions(&self, from: usize, to: usize) -> Result<tilo_doc::DocDiff> {
        let a = self.checkout_version(from).await?;
        let b = self.checkout_version(to).await?;
        let ca =
            tilo_doc::model::open(&a, &automerge::ROOT).map_err(|e| anyhow!("open old: {e}"))?;
        let cb =
            tilo_doc::model::open(&b, &automerge::ROOT).map_err(|e| anyhow!("open new: {e}"))?;
        let sa = tilo_doc::DocRef { doc: &a, c: &ca }.materialize();
        let sb = tilo_doc::DocRef { doc: &b, c: &cb }.materialize();
        Ok(tilo_doc::diff_snapshots(&sa, &sb))
    }

    /// Structural spreadsheet diff — sheets align by id, cells by A1 ref (see [`tilo_doc::diff_workbooks`]).
    ///
    /// # Errors
    ///
    /// Returns an error if either version fails to check out or open as a workbook.
    pub async fn diff_workbook_versions(
        &self,
        from: usize,
        to: usize,
    ) -> Result<tilo_doc::SheetDiff> {
        let a = self.checkout_version(from).await?;
        let b = self.checkout_version(to).await?;
        let ca = tilo_doc::sheet::model::open(&a, &automerge::ROOT)
            .map_err(|e| anyhow!("open old workbook: {e}"))?;
        let cb = tilo_doc::sheet::model::open(&b, &automerge::ROOT)
            .map_err(|e| anyhow!("open new workbook: {e}"))?;
        let sa = tilo_doc::sheet::model::materialize(&a, &ca);
        let sb = tilo_doc::sheet::model::materialize(&b, &cb);
        Ok(tilo_doc::diff_workbooks(&sa, &sb))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    async fn document_and_workbook_versions_commit_as_diffable_history_and_export() {
        // Phase `commits_versions_as_change_history_and_diffs`.
        // Import real `.docx` bytes as v0, edit as v1, then checkout + diff — proves reconstruction from content-addressed history.
        {
            let docx = super::super::tests::sample_docx();

            let mut doc = tilo_doc::Document::new();
            tilo_doc_codecs::provider_for_ext("docx")
                .unwrap()
                .import(
                    &docx,
                    &mut doc.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();

            let store = ContentStore::memory();
            let mut ds = DocStore::new(&store);

            let v0 = ds.commit_document(&mut doc, "alice").await.unwrap();
            assert!(
                v0.change_count >= 1,
                "a version records the CRDT change history, not a blob"
            );
            let m0 = ds.load_manifest(v0.manifest_hash).await.unwrap();
            assert!(m0.parent.is_none(), "first version has no parent");

            let hid = doc
                .blocks_in_order()
                .into_iter()
                .find(|id| doc.block_kind(id).as_deref() == Some("heading"))
                .unwrap();
            let hlen = doc.block_text(&hid).unwrap_or_default().chars().count();
            doc.splice_text(&hid, hlen, 0, " (DRAFT)");

            let v1 = ds.commit_document(&mut doc, "alice").await.unwrap();
            let m1 = ds.load_manifest(v1.manifest_hash).await.unwrap();
            assert_eq!(
                m1.parent,
                Some(*v0.manifest_hash.as_bytes()),
                "v1 chains to v0"
            );
            assert!(
                v1.change_count > v0.change_count,
                "the edit added new changes"
            );
            assert!(
                m1.change_hashes.starts_with(&m0.change_hashes),
                "unchanged history is reused, not re-saved"
            );

            let mut back0 = ds.checkout_version(0).await.unwrap();
            let c0 = tilo_doc::model::open(&back0, &automerge::ROOT).unwrap();
            let heading0 = tilo_doc::model::block_text(&back0, &c0, &hid).unwrap();
            assert_eq!(heading0, "Quarterly Report", "v0 has the original heading");
            let _ = back0.save();

            let back1 = ds.checkout_document(1).await.unwrap();
            assert!(
                back1
                    .materialize()
                    .blocks
                    .iter()
                    .any(|b| b.kind == "heading" && b.text == "Quarterly Report (DRAFT)"),
                "v1 has the edited heading"
            );

            let diff = ds.diff_versions(0, 1).await.unwrap();
            assert_eq!(
                diff.summary.changed, 1,
                "one block changed between versions"
            );
            assert_eq!(diff.summary.added, 0);
            assert_eq!(diff.summary.removed, 0);
            let ch = diff
                .blocks
                .iter()
                .find(|b| b.op == tilo_doc::BlockOp::Changed)
                .unwrap();
            assert_eq!(ch.new_text.as_deref(), Some("Quarterly Report (DRAFT)"));

            let v2 = ds.commit_document(&mut doc, "alice").await.unwrap();
            let m2 = ds.load_manifest(v2.manifest_hash).await.unwrap();
            assert_eq!(
                m2.change_hashes, m1.change_hashes,
                "no edit → identical change set"
            );
        }
        // Phase `checked_out_version_exports_back_to_docx`.
        // A version can be exported back to a valid `.docx` that carries the edit.
        {
            let docx = super::super::tests::sample_docx();
            let mut doc = tilo_doc::Document::new();
            tilo_doc_codecs::provider_for_ext("docx")
                .unwrap()
                .import(
                    &docx,
                    &mut doc.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();

            let store = ContentStore::memory();
            let mut ds = DocStore::new(&store);
            ds.commit_document(&mut doc, "bob").await.unwrap();

            let hid = doc
                .blocks_in_order()
                .into_iter()
                .find(|id| doc.block_kind(id).as_deref() == Some("heading"))
                .unwrap();
            let hlen = doc.block_text(&hid).unwrap_or_default().chars().count();
            doc.splice_text(&hid, hlen, 0, " v2");
            ds.commit_document(&mut doc, "bob").await.unwrap();

            let back = ds.checkout_document(1).await.unwrap();
            let out = tilo_doc_codecs::provider_for_ext("docx")
                .unwrap()
                .export(back.as_ref(), &tilo_content::NoContent)
                .unwrap();
            let mut reopened = tilo_doc::Document::new();
            tilo_doc_codecs::provider_for_ext("docx")
                .unwrap()
                .import(
                    &out,
                    &mut reopened.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            assert!(
                reopened
                    .materialize()
                    .blocks
                    .iter()
                    .any(|b| b.text == "Quarterly Report v2"),
                "exported version reopens with the edit"
            );
        }
        // Phase `commits_workbook_versions_as_change_history_and_diffs`.
        // A spreadsheet gets the same treatment as a text document: content-addressed, versioned CRDT history with exact reconstruction.
        {
            let mut wb = tilo_doc::Workbook::new();
            let s = wb.add_sheet("Sheet1");
            wb.set_cell(&s, "A1", tilo_doc::CellInput::number("10"));
            wb.set_cell(
                &s,
                "A2",
                tilo_doc::CellInput::formula("A1*2", "20", tilo_doc::CellType::Number),
            );

            let store = ContentStore::memory();
            let mut ds = DocStore::new(&store);

            let v0 = ds.commit_workbook(&mut wb, "alice").await.unwrap();
            assert!(
                v0.change_count >= 1,
                "a version records the CRDT change history, not a blob"
            );
            let m0 = ds.load_manifest(v0.manifest_hash).await.unwrap();
            assert!(m0.parent.is_none(), "first version has no parent");

            wb.set_cell(&s, "A1", tilo_doc::CellInput::number("15"));
            wb.set_cell(
                &s,
                "B1",
                tilo_doc::CellInput::formula("A1+A2", "35", tilo_doc::CellType::Number),
            );

            let v1 = ds.commit_workbook(&mut wb, "alice").await.unwrap();
            let m1 = ds.load_manifest(v1.manifest_hash).await.unwrap();
            assert_eq!(
                m1.parent,
                Some(*v0.manifest_hash.as_bytes()),
                "v1 chains to v0"
            );
            assert!(
                v1.change_count > v0.change_count,
                "the edit added new changes"
            );
            assert!(
                m1.change_hashes.starts_with(&m0.change_hashes),
                "unchanged history is reused, not re-saved"
            );

            let back0 = ds.checkout_workbook(0).await.unwrap();
            let snap0 = back0.materialize();
            let sheet0 = snap0.sheet_by_name("Sheet1").unwrap();
            assert_eq!(
                sheet0.cell("A1").unwrap().value,
                "10",
                "v0 has the original A1"
            );
            assert!(sheet0.cell("B1").is_none(), "v0 predates B1");

            let diff = ds.diff_workbook_versions(0, 1).await.unwrap();
            assert_eq!(diff.summary.cells_changed, 1, "one cell changed (A1)");
            assert_eq!(diff.summary.cells_added, 1, "one cell added (B1)");
            let sheet = diff.changed_sheets().next().unwrap();
            let a1 = sheet.cells.iter().find(|c| c.reference == "A1").unwrap();
            assert_eq!(a1.op, tilo_doc::CellOp::Changed);
            assert_eq!(a1.old.as_ref().unwrap().value, "10");
            assert_eq!(a1.new.as_ref().unwrap().value, "15");
            let b1 = sheet.cells.iter().find(|c| c.reference == "B1").unwrap();
            assert_eq!(b1.op, tilo_doc::CellOp::Added);
            assert_eq!(
                b1.new.as_ref().unwrap().formula.as_deref(),
                Some("A1+A2"),
                "added formula preserved in the diff"
            );
        }
    }

    /// Two peers co-edit the same file, merge, and converge; committing reuses the seed history (common prefix dedups) on both sides.
    #[tokio::test]
    async fn two_replicas_of_a_file_coedit_and_converge_incrementally() {
        let docx = super::super::tests::sample_docx();
        let mut base = tilo_doc::Document::new();
        tilo_doc_codecs::provider_for_ext("docx")
            .unwrap()
            .import(
                &docx,
                &mut base.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();

        let store = ContentStore::memory();
        let mut ds = DocStore::new(&store);

        let v0 = ds.commit_document(&mut base, "seed").await.unwrap();
        let m0 = ds.load_manifest(v0.manifest_hash).await.unwrap();

        let hid = base
            .blocks_in_order()
            .into_iter()
            .find(|id| base.block_kind(id).as_deref() == Some("heading"))
            .expect("seed document has a heading block");

        // Two peers fork and edit the SAME heading concurrently — A appends, B prepends.
        let mut peer_a = base.fork();
        let mut peer_b = base.fork();
        let hlen = peer_a.block_text(&hid).unwrap_or_default().chars().count();
        peer_a.splice_text(&hid, hlen, 0, " [A]");
        peer_b.splice_text(&hid, 0, 0, "[B] ");

        peer_a.merge(&mut peer_b);
        peer_b.merge(&mut peer_a);

        let text_a = peer_a.block_text(&hid).unwrap_or_default();
        let text_b = peer_b.block_text(&hid).unwrap_or_default();
        assert_eq!(text_a, text_b, "both replicas converge on the shared file");
        assert!(
            text_a.contains("[A]") && text_a.contains("[B]"),
            "the converged heading carries both concurrent edits: {text_a:?}"
        );

        let va = ds.commit_document(&mut peer_a, "alice").await.unwrap();
        let ma = ds.load_manifest(va.manifest_hash).await.unwrap();
        assert!(
            ma.change_hashes.starts_with(&m0.change_hashes),
            "the seed history is reused, not re-saved (incremental sync)"
        );
        assert!(
            va.change_count > v0.change_count,
            "the two concurrent edits added to the change history"
        );

        let vb = ds.commit_document(&mut peer_b, "bob").await.unwrap();
        let mb = ds.load_manifest(vb.manifest_hash).await.unwrap();
        let mut a_set = ma.change_hashes.clone();
        let mut b_set = mb.change_hashes;
        a_set.sort_unstable();
        b_set.sort_unstable();
        assert_eq!(
            a_set, b_set,
            "converged replicas store an identical set of content-addressed changes"
        );
    }
}
