//! Opaque disk custody; actual signed manifest and owner-pin checks belong to the caller.
use super::staging::files::{directory_barrier, optional, unlink};
use super::staging::{disk_preflight, io, open_at, private, visit};
use super::*;
use std::{
    ffi::CStr,
    fs::File,
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    sync::MutexGuard,
};
const PAIR_BYTES: usize = 128 * 1024;

pub struct SnapshotDirectory {
    directory: File,
    key: DirectoryKey,
    inventory: HostWorkInventory,
}
pub struct SnapshotReservation {
    directory: SnapshotDirectory,
    pin: [u8; 16],
}
/// Original-pool read backing. The Vec dies before its real byte credit.
pub struct SidecarBytes {
    bytes: Vec<u8>,
    _credit: ByteCredit,
}
impl SidecarBytes {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
struct Name {
    bytes: [u8; 41],
    len: usize,
}
impl Name {
    fn new(pin: &[u8; 16], pending: bool) -> Self {
        let suffix = if pending {
            b".pending".as_slice()
        } else {
            b".pair".as_slice()
        };
        let mut value = Self {
            bytes: [0; 41],
            len: 32 + suffix.len(),
        };
        for (i, b) in pin.iter().enumerate() {
            value.bytes[2 * i] = b"0123456789abcdef"[(b >> 4) as usize];
            value.bytes[2 * i + 1] = b"0123456789abcdef"[(b & 15) as usize];
        }
        value.bytes[32..value.len].copy_from_slice(suffix);
        value
    }
    fn cstr(&self) -> &CStr {
        CStr::from_bytes_with_nul(&self.bytes[..=self.len])
            .expect("canonical bounded snapshot name")
    }
}
fn parse(name: &CStr) -> Result<([u8; 16], bool), HostWorkFailure> {
    let bytes = name.to_bytes();
    let (stem, pending) = if let Some(s) = bytes.strip_suffix(b".pending") {
        (s, true)
    } else if let Some(s) = bytes.strip_suffix(b".pair") {
        (s, false)
    } else {
        return Err(HostWorkFailure::Unavailable);
    };
    if stem.len() != 32 {
        return Err(HostWorkFailure::Unavailable);
    }
    fn digit(b: u8) -> Result<u8, HostWorkFailure> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(HostWorkFailure::Unavailable),
        }
    }
    let mut pin = [0; 16];
    for (i, b) in pin.iter_mut().enumerate() {
        *b = digit(stem[2 * i])? * 16 + digit(stem[2 * i + 1])?;
    }
    if pin == [0; 16] {
        return Err(HostWorkFailure::Unavailable);
    }
    Ok((pin, pending))
}
pub(super) fn open_root(root: &File) -> Result<File, HostWorkFailure> {
    let created = unsafe { libc::mkdirat(root.as_raw_fd(), c"snapshots-v1".as_ptr(), 0o700) } == 0;
    if !created && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
        return Err(HostWorkFailure::Unavailable);
    }
    let directory = open_at(root, c"snapshots-v1", libc::O_RDONLY | libc::O_DIRECTORY)?;
    private(&directory, true)?;
    // Also repair a prior mkdir + uncertain parent barrier on EEXIST retry.
    directory_barrier(root)?;
    Ok(directory)
}
/// Every recognized file, including an interrupted partial pending, retains a
/// full pair+pending reservation. Only the actual owner may decide it is stale.
pub(super) fn recover(
    directory: &File,
) -> Result<[Option<SnapshotRecord>; SNAPSHOT_SLOTS], HostWorkFailure> {
    let mut records: [Option<SnapshotRecord>; SNAPSHOT_SLOTS] = [None; SNAPSHOT_SLOTS];
    let mut kinds = [0u8; SNAPSHOT_SLOTS];
    let mut count = 0;
    visit(directory, |name| {
        count += 1;
        if count > 64 {
            return Err(HostWorkFailure::Capacity);
        }
        let (pin, pending) = parse(name)?;
        let file =
            optional(directory, name, libc::O_RDONLY)?.ok_or(HostWorkFailure::Unavailable)?;
        let len = file.metadata().map_err(io)?.len();
        if len > PAIR_BYTES as u64 || (!pending && len == 0) {
            return Err(HostWorkFailure::Unavailable);
        }
        let index = records
            .iter()
            .position(|r| r.is_some_and(|r| r.pin == pin))
            .or_else(|| records.iter().position(Option::is_none))
            .ok_or(HostWorkFailure::Capacity)?;
        let bit = if pending { 2 } else { 1 };
        if kinds[index] & bit != 0 {
            return Err(HostWorkFailure::Unavailable);
        }
        kinds[index] |= bit;
        records[index] = Some(SnapshotRecord { pin, leases: 0 });
        Ok(())
    })?;
    directory_barrier(directory)?;
    Ok(records)
}
impl crate::store::SessionStore {
    pub(crate) fn snapshot_directory(
        &self,
        inventory: &HostWorkInventory,
        workspace: u64,
    ) -> Result<SnapshotDirectory, HostWorkFailure> {
        if self.is_poisoned() {
            return Err(HostWorkFailure::Unavailable);
        }
        inventory.snapshot_directory(&self.directory, workspace)
    }
    pub(crate) fn matches_snapshot_reservation(
        &self,
        reservation: &SnapshotReservation,
        workspace: u64,
    ) -> Result<bool, HostWorkFailure> {
        if self.is_poisoned() {
            return Err(HostWorkFailure::Unavailable);
        }
        let metadata = self.directory.metadata().map_err(io)?;
        Ok(reservation.directory.key
            == DirectoryKey {
                device: metadata.dev(),
                inode: metadata.ino(),
                workspace,
            })
    }
}
impl HostWorkInventory {
    /// Reconcile both inert transport directories through the same admitted
    /// root handle before either kind of fresh disk reservation can be issued.
    pub fn snapshot_directory(
        &self,
        root: &File,
        workspace: u64,
    ) -> Result<SnapshotDirectory, HostWorkFailure> {
        let staging = self.staging_directory(root, workspace)?;
        drop(staging);
        let metadata = root.metadata().map_err(io)?;
        Ok(SnapshotDirectory {
            directory: open_root(root)?,
            key: DirectoryKey {
                device: metadata.dev(),
                inode: metadata.ino(),
                workspace,
            },
            inventory: self.clone(),
        })
    }
}
impl SnapshotDirectory {
    #[cfg(test)]
    pub(crate) fn with_barrier_fault<T>(&self, callback: impl FnOnce() -> T) -> T {
        super::staging::files::barrier_fault::with(&self.directory, callback)
    }
    /// Borrow custody for a record already present in the reconciled inventory.
    /// A lookup cannot create a disk reservation or establish pin authority.
    pub fn open_existing(
        &self,
        pin: [u8; 16],
    ) -> Result<Option<SnapshotReservation>, HostWorkFailure> {
        if pin == [0; 16] {
            return Err(HostWorkFailure::Conflict);
        }
        let mut state = self
            .inventory
            .inner()
            .state
            .lock()
            .map_err(|_| HostWorkFailure::Unavailable)?;
        let slot = state
            .slots
            .iter_mut()
            .find(|slot| slot.key == Some(self.key))
            .ok_or(HostWorkFailure::Unavailable)?;
        let Some(record) = slot.snapshots.iter_mut().flatten().find(|r| r.pin == pin) else {
            return Ok(None);
        };
        let leases = record
            .leases
            .checked_add(1)
            .ok_or(HostWorkFailure::Capacity)?;
        // A failed descriptor clone must leave the lease count unchanged.
        // Keep the inventory locked until the retained directory is ready.
        let directory = Self {
            directory: self.directory.try_clone().map_err(io)?,
            key: self.key,
            inventory: self.inventory.clone(),
        };
        record.leases = leases;
        Ok(Some(SnapshotReservation { directory, pin }))
    }
    /// Pins of the reconciled sidecar records without a live lease, for the
    /// owner's scoped sweep of expired or orphaned transfers. Grants nothing.
    pub fn idle_pins(&self) -> Result<Vec<[u8; 16]>, HostWorkFailure> {
        let state = self
            .inventory
            .inner()
            .state
            .lock()
            .map_err(|_| HostWorkFailure::Unavailable)?;
        let slot = state
            .slots
            .iter()
            .find(|slot| slot.key == Some(self.key))
            .ok_or(HostWorkFailure::Unavailable)?;
        Ok(slot
            .snapshots
            .iter()
            .flatten()
            .filter(|record| record.leases == 0)
            .map(|record| record.pin)
            .collect())
    }
    pub fn reserve(&self, pin: [u8; 16]) -> Result<SnapshotReservation, HostWorkFailure> {
        if pin == [0; 16] {
            return Err(HostWorkFailure::Conflict);
        }
        let directory = Self {
            directory: self.directory.try_clone().map_err(io)?,
            key: self.key,
            inventory: self.inventory.clone(),
        };
        let mut state = self
            .inventory
            .inner()
            .state
            .lock()
            .map_err(|_| HostWorkFailure::Unavailable)?;
        let index = state
            .slots
            .iter()
            .position(|s| s.key == Some(self.key))
            .ok_or(HostWorkFailure::Unavailable)?;
        if let Some(record) = state.slots[index]
            .snapshots
            .iter_mut()
            .flatten()
            .find(|r| r.pin == pin)
        {
            record.leases = record
                .leases
                .checked_add(1)
                .ok_or(HostWorkFailure::Capacity)?;
            return Ok(SnapshotReservation { directory, pin });
        }
        let position = state.slots[index]
            .snapshots
            .iter()
            .position(Option::is_none)
            .ok_or(HostWorkFailure::Capacity)?;
        let workspace = state.slots[index]
            .reserved_bytes()?
            .checked_add(SNAPSHOT_BYTES)
            .ok_or(HostWorkFailure::Capacity)?;
        let host = state
            .disk
            .checked_add(SNAPSHOT_BYTES)
            .ok_or(HostWorkFailure::Capacity)?;
        if workspace > WORKSPACE_STAGE_BYTES || host > HOST_STAGE_BYTES {
            return Err(HostWorkFailure::Capacity);
        }
        disk_preflight(&self.directory, host)?;
        state.slots[index].snapshots[position] = Some(SnapshotRecord { pin, leases: 1 });
        state.disk = host;
        Ok(SnapshotReservation { directory, pin })
    }
}
impl SnapshotReservation {
    fn position(&self, state: &InventoryState) -> Result<(usize, usize), HostWorkFailure> {
        let index = state
            .slots
            .iter()
            .position(|s| s.key == Some(self.directory.key))
            .ok_or(HostWorkFailure::Unavailable)?;
        let position = state.slots[index]
            .snapshots
            .iter()
            .position(|r| r.is_some_and(|r| r.pin == self.pin))
            .ok_or(HostWorkFailure::Unavailable)?;
        if state.slots[index].snapshots[position]
            .as_ref()
            .unwrap()
            .leases
            != 1
        {
            return Err(HostWorkFailure::Capacity);
        }
        Ok((index, position))
    }
    fn exclusive(&self) -> Result<MutexGuard<'_, InventoryState>, HostWorkFailure> {
        let state = self
            .directory
            .inventory
            .inner()
            .state
            .lock()
            .map_err(|_| HostWorkFailure::Unavailable)?;
        self.position(&state)?;
        Ok(state)
    }
    pub fn same_pool(&self, pool: &HostBytes) -> bool {
        self.directory.inventory.same_pool(pool)
    }
    /// Prepare a candidate only. This does not commit or authorize an owner pin.
    /// A failed write leaves an inert pending file and its full quota charge.
    pub fn prepare(&self, bytes: &[u8]) -> Result<(), HostWorkFailure> {
        if bytes.is_empty() || bytes.len() > PAIR_BYTES {
            return Err(HostWorkFailure::Capacity);
        }
        let state = self.exclusive()?;
        disk_preflight(&self.directory.directory, state.disk)?;
        let name = Name::new(&self.pin, true);
        if optional(&self.directory.directory, name.cstr(), libc::O_RDONLY)?.is_some() {
            return Err(HostWorkFailure::Conflict);
        }
        let mut file = optional(
            &self.directory.directory,
            name.cstr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        )?
        .ok_or(HostWorkFailure::Unavailable)?;
        file.write_all(bytes).map_err(io)?;
        file.sync_all().map_err(io)?;
        drop(file);
        directory_barrier(&self.directory.directory)
    }
    fn read(&self, pending: bool) -> Result<Option<SidecarBytes>, HostWorkFailure> {
        let _state = self.exclusive()?;
        // Re-establish durability even after a previous post-rename failure.
        directory_barrier(&self.directory.directory)?;
        let name = Name::new(&self.pin, pending);
        let Some(mut file) = optional(&self.directory.directory, name.cstr(), libc::O_RDONLY)?
        else {
            return Ok(None);
        };
        let len: usize = file
            .metadata()
            .map_err(io)?
            .len()
            .try_into()
            .map_err(|_| HostWorkFailure::Capacity)?;
        if len > PAIR_BYTES || (!pending && len == 0) {
            return Err(HostWorkFailure::Unavailable);
        }
        let credit = self
            .directory
            .inventory
            .inner()
            .pool
            .try_acquire_bulk(len)
            .ok_or(HostWorkFailure::Capacity)?;
        let mut bytes = vec![0; len];
        if bytes.capacity() != len {
            return Err(HostWorkFailure::Capacity);
        }
        file.read_exact(&mut bytes).map_err(io)?;
        if file.metadata().map_err(io)?.len() != len as u64 {
            return Err(HostWorkFailure::Conflict);
        }
        Ok(Some(SidecarBytes {
            bytes,
            _credit: credit,
        }))
    }
    pub fn read_current(&self) -> Result<Option<SidecarBytes>, HostWorkFailure> {
        self.read(false)
    }
    pub fn read_pending(&self) -> Result<Option<SidecarBytes>, HostWorkFailure> {
        self.read(true)
    }
    /// The caller must first establish the matching actual durable owner pin.
    pub fn finalize(&self) -> Result<(), HostWorkFailure> {
        let _state = self.exclusive()?;
        let pending = Name::new(&self.pin, true);
        let current = Name::new(&self.pin, false);
        let file = optional(&self.directory.directory, pending.cstr(), libc::O_RDONLY)?
            .ok_or(HostWorkFailure::Unavailable)?;
        let len = file.metadata().map_err(io)?.len();
        if len == 0 || len > PAIR_BYTES as u64 {
            return Err(HostWorkFailure::Unavailable);
        }
        // A failed prepare may have left a complete write before its fsync.
        // Synchronize the actual candidate again before promoting its name.
        file.sync_all().map_err(io)?;
        drop(file);
        if let Some(current_file) =
            optional(&self.directory.directory, current.cstr(), libc::O_RDONLY)?
        {
            let len = current_file.metadata().map_err(io)?.len();
            if len == 0 || len > PAIR_BYTES as u64 {
                return Err(HostWorkFailure::Unavailable);
            }
        }
        let fd = self.directory.directory.as_raw_fd();
        if unsafe { libc::renameat(fd, pending.cstr().as_ptr(), fd, current.cstr().as_ptr()) } != 0
        {
            return Err(HostWorkFailure::Unavailable);
        }
        directory_barrier(&self.directory.directory)
    }
    /// Only discard after the actual owner proves this candidate uncommitted.
    pub fn discard_pending(&self) -> Result<(), HostWorkFailure> {
        let _state = self.exclusive()?;
        let name = Name::new(&self.pin, true);
        // Refuse replacing/deleting an unrecognized special file.
        let _file = optional(&self.directory.directory, name.cstr(), libc::O_RDONLY)?;
        unlink(&self.directory.directory, name.cstr())?;
        directory_barrier(&self.directory.directory)
    }
    /// Release quota only after both names are removed and the directory synced.
    pub fn remove(self) -> Result<(), HostWorkFailure> {
        let mut state = self.exclusive()?;
        let (index, position) = self.position(&state)?;
        for pending in [false, true] {
            let name = Name::new(&self.pin, pending);
            let _file = optional(&self.directory.directory, name.cstr(), libc::O_RDONLY)?;
        }
        for pending in [false, true] {
            unlink(
                &self.directory.directory,
                Name::new(&self.pin, pending).cstr(),
            )?;
        }
        directory_barrier(&self.directory.directory)?;
        state.slots[index].snapshots[position] = None;
        state.disk = state
            .disk
            .checked_sub(SNAPSHOT_BYTES)
            .ok_or(HostWorkFailure::Unavailable)?;
        drop(state);
        Ok(())
    }
}
impl Drop for SnapshotReservation {
    fn drop(&mut self) {
        let mut state = self
            .directory
            .inventory
            .inner()
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(record) = state
            .slots
            .iter_mut()
            .find(|s| s.key == Some(self.directory.key))
            .and_then(|s| s.snapshots.iter_mut().flatten().find(|r| r.pin == self.pin))
        {
            record.leases = record.leases.saturating_sub(1);
        }
        // Physical files may survive cancellation. Their reservation does too.
    }
}
#[cfg(test)]
#[path = "snapshot/tests.rs"]
mod tests;
