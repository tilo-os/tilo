//! Opening documents for a thread that must not wait on the disk.
//!
//! A thread that must not wait on storage ([`durable_store::waiting`], the
//! compositor's frame loop) never names a path itself. When it leases or
//! reads a key the store has not opened for it, the store's own thread does
//! every path operation an opener would (creating the directory, opening it
//! and its gate and lock files, checking they are the ones at those paths,
//! and reading the stored bytes, writer identity and load hint) and hands
//! the open descriptors and the bytes back. Until then the caller is told
//! [`SeamError::Opening`]; its wake ([`durable_store::waiting::wake`]) is
//! called once when the work is done, however often it asked, and it asks
//! again.
//!
//! The lease the caller then takes only locks those descriptors (`flock`
//! takes a descriptor, never waits here, and names no path), and its reads
//! are answered from the bytes read for it, kept current with its own writes.
//! Nothing is locked on the store's thread, so a preparation nobody takes
//! keeps no other writer out. A directory replaced between the preparation
//! and a write is still caught: every commit checks its owner's handles
//! against their paths first.

use std::path::Path;

use durable_store::{ReadyWake, StorageKey, StoreError as SeamError, Stored, WriteOp};

use super::{
    Files, Shared, Target, keep_hint, lock, read_hint_in, read_stored, seam_to_store, target_in,
};
use crate::{CANONICAL_GATE, LOCK_FILE, StoreError, io_err, open_directory, same_file};

/// What a caller is about to do with a key, which decides what is opened
/// for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Access {
    /// Read it without leasing it.
    Read,
    /// Become its only writer ([`DurableStore::lease`]).
    ///
    /// [`DurableStore::lease`]: durable_store::DurableStore::lease
    Exclusive,
    /// Join its shared writers ([`DurableStore::lease_shared`]).
    ///
    /// [`DurableStore::lease_shared`]: durable_store::DurableStore::lease_shared
    Shared,
}

/// A key's bytes as a reader sees them: what is stored, with the writes this
/// store has accepted for it applied, and the load hint kept beside it.
#[derive(Clone, Debug, Default)]
pub(super) struct Snapshot {
    pub(super) stored: Option<Stored>,
    pub(super) hint: Option<Vec<u8>>,
}

impl Snapshot {
    /// Follow a write this store accepted, as a later read would see it.
    pub(super) fn apply(&mut self, op: &WriteOp, writer: Option<&[u8]>) {
        let stored = self.stored.get_or_insert_with(Stored::default);
        op.apply(&mut stored.bytes);
        if let Some(writer) = writer {
            stored.writer = Some(writer.to_vec());
        }
    }
}

/// A finished preparation, waiting for its caller.
pub(super) struct Prepared {
    pub(super) access: Access,
    /// `None` for [`Access::Read`].
    pub(super) files: Option<Files>,
    pub(super) snapshot: Snapshot,
}

pub(super) enum Preparation {
    /// On its way; each waker is called once it is done.
    Queued(Access, Vec<ReadyWake>),
    Ready(Prepared),
    /// The work failed; reported once, to the next caller.
    Failed(String),
}

impl Shared {
    /// Take the preparation of `key` for `access`, or have the store's
    /// thread start it and answer [`SeamError::Opening`]. The caller has
    /// already refused what needs no disk (a key held here).
    pub(super) fn take_prepared(
        &self,
        key: &StorageKey,
        access: Access,
        wake: &ReadyWake,
    ) -> Result<Prepared, SeamError> {
        let mut state = lock(&self.state);
        match state.prepared.remove(key) {
            Some(Preparation::Ready(prepared)) if prepared.access == access => {
                return Ok(prepared);
            }
            Some(Preparation::Queued(queued, mut waiting)) if queued == access => {
                add_wake(&mut waiting, wake);
                state
                    .prepared
                    .insert(key.clone(), Preparation::Queued(queued, waiting));
                return Err(SeamError::Opening(key.to_string()));
            }
            Some(Preparation::Failed(message)) => {
                return Err(SeamError::Failed {
                    context: format!("opening {key}"),
                    message,
                });
            }
            // Nothing yet, or prepared for something else: (re)prepare.
            Some(Preparation::Ready(_) | Preparation::Queued(..)) | None => {}
        }
        state.prepared.insert(
            key.clone(),
            Preparation::Queued(access, vec![std::sync::Arc::clone(wake)]),
        );
        state.prepare_queue.push_back((key.clone(), access));
        drop(state);
        self.changed.notify_all();
        Err(SeamError::Opening(key.to_string()))
    }

    /// A read on the thread that declined waiting: the snapshot of a key
    /// leased here or prepared for it, else [`SeamError::Opening`] while the
    /// store's thread reads it. A snapshot prepared only to be read is
    /// handed over once.
    pub(super) fn read_without_waiting(
        &self,
        key: &StorageKey,
        wake: &ReadyWake,
    ) -> Result<Option<Stored>, SeamError> {
        let mut state = lock(&self.state);
        if let Some(snapshot) = state.cache.get(key) {
            return Ok(snapshot.stored.clone());
        }
        match state.prepared.get_mut(key) {
            Some(Preparation::Ready(prepared)) if prepared.access != Access::Read => {
                return Ok(prepared.snapshot.stored.clone());
            }
            Some(Preparation::Queued(_, waiting)) => {
                add_wake(waiting, wake);
                return Err(SeamError::Opening(key.to_string()));
            }
            _ => {}
        }
        drop(state);
        self.take_prepared(key, Access::Read, wake)
            .map(|prepared| prepared.snapshot.stored)
    }

