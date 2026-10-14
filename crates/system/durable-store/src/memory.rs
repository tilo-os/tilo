//! In-process stores: an immediately-durable one for tests and ephemeral
//! sessions, and a host-acknowledged cache for hosts that commit elsewhere.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Durability, Lease, Revision, SharedLease, StorageKey, StoreError, Stored, WriteOp};

/// One write a host must commit, coalescing everything accepted for its key
/// since the previous [`MemoryStore::drain`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingWrite {
    pub key: StorageKey,
    /// Acknowledge with this revision; it is the newest write folded in.
    pub revision: Revision,
    pub op: WriteOp,
    pub writer: Option<Vec<u8>>,
}

/// A [`crate::DurableStore`] held entirely in memory.
///
/// [`MemoryStore::new`] treats every accepted write as durable at once.
/// [`MemoryStore::queued`] instead acts as a hydrate-then-run cache for a
/// host with its own persistence: the host loads existing values with
/// [`MemoryStore::hydrate`], collects accepted writes with
/// [`MemoryStore::drain`], commits them, and reports each outcome with
/// [`MemoryStore::acknowledge`]. A write is durable only once acknowledged.
pub struct MemoryStore {
    namespace: String,
    queued: bool,
    state: RefCell<State>,
    leases: Rc<RefCell<BTreeMap<StorageKey, Held>>>,
    claims: RefCell<BTreeMap<StorageKey, String>>,
    write_hook: RefCell<Option<Rc<dyn Fn()>>>,
}

/// Who holds one key: an exclusive owner, or shared writers.
#[derive(Default)]
struct Held {
    exclusive: bool,
    shared: usize,
    primary: bool,
}

#[derive(Default)]
struct State {
    issued: u64,
    slots: BTreeMap<StorageKey, Slot>,
}

#[derive(Default)]
struct Slot {
    current: Option<Stored>,
    /// Accepted but not yet handed to the host.
    queued: Option<Batch>,
    /// Handed to the host, awaiting acknowledgement.
    in_flight: Vec<Batch>,
    /// Newest acknowledged revision.
    durable: u64,
    /// Failed revision ranges not yet superseded by a durable replacement.
    failed: Vec<(u64, u64, String)>,
    /// A failed write may have left a partial tail on the host's side, so
    /// the next write is widened to a full replacement.
    replace_required: bool,
}

impl Slot {
    fn pending(&self) -> bool {
        self.queued.is_some() || !self.in_flight.is_empty()
    }
}

struct Batch {
    first: u64,
    last: u64,
    op: WriteOp,
    writer: Option<Vec<u8>>,
}

impl Batch {
    fn covers(&self, revision: u64) -> bool {
        (self.first..=self.last).contains(&revision)
    }
}

struct MemoryLease {
    key: StorageKey,
    shared: bool,
    primary: bool,
    leases: Weak<RefCell<BTreeMap<StorageKey, Held>>>,
}

impl Drop for MemoryLease {
    fn drop(&mut self) {
        let Some(leases) = self.leases.upgrade() else {
            return;
        };
        let mut leases = leases.borrow_mut();
        let Some(held) = leases.get_mut(&self.key) else {
            return;
        };
        if self.shared {
            held.shared = held.shared.saturating_sub(1);
            if self.primary {
                held.primary = false;
            }
        } else {
            held.exclusive = false;
        }
        if !held.exclusive && held.shared == 0 {
            leases.remove(&self.key);
        }
    }
}

