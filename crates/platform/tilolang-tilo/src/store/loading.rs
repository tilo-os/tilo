//! How the store gets what it was not sent: the workspace record on a first write, and a chunk
//! when something reads into it (a row a window shows, or a lookup the index routes to it) or a
//! write is addressed to it. What a workspace opens with is its manifest, its index and the
//! chunks its window shows; the rest come this way.

use tilolang_runtime::store::StoreError;
use tilolang_runtime::value::ResourceRef;

use super::{ChunkLoader, Inner, Planned, TiloStore};
use crate::chunk::{self, ChunkNo};

impl TiloStore {
    /// How the store gets a chunk it was not sent: when something reads into it, or a write is
    /// addressed to it.
    pub fn set_chunk_loader(&self, loader: ChunkLoader) {
        let mut inner = self.mutate();
        // A new source may hold what the last one could not give: the chunks waited on are asked
        // for again.
        inner.waited.clear();
        inner.chunk_loader = Some(loader);
    }

    /// Have the workspace record, if the store opened without it: from the loader.
    pub(super) fn load_record(&self) -> Result<(), StoreError> {
        let loader = {
            let mut inner = self.changing();
            if inner.chunks.has_record() {
                return Ok(());
            }
            inner.loader.take()
        };
        let Some(mut loader) = loader else {
            return Err(StoreError::Backend(
                "the workspace record is not loaded".into(),
            ));
        };
        let record = loader();
        let mut inner = self.mutate();
        inner.loader = Some(loader);
        let Some(record) = record else {
            return Err(StoreError::Backend(
                "the workspace record cannot be loaded".into(),
            ));
        };
        let moved = inner.attach_document(chunk::FIRST, &record);
        drop(inner);
        if moved {
            self.changing().notify_all();
        }
        Ok(())
    }

    /// Have the chunk `chunk`, if the store was not sent it: from the chunk loader. A chunk the
    /// manifest names not at all is one no write has started yet, which there is nothing to
    /// load for.
    ///
    /// A chunk the source cannot give — one this device has not been sent yet, and no peer
    /// holding it is reachable — is asked for once, and waited on: the sync that brings it reads
    /// it then ([`Inner::attach_document`]), so the store neither polls for it nor asks its
    /// source again. What a person reads of it says it is not here yet, and what a window reads
    /// meanwhile is what the store holds, its counts not yet known
    /// ([`TiloStore::index_whole`]).
    pub(super) fn load_chunk(&self, chunk: ChunkNo) -> Result<(), StoreError> {
        if chunk == chunk::FIRST {
            return self.load_record();
        }
        let loader = {
            let mut inner = self.changing();
            if inner.chunks.mirror(chunk).is_some() {
                return Ok(());
            }
            // Which chunks exist is what the record's manifest says: without it there is no
            // telling a chunk it names from one a write will start.
            if !inner.chunks.has_record() {
                return Err(StoreError::Backend(
                    "the workspace record is not loaded".into(),
                ));
            }
            if !inner.chunks.placement().chunks().contains(&chunk) {
                return Ok(());
            }
            // Asked for once already, and not given: nothing of it is owed until the sync that
            // brings it ([`TiloStore::sync`]).
            if inner.waited.contains(&chunk) {
                return Err(StoreError::Backend(format!(
                    "the chunk {chunk} is not here yet"
                )));
            }
            inner.chunk_loader.take()
        };
        let Some(mut loader) = loader else {
            // Nothing to load it with: what reads into the chunk says so where it reads it, and
            // what writes there is the whole workspace of its own (see [`Inner::write_one`]).
            return Ok(());
        };
        let doc = loader(chunk);
        let mut inner = self.mutate();
        inner.chunk_loader = Some(loader);
        let Some(doc) = doc else {
            inner.waited.insert(chunk);
            return Err(StoreError::Backend(format!(
                "the chunk {chunk} cannot be loaded"
            )));
        };
        let moved = inner.attach_document(chunk, &doc);
        drop(inner);
        if moved {
            self.changing().notify_all();
        }
        Ok(())
    }

    /// Have the chunk the resource `doc` is stored in, if the store was not sent it.
    pub(super) fn load_doc(&self, doc: &str) -> Result<(), StoreError> {
        self.load_record()?;
        let chunk = self.held().chunks.chunk(doc);
        self.load_chunk(chunk)
    }

    /// Have the chunk the resource at `at` is read and written in, if the store was not sent it:
    /// of a record, the one the manifest places it in, and of an item, the one the index routes
    /// it to.
    ///
    /// An item the index names no chunk for — one no step has derived yet, and the chunks read
    /// so far hold none of — is in one of the chunks the manifest places, and those are read
    /// until one holds it. A window opens over the chunk its first rows are in and reads the
    /// rest as it reaches for them ([`Self::load_short`]), so what a person reaches for in one
    /// of those is read, never missing.
    pub(super) fn load_at(&self, at: &ResourceRef) -> Result<(), StoreError> {
        // A store that opened without its record reads what its indexes name: a resource no
        // index names is not one the workspace is known to hold, and reading the record for it
        // would read the whole workspace to answer nothing. A window shown before its record
        // shows what the index holds and reads the record for what it names.
        if !self.held().chunks.has_record()
            && self.held().apps.values().any(super::AppIndex::answers)
            && self.held().locate(at).is_err()
        {
            return Ok(());
        }
        self.load_record()?;
        let Some(item) = &at.item else {
            let chunk = self.held().chunks.chunk(&at.doc);
            return self.load_chunk(chunk);
        };
        let routed = self.held().routed_chunk(at);
        if let Some(chunk) = routed {
            return self.load_chunk(chunk);
        }
        // The chunks the record places that the store was not sent: an item is in one of them,
        // or in one it holds already, which is where its row stands.
        let missing: Vec<ChunkNo> = {
            let inner = self.held();
            if inner.holds_item(&at.doc, item) {
                return Ok(());
            }
            inner
                .chunks
                .placement()
                .chunks()
                .into_iter()
                .filter(|chunk| inner.chunks.mirror(*chunk).is_none())
                .collect()
        };
        for chunk in missing {
            self.load_chunk(chunk)?;
            if self.held().holds_item(&at.doc, item) {
                return Ok(());
            }
        }
        Err(StoreError::NotFound(format!("item `{item}`")))
    }
}

/// Whether writing `chunk` would start it empty over what the manifest says it holds: a store
/// that has no way to load it is the whole workspace of its own, and writes where its manifest
/// places; one that can is told to load it first, never to write over it.
pub(super) fn unloaded(inner: &Inner, chunk: ChunkNo) -> bool {
    inner.chunk_loader.is_some()
        && inner.chunks.mirror(chunk).is_none()
        && inner.chunks.placement().chunks().contains(&chunk)
}

/// The chunks the plan of a write names that the store was not sent: a collection's items go to
/// the chunk of its newest run, which the resource's own chunk need not be. A chunk the manifest
/// names not at all is one the write starts ([`unloaded`]): there is nothing to load, and the plan
/// it was made from stands.
pub(super) fn planned_unheld(inner: &Inner, planned: &Planned) -> Vec<ChunkNo> {
    let mut needed: Vec<ChunkNo> = Vec::new();
    for edit in &planned.edits {
        let chunk = edit
            .chunk()
            .unwrap_or_else(|| inner.chunks.chunk(edit.resource()));
        if unloaded(inner, chunk) && !needed.contains(&chunk) {
            needed.push(chunk);
        }
    }
    needed
}