    /// The load hint a reader on the thread that declined waiting may use:
    /// only one already read for it. A hint is an optimisation, so none is
    /// not a reason to wait.
    pub(super) fn hint_without_waiting(&self, key: &StorageKey) -> Option<Vec<u8>> {
        let state = lock(&self.state);
        if let Some(snapshot) = state.cache.get(key) {
            return snapshot.hint.clone();
        }
        match state.prepared.get(key) {
            Some(Preparation::Ready(prepared)) => prepared.snapshot.hint.clone(),
            _ => None,
        }
    }
}

/// Wait for a preparation with `wake` too, unless it already does: a thread
/// that asks again before the work is done is woken once.
fn add_wake(waiting: &mut Vec<ReadyWake>, wake: &ReadyWake) {
    if !waiting
        .iter()
        .any(|known| durable_store::waiting::same_wake(known, wake))
    {
        waiting.push(std::sync::Arc::clone(wake));
    }
}

/// On the store's thread: open what `access` to `key` needs and read what an
/// opener reads.
pub(super) fn prepare_in(
    shared: &Shared,
    key: &StorageKey,
    access: Access,
) -> Result<Prepared, StoreError> {
    #[cfg(test)]
    {
        let hold = lock(&shared.faults).prepare_hold.take();
        if let Some(hold) = hold {
            let _ = hold.recv();
        }
    }
    let target = target_in(&shared.root, key).map_err(seam_to_store)?;
    let files = match (&target, access) {
        (_, Access::Read) => None,
        (Target::Document(directory), Access::Exclusive | Access::Shared) => {
            Some(open_document(directory, access)?)
        }
        (Target::View(path), Access::Exclusive | Access::Shared) => Some(open_view(path)?),
    };
    let mut snapshot = read_snapshot(&target)?;
    // Writes accepted but not yet committed are part of what a reader sees.
    // Only this thread commits, so none is half-written while it reads.
    if let Some(batch) = lock(&shared.state).queue.get(key) {
        snapshot.apply(&batch.op, batch.writer.as_deref());
    }
    Ok(Prepared {
        access,
        files,
        snapshot,
    })
}

fn open_document(directory: &Path, access: Access) -> Result<Files, StoreError> {
    tilo_files::native::create_dir_all(directory).map_err(|e| {
        io_err(
            format!("creating state directory {}", directory.display()),
            e,
        )
    })?;
    let handle =
        open_directory(directory, false).map_err(|e| io_err("opening state directory", e))?;
    let gate_path = directory.join(CANONICAL_GATE);
    let gate = advisory_lock::open_lock_file(&gate_path)
        .map_err(|e| io_err(format!("opening gate {}", gate_path.display()), e))?;
    for (opened, path) in [(&handle, directory), (&gate, gate_path.as_path())] {
        if !same_file(opened, path) {
            return Err(StoreError::SourceChanged {
                path: path.to_path_buf(),
            });
        }
    }
    let writer_lock = match access {
        Access::Shared => {
            let path = directory.join(LOCK_FILE);
            Some(
                advisory_lock::open_lock_file(&path)
                    .map_err(|e| io_err(format!("opening {}", path.display()), e))?,
            )
        }
        Access::Exclusive | Access::Read => None,
    };
    Ok(Files {
        directory: Some((directory.to_path_buf(), handle)),
        gate,
        writer_lock,
    })
}

fn open_view(path: &Path) -> Result<Files, StoreError> {
    if let Some(parent) = path.parent() {
        tilo_files::native::create_dir_all(parent)
            .map_err(|e| io_err("creating view directory", e))?;
    }
    let gate_path = path.with_extension("lock");
    let gate = advisory_lock::open_lock_file(&gate_path)
        .map_err(|e| io_err(format!("opening gate {}", gate_path.display()), e))?;
    Ok(Files {
        directory: None,
        gate,
        writer_lock: None,
    })
}

fn read_snapshot(target: &Target) -> Result<Snapshot, StoreError> {
    let Some(stored) = read_stored(target).map_err(StoreError::from)? else {
        return Ok(Snapshot::default());
    };
    let hint = match target {
        Target::Document(directory) => read_hint_in(directory),
        Target::View(_) => None,
    };
    Ok(Snapshot {
        stored: Some(stored),
        hint,
    })
}

/// Hand a finished preparation to the callers waiting for it, unless they
/// asked for something else meanwhile (their own preparation is queued).
pub(super) fn finish_prepare(
    shared: &Shared,
    key: &StorageKey,
    access: Access,
    result: Result<Prepared, StoreError>,
) {
    let mut state = lock(&shared.state);
    let waiting = match state.prepared.remove(key) {
        Some(Preparation::Queued(queued, waiting)) if queued == access => {
            let outcome = match result {
                Ok(prepared) => Preparation::Ready(prepared),
                Err(error) => Preparation::Failed(error.to_string()),
            };
            state.prepared.insert(key.clone(), outcome);
            waiting
        }
        other => {
            if let Some(other) = other {
                state.prepared.insert(key.clone(), other);
            }
            Vec::new()
        }
    };
    drop(state);
    for ready in waiting {
        ready();
    }
}

/// On the store's thread: keep a load hint a caller that declined waiting
/// handed over. Unsynced, as [`durable_store::DurableStore::write_hint`]
/// allows.
pub(super) fn write_hint_in(shared: &Shared, key: &StorageKey, hint: &[u8]) {
    let Ok(Target::Document(directory)) = target_in(&shared.root, key) else {
        return;
    };
    if let Err(error) = keep_hint(&directory, hint) {
        tracing::debug!(%key, %error, "could not keep a load hint");
    }
}
