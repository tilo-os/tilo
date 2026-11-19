//! The ownership a store keeps beside a workspace, and what this session read of it.
//!
//! More writes than a workspace's state holds at its head wait in stretches the store keeps beside
//! it, each named by the hash of its bytes. Every pass asks what ownership still holds: a barrier
//! asks, a record's history asks, a publication asks. A stretch never changes once kept, so this
//! reads and checks each one once and answers every later question from what it read.

use super::WorkspaceId;
use super::publication::{MAX_RUN_BYTES, SpilledRun, StoredOutgoing};
use std::sync::Arc;

/// The store that keeps a workspace's ownership beside it, the workspace, and what it read of it
/// so far.
pub(crate) type Ownership<'a> = (&'a dyn super::Store, WorkspaceId, &'a Backlog);

/// What this session read of the ownership kept beside a workspace, by the hash of its bytes: such
/// a stretch of writes never changes once kept, so the store is read and checked for each one once,
/// and every later question about what ownership still holds costs a pointer.
#[derive(Default)]
pub(crate) struct Backlog(Reads);

/// The stretches read so far, by the hash of their bytes.
type Reads = std::sync::Mutex<std::collections::HashMap<[u8; 32], Arc<Vec<StoredOutgoing>>>>;

impl Backlog {
    /// The writes of `spilled`, read from the store unless read before.
    pub(crate) fn read(
        &self,
        store: &dyn super::Store,
        workspace: WorkspaceId,
        spilled: &SpilledRun,
    ) -> Result<Arc<Vec<StoredOutgoing>>, String> {
        let kept = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&spilled.hash)
            .filter(|kept| usize::try_from(spilled.writes).ok() == Some(kept.len()))
            .map(Arc::clone);
        if let Some(kept) = kept {
            return Ok(kept);
        }
        let writes = Arc::new(read_run(store, workspace, spilled)?);
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(spilled.hash, Arc::clone(&writes));
        Ok(writes)
    }

    /// Forget every stretch `kept` does not name.
    pub(crate) fn retain(&self, kept: &std::collections::BTreeSet<[u8; 32]>) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|hash, _| kept.contains(hash));
    }
}

/// The writes of `spilled`, read back from `store` and checked against what the record says of it.
fn read_run(
    store: &dyn super::Store,
    workspace: WorkspaceId,
    spilled: &SpilledRun,
) -> Result<Vec<StoredOutgoing>, String> {
    let bytes = store.read_ownership(workspace, spilled.hash)?;
    if bytes.len() > MAX_RUN_BYTES || *blake3::hash(&bytes).as_bytes() != spilled.hash {
        return Err("a kept stretch of ownership differs from what names it".into());
    }
    let writes: Vec<StoredOutgoing> = postcard::from_bytes(&bytes).map_err(|e| e.to_string())?;
    if usize::try_from(spilled.writes).ok() != Some(writes.len())
        || writes
            .iter()
            .any(|write| write.id != *blake3::hash(&write.bytes).as_bytes())
    {
        return Err("a kept stretch of ownership differs from what names it".into());
    }
    Ok(writes)
}