fn fresh_namespace(kind: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("{kind}:{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

impl MemoryStore {
    /// A store whose writes are durable as soon as they are accepted.
    pub fn new() -> Self {
        Self::with_mode(fresh_namespace("memory"), false)
    }

    /// A cache whose writes are durable only once its host acknowledges them.
    pub fn queued() -> Self {
        Self::with_mode(fresh_namespace("queued"), true)
    }

    /// Name the profile this store holds, replacing the process-unique
    /// default. Two stores given the same namespace claim the same data.
    #[must_use]
    pub fn named(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    fn with_mode(namespace: String, queued: bool) -> Self {
        Self {
            namespace,
            queued,
            state: RefCell::new(State::default()),
            leases: Rc::default(),
            claims: RefCell::default(),
            write_hook: RefCell::default(),
        }
    }

    /// Call `hook` after each write a [`queued`](Self::queued) store accepts,
    /// so its host can schedule a [`drain`](Self::drain) instead of polling.
    /// The hook runs with no borrow of the store held; it may drain at once,
    /// but a host that commits asynchronously should coalesce.
    pub fn set_write_hook(&self, hook: Option<Rc<dyn Fn()>>) {
        *self.write_hook.borrow_mut() = hook;
    }

    /// Load a value the host already holds durably, before any owner writes
    /// to it.
    ///
    /// # Errors
    /// Refuses to replace a key with accepted writes the host has not yet
    /// committed: hydration would silently discard them.
    pub fn hydrate(&self, key: StorageKey, stored: Stored) -> Result<(), StoreError> {
        let mut state = self.state.borrow_mut();
        if state.slots.get(&key).is_some_and(Slot::pending) {
            return Err(StoreError::Pending(key.to_string()));
        }
        state.slots.entry(key).or_default().current = Some(stored);
        Ok(())
    }

    /// Take every write awaiting its host, oldest first. Each key appears at
    /// most once, with all of its writes since the last drain folded together.
    pub fn drain(&self) -> Vec<PendingWrite> {
        let mut state = self.state.borrow_mut();
        let mut out = Vec::new();
        for (key, slot) in &mut state.slots {
            if let Some(batch) = slot.queued.take() {
                out.push(PendingWrite {
                    key: key.clone(),
                    revision: Revision(batch.last),
                    op: batch.op.clone(),
                    writer: batch.writer.clone(),
                });
                slot.in_flight.push(batch);
            }
        }
        out.sort_by_key(|write| write.revision);
        out
    }

    /// Queue each key whose commit failed, and that has nothing queued, for a
    /// full replacement with its current value, so the host can retry without
    /// waiting for the owner's next write. Returns how many were queued.
    pub fn retry_failed(&self) -> usize {
        let mut state = self.state.borrow_mut();
        let State { issued, slots } = &mut *state;
        let mut queued = 0_usize;
        for slot in slots.values_mut() {
            if slot.failed.is_empty() || slot.queued.is_some() {
                continue;
            }
            let Some(revision) = issued.checked_add(1) else {
                break;
            };
            *issued = revision;
            let current = slot.current.clone().unwrap_or_default();
            slot.queued = Some(Batch {
                first: revision,
                last: revision,
                op: WriteOp::Replace(current.bytes),
                writer: current.writer,
            });
            queued = queued.saturating_add(1);
        }
        queued
    }

    /// Report the host's outcome for a drained write.
    ///
    /// # Errors
    /// Refuses a revision that is not awaiting acknowledgement from this store.
    pub fn acknowledge(
        &self,
        revision: Revision,
        result: Result<(), String>,
    ) -> Result<(), StoreError> {
        let mut state = self.state.borrow_mut();
        let (slot, index) = state
            .slots
            .values_mut()
            .find_map(|slot| {
                let index = slot.in_flight.iter().position(|b| b.last == revision.0)?;
                Some((slot, index))
            })
            .ok_or(StoreError::UnknownRevision(revision.0))?;
        let batch = slot.in_flight.remove(index);
        match result {
            Ok(()) => {
                slot.durable = slot.durable.max(batch.last);
                if batch.op.is_replace() {
                    slot.replace_required = false;
                    slot.failed.retain(|(_, last, _)| *last > batch.last);
                }
            }
            Err(message) => {
                slot.failed.push((batch.first, batch.last, message));
                slot.replace_required = true;
            }
        }
        Ok(())
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::DurableStore for MemoryStore {
    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn read(&self, key: &StorageKey) -> Result<Option<Stored>, StoreError> {
        Ok(self
            .state
            .borrow()
            .slots
            .get(key)
            .and_then(|slot| slot.current.clone()))
    }

    fn write(
        &self,
        key: &StorageKey,
        op: WriteOp,
        writer: Option<&[u8]>,
    ) -> Result<Revision, StoreError> {
        let mut state = self.state.borrow_mut();
        let revision = state
            .issued
            .checked_add(1)
            .ok_or_else(|| StoreError::Failed {
                context: key.to_string(),
                message: "revision space exhausted".into(),
            })?;
        state.issued = revision;
        let slot = state.slots.entry(key.clone()).or_default();
        let current = slot.current.get_or_insert_with(Stored::default);
        op.apply(&mut current.bytes);
        if let Some(writer) = writer {
            current.writer = Some(writer.to_vec());
        }
        if !self.queued {
            slot.durable = revision;
            return Ok(Revision(revision));
        }
        let op = if slot.replace_required {
            WriteOp::Replace(current.bytes.clone())
        } else {
            op
        };
        let writer = writer.map(<[u8]>::to_vec);
        match slot.queued.as_mut() {
            Some(batch) => {
                batch.op.coalesce(op);
                batch.last = revision;
                if writer.is_some() {
                    batch.writer = writer;
                }
            }
            None => {
                slot.queued = Some(Batch {
                    first: revision,
                    last: revision,
                    op,
                    writer,
                });
            }
        }
        drop(state);
        let hook = self.write_hook.borrow().clone();
        if let Some(hook) = hook {
            hook();
        }
        Ok(Revision(revision))
    }

    fn durability(&self, key: &StorageKey, revision: Revision) -> Durability {
        let state = self.state.borrow();
        if revision.0 == 0 || revision.0 > state.issued {
            return Durability::Failed(format!("revision {} was never issued", revision.0));
        }
        let Some(slot) = state.slots.get(key) else {
            return Durability::Failed(format!("{key} has no revision {}", revision.0));
        };
        if slot
            .queued
            .iter()
            .chain(&slot.in_flight)
            .any(|b| b.covers(revision.0))
        {
            return Durability::Pending;
        }
        if let Some((_, _, message)) = slot
            .failed
            .iter()
            .find(|(first, last, _)| (*first..=*last).contains(&revision.0))
        {
            return Durability::Failed(message.clone());
        }
        if revision.0 <= slot.durable {
            Durability::Durable
        } else {
            Durability::Failed(format!("revision {} does not belong to {key}", revision.0))
        }
    }

    fn flush(&self, key: Option<&StorageKey>) -> Result<(), StoreError> {
        let state = self.state.borrow();
        for (slot_key, slot) in &state.slots {
            if key.is_some_and(|key| key != slot_key) {
                continue;
            }
            if slot.pending() {
                return Err(StoreError::Pending(slot_key.to_string()));
            }
            if let Some((_, _, message)) = slot.failed.last() {
                return Err(StoreError::Failed {
                    context: slot_key.to_string(),
                    message: message.clone(),
                });
            }
        }
        Ok(())
    }

    fn lease(&self, key: &StorageKey) -> Result<Lease, StoreError> {
        let mut leases = self.leases.borrow_mut();
        let held = leases.entry(key.clone()).or_default();
        if held.exclusive || held.shared > 0 {
            return Err(StoreError::Busy(key.to_string()));
        }
        held.exclusive = true;
        drop(leases);
        Ok(Box::new(MemoryLease {
            key: key.clone(),
            shared: false,
            primary: false,
            leases: Rc::downgrade(&self.leases),
        }))
    }

    fn records(&self, package: &str, workspace: &str) -> Result<Vec<String>, StoreError> {
        // `slots` is ordered, so the answer is sorted.
        Ok(self
            .state
            .borrow()
            .slots
            .iter()
            .filter(|(_, slot)| slot.current.as_ref().is_some_and(|s| !s.bytes.is_empty()))
            .filter_map(|(key, _)| match key {
                StorageKey::Record {
                    package: p,
                    workspace: w,
                    record,
                } if p == package && w == workspace => Some(record.clone()),
                _ => None,
            })
            .collect())
    }

    fn claim(&self, key: &StorageKey, claimant: &str) -> Result<bool, StoreError> {
        let mut claims = self.claims.borrow_mut();
        let owner = claims
            .entry(key.clone())
            .or_insert_with(|| claimant.to_owned());
        Ok(owner == claimant)
    }

    fn lease_shared(&self, key: &StorageKey) -> Result<SharedLease, StoreError> {
        let mut leases = self.leases.borrow_mut();
        let held = leases.entry(key.clone()).or_default();
        if held.exclusive {
            return Err(StoreError::Busy(key.to_string()));
        }
        held.shared = held.shared.saturating_add(1);
        let primary = !held.primary;
        held.primary = true;
        drop(leases);
        Ok(SharedLease {
            lease: Box::new(MemoryLease {
                key: key.clone(),
                shared: true,
                primary,
                leases: Rc::downgrade(&self.leases),
            }),
            primary,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DurableStore;

    fn key() -> StorageKey {
        StorageKey::record("pkg", "room", "rec")
    }

    #[test]
    fn memory_stores_pass_conformance_with_unique_namespaces() {
        // Phase `immediate_store_passes_conformance`.
        {
            let store = MemoryStore::new();
            crate::conformance::run(&crate::conformance::Subject {
                store: &store,
                settle: &|| {},
            });
        }
        // Phase `queued_store_passes_conformance_with_a_committing_host`.
        {
            let store = MemoryStore::queued();
            let settle = || {
                for write in store.drain() {
                    store.acknowledge(write.revision, Ok(())).unwrap();
                }
            };
            crate::conformance::run(&crate::conformance::Subject {
                store: &store,
                settle: &settle,
            });
        }
        // Phase `default_namespaces_are_unique_per_store`.
        {
            assert_ne!(
                MemoryStore::new().namespace(),
                MemoryStore::new().namespace()
            );
            assert_eq!(
                MemoryStore::queued().named("profile").namespace(),
                "profile"
            );
        }
    }

    #[test]
    fn queued_writes_are_durable_only_after_acknowledgement_and_survive_hydration() {
        // Phase `queued_writes_are_durable_only_after_their_acknowledgement`.
        {
            let store = MemoryStore::queued();
            let first = store
                .write(&key(), WriteOp::Replace(b"a".to_vec()), Some(b"actor"))
                .unwrap();
            assert_eq!(store.durability(&key(), first), Durability::Pending);
            assert!(matches!(
                store.flush(Some(&key())),
                Err(StoreError::Pending(_))
            ));
            let second = store
                .write(&key(), WriteOp::Append(b"b".to_vec()), None)
                .unwrap();
            let drained = store.drain();
            assert_eq!(
                drained,
                vec![PendingWrite {
                    key: key(),
                    revision: second,
                    op: WriteOp::Replace(b"ab".to_vec()),
                    writer: Some(b"actor".to_vec()),
                }]
            );
            assert_eq!(store.durability(&key(), first), Durability::Pending);
            store.acknowledge(second, Ok(())).unwrap();
            assert_eq!(store.durability(&key(), first), Durability::Durable);
            assert_eq!(store.durability(&key(), second), Durability::Durable);
            assert!(store.flush(Some(&key())).is_ok());
            assert!(matches!(
                store.acknowledge(second, Ok(())),
                Err(StoreError::UnknownRevision(_))
            ));
        }
        // Phase `failed_acknowledgement_widens_the_next_write_to_a_replacement`.
        {
            let store = MemoryStore::queued();
            let base = store
                .write(&key(), WriteOp::Replace(b"base".to_vec()), None)
                .unwrap();
            store.drain();
            store.acknowledge(base, Ok(())).unwrap();
            let tail = store
                .write(&key(), WriteOp::Append(b"+1".to_vec()), None)
                .unwrap();
            store.drain();
            store.acknowledge(tail, Err("quota".into())).unwrap();
            assert_eq!(
                store.durability(&key(), tail),
                Durability::Failed("quota".into())
            );
            assert_eq!(store.durability(&key(), base), Durability::Durable);
            assert!(matches!(store.flush(None), Err(StoreError::Failed { .. })));
            let retry = store
                .write(&key(), WriteOp::Append(b"+2".to_vec()), None)
                .unwrap();
            let drained = store.drain();
            assert_eq!(drained[0].op, WriteOp::Replace(b"base+1+2".to_vec()));
            store.acknowledge(retry, Ok(())).unwrap();
            assert_eq!(store.durability(&key(), tail), Durability::Durable);
            assert!(store.flush(None).is_ok());
            assert_eq!(store.read(&key()).unwrap().unwrap().bytes, b"base+1+2");
        }
        // Phase `hydration_never_discards_unacknowledged_writes`.
        {
            let store = MemoryStore::queued();
            let stored = Stored {
                bytes: b"host".to_vec(),
                writer: Some(b"actor".to_vec()),
            };
            store.hydrate(key(), stored.clone()).unwrap();
            assert_eq!(store.read(&key()).unwrap(), Some(stored));
            assert!(store.flush(None).is_ok());
            store
                .write(&key(), WriteOp::Append(b"!".to_vec()), None)
                .unwrap();
            assert!(store.hydrate(key(), Stored::default()).is_err());
            assert_eq!(store.read(&key()).unwrap().unwrap().bytes, b"host!");
        }
    }
}
