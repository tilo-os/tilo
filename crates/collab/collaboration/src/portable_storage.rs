//! Hydrated, explicitly namespaced browser persistence. Durable writes are acknowledged
//! by the host only after its backing transaction commits.
#[cfg(test)]
thread_local! { static DOCUMENT_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
mod tests {
    use super::*;
    /// A window showing "Saving…" over a browser cache hears when the host
    /// acknowledges the current bytes, durable or refused, and not for an
    /// older revision the cache has moved past.
    #[test]
    fn the_host_acknowledging_the_current_bytes_wakes_who_watches_them() {
        let cache = PortableStorage::new("profile", "device").unwrap();
        let key = StorageKey::Record {
            package: "app".into(),
            workspace: "room".into(),
            record: "record".into(),
        };
        let woken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake: durable_store::ReadyWake = {
            let woken = std::sync::Arc::clone(&woken);
            std::sync::Arc::new(move || {
                woken.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
        };
        let count = || woken.load(std::sync::atomic::Ordering::Relaxed);
        cache.watch_durability(&key, std::sync::Arc::clone(&wake));
        cache.watch_durability(&key, std::sync::Arc::clone(&wake));
        cache.put(key.clone(), b"first".to_vec(), None).unwrap();
        let first = cache.pending_writes().remove(0);
        cache.put(key.clone(), b"second".to_vec(), None).unwrap();
        let second = cache.pending_writes().remove(0);
        assert_eq!(count(), 0, "nothing acknowledged yet");
        cache.acknowledge(&first, Ok(())).unwrap();
        assert_eq!(count(), 0, "an older revision leaves the key saving");
        cache.acknowledge(&second, Err("quota".into())).unwrap();
        assert_eq!(count(), 1, "the refusal is heard, once per watcher");
        cache.put(key.clone(), b"third".to_vec(), None).unwrap();
        let third = cache.pending_writes().remove(0);
        cache.acknowledge(&third, Ok(())).unwrap();
        assert_eq!(count(), 2, "and so is the bytes becoming durable");
        assert_eq!(cache.flush(&key), Ok(()));
    }
    #[test]
    fn pending_write_accounting_includes_spare_byte_and_private_head_capacity() {
        use automerge::transaction::Transactable;
        let cache = PortableStorage::new("profile", "device").unwrap();
        let key = StorageKey::Record {
            package: "app".into(),
            workspace: "room".into(),
            record: "record".into(),
        };
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "value", 1).unwrap();
        cache.put(key.clone(), doc.save(), Some(vec![1])).unwrap();
        cache.document_heads(&key).unwrap().unwrap();
        let mut write = cache.pending_writes().remove(0);
        let initial = write.retained_bytes().unwrap();
        let before = write.value.bytes.capacity();
        write.value.bytes.reserve_exact(before + 4096);
        let growth = write.value.bytes.capacity() - before;
        assert_eq!(write.retained_bytes().unwrap() - initial, growth);
        let charged = write.retained_bytes().unwrap();
        let heads = write
            .document_heads
            .as_mut()
            .unwrap()
            .heads
            .as_mut()
            .unwrap();
        let before = heads.capacity();
        heads.reserve_exact(32);
        let growth = (heads.capacity() - before) * std::mem::size_of::<automerge::ChangeHash>();
        assert!(growth >= 32 * std::mem::size_of::<automerge::ChangeHash>());
        assert_eq!(write.retained_bytes().unwrap() - charged, growth);
    }

    #[test]
    fn serialized_document_seeds_heads_without_reloading_its_own_bytes() {
        use automerge::transaction::Transactable;
        let cache = PortableStorage::new("profile", "device").unwrap();
        let key = StorageKey::Primary {
            package: "test".into(),
            workspace: "room".into(),
        };
        let mut doc = automerge::AutoCommit::new();
        DOCUMENT_DECODES.with(|count| count.set(0));
        for value in 0..3 {
            doc.put(automerge::ROOT, "value", value).unwrap();
            cache.put_document(key.clone(), &mut doc, None).unwrap();
            assert_eq!(cache.document_heads(&key), Some(Ok(doc.base_heads())));
            assert_eq!(DOCUMENT_DECODES.with(|count| count.get()), 0);
            assert!(
                cache.flush(&key).is_err(),
                "queuing known heads does not acknowledge persistence"
            );
            let mut loaded = automerge::AutoCommit::load(&cache.get(&key).unwrap().bytes).unwrap();
            assert_eq!(loaded.get_heads(), doc.base_heads());
        }
        cache.put(key.clone(), vec![1, 2, 3], None).unwrap();
        assert_eq!(cache.document_heads(&key), Some(Err(())));
        assert_eq!(DOCUMENT_DECODES.with(|count| count.get()), 1);
    }
    #[test]
    fn serialized_document_heads_follow_full_graph_when_owner_is_isolated() {
        use automerge::transaction::Transactable;
        let cache = PortableStorage::new("profile", "device").unwrap();
        let key = StorageKey::Primary {
            package: "test".into(),
            workspace: "room".into(),
        };
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "value", 1).unwrap();
        let historical = doc.get_heads();
        doc.put(automerge::ROOT, "value", 2).unwrap();
        let current = doc.get_heads();
        doc.isolate(&historical);
        cache.put_document(key.clone(), &mut doc, None).unwrap();
        assert_eq!(cache.document_heads(&key), Some(Ok(current)));
        assert_eq!(
            doc.base_heads(),
            historical,
            "saving must preserve isolation"
        );
        let mut loaded = automerge::AutoCommit::load(&cache.get(&key).unwrap().bytes).unwrap();
        assert_eq!(cache.document_heads(&key), Some(Ok(loaded.get_heads())));
    }
    #[test]
    fn document_heads_decode_once_per_value_and_preserve_ack_state() {
        use automerge::transaction::Transactable;
        let cache = PortableStorage::new("profile", "device").unwrap();
        let key = StorageKey::Primary {
            package: "test".into(),
            workspace: "room".into(),
        };
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "value", 1).unwrap();
        let bytes = doc.save();
        let first_heads = doc.get_heads();
        cache.hydrate(key.clone(), bytes.clone(), None).unwrap();
        DOCUMENT_DECODES.with(|count| count.set(0));
        for _ in 0..100 {
            assert_eq!(cache.document_heads(&key), Some(Ok(first_heads.clone())));
        }
        cache.put(key.clone(), bytes, None).unwrap();
        assert_eq!(cache.document_heads(&key), Some(Ok(first_heads)));
        assert_eq!(DOCUMENT_DECODES.with(|count| count.get()), 1);
        doc.put(automerge::ROOT, "value", 2).unwrap();
        cache.put(key.clone(), doc.save(), None).unwrap();
        let pending = cache.pending_writes().remove(0);
        assert_eq!(cache.document_heads(&key), Some(Ok(doc.get_heads())));
        assert!(
            cache.flush(&key).is_err(),
            "head decoding must not acknowledge persistence"
        );
        cache.acknowledge(&pending, Err("quota".into())).unwrap();
        assert_eq!(cache.document_heads(&key), Some(Ok(doc.get_heads())));
        assert_eq!(cache.flush(&key), Err("quota".into()));
        cache.acknowledge(&pending, Ok(())).unwrap();
        assert_eq!(cache.document_heads(&key), Some(Ok(doc.get_heads())));
        assert_eq!(DOCUMENT_DECODES.with(|count| count.get()), 2);
        cache.put(key.clone(), vec![1, 2, 3], None).unwrap();
        for _ in 0..3 {
            assert_eq!(cache.document_heads(&key), Some(Err(())));
        }
        assert_eq!(DOCUMENT_DECODES.with(|count| count.get()), 3);
        cache.put(key.clone(), doc.save(), None).unwrap();
        assert_eq!(cache.document_heads(&key), Some(Ok(doc.get_heads())));
        assert_eq!(DOCUMENT_DECODES.with(|count| count.get()), 4);
    }
    #[test]
    fn status_retains_exact_prior_commit_across_newer_pending_and_failed_writes() {
        let cache = PortableStorage::new("profile", "device").unwrap();
        let key = StorageKey::Admission {
            workspace: "room".into(),
        };
        cache.put(key.clone(), vec![1], None).unwrap();
        let first = cache.pending_writes().remove(0);
        cache.put(key.clone(), vec![2], None).unwrap();
        cache.acknowledge(&first, Ok(())).unwrap();
        let status = cache.status(&key).unwrap();
        assert!(!status.durable);
        assert_eq!(status.committed.unwrap().1.bytes, vec![1]);
        let second = cache.pending_writes().remove(0);
        cache.acknowledge(&second, Err("quota".into())).unwrap();
        let status = cache.status(&key).unwrap();
        assert_eq!(status.error.as_deref(), Some("quota"));
        assert_eq!(status.committed.unwrap().0, first.revision);
        cache.acknowledge(&second, Ok(())).unwrap();
        let status = cache.status(&key).unwrap();
        assert!(status.durable);
        assert_eq!(status.committed.unwrap().1.bytes, vec![2]);
    }
    #[test]
    fn pending_writes_capture_heads_bound_to_their_exact_bytes() {
        use automerge::transaction::Transactable;
        let cache = PortableStorage::new("profile", "device").unwrap();
        assert!(cache.pending_writes().is_empty());
        let key = StorageKey::Record {
            package: "app".into(),
            workspace: "room".into(),
            record: "record".into(),
        };
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "value", 1).unwrap();
        cache.put_document(key.clone(), &mut doc, None).unwrap();
        cache
            .put(
                StorageKey::Admission {
                    workspace: "room".into(),
                },
                vec![1, 2, 3],
                None,
            )
            .unwrap();
        let writes = cache.pending_writes();
        assert_eq!(writes.len(), 2);
        let record = writes.iter().find(|w| w.key == key).unwrap();
        assert_eq!(record.document_heads(), Some(Ok(doc.get_heads())));
        let mut altered = record.clone();
        altered.value.bytes.push(0);
        assert_eq!(
            altered.document_heads(),
            None,
            "heads are bound to the exact bytes"
        );
        let plain = writes.iter().find(|w| w.key != key).unwrap();
        assert_eq!(plain.document_heads(), None);
        for write in &writes {
            cache.acknowledge(write, Ok(())).unwrap();
        }
        assert!(cache.pending_writes().is_empty());
    }
    #[test]
    fn cache_flush_requires_matching_durable_ack_and_retains_failed_bytes() {
        let cache = PortableStorage::new("profile-a", "device-a").unwrap();
        let key = StorageKey::View {
            workspace: "room".into(),
            instance: 3,
        };
        cache.put(key.clone(), vec![1, 2], None).unwrap();
        let first = cache.pending_writes().remove(0);
        assert!(cache.flush(&key).is_err());
        cache.put(key.clone(), vec![3], None).unwrap();
        cache.acknowledge(&first, Ok(())).unwrap();
        assert!(cache.flush(&key).is_err());
        let second = cache.pending_writes().remove(0);
        cache
            .acknowledge(&second, Err("quota exceeded".into()))
            .unwrap();
        assert!(cache.flush(&key).unwrap_err().contains("quota exceeded"));
        assert_eq!(cache.get(&key).unwrap().bytes, vec![3]);
        cache.acknowledge(&second, Ok(())).unwrap();
        assert!(cache.flush(&key).is_ok());
    }
    #[test]
    fn cache_rejects_foreign_ack_and_hydration_over_pending_changes() {
        let a = PortableStorage::new("a", "device").unwrap();
        let b = PortableStorage::new("b", "device").unwrap();
        let key = StorageKey::View {
            workspace: "room".into(),
            instance: 1,
        };
        a.put(key.clone(), vec![1], None).unwrap();
        let write = a.pending_writes().remove(0);
        assert!(b.acknowledge(&write, Ok(())).is_err());
        assert!(a.hydrate(key.clone(), vec![2], None).is_err());
        assert_eq!(a.get(&key).unwrap().bytes, vec![1]);
        assert!(PortableStorage::new("", "device").is_err());
    }
}

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

