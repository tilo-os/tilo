//! The daemon's one long-lived Store per state root.
//!
//! `serve` installs the Store it opened here and keeps it for the life of the
//! process. Every in-process component that asks [`crate::Store::open`] for
//! that root then borrows the same in-memory Store under this owner lock,
//! instead of re-reading, re-decoding and revalidating the aggregate from disk
//! each turn. Turns stay strictly serialized, exactly as the file lock
//! serialized them. The Store keeps holding the file lock, so another process
//! reaches the owner only through the daemon's control socket.
//!
//! A turn whose commit latched a failure leaves the Store poisoned. Its guard
//! then drops that Store (releasing the file lock) and the next turn reopens
//! from disk, which reconciles whatever did or did not reach it.
use crate::{Store, delivery::access::AccessError};
use anyhow::{Context, Result};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
};

enum Slot {
    /// Between turns.
    Idle(Box<Store>),
    /// Lent to one turn.
    Lent,
    /// The last turn poisoned its Store; the next turn reopens from disk.
    Reopen,
}

struct Cell {
    root: PathBuf,
    machine: String,
    slot: Mutex<Slot>,
    returned: Condvar,
    /// Set once the owning handle is gone: a returning turn drops its Store.
    retired: std::sync::atomic::AtomicBool,
    /// Set once the relay-retained room state has been merged since open.
    governance_reconciled: std::sync::atomic::AtomicBool,
}

static CELLS: Mutex<Vec<Arc<Cell>>> = Mutex::new(Vec::new());

fn find(root: &Path) -> Option<Arc<Cell>> {
    let root = std::fs::canonicalize(root).ok()?;
    CELLS
        .lock()
        .ok()?
        .iter()
        .find(|cell| cell.root == root)
        .cloned()
}

/// A Store for one turn: the daemon's long-lived Store, or one opened from
/// disk for this turn alone when no daemon owns the root in this process.
pub struct StoreGuard {
    store: Option<Box<Store>>,
    cell: Option<Arc<Cell>>,
}

impl StoreGuard {
    pub(crate) fn owned(store: Store) -> Self {
        Self {
            store: Some(Box::new(store)),
            cell: None,
        }
    }
    /// Keep this turn's Store in memory as `root`'s long-lived owner for the
    /// life of the returned handle; this guard becomes its first turn. A guard
    /// already lent by a long-lived owner returns that owner's handle.
    pub(crate) fn long_lived(&mut self) -> Result<Arc<Installed>> {
        if let Some(cell) = &self.cell {
            return Ok(Arc::new(Installed {
                cell: cell.clone(),
                owns: false,
            }));
        }
        let store = self.store.as_ref().context("store lent for this turn")?;
        let root = std::fs::canonicalize(&store.root).context("state root")?;
        let mut cells = CELLS
            .lock()
            .map_err(|_| anyhow::anyhow!("owner registry poisoned"))?;
        if let Some(existing) = cells.iter().find(|cell| cell.root == root) {
            anyhow::bail!(
                "state root {} already has a long-lived owner",
                existing.root.display()
            );
        }
        let cell = Arc::new(Cell {
            root,
            machine: store.state.machine_id.clone(),
            slot: Mutex::new(Slot::Lent),
            returned: Condvar::new(),
            retired: std::sync::atomic::AtomicBool::new(false),
            governance_reconciled: std::sync::atomic::AtomicBool::new(false),
        });
        cells.push(cell.clone());
        self.cell = Some(cell.clone());
        Ok(Arc::new(Installed { cell, owns: true }))
    }
}

impl From<Store> for StoreGuard {
    fn from(store: Store) -> Self {
        Self::owned(store)
    }
}

impl std::ops::Deref for StoreGuard {
    type Target = Store;
    fn deref(&self) -> &Store {
        self.store.as_deref().expect("store lent for this turn")
    }
}

impl std::ops::DerefMut for StoreGuard {
    fn deref_mut(&mut self) -> &mut Store {
        self.store.as_deref_mut().expect("store lent for this turn")
    }
}

impl Drop for StoreGuard {
    fn drop(&mut self) {
        let (Some(cell), Some(mut store)) = (self.cell.take(), self.store.take()) else {
            return;
        };
        let poisoned = store.failed_commit.get().is_some()
            || cell.retired.load(std::sync::atomic::Ordering::Acquire);
        store.end_turn();
        let Ok(mut slot) = cell.slot.lock() else {
            return;
        };
        *slot = if poisoned {
            drop(store);
            Slot::Reopen
        } else {
            Slot::Idle(store)
        };
        drop(slot);
        cell.returned.notify_one();
    }
}

