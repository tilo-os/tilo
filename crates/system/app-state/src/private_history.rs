//! Bounded device-private action evidence, checkpointed through the owner's store.
//! This is not an Automerge value and must never be offered to document sync.
//!
//! Scope: replacement evidence, not full application history. Each owner has
//! eight reusable slots (at most 1,024 actions / 32 MiB plus format headers).
//! A session exclusively leases one slot, retaining that slot's recent evidence
//! across actor changes. Reusing it evicts obsolete evidence through the normal
//! bounded replacement checkpoint; no actor-named files accumulate. Other live
//! slots are never reclaimed. If all eight are busy, evidence is unavailable
//! rather than overwriting an active writer. Nothing scans or deletes user data.
use automerge::{AutoCommit, ChangeHash, ReadDoc};
use durable_store::{Durability, DurableStore, Lease, Revision, StorageKey, WriteOp};
use std::{collections::VecDeque, rc::Rc};

const SLOTS: u64 = 8;
const LIMIT: usize = 128;
const BUDGET: usize = 4 * 1024 * 1024;
const MAGIC: &[u8] = b"tilo-private-actions-v1\0";

/// Private evidence for an already executed action, not permission to publish it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateAction {
    pub id: [u8; 16],
    pub hashes: Vec<[u8; 32]>,
    pub intent: Vec<u8>,
    pub receipt: Option<Vec<u8>>,
}
impl PrivateAction {
    fn size(&self) -> usize {
        32_usize
            .saturating_add(self.hashes.len().saturating_mul(32))
            .saturating_add(self.intent.len())
            .saturating_add(self.receipt.as_ref().map_or(0, Vec::len))
    }
    /// A checkpoint can reach disk before the document. Such an orphan is not
    /// recovered as evidence: every referenced head and mutation must exist.
    pub fn is_present_in(&self, doc: &AutoCommit) -> bool {
        self.hashes
            .iter()
            .all(|hash| doc.get_change_by_hash(&ChangeHash(*hash)).is_some())
    }
}