/// A storage identity is not a filesystem path or a source of authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileNamespace {
    Native(std::path::PathBuf),
    Portable { profile: String, device: String },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StorageKey {
    Admission {
        workspace: String,
    },
    Primary {
        package: String,
        workspace: String,
    },
    Record {
        package: String,
        workspace: String,
        record: String,
    },
    View {
        workspace: String,
        instance: u64,
    },
}
impl StorageKey {
    fn validate(&self) -> Result<(), String> {
        let ids = match self {
            Self::Admission { workspace } | Self::View { workspace, .. } => vec![workspace],
            Self::Primary { package, workspace } => vec![package, workspace],
            Self::Record {
                package,
                workspace,
                record,
            } => vec![package, workspace, record],
        };
        for id in ids {
            validate(id)?;
        }
        Ok(())
    }
}
fn validate(id: &str) -> Result<(), String> {
    if id.trim().is_empty() || id.chars().any(char::is_control) {
        Err("empty or invalid storage identity".into())
    } else {
        Ok(())
    }
}

/// The cache's answer while the host has not yet acknowledged a write: the
/// bytes are queued, which is "saving", not a failure.
pub const PENDING_ACKNOWLEDGEMENT: &str = "persistence pending host acknowledgement";

#[derive(Clone, Debug)]
pub struct CachedBytes {
    pub bytes: Vec<u8>,
    /// Persistent local Automerge actor; absent for local view envelopes.
    pub actor: Option<Vec<u8>>,
}
#[derive(Clone, Debug)]
pub struct PendingWrite {
    pub namespace: ProfileNamespace,
    pub key: StorageKey,
    pub value: CachedBytes,
    pub revision: u64,
    cache_id: uuid::Uuid,
    document_heads: Option<DocumentHeads>,
}
impl PendingWrite {
    /// Heads recorded for exactly these bytes when the canonical owner queued
    /// them (`put_document`), or `None` when unknown or when the bytes were
    /// changed after capture. Saves decoding a whole document to learn them.
    pub fn document_heads(&self) -> Option<Result<Vec<automerge::ChangeHash>, ()>> {
        self.document_heads
            .as_ref()
            .filter(|metadata| metadata.fingerprint == blake3::hash(&self.value.bytes))
            .map(|metadata| metadata.heads.clone())
    }
    /// Bytes owned by this immutable capture: inline `Self` plus all owned
    /// buffer capacities, including private cached document heads. Excludes
    /// allocator bookkeeping and the enclosing container's spare capacity.
    /// `None` reports arithmetic overflow; callers must reject that capture.
    pub fn retained_bytes(&self) -> Option<usize> {
        let mut total = std::mem::size_of::<Self>().checked_add(self.value.bytes.capacity())?;
        if let Some(actor) = &self.value.actor {
            total = total.checked_add(actor.capacity())?;
        }
        match &self.namespace {
            ProfileNamespace::Native(path) => total = total.checked_add(path.capacity())?,
            ProfileNamespace::Portable { profile, device } => {
                total = total
                    .checked_add(profile.capacity())?
                    .checked_add(device.capacity())?;
            }
        }
        let (workspace, package, record) = match &self.key {
            StorageKey::Admission { workspace } | StorageKey::View { workspace, .. } => {
                (workspace, None, None)
            }
            StorageKey::Primary { workspace, package } => (workspace, Some(package), None),
            StorageKey::Record {
                workspace,
                package,
                record,
            } => (workspace, Some(package), Some(record)),
        };
        total = total.checked_add(workspace.capacity())?;
        if let Some(package) = package {
            total = total.checked_add(package.capacity())?;
        }
        if let Some(record) = record {
            total = total.checked_add(record.capacity())?;
        }
        if let Some(metadata) = &self.document_heads
            && let Ok(heads) = &metadata.heads
        {
            total = total.checked_add(
                heads
                    .capacity()
                    .checked_mul(std::mem::size_of::<automerge::ChangeHash>())?,
            )?;
        }
        Some(total)
    }
}

