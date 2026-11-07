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
        self.mutate().chunk_loader = Some(loader);
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
        inner.attach_document(chunk::FIRST, &record);
        drop(inner);
        self.changing().notify_all();
        Ok(())
    }

    /// Have the chunk `chunk`, if the store was not sent it: from the chunk loader. A chunk the
    /// manifest names not at all is one no write has started yet, which there is nothing to
    /// load for.
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
            return Err(StoreError::Backend(format!(
                "the chunk {chunk} cannot be loaded"
            )));
        };
        inner.attach_document(chunk, &doc);
        drop(inner);
        self.changing().notify_all();
        Ok(())
    }

    /// Have the chunk the resource `doc` is stored in, if the store was not sent it.
    pub(super) fn load_doc(&self, doc: &str) -> Result<(), StoreError> {
        self.load_record()?;
        let chunk = self.held().chunks.chunk(doc);
        self.load_chunk(chunk)
    }

    /// Have the chunk the resource at `at` is read and written in, if the store was not sent it:
    /// of an item, the chunk the index routes it to.
    pub(super) fn load_at(&self, at: &ResourceRef) -> Result<(), StoreError> {
        self.load_record()?;
        let chunk = {
            let inner = self.held();
            inner.at_chunk(at)?
        };
        self.load_chunk(chunk)
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
