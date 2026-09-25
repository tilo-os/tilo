//! Single-ingress disk and large-decode inventory. No authority or native qualifier.
use crate::host_bytes::{ByteCredit, HostBytes};
use crate::staging_manifest::StageIdentity;
use std::sync::{Arc, Mutex};
const WORKSPACE_SLOTS: usize = 1024;
const STAGE_SLOTS: usize = 32;
const SNAPSHOT_SLOTS: usize = 32;
const SNAPSHOT_BYTES: u64 = 256 * 1024;
const HOST_STAGE_BYTES: u64 = 1024 * 1024 * 1024;
const WORKSPACE_STAGE_BYTES: u64 = 256 * 1024 * 1024;
#[derive(Clone, Copy, PartialEq, Eq)]
struct DirectoryKey {
    device: u64,
    inode: u64,
    workspace: u64,
}
#[derive(Clone, Copy)]
struct StageRecord {
    identity: StageIdentity,
    leases: usize,
}
#[derive(Clone, Copy)]
struct SnapshotRecord {
    pin: [u8; 16],
    leases: usize,
}
struct WorkspaceSlot {
    key: Option<DirectoryKey>,
    stages: [Option<StageRecord>; STAGE_SLOTS],
    snapshots: [Option<SnapshotRecord>; SNAPSHOT_SLOTS],
}
impl WorkspaceSlot {
    fn empty_records(&self) -> bool {
        self.stages.iter().all(Option::is_none) && self.snapshots.iter().all(Option::is_none)
    }
    fn has_live_leases(&self) -> bool {
        self.stages.iter().flatten().any(|r| r.leases > 0)
            || self.snapshots.iter().flatten().any(|r| r.leases > 0)
    }
    fn reserved_bytes(&self) -> Result<u64, HostWorkFailure> {
        self.stages
            .iter()
            .flatten()
            .try_fold(
                self.snapshots.iter().flatten().count() as u64 * SNAPSHOT_BYTES,
                |sum, r| sum.checked_add(r.identity.total),
            )
            .ok_or(HostWorkFailure::Capacity)
    }
    fn empty() -> Self {
        Self {
            key: None,
            stages: [None; STAGE_SLOTS],
            snapshots: [None; SNAPSHOT_SLOTS],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostWorkFailure {
    Capacity,
    Conflict,
    Unavailable,
}
impl std::fmt::Display for HostWorkFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Capacity => "host work resource capacity unavailable",
            Self::Conflict => "staging workspace or identity conflict",
            Self::Unavailable => "staging storage is unavailable or unrecognized",
        })
    }
}
impl std::error::Error for HostWorkFailure {}
/// All clones retain one original ingress allocation; no Weak or raw Arc escapes.
pub struct HostWorkInventory(Option<Arc<Inventory>>);
struct Inventory {
    state: Mutex<InventoryState>,
    pool: HostBytes,
    credit: Option<ByteCredit>,
}
struct InventoryState {
    large: bool,
    slots: Vec<WorkspaceSlot>,
    disk: u64,
}
pub struct LargeDecodeLease {
    inventory: HostWorkInventory,
}
impl HostWorkInventory {
    pub(crate) fn new(pool: &HostBytes) -> Result<Self, HostWorkFailure> {
        let arc = std::alloc::Layout::new::<[usize; 2]>()
            .extend(std::alloc::Layout::new::<Inventory>())
            .map_err(|_| HostWorkFailure::Capacity)?
            .0
            .pad_to_align()
            .size();
        let bytes = size_of::<WorkspaceSlot>()
            .checked_mul(WORKSPACE_SLOTS)
            .and_then(|n| n.checked_add(arc))
            .ok_or(HostWorkFailure::Capacity)?;
        let credit = pool
            .try_acquire_bulk(bytes)
            .ok_or(HostWorkFailure::Capacity)?;
        let mut slots = Vec::with_capacity(WORKSPACE_SLOTS);
        if slots.capacity() != WORKSPACE_SLOTS {
            return Err(HostWorkFailure::Capacity);
        }
        slots.resize_with(WORKSPACE_SLOTS, WorkspaceSlot::empty);
        Ok(Self(Some(Arc::new(Inventory {
            state: Mutex::new(InventoryState {
                large: false,
                slots,
                disk: 0,
            }),
            pool: pool.clone(),
            credit: Some(credit),
        }))))
    }
    fn inner(&self) -> &Inventory {
        self.0.as_ref().expect("live inventory")
    }
    pub fn same_pool(&self, pool: &HostBytes) -> bool {
        self.inner().pool.same_pool(pool)
    }
    pub fn try_large_decode(&self) -> Result<LargeDecodeLease, HostWorkFailure> {
        let mut state = self
            .inner()
            .state
            .lock()
            .map_err(|_| HostWorkFailure::Unavailable)?;
        if state.large {
            return Err(HostWorkFailure::Capacity);
        }
        state.large = true;
        Ok(LargeDecodeLease {
            inventory: self.clone(),
        })
    }
}
impl Clone for HostWorkInventory {
    fn clone(&self) -> Self {
        Self(Some(self.0.as_ref().expect("live inventory").clone()))
    }
}
impl Drop for HostWorkInventory {
    fn drop(&mut self) {
        if let Some(inventory) = Arc::into_inner(self.0.take().expect("live inventory")) {
            // Arc allocation has already been freed. Move real credit to stack
            // until all inventory state backing is destroyed.
            let Inventory {
                state,
                pool,
                credit,
            } = inventory;
            drop(state);
            drop(pool);
            drop(credit);
        }
    }
}
impl Drop for LargeDecodeLease {
    fn drop(&mut self) {
        self.inventory
            .inner()
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .large = false;
    }
}
#[cfg(test)]
#[path = "host_work/tests.rs"]
mod tests;

#[path = "host_work/staging.rs"]
mod staging;
pub use staging::{StagingDirectory, StagingReservation};

#[path = "host_work/snapshot.rs"]
mod snapshot;
pub use snapshot::{SidecarBytes, SnapshotDirectory, SnapshotReservation};