pub(crate) struct PrivateHistory {
    store: Rc<dyn DurableStore>,
    key: StorageKey,
    entries: VecDeque<PrivateAction>,
    dirty: bool,
    revision: Option<Revision>,
    error: Option<String>,
    _lease: Lease,
}
impl PrivateHistory {
    pub(crate) fn open(
        store: Rc<dyn DurableStore>,
        owner: &StorageKey,
        actor: &[u8],
    ) -> Result<Self, String> {
        let identity = blake3::hash(format!("{owner:?}").as_bytes())
            .to_hex()
            .to_string();
        let first = u64::from(
            blake3::hash(actor)
                .as_bytes()
                .first()
                .copied()
                .unwrap_or_default(),
        ) % SLOTS;
        let mut acquired = None;
        for offset in 0..SLOTS {
            let key = StorageKey::view(
                "private-action-history",
                &identity,
                first.wrapping_add(offset) % SLOTS,
            );
            match store.lease(&key) {
                Ok(lease) => {
                    acquired = Some((key, lease));
                    break;
                }
                Err(durable_store::StoreError::Busy(_)) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        let (key, lease) = acquired.ok_or("all private history slots are in use")?;
        let entries = match store.read(&key).map_err(|e| e.to_string())? {
            Some(stored) => decode(&stored.bytes)?,
            None => VecDeque::new(),
        };
        // Reads may include another live session's accepted writes. Ask the
        // existing worker to settle recovered bytes without awaiting its host.
        let error = if entries.is_empty() {
            None
        } else {
            match store.flush_without_waiting(&key) {
                Ok(()) | Err(durable_store::StoreError::Pending(_)) => None,
                Err(error) => Some(error.to_string()),
            }
        };
        Ok(Self {
            store,
            key,
            entries,
            dirty: false,
            revision: None,
            error,
            _lease: lease,
        })
    }
    pub(crate) fn entries(&self) -> impl Iterator<Item = &PrivateAction> {
        self.entries.iter()
    }
    pub(crate) fn record(&mut self, entry: PrivateAction) -> Result<(), String> {
        if entry.size() > BUDGET {
            return Err("private action exceeds history budget".into());
        }
        if let Some(old) = self.entries.iter_mut().find(|old| old.id == entry.id) {
            if old.hashes != entry.hashes
                || old.intent != entry.intent
                || (old.receipt.is_some()
                    && entry.receipt.is_some()
                    && old.receipt != entry.receipt)
            {
                return Err("private action retry changed its evidence".into());
            }
            if old == &entry || (old.receipt.is_some() && entry.receipt.is_none()) {
                return Ok(());
            }
            *old = entry;
        } else {
            self.entries.push_back(entry);
        }
        while self.entries.len() > LIMIT
            || self
                .entries
                .iter()
                .fold(0_usize, |sum, entry| sum.saturating_add(entry.size()))
                > BUDGET
        {
            self.entries.pop_front();
        }
        self.dirty = true;
        Ok(())
    }
    pub(crate) fn checkpoint(&mut self) -> Result<(), String> {
        let failed = matches!(self.durability(), Durability::Failed(_));
        if !self.dirty && !failed && self.error.is_none() {
            return Ok(());
        }
        let bytes = encode(&self.entries)?;
        self.revision = Some(
            match self.store.write(&self.key, WriteOp::Replace(bytes), None) {
                Ok(revision) => revision,
                Err(error) => {
                    let error = error.to_string();
                    self.error = Some(error.clone());
                    return Err(error);
                }
            },
        );
        self.error = None;
        self.dirty = false;
        Ok(())
    }
    pub(crate) fn flush(&self) -> Result<(), String> {
        if self.dirty {
            return Err("private action checkpoint is not submitted".into());
        }
        if self.revision.is_none() && self.entries.is_empty() {
            return Ok(());
        }
        self.store
            .flush(Some(&self.key))
            .map_err(|error| error.to_string())
    }
    pub(crate) fn durability(&self) -> Durability {
        if let Some(error) = &self.error {
            return Durability::Failed(error.clone());
        }
        if self.dirty {
            return Durability::Pending;
        }
        self.revision.map_or_else(
            || {
                if self.entries.is_empty() || self.store.is_settled(&self.key) {
                    Durability::Durable
                } else {
                    match self.store.flush_without_waiting(&self.key) {
                        Ok(()) => Durability::Durable,
                        Err(durable_store::StoreError::Pending(_)) => Durability::Pending,
                        Err(error) => Durability::Failed(error.to_string()),
                    }
                }
            },
            |revision| self.store.durability(&self.key, revision),
        )
    }
}
fn put(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), String> {
    let len = u32::try_from(bytes.len()).map_err(|_| "private action field exceeds capacity")?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}
fn encode(entries: &VecDeque<PrivateAction>) -> Result<Vec<u8>, String> {
    let mut out = MAGIC.to_vec();
    for entry in entries {
        out.extend_from_slice(&entry.id);
        let hashes: Vec<u8> = entry.hashes.iter().flatten().copied().collect();
        put(&mut out, &hashes)?;
        put(&mut out, &entry.intent)?;
        out.push(u8::from(entry.receipt.is_some()));
        put(&mut out, entry.receipt.as_deref().unwrap_or_default())?;
    }
    Ok(out)
}
fn take<'a>(bytes: &mut &'a [u8], len: usize) -> Result<&'a [u8], String> {
    if bytes.len() < len {
        return Err("truncated private action checkpoint".into());
    }
    let (value, rest) = bytes.split_at(len);
    *bytes = rest;
    Ok(value)
}
fn fixed<const N: usize>(bytes: &mut &[u8]) -> Result<[u8; N], String> {
    take(bytes, N)?
        .try_into()
        .map_err(|_| "invalid private action field width".into())
}
fn field<'a>(bytes: &mut &'a [u8]) -> Result<&'a [u8], String> {
    let len = u32::from_le_bytes(fixed(bytes)?) as usize;
    take(bytes, len)
}
fn decode(mut bytes: &[u8]) -> Result<VecDeque<PrivateAction>, String> {
    if bytes.len() > BUDGET.saturating_add(MAGIC.len()) || take(&mut bytes, MAGIC.len())? != MAGIC {
        return Err("invalid private action checkpoint".into());
    }
    let mut entries = VecDeque::new();
    while !bytes.is_empty() {
        let id = fixed(&mut bytes)?;
        let raw = field(&mut bytes)?;
        if raw.len() % 32 != 0 {
            return Err("invalid private action hashes".into());
        }
        let hashes = raw
            .chunks_exact(32)
            .map(|mut hash| fixed(&mut hash))
            .collect::<Result<_, _>>()?;
        let intent = field(&mut bytes)?.to_vec();
        let flag = u8::from_le_bytes(fixed(&mut bytes)?);
        if flag > 1 {
            return Err("invalid private receipt flag".into());
        }
        let receipt = field(&mut bytes)?.to_vec();
        if flag == 0 && !receipt.is_empty() {
            return Err("invalid absent private receipt".into());
        }
        let entry = PrivateAction {
            id,
            hashes,
            intent,
            receipt: (flag == 1).then_some(receipt),
        };
        if entries.iter().any(|old: &PrivateAction| old.id == id) {
            return Err("duplicate private action identity".into());
        }
        entries.push_back(entry);
        if entries.len() > LIMIT {
            return Err("private action checkpoint exceeds capacity".into());
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use durable_store::MemoryStore;

    #[test]
    fn successive_actors_reuse_bounded_owner_slots_without_touching_live_writers() {
        let directory = tempfile::tempdir().unwrap();
        let owner = StorageKey::package("so.tilo.test");
        let other = StorageKey::package("so.tilo.other");
        let store: Rc<dyn DurableStore> = Rc::new(crate::FsStore::new(directory.path()));
        let mut live = PrivateHistory::open(store.clone(), &owner, b"live").unwrap();
        let evidence = PrivateAction {
            id: [42; 16],
            hashes: vec![],
            intent: b"live evidence".to_vec(),
            receipt: None,
        };
        live.record(evidence.clone()).unwrap();
        live.checkpoint().unwrap();
        live.flush().unwrap();
        let live_bytes = store.read(&live.key).unwrap().unwrap().bytes;
        let mut other_history = PrivateHistory::open(store.clone(), &other, b"other").unwrap();
        other_history.record(evidence).unwrap();
        other_history.checkpoint().unwrap();
        other_history.flush().unwrap();
        let other_bytes = store.read(&other_history.key).unwrap().unwrap().bytes;
        let sentinel = directory.path().join("user-document");
        std::fs::write(&sentinel, b"legitimate user data").unwrap();

        for actor in 0..(SLOTS * 2 + 3) {
            // Independent stores exercise on-disk exclusion, not a shared map.
            let session: Rc<dyn DurableStore> = Rc::new(crate::FsStore::new(directory.path()));
            let mut history =
                PrivateHistory::open(session.clone(), &owner, &actor.to_le_bytes()).unwrap();
            assert_ne!(history.key, live.key);
            for id in 0..=LIMIT {
                history
                    .record(PrivateAction {
                        id: ((u128::from(actor) + 1) * 1000 + id as u128).to_le_bytes(),
                        hashes: vec![],
                        intent: vec![1],
                        receipt: None,
                    })
                    .unwrap();
            }
            assert_eq!(history.entries.len(), LIMIT);
            history.checkpoint().unwrap();
            history.flush().unwrap();
            // Also prove the byte boundary on the persisted replacement path.
            history
                .record(PrivateAction {
                    id: [255; 16],
                    hashes: vec![],
                    intent: vec![2; BUDGET - 32],
                    receipt: None,
                })
                .unwrap();
            history.checkpoint().unwrap();
            history.flush().unwrap();
            drop((history, session));
            let mut count = 0;
            let mut bytes = 0;
            let StorageKey::View {
                device, workspace, ..
            } = &live.key
            else {
                panic!("private history must use a view key")
            };
            for slot in 0..SLOTS {
                if let Some(stored) = store
                    .read(&StorageKey::view(device, workspace, slot))
                    .unwrap()
                {
                    count += decode(&stored.bytes).unwrap().len();
                    bytes += stored.bytes.len();
                }
            }
            assert!(count <= usize::try_from(SLOTS).unwrap() * LIMIT);
            assert!(bytes <= usize::try_from(SLOTS).unwrap() * (BUDGET + MAGIC.len()));
            assert_eq!(store.read(&live.key).unwrap().unwrap().bytes, live_bytes);
            assert_eq!(
                store.read(&other_history.key).unwrap().unwrap().bytes,
                other_bytes
            );
        }
        // Every live slot is protected, including across separate FsStores.
        let mut held = vec![live];
        for actor in 1..SLOTS {
            let session: Rc<dyn DurableStore> = Rc::new(crate::FsStore::new(directory.path()));
            held.push(PrivateHistory::open(session, &owner, &actor.to_le_bytes()).unwrap());
        }
        assert!(PrivateHistory::open(store.clone(), &owner, b"overflow").is_err());
        let freed = held.pop().unwrap().key;
        let replacement = PrivateHistory::open(store.clone(), &owner, b"replacement").unwrap();
        assert_eq!(replacement.key, freed);
        assert_eq!(std::fs::read(sentinel).unwrap(), b"legitimate user data");
        // Slot files and their permanent gates are the only owner metadata.
        let path = store.local_path(&replacement.key).unwrap();
        assert!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count()
                <= 2 * usize::try_from(SLOTS).unwrap()
        );
    }

    #[test]
    fn queued_failures_require_a_replacement_and_never_claim_durability() {
        let backend = Rc::new(MemoryStore::queued());
        let store: Rc<dyn DurableStore> = backend.clone();
        let mut history =
            PrivateHistory::open(store, &StorageKey::package("so.tilo.test"), b"actor").unwrap();
        history
            .record(PrivateAction {
                id: [1; 16],
                hashes: vec![],
                intent: vec![1],
                receipt: None,
            })
            .unwrap();
        assert_eq!(history.durability(), Durability::Pending);
        history.checkpoint().unwrap();
        assert_eq!(history.durability(), Durability::Pending);
        let write = backend.drain().pop().unwrap();
        backend
            .acknowledge(write.revision, Err("disk full".into()))
            .unwrap();
        assert!(matches!(history.durability(), Durability::Failed(_)));
        history.checkpoint().unwrap();
        let retry = backend.drain().pop().unwrap();
        assert!(retry.op.is_replace());
        assert_eq!(history.durability(), Durability::Pending);
        backend.acknowledge(retry.revision, Ok(())).unwrap();
        assert_eq!(history.durability(), Durability::Durable);
    }

    #[test]
    fn receipt_ahead_of_failed_document_is_hidden_until_checkpoint_retry_is_durable() {
        let backend = Rc::new(MemoryStore::queued());
        let store: Rc<dyn DurableStore> = backend.clone();
        let key = StorageKey::package("so.tilo.test");
        let owner = Rc::new(crate::DocOwner::acquire(store, key.clone()).unwrap());
        let mut doc = owner.new_document().unwrap();
        let mut writer = crate::DocWriter::start_new(&owner, &mut doc).unwrap();
        let initial = backend.drain().pop().unwrap();
        backend.acknowledge(initial.revision, Ok(())).unwrap();
        doc.put(automerge::ROOT, "body", "later edit").unwrap();
        let evidence = PrivateAction {
            id: [7; 16],
            hashes: doc.get_heads().iter().map(|hash| hash.0).collect(),
            intent: vec![1],
            receipt: Some(vec![2]),
        };
        writer.record_private_action(evidence.clone()).unwrap();
        writer.request(&mut doc);
        let submitted = backend.drain();
        assert_eq!(submitted.len(), 2);
        for write in submitted {
            backend
                .acknowledge(
                    write.revision,
                    if write.key == key {
                        Err("document disk full".into())
                    } else {
                        Ok(())
                    },
                )
                .unwrap();
        }
        assert_eq!(writer.private_action_durability(), Durability::Durable);
        assert!(writer.private_actions(&doc).unwrap().is_empty());
        writer.request_checkpoint(&mut doc);
        let retry = backend.drain().pop().unwrap();
        assert_eq!(retry.key, key);
        assert!(retry.op.is_replace());
        assert!(writer.private_actions(&doc).unwrap().is_empty());
        backend.acknowledge(retry.revision, Ok(())).unwrap();
        assert_eq!(writer.private_actions(&doc).unwrap(), vec![evidence]);
    }

    #[test]
    fn sustained_private_actions_batch_into_one_checkpoint_per_writer_tick() {
        let backend = Rc::new(MemoryStore::queued());
        let store: Rc<dyn DurableStore> = backend.clone();
        let key = StorageKey::package("so.tilo.test");
        let owner = Rc::new(crate::DocOwner::acquire(store, key.clone()).unwrap());
        let mut doc = owner.new_document().unwrap();
        let mut writer = crate::DocWriter::start_new(&owner, &mut doc).unwrap();
        for write in backend.drain() {
            backend.acknowledge(write.revision, Ok(())).unwrap();
        }
        let write_count = Rc::new(std::cell::Cell::new(0));
        let observed = write_count.clone();
        backend.set_write_hook(Some(Rc::new(move || observed.set(observed.get() + 1))));
        // Several full history windows exercise eviction as well as batching.
        for batch in 0..4_u64 {
            for offset in 0..LIMIT as u64 {
                let id = batch * LIMIT as u64 + offset;
                writer
                    .record_private_action(PrivateAction {
                        id: u128::from(id).to_le_bytes(),
                        hashes: vec![],
                        intent: id.to_le_bytes().to_vec(),
                        receipt: Some(b"private admission".to_vec()),
                    })
                    .unwrap();
                assert_eq!(
                    write_count.get(),
                    usize::try_from(batch).unwrap(),
                    "no per-gesture storage write"
                );
                assert!(backend.drain().is_empty());
            }
            assert_eq!(writer.private_action_durability(), Durability::Pending);
            writer.request(&mut doc);
            let pending = backend.drain();
            assert_eq!(pending.len(), 1, "one checkpoint for the entire batch");
            assert_ne!(pending[0].key, key, "shared document is unchanged");
            assert!(pending[0].op.is_replace());
            assert_eq!(write_count.get(), usize::try_from(batch).unwrap() + 1);
            assert_eq!(writer.private_action_durability(), Durability::Pending);
            backend.acknowledge(pending[0].revision, Ok(())).unwrap();
            assert_eq!(writer.private_action_durability(), Durability::Durable);
            assert_eq!(writer.private_actions(&doc).unwrap().len(), LIMIT);
            writer.request(&mut doc);
            assert!(
                backend.drain().is_empty(),
                "clean ticks do not rewrite checkpoints"
            );
        }
    }

    #[test]
    fn writer_tick_checkpoints_private_receipts_even_without_document_mutations() {
        let directory = tempfile::tempdir().unwrap();
        let key = StorageKey::package("so.tilo.test");
        let store: Rc<dyn DurableStore> = Rc::new(crate::FsStore::new(directory.path()));
        let owner = Rc::new(crate::DocOwner::acquire(store.clone(), key.clone()).unwrap());
        let mut doc = owner.new_document().unwrap();
        doc.put(automerge::ROOT, "body", "saved edit").unwrap();
        let mut writer = crate::DocWriter::start_new(&owner, &mut doc).unwrap();
        writer.try_flush().unwrap();
        let before = store.read(&key).unwrap().unwrap().bytes;
        let evidence = PrivateAction {
            id: [9; 16],
            hashes: doc.get_heads().iter().map(|hash| hash.0).collect(),
            intent: b"no-op report".to_vec(),
            receipt: Some(b"private admission".to_vec()),
        };
        writer.record_private_action(evidence.clone()).unwrap();
        assert_eq!(writer.private_action_durability(), Durability::Pending);
        writer.request(&mut doc);
        writer.try_flush().unwrap();
        assert_eq!(writer.private_action_durability(), Durability::Durable);
        assert_eq!(
            store.read(&key).unwrap().unwrap().bytes,
            before,
            "private evidence never alters shared document bytes"
        );
        drop((writer, owner, store));
        let store: Rc<dyn DurableStore> = Rc::new(crate::FsStore::new(directory.path()));
        let owner = Rc::new(crate::DocOwner::acquire(store, key).unwrap());
        let loaded = owner.load_complete().unwrap().unwrap();
        let doc = owner.accept(loaded).unwrap();
        let writer = crate::DocWriter::try_start(&owner).unwrap();
        writer.try_flush().unwrap();
        assert_eq!(writer.private_actions(&doc).unwrap(), vec![evidence]);
    }

    #[test]
    fn checkpoint_restart_preserves_retry_and_rejects_orphaned_document_history() {
        let store: Rc<dyn DurableStore> = Rc::new(MemoryStore::new());
        let key = StorageKey::package("so.tilo.test");
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "body", "edit").unwrap();
        let hashes = doc.get_heads().iter().map(|hash| hash.0).collect();
        let entry = PrivateAction {
            id: [1; 16],
            hashes,
            intent: b"exact report".to_vec(),
            receipt: None,
        };
        let mut history = PrivateHistory::open(store.clone(), &key, b"actor").unwrap();
        history.record(entry.clone()).unwrap();
        history.checkpoint().unwrap();
        drop(history);
        let mut history = PrivateHistory::open(store.clone(), &key, b"actor").unwrap();
        let recovered = history.entries().next().unwrap();
        assert_eq!(recovered, &entry);
        assert!(!recovered.is_present_in(&AutoCommit::new()));
        let loaded = AutoCommit::load(&doc.save()).unwrap();
        assert!(recovered.is_present_in(&loaded));
        history.record(entry.clone()).unwrap();
        let mut changed = entry.clone();
        changed.intent.push(0);
        assert!(history.record(changed).is_err());
        let mut admitted = entry;
        admitted.receipt = Some(b"private admission".to_vec());
        history.record(admitted.clone()).unwrap();
        history.checkpoint().unwrap();
        drop(history);
        let history = PrivateHistory::open(store, &key, b"actor").unwrap();
        assert_eq!(history.entries().next(), Some(&admitted));
        assert_eq!(history.durability(), Durability::Durable);
        assert!(
            loaded
                .get_change_by_hash(&ChangeHash(admitted.hashes[0]))
                .is_some()
        );
    }
}