#[derive(Clone, Debug)]
pub struct StorageStatus {
    pub revision: u64,
    pub durable: bool,
    pub error: Option<String>,
    pub committed: Option<(u64, CachedBytes)>,
}
// Bind derived metadata to exact serialized bytes, including across queued
// snapshots whose public PendingWrite fields callers may subsequently modify.
#[derive(Clone, Debug)]
struct DocumentHeads {
    fingerprint: blake3::Hash,
    heads: Result<Vec<automerge::ChangeHash>, ()>,
}
impl DocumentHeads {
    fn new(bytes: &[u8], heads: Result<Vec<automerge::ChangeHash>, ()>) -> Self {
        Self {
            fingerprint: blake3::hash(bytes),
            heads,
        }
    }
}
struct CommittedValue {
    revision: u64,
    value: CachedBytes,
}
enum CurrentValue {
    Owned { value: CachedBytes },
}
impl CurrentValue {
    fn ordinary(value: CachedBytes) -> Self {
        Self::Owned { value }
    }
}
struct Entry {
    value: CurrentValue,
    // Derived only from this entry's immutable bytes; replacement invalidates it.
    document_heads: Option<DocumentHeads>,
    revision: u64,
    durable: bool,
    error: Option<String>,
    committed: Option<CommittedValue>,
}
impl Entry {
    fn current(&self) -> &CachedBytes {
        match &self.value {
            CurrentValue::Owned { value, .. } => value,
        }
    }
}
#[derive(Default)]
struct Cache {
    entries: BTreeMap<StorageKey, Entry>,
    revision: u64,
    /// The record store that owns each package's records in a workspace
    /// (the toolkit's `ui::record::RecordSet`), so every opening shares it.
    owners: BTreeMap<(String, String), std::rc::Weak<dyn std::any::Any>>,
    /// Who hears each time a key's durability may have changed without
    /// input ([`PortableStorage::watch_durability`]).
    watchers: BTreeMap<StorageKey, Vec<durable_store::ReadyWake>>,
}
// One original Rc holds immutable binding and mutable entries. Namespace
// buffers are moved here once; creating the binding adds no duplicate strings.
struct CacheBinding {
    namespace: ProfileNamespace,
    id: uuid::Uuid,
    inner: RefCell<Cache>,
}
impl CacheBinding {
    fn borrow(&self) -> std::cell::Ref<'_, Cache> {
        self.inner.borrow()
    }
    fn borrow_mut(&self) -> std::cell::RefMut<'_, Cache> {
        self.inner.borrow_mut()
    }
}
/// Clone one cache throughout a browser host. Hydrate before opening its owners;
/// only acknowledge writes after the external storage transaction commits.
#[derive(Clone)]
pub struct PortableStorage {
    cache: Rc<CacheBinding>,
}
impl PortableStorage {
    pub fn new(profile: &str, device: &str) -> Result<Self, String> {
        validate(profile)?;
        validate(device)?;
        Ok(Self {
            cache: Rc::new(CacheBinding {
                namespace: ProfileNamespace::Portable {
                    profile: profile.into(),
                    device: device.into(),
                },
                id: uuid::Uuid::new_v4(),
                inner: RefCell::new(Cache::default()),
            }),
        })
    }
    /// The live record store registered for `package` in `workspace`, if it
    /// is an `Owner`.
    pub fn record_owner<Owner: 'static>(
        &self,
        package: &str,
        workspace: &str,
    ) -> Option<Rc<Owner>> {
        self.cache
            .borrow()
            .owners
            .get(&(package.into(), workspace.into()))
            .and_then(std::rc::Weak::upgrade)
            .and_then(|owner| owner.downcast::<Owner>().ok())
    }
    /// Register `owner` as the record store of `package` in `workspace`
    /// while it lives.
    pub fn register_record_owner<Owner: 'static>(
        &self,
        package: &str,
        workspace: &str,
        owner: &Rc<Owner>,
    ) {
        let owner: Rc<dyn std::any::Any> = owner.clone();
        self.cache
            .borrow_mut()
            .owners
            .insert((package.into(), workspace.into()), Rc::downgrade(&owner));
    }
    pub fn namespace(&self) -> &ProfileNamespace {
        &self.cache.namespace
    }
    /// The `record` component of every cached record of `package` in
    /// `workspace`, sorted, as [`durable_store::DurableStore::records`]
    /// lists a store's.
    pub fn records(&self, package: &str, workspace: &str) -> Vec<String> {
        self.cache
            .borrow()
            .entries
            .keys()
            .filter_map(|key| match key {
                StorageKey::Record {
                    package: of,
                    workspace: in_,
                    record,
                } if of == package && in_ == workspace => Some(record.clone()),
                _ => None,
            })
            .collect()
    }
    pub fn get(&self, key: &StorageKey) -> Option<CachedBytes> {
        self.cache
            .borrow()
            .entries
            .get(key)
            .map(|entry| entry.current().clone())
    }
    /// Decode the current cached document without asserting that it is durable.
    pub fn document_heads(
        &self,
        key: &StorageKey,
    ) -> Option<Result<Vec<automerge::ChangeHash>, ()>> {
        let mut cache = self.cache.borrow_mut();
        let entry = cache.entries.get_mut(key)?;
        if entry.document_heads.is_none() {
            #[cfg(test)]
            DOCUMENT_DECODES.with(|count| count.set(count.get() + 1));
            let heads = automerge::AutoCommit::load(&entry.current().bytes)
                .map(|mut doc| doc.get_heads())
                .map_err(|_| ());
            entry.document_heads = Some(DocumentHeads::new(&entry.current().bytes, heads));
        }
        entry
            .document_heads
            .as_ref()
            .map(|metadata| metadata.heads.clone())
    }
    /// Initial durable bytes. Existing entries may never be replaced through hydration.
    pub fn hydrate(
        &self,
        key: StorageKey,
        bytes: Vec<u8>,
        actor: Option<Vec<u8>>,
    ) -> Result<(), String> {
        key.validate()?;
        let mut cache = self.cache.borrow_mut();
        if cache.entries.contains_key(&key) {
            return Err("storage key already hydrated or changed".into());
        }
        cache.entries.insert(
            key,
            Entry {
                committed: Some(CommittedValue {
                    revision: 0,
                    value: CachedBytes {
                        bytes: bytes.clone(),
                        actor: actor.clone(),
                    },
                }),
                value: CurrentValue::ordinary(CachedBytes { bytes, actor }),
                document_heads: None,
                revision: 0,
                durable: true,
                error: None,
            },
        );
        Ok(())
    }
    pub fn put(
        &self,
        key: StorageKey,
        bytes: Vec<u8>,
        actor: Option<Vec<u8>>,
    ) -> Result<(), String> {
        key.validate()?;
        let mut cache = self.cache.borrow_mut();
        if let Some(entry) = cache.entries.get(&key) {
            if entry.current().actor != actor {
                return Err("persistent actor binding mismatch".into());
            }
            if entry.current().bytes == bytes {
                return Ok(());
            }
        }
        cache.revision = cache
            .revision
            .checked_add(1)
            .ok_or("storage revision exhausted")?;
        let revision = cache.revision;
        // Move, never duplicate, a previous paid committed value and its guard.
        let committed = cache
            .entries
            .get_mut(&key)
            .and_then(|entry| entry.committed.take());
        cache.entries.insert(
            key,
            Entry {
                committed,
                value: CurrentValue::ordinary(CachedBytes { bytes, actor }),
                document_heads: None,
                revision,
                durable: false,
                error: None,
            },
        );
        Ok(())
    }
    /// Serialize a canonical document and queue its bytes through the ordinary
    /// persistence path. Callers cannot supply detached or unverified heads.
    pub fn put_document(
        &self,
        key: StorageKey,
        doc: &mut automerge::AutoCommit,
        actor: Option<Vec<u8>>,
    ) -> Result<(), String> {
        let bytes = doc.save();
        // Save serializes the full graph, including when the owner is viewing
        // historical heads through AutoCommit isolation.
        let heads = doc.document().get_heads();
        self.put(key.clone(), bytes, actor)?;
        // `put` synchronously installs these exact bytes. Retain the heads from
        // their canonical owner instead of decoding our own serialization.
        let mut cache = self.cache.borrow_mut();
        if let Some(entry) = cache.entries.get_mut(&key)
            && entry.document_heads.is_none()
        {
            entry.document_heads = Some(DocumentHeads::new(&entry.current().bytes, Ok(heads)));
        }
        Ok(())
    }
    pub fn pending_writes(&self) -> Vec<PendingWrite> {
        self.cache
            .borrow()
            .entries
            .iter()
            .filter(|(_, entry)| !entry.durable)
            .map(|(key, entry)| PendingWrite {
                namespace: self.cache.namespace.clone(),
                key: key.clone(),
                value: entry.current().clone(),
                revision: entry.revision,
                cache_id: self.cache.id,
                document_heads: entry.document_heads.clone(),
            })
            .collect()
    }
    /// Call `wake` each time what [`Self::flush`] says of `key` may have
    /// changed without input: when the host acknowledges its current bytes,
    /// or new bytes are durable at once ([`Self::durability_changed`]). How a
    /// window showing "Saving…" hears it saved; the same wake
    /// ([`durable_store::waiting::same_wake`]) given again is kept once.
    pub fn watch_durability(&self, key: &StorageKey, wake: durable_store::ReadyWake) {
        let mut cache = self.cache.borrow_mut();
        let watching = cache.watchers.entry(key.clone()).or_default();
        if !watching
            .iter()
            .any(|known| durable_store::waiting::same_wake(known, &wake))
        {
            watching.push(wake);
        }
    }
    /// Call everyone watching `key`'s durability.
    pub fn durability_changed(&self, key: &StorageKey) {
        let watching = self
            .cache
            .borrow()
            .watchers
            .get(key)
            .cloned()
            .unwrap_or_default();
        for wake in watching {
            wake();
        }
    }
    /// Acknowledging an old revision never makes newer bytes durable.
    pub fn acknowledge(
        &self,
        write: &PendingWrite,
        result: Result<(), String>,
    ) -> Result<(), String> {
        if write.namespace != self.cache.namespace || write.cache_id != self.cache.id {
            return Err("foreign persistence acknowledgement".into());
        }
        let resolved = self.resolve(write, result)?;
        if resolved {
            self.durability_changed(&write.key);
        }
        Ok(())
    }
    /// [`Self::acknowledge`] in the cache: whether it resolved the key's
    /// current bytes, durable or failed.
    fn resolve(&self, write: &PendingWrite, result: Result<(), String>) -> Result<bool, String> {
        let mut cache = self.cache.borrow_mut();
        let entry = cache
            .entries
            .get_mut(&write.key)
            .ok_or("unknown persistence key")?;
        if write.revision <= entry.revision
            && result.is_ok()
            && entry
                .committed
                .as_ref()
                .is_none_or(|committed| committed.revision < write.revision)
        {
            if write.value.actor != entry.current().actor {
                return Err("acknowledgement actor differs from persistent custody".into());
            }
            let same_revision = write.revision == entry.revision;
            if same_revision && write.value.bytes != entry.current().bytes {
                return Err("acknowledgement bytes differ from the queued write".into());
            }
            entry.committed = Some(CommittedValue {
                revision: write.revision,
                value: write.value.clone(),
            });
        }
        if write.revision != entry.revision {
            return Ok(false);
        }
        if write.value.bytes != entry.current().bytes || write.value.actor != entry.current().actor
        {
            return Err("acknowledgement bytes differ from the queued write".into());
        }
        match result {
            Ok(()) => {
                entry.durable = true;
                entry.error = None;
            }
            Err(error) => {
                entry.durable = false;
                entry.error = Some(error);
            }
        }
        Ok(true)
    }
    pub fn status(&self, key: &StorageKey) -> Option<StorageStatus> {
        self.cache
            .borrow()
            .entries
            .get(key)
            .map(|entry| StorageStatus {
                revision: entry.revision,
                durable: entry.durable,
                error: entry.error.clone(),
                committed: entry
                    .committed
                    .as_ref()
                    .map(|v| (v.revision, v.value.clone())),
            })
    }
    pub fn flush(&self, key: &StorageKey) -> Result<(), String> {
        let cache = self.cache.borrow();
        let entry = cache.entries.get(key).ok_or("storage key not queued")?;
        if entry.durable {
            Ok(())
        } else {
            Err(entry
                .error
                .clone()
                .unwrap_or_else(|| PENDING_ACKNOWLEDGEMENT.to_owned()))
        }
    }
}