/// Lend the long-lived Store of `root`, if this process owns one. `wait`
/// blocks until the current turn ends; otherwise a lent Store is `Busy`.
pub(crate) fn acquire(
    root: &Path,
    machine: &str,
    wait: bool,
) -> Option<std::result::Result<StoreGuard, AccessError>> {
    let cell = find(root)?;
    Some(lend(cell, machine, wait))
}

fn lend(
    cell: Arc<Cell>,
    machine: &str,
    wait: bool,
) -> std::result::Result<StoreGuard, AccessError> {
    if cell.machine != machine {
        return Err(AccessError::Denied(anyhow::anyhow!(
            "persisted machine namespace mismatch"
        )));
    }
    let mut slot = cell
        .slot
        .lock()
        .map_err(|_| AccessError::Denied(anyhow::anyhow!("owner lock poisoned")))?;
    loop {
        if cell.retired.load(std::sync::atomic::Ordering::Acquire) {
            return Err(AccessError::Denied(anyhow::anyhow!("cloud owner retired")));
        }
        match std::mem::replace(&mut *slot, Slot::Lent) {
            Slot::Idle(mut store) => {
                store.begin_turn();
                drop(slot);
                return Ok(StoreGuard {
                    store: Some(store),
                    cell: Some(cell),
                });
            }
            Slot::Reopen => {
                // Reopen under this slot's lock: the turn is ours.
                match Store::open_from_disk(&cell.root, &cell.machine) {
                    Ok(store) => {
                        drop(slot);
                        return Ok(StoreGuard {
                            store: Some(Box::new(store)),
                            cell: Some(cell),
                        });
                    }
                    Err(error) => {
                        *slot = Slot::Reopen;
                        return Err(AccessError::Denied(error));
                    }
                }
            }
            Slot::Lent => {
                if !wait {
                    return Err(AccessError::Busy);
                }
                slot = cell
                    .returned
                    .wait(slot)
                    .map_err(|_| AccessError::Denied(anyhow::anyhow!("owner lock poisoned")))?;
            }
        }
    }
}

/// Ownership of one root's long-lived Store. Dropping the owning handle
/// retires it: later opens read from disk again, and a turn still in flight
/// drops its Store when it ends.
pub(crate) struct Installed {
    cell: Arc<Cell>,
    owns: bool,
}

impl Installed {
    /// Lend the long-lived Store for one turn; `wait` blocks for the current
    /// turn, otherwise a lent Store is `Busy`.
    pub(crate) fn lend(&self, wait: bool) -> std::result::Result<StoreGuard, AccessError> {
        if self.cell.retired.load(std::sync::atomic::Ordering::Acquire) {
            return Err(AccessError::Denied(anyhow::anyhow!("cloud owner retired")));
        }
        lend(self.cell.clone(), &self.cell.machine, wait)
    }
}

impl Installed {
    /// Whether grants may be issued: this owner has merged everything the
    /// relay retains for the room since it opened. A state restored from an
    /// older copy while the host was stopped can still hold members, roles or
    /// invitations that were since revoked; the relay retains the newer,
    /// owner-signed governance and replays it before its first `Ready`.
    pub(crate) fn governance_reconciled(&self) -> bool {
        self.cell
            .governance_reconciled
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Whether `root`'s long-lived owner may issue grants (see
/// [`Installed::governance_reconciled`]). Without a long-lived owner in this
/// process nothing issues grants.
pub(crate) fn governance_reconciled(root: &Path) -> bool {
    find(root).is_some_and(|cell| {
        cell.governance_reconciled
            .load(std::sync::atomic::Ordering::Acquire)
    })
}

/// Record that the relay-retained room state has been merged: the host calls
/// this once its relay subscription reports `Ready`.
pub(crate) fn mark_governance_reconciled(root: &Path) {
    if let Some(cell) = find(root) {
        cell.governance_reconciled
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

impl Installed {
    /// Retire the long-lived Store: later opens read, reconcile and validate
    /// the persisted Store from disk again, and a turn in flight drops its
    /// Store when it ends. Only the owning handle can retire; idempotent.
    pub(crate) fn retire(&self) {
        if !self.owns {
            return;
        }
        self.cell
            .retired
            .store(true, std::sync::atomic::Ordering::Release);
        if let Ok(mut cells) = CELLS.lock() {
            cells.retain(|cell| !Arc::ptr_eq(cell, &self.cell));
        }
        if let Ok(mut slot) = self.cell.slot.lock()
            && matches!(*slot, Slot::Idle(_))
        {
            *slot = Slot::Reopen;
        }
        self.cell.returned.notify_all();
    }
}

impl Drop for Installed {
    fn drop(&mut self) {
        self.retire();
    }
}

/// Keep `root`'s Store in memory for the life of the returned handle.
pub(crate) fn install(root: &Path, machine: &str) -> Result<Arc<Installed>> {
    let mut guard = Store::open(root, machine)?;
    guard.long_lived()
}
