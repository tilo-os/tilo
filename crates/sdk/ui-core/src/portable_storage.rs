//! Hydrated, explicitly namespaced browser persistence. Durable writes are acknowledged
//! by the host only after its backing transaction commits.
#[cfg(test)]
thread_local! { static DOCUMENT_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
mod tests {
    use super::*;
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
    fn committed_document_heads_reuse_saved_metadata_only_after_exact_acknowledgement() {
        use automerge::transaction::Transactable;
        let cache = PortableStorage::new("profile", "device").unwrap();
        let key = StorageKey::Record {
            package: "test".into(),
            workspace: "room".into(),
            record: "record".into(),
        };
        let mut doc = automerge::AutoCommit::new();
        DOCUMENT_DECODES.with(|count| count.set(0));
        for value in 0..3 {
            let before = cache.committed_document_heads(&key);
            doc.put(automerge::ROOT, "value", value).unwrap();
            cache.put_document(key.clone(), &mut doc, None).unwrap();
            assert_eq!(
                cache.committed_document_heads(&key),
                before,
                "queued heads are not durable heads"
            );
            let write = cache.pending_writes().remove(0);
            cache.acknowledge(&write, Ok(())).unwrap();
            assert_eq!(
                cache.committed_document_heads(&key),
                Some(Ok(doc.base_heads()))
            );
        }
        assert_eq!(
            DOCUMENT_DECODES.with(|count| count.get()),
            0,
            "saved document heads must not require decoding our own bytes"
        );
    }
    #[test]
    fn old_commits_and_failed_new_writes_reuse_their_own_saved_heads() {
        use automerge::transaction::Transactable;
        let cache = PortableStorage::new("profile", "device").unwrap();
        let key = StorageKey::Record {
            package: "test".into(),
            workspace: "room".into(),
            record: "record".into(),
        };
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "value", 1).unwrap();
        cache.put_document(key.clone(), &mut doc, None).unwrap();
        let old_heads = doc.base_heads();
        let old = cache.pending_writes().remove(0);
        doc.put(automerge::ROOT, "value", 2).unwrap();
        cache.put_document(key.clone(), &mut doc, None).unwrap();
        let new = cache.pending_writes().remove(0);
        cache.acknowledge(&old, Ok(())).unwrap();
        DOCUMENT_DECODES.with(|count| count.set(0));
        for _ in 0..10 {
            assert_eq!(
                cache.committed_document_heads(&key),
                Some(Ok(old_heads.clone()))
            );
        }
        assert_eq!(DOCUMENT_DECODES.with(|count| count.get()), 0);
        cache.acknowledge(&new, Err("aborted".into())).unwrap();
        assert_eq!(cache.committed_document_heads(&key), Some(Ok(old_heads)));
        cache.acknowledge(&new, Ok(())).unwrap();
        assert_eq!(
            cache.committed_document_heads(&key),
            Some(Ok(doc.base_heads()))
        );
        cache.acknowledge(&old, Ok(())).unwrap();
        assert_eq!(
            cache.committed_document_heads(&key),
            Some(Ok(doc.base_heads()))
        );
        assert_eq!(DOCUMENT_DECODES.with(|count| count.get()), 0);
        cache.put(key.clone(), vec![1, 2, 3], None).unwrap();
        cache
            .acknowledge(&cache.pending_writes().remove(0), Ok(()))
            .unwrap();
        for _ in 0..3 {
            assert_eq!(cache.committed_document_heads(&key), Some(Err(())));
        }
        assert_eq!(DOCUMENT_DECODES.with(|count| count.get()), 1);
    }
    #[test]
    fn changed_old_write_bytes_cannot_reuse_detached_head_metadata() {
        use automerge::transaction::Transactable;
        for corrupt in [false, true] {
            let cache = PortableStorage::new("profile", "device").unwrap();
            let key = StorageKey::Record {
                package: "test".into(),
                workspace: "room".into(),
                record: "record".into(),
            };
            let mut doc = automerge::AutoCommit::new();
            doc.put(automerge::ROOT, "value", 1).unwrap();
            cache.put_document(key.clone(), &mut doc, None).unwrap();
            let mut old = cache.pending_writes().remove(0);
            doc.put(automerge::ROOT, "value", 2).unwrap();
            let replacement = doc.save();
            let expected = if corrupt {
                Err(())
            } else {
                Ok(doc.get_heads())
            };
            old.value.bytes = if corrupt { vec![1, 2, 3] } else { replacement };
            doc.put(automerge::ROOT, "value", 3).unwrap();
            cache.put_document(key.clone(), &mut doc, None).unwrap();
            cache.acknowledge(&old, Ok(())).unwrap();
            DOCUMENT_DECODES.with(|count| count.set(0));
            for _ in 0..3 {
                assert_eq!(cache.committed_document_heads(&key), Some(expected.clone()));
            }
            assert_eq!(DOCUMENT_DECODES.with(|count| count.get()), 1);
            assert!(
                cache.flush(&key).is_err(),
                "old acknowledgement cannot commit newer bytes"
            );
            assert_eq!(cache.document_heads(&key), Some(Ok(doc.get_heads())));
        }
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
    fn pending_summaries_and_captured_heads_match_the_copying_forms() {
        use automerge::transaction::Transactable;
        let cache = PortableStorage::new("profile", "device").unwrap();
        assert!(!cache.has_pending_writes());
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
        assert!(cache.has_pending_writes());
        let writes = cache.pending_writes();
        let mut sizes = cache.pending_write_sizes();
        sizes.sort_by(|a, b| format!("{:?}", a.0).cmp(&format!("{:?}", b.0)));
        let mut expected: Vec<_> = writes
            .iter()
            .map(|w| (w.key.clone(), w.value.bytes.len()))
            .collect();
        expected.sort_by(|a, b| format!("{:?}", a.0).cmp(&format!("{:?}", b.0)));
        assert_eq!(sizes, expected);
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
        assert!(!cache.has_pending_writes());
        assert!(cache.pending_write_sizes().is_empty());
    }
    #[test]
    fn status_meta_and_committed_borrow_match_the_copying_status() {
        let cache = PortableStorage::new("profile", "device").unwrap();
        let key = StorageKey::Admission {
            workspace: "room".into(),
        };
        assert_eq!(cache.status_meta(&key), None);
        assert_eq!(cache.with_committed(&key, |_, _| ()), None);
        cache.put(key.clone(), vec![1], None).unwrap();
        let first = cache.pending_writes().remove(0);
        assert_eq!(cache.with_committed(&key, |_, _| ()), None);
        cache.put(key.clone(), vec![2], None).unwrap();
        cache.acknowledge(&first, Ok(())).unwrap();
        let second = cache.pending_writes().remove(0);
        cache.acknowledge(&second, Err("quota".into())).unwrap();
        let status = cache.status(&key).unwrap();
        let meta = cache.status_meta(&key).unwrap();
        assert_eq!(
            (
                meta.revision,
                meta.durable,
                meta.error.clone(),
                meta.committed_revision
            ),
            (
                status.revision,
                status.durable,
                status.error.clone(),
                status.committed.as_ref().map(|c| c.0)
            )
        );
        assert_eq!(
            cache.with_committed(&key, |revision, value| (revision, value.bytes.clone())),
            status
                .committed
                .map(|(revision, value)| (revision, value.bytes))
        );
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
    /// Private device action intents and observations; never shared app state.
    PrivateActions {
        workspace: String,
    },
}
impl StorageKey {
    fn validate(&self) -> Result<(), String> {
        let ids = match self {
            Self::Admission { workspace }
            | Self::PrivateActions { workspace }
            | Self::View { workspace, .. } => vec![workspace],
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
            StorageKey::Admission { workspace }
            | StorageKey::PrivateActions { workspace }
            | StorageKey::View { workspace, .. } => (workspace, None, None),
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
/// [`StorageStatus`] without the committed bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageMeta {
    pub revision: u64,
    pub durable: bool,
    pub error: Option<String>,
    pub committed_revision: Option<u64>,
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
    #[cfg(unix)]
    provenance: Option<crate::host_worker::private_actions::PrivateCommitStamp>,
    revision: u64,
    value: CachedBytes,
}
enum CurrentValue {
    Owned { value: CachedBytes },
    Committed,
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
    // Independently keyed by `committed`: newer queued bytes are not durable.
    committed_document_heads: Option<Result<Vec<automerge::ChangeHash>, ()>>,
    revision: u64,
    durable: bool,
    error: Option<String>,
    committed: Option<CommittedValue>,
}
impl Entry {
    #[expect(
        clippy::expect_used,
        reason = "an entry is only marked Committed together with its committed alias"
    )]
    fn current(&self) -> &CachedBytes {
        match &self.value {
            CurrentValue::Owned { value, .. } => value,
            CurrentValue::Committed => {
                &self
                    .committed
                    .as_ref()
                    .expect("private committed alias")
                    .value
            }
        }
    }
}
#[derive(Default)]
struct Cache {
    entries: BTreeMap<StorageKey, Entry>,
    revision: u64,
    owners: BTreeMap<(String, String), std::rc::Weak<RefCell<crate::record::RecordSet>>>,
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
    fn try_borrow(&self) -> Result<std::cell::Ref<'_, Cache>, std::cell::BorrowError> {
        self.inner.try_borrow()
    }
    fn try_borrow_mut(&self) -> Result<std::cell::RefMut<'_, Cache>, std::cell::BorrowMutError> {
        self.inner.try_borrow_mut()
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
    pub(crate) fn record_owner(
        &self,
        package: &str,
        workspace: &str,
    ) -> Option<Rc<RefCell<crate::record::RecordSet>>> {
        self.cache
            .borrow()
            .owners
            .get(&(package.into(), workspace.into()))
            .and_then(std::rc::Weak::upgrade)
    }
    pub(crate) fn register_record_owner(
        &self,
        package: &str,
        workspace: &str,
        owner: &Rc<RefCell<crate::record::RecordSet>>,
    ) {
        self.cache
            .borrow_mut()
            .owners
            .insert((package.into(), workspace.into()), Rc::downgrade(owner));
    }
    /// Every live record owner of `workspace` in this cache: the stores the
    /// page's mounts share, for a host replicating the room itself.
    pub fn record_stores(&self, workspace: &str) -> Vec<crate::record::RecordStore> {
        self.cache
            .borrow()
            .owners
            .iter()
            .filter(|((_, home), _)| home == workspace)
            .filter_map(|(_, owner)| owner.upgrade())
            .map(crate::record::RecordStore::from_owner)
            .collect()
    }
    pub fn namespace(&self) -> &ProfileNamespace {
        &self.cache.namespace
    }
    pub fn get(&self, key: &StorageKey) -> Option<CachedBytes> {
        self.cache
            .borrow()
            .entries
            .get(key)
            .map(|entry| entry.current().clone())
    }
    /// Decode the current cached document without asserting that it is durable.
    pub(crate) fn document_heads(
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
    /// Heads of the exact acknowledged snapshot, never a newer queued value.
    /// Reuses canonical save metadata only when the committed bytes match it.
    pub fn committed_document_heads(
        &self,
        key: &StorageKey,
    ) -> Option<Result<Vec<automerge::ChangeHash>, ()>> {
        let mut cache = self.cache.borrow_mut();
        let entry = cache.entries.get_mut(key)?;
        let committed = &entry.committed.as_ref()?.value;
        if entry.committed_document_heads.is_none() {
            let same_bytes = committed.bytes == entry.current().bytes;
            if same_bytes {
                entry.committed_document_heads = entry
                    .document_heads
                    .as_ref()
                    .map(|metadata| metadata.heads.clone());
            }
            if entry.committed_document_heads.is_none() {
                #[cfg(test)]
                DOCUMENT_DECODES.with(|count| count.set(count.get() + 1));
                entry.committed_document_heads = Some(
                    automerge::AutoCommit::load(&committed.bytes)
                        .map(|mut doc| doc.get_heads())
                        .map_err(|_| ()),
                );
            }
            if same_bytes && entry.document_heads.is_none() {
                entry.document_heads = entry
                    .committed_document_heads
                    .clone()
                    .map(|heads| DocumentHeads::new(&entry.current().bytes, heads));
            }
        }
        entry.committed_document_heads.clone()
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
                    #[cfg(unix)]
                    provenance: None,
                    revision: 0,
                    value: CachedBytes {
                        bytes: bytes.clone(),
                        actor: actor.clone(),
                    },
                }),
                value: CurrentValue::ordinary(CachedBytes { bytes, actor }),
                document_heads: None,
                committed_document_heads: None,
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
        let (committed, committed_document_heads) = match cache.entries.get_mut(&key) {
            Some(entry) => (
                entry.committed.take(),
                entry.committed_document_heads.take(),
            ),
            None => (None, None),
        };
        cache.entries.insert(
            key,
            Entry {
                committed,
                value: CurrentValue::ordinary(CachedBytes { bytes, actor }),
                document_heads: None,
                committed_document_heads,
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
    /// Whether any value awaits host acknowledgement, without copying the
    /// queued values (`pending_writes` copies every queued document).
    pub fn has_pending_writes(&self) -> bool {
        self.cache
            .borrow()
            .entries
            .values()
            .any(|entry| !entry.durable)
    }
    /// Key and byte length of every queued value, without copying the bytes.
    pub fn pending_write_sizes(&self) -> Vec<(StorageKey, usize)> {
        self.cache
            .borrow()
            .entries
            .iter()
            .filter(|(_, entry)| !entry.durable)
            .map(|(key, entry)| (key.clone(), entry.current().bytes.len()))
            .collect()
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
    /// Acknowledging an old revision never makes newer bytes durable.
    pub fn acknowledge(
        &self,
        write: &PendingWrite,
        result: Result<(), String>,
    ) -> Result<(), String> {
        if write.namespace != self.cache.namespace || write.cache_id != self.cache.id {
            return Err("foreign persistence acknowledgement".into());
        }
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
                #[cfg(unix)]
                provenance: None,
                revision: write.revision,
                value: write.value.clone(),
            });
            entry.committed_document_heads = if write.value.bytes == entry.current().bytes {
                entry
                    .document_heads
                    .as_ref()
                    .map(|metadata| metadata.heads.clone())
            } else {
                write
                    .document_heads
                    .as_ref()
                    .filter(|metadata| metadata.fingerprint == blake3::hash(&write.value.bytes))
                    .map(|metadata| metadata.heads.clone())
            };
        }
        if write.revision != entry.revision {
            return Ok(());
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
        Ok(())
    }
    /// [`Self::status`] without copying the committed bytes: hot readers
    /// (health reports, delivery checks) run several times per frame and the
    /// admission journal grows with the session.
    pub fn status_meta(&self, key: &StorageKey) -> Option<StorageMeta> {
        self.cache
            .borrow()
            .entries
            .get(key)
            .map(|entry| StorageMeta {
                revision: entry.revision,
                durable: entry.durable,
                error: entry.error.clone(),
                committed_revision: entry.committed.as_ref().map(|v| v.revision),
            })
    }
    /// Borrow the committed value (revision and bytes) without copying it.
    /// The closure must not re-enter this storage.
    pub fn with_committed<R>(
        &self,
        key: &StorageKey,
        read: impl FnOnce(u64, &CachedBytes) -> R,
    ) -> Option<R> {
        let cache = self.cache.borrow();
        let committed = cache.entries.get(key)?.committed.as_ref()?;
        Some(read(committed.revision, &committed.value))
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
                .unwrap_or_else(|| "persistence pending host acknowledgement".into()))
        }
    }
}

/// Immutable capture of exactly one pending private sender value.
#[cfg(unix)]
pub struct PrivateActionsCapture {
    cache: Rc<CacheBinding>,
    cache_id: uuid::Uuid,
    namespace: ProfileNamespace,
    key: StorageKey,
    revision: u64,
    value: CachedBytes,
}
#[cfg(unix)]
impl PrivateActionsCapture {
    fn entry<'a>(
        cache: &'a Cache,
        workspace: &str,
    ) -> Result<
        Option<(&'a StorageKey, &'a Entry)>,
        crate::host_worker::private_actions::SenderStorageCode,
    > {
        use crate::host_worker::private_actions::SenderStorageCode as Code;
        if workspace.trim().is_empty() || workspace.chars().any(char::is_control) {
            return Err(Code::InvalidScope);
        }
        let found = cache
            .entries
            .iter()
            .find(|(key, _)| matches!(key,StorageKey::PrivateActions{workspace:w} if w==workspace));
        let Some((key, entry)) = found else {
            return Ok(None);
        };
        if entry.current().actor.is_some() {
            return Err(Code::InvalidScope);
        }
        if entry.durable {
            return Ok(None);
        }
        if entry.current().bytes.len() > 4 * 1024 * 1024 {
            return Err(Code::Capacity);
        }
        Ok(Some((key, entry)))
    }
    pub fn capture(
        cache: &PortableStorage,
        workspace: &str,
    ) -> Result<Option<Self>, crate::host_worker::private_actions::SenderStorageCode> {
        use crate::host_worker::private_actions::{
            SenderStorageCode as Code, copy_bytes, copy_string,
        };
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let inner = cache.cache.try_borrow().map_err(|_| Code::Busy)?;
        let Some((_, entry)) = Self::entry(&inner, workspace)? else {
            return Ok(None);
        };
        let namespace = match &cache.cache.namespace {
            ProfileNamespace::Native(path) => ProfileNamespace::Native(
                std::ffi::OsString::from_vec(copy_bytes(path.as_os_str().as_bytes())?).into(),
            ),
            ProfileNamespace::Portable { profile, device } => ProfileNamespace::Portable {
                profile: copy_string(profile)?,
                device: copy_string(device)?,
            },
        };
        let key = StorageKey::PrivateActions {
            workspace: copy_string(workspace)?,
        };
        let value = CachedBytes {
            bytes: copy_bytes(&entry.current().bytes)?,
            actor: None,
        };
        Ok(Some(Self {
            cache: Rc::clone(&cache.cache),
            cache_id: cache.cache.id,
            namespace,
            key,
            revision: entry.revision,
            value,
        }))
    }
    pub(crate) fn capture_for_revision(
        cache: &PortableStorage,
        workspace: &str,
        expected: u64,
    ) -> Result<Self, crate::host_worker::private_actions::SenderStorageCode> {
        use crate::host_worker::private_actions::SenderStorageCode as Code;
        // Keep this selected-entry borrow through the existing capture/copy engine.
        let inner = cache.cache.try_borrow().map_err(|_| Code::Busy)?;
        let (_, entry) = Self::entry(&inner, workspace)?.ok_or(Code::StaleCapture)?;
        if entry.revision != expected {
            return Err(Code::StaleCapture);
        }
        Self::capture(cache, workspace)?.ok_or(Code::StaleCapture)
    }
    fn matches(&self, cache: &Cache) -> bool {
        // The original cache Rc and immutable namespace/cache UUID are retained,
        // never reconstructed from caller-supplied PendingWrite fields.
        self.cache.id == self.cache_id
            && self.cache.namespace == self.namespace
            && cache.entries.get(&self.key).is_some_and(|entry| {
                !entry.durable
                    && entry.revision == self.revision
                    && entry.current().bytes == self.value.bytes
                    && entry.current().actor.is_none()
                    && self.value.actor.is_none()
            })
    }
    pub(crate) fn check_current(
        &self,
    ) -> Result<(), crate::host_worker::private_actions::SenderStorageCode> {
        use crate::host_worker::private_actions::SenderStorageCode as Code;
        let cache = self.cache.try_borrow().map_err(|_| Code::Busy)?;
        if self.matches(&cache) {
            Ok(())
        } else {
            Err(Code::StaleCapture)
        }
    }
    pub fn is_current(&self) -> bool {
        self.cache
            .try_borrow()
            .is_ok_and(|cache| self.matches(&cache))
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn bytes(&self) -> &[u8] {
        &self.value.bytes
    }
    #[cfg(test)]
    pub(super) fn finish(
        &mut self,
    ) -> Result<u64, crate::host_worker::private_actions::SenderStorageCode> {
        self.finish_physical(None)
    }
    pub(crate) fn bound_to(&self, cache: &PortableStorage, workspace: &str) -> bool {
        Rc::ptr_eq(&self.cache, &cache.cache)
            && matches!(&self.key, StorageKey::PrivateActions { workspace: w } if w == workspace)
    }
    pub(crate) fn finish_physical(
        &mut self,
        stamp: Option<crate::host_worker::private_actions::PrivateCommitStamp>,
    ) -> Result<u64, crate::host_worker::private_actions::SenderStorageCode> {
        use crate::host_worker::private_actions::SenderStorageCode as Code;
        let mut cache = self.cache.try_borrow_mut().map_err(|_| Code::Busy)?;
        if !self.matches(&cache) {
            return Err(Code::StaleCapture);
        }
        let entry = cache.entries.get_mut(&self.key).ok_or(Code::StaleCapture)?;
        let value = CachedBytes {
            bytes: std::mem::take(&mut self.value.bytes),
            actor: None,
        };
        // Free capture metadata before moving the bytes into the committed value.
        self.namespace = ProfileNamespace::Native(std::path::PathBuf::new());
        if let StorageKey::PrivateActions { workspace } = &mut self.key {
            *workspace = String::new();
        }
        entry.committed = Some(CommittedValue {
            #[cfg(unix)]
            provenance: stamp,
            revision: self.revision,
            value,
        });
        entry.value = CurrentValue::Committed;
        entry.durable = true;
        entry.error = None;
        entry.committed_document_heads = None;
        Ok(self.revision)
    }
}

#[cfg(all(test, unix))]
mod private_capture_tests {
    use super::*;
    use crate::host_worker::private_actions::SenderStorageCode as Code;
    #[test]
    fn private_actions_storage_capture_rejects_foreign_binding_and_borrow_contention() {
        let cache = PortableStorage::new("profile", "device").unwrap();
        let key = StorageKey::PrivateActions {
            workspace: "room".into(),
        };
        cache.put(key, b"original".to_vec(), None).unwrap();
        let mut capture = PrivateActionsCapture::capture(&cache, "room")
            .unwrap()
            .unwrap();
        // Inert unit fault injection into closed resource fields; no authority.
        let original = capture.cache_id;
        capture.cache_id = uuid::Uuid::new_v4();
        assert!(!capture.is_current());
        assert_eq!(capture.finish(), Err(Code::StaleCapture));
        capture.cache_id = original;
        capture.namespace = ProfileNamespace::Portable {
            profile: "other".into(),
            device: "device".into(),
        };
        assert!(!capture.is_current());
        drop(capture);
        let mut capture = PrivateActionsCapture::capture(&cache, "room")
            .unwrap()
            .unwrap();
        let held = cache.cache.borrow_mut();
        assert!(!capture.is_current());
        assert_eq!(capture.finish(), Err(Code::Busy));
        assert!(matches!(
            PrivateActionsCapture::capture(&cache, "room"),
            Err(Code::Busy)
        ));
        drop(held);
        assert!(capture.is_current());
    }
}

#[cfg(all(test, unix))]
mod private_capture_transfer_tests {
    use super::*;
    #[test]
    fn private_actions_storage_finish_transfers_backing_without_allocation() {
        let cache = PortableStorage::new("profile", "device").unwrap();
        let key = StorageKey::PrivateActions {
            workspace: "room".into(),
        };
        cache.put(key.clone(), vec![7; 4096], None).unwrap();
        let mut saved = PrivateActionsCapture::capture(&cache, "room")
            .unwrap()
            .unwrap();
        let original = saved.bytes().as_ptr();
        let (result, stats) =
            crate::semantic_document_read::retirement::measure_test_allocations(|| saved.finish());
        assert!(result.is_ok());
        assert_eq!(stats.allocations, 0);
        drop(saved);
        assert_eq!(
            cache.cache.borrow().entries[&key]
                .committed
                .as_ref()
                .unwrap()
                .value
                .bytes
                .as_ptr(),
            original
        );
        cache.put(key.clone(), vec![9; 2048], None).unwrap();
        assert_eq!(
            cache.cache.borrow().entries[&key]
                .committed
                .as_ref()
                .unwrap()
                .value
                .bytes
                .as_ptr(),
            original
        );
    }
}

#[cfg(unix)]
pub(crate) struct PrivateCurrentBorrow<'a> {
    committed: std::cell::Ref<'a, CommittedValue>,
}
#[cfg(unix)]
impl PrivateCurrentBorrow<'_> {
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.committed.value.bytes
    }
}
#[cfg(unix)]
impl PortableStorage {
    fn private_entry<'a>(cache: &'a Cache, workspace: &str) -> Option<&'a Entry> {
        cache.entries.iter().find_map(|(key, entry)| {
            matches!(key, StorageKey::PrivateActions {workspace:w} if w==workspace).then_some(entry)
        })
    }
    fn private_matches(
        entry: &Entry,
        revision: u64,
        stamp: &crate::host_worker::private_actions::PrivateCommitStamp,
    ) -> bool {
        entry.revision == revision
            && entry.durable
            && entry.error.is_none()
            && entry.current().actor.is_none()
            && entry.committed.as_ref().is_some_and(|c| {
                c.revision == revision
                    && c.provenance.as_ref() == Some(stamp)
                    && c.value.actor.is_none()
                    && entry.current().bytes == c.value.bytes
            })
    }
    pub(crate) fn private_current(
        &self,
        workspace: &str,
        revision: u64,
        stamp: &crate::host_worker::private_actions::PrivateCommitStamp,
    ) -> Result<PrivateCurrentBorrow<'_>, crate::host_worker::private_actions::SenderStorageCode>
    {
        use crate::host_worker::private_actions::SenderStorageCode as Code;
        let cache = self.cache.try_borrow().map_err(|_| Code::Busy)?;
        let entry = Self::private_entry(&cache, workspace).ok_or(Code::StaleCapture)?;
        if !Self::private_matches(entry, revision, stamp) {
            return Err(Code::StaleCapture);
        }
        let committed = std::cell::Ref::filter_map(cache, |cache| {
            Self::private_entry(cache, workspace).and_then(|entry| entry.committed.as_ref())
        })
        .map_err(|_| Code::StaleCapture)?;
        Ok(PrivateCurrentBorrow { committed })
    }
    pub(crate) fn private_hydrate(
        &self,
        workspace: &str,
        bytes: &mut Vec<u8>,
        payload: std::ops::Range<usize>,
        stamp: crate::host_worker::private_actions::PrivateCommitStamp,
    ) -> Result<(), crate::host_worker::private_actions::SenderStorageCode> {
        use crate::host_worker::private_actions::{SenderStorageCode as Code, copy_string};
        let mut cache = self.cache.try_borrow_mut().map_err(|_| Code::Busy)?;
        if Self::private_entry(&cache, workspace).is_some() {
            return Err(Code::StaleCapture);
        }
        if payload.start > payload.end || payload.end > bytes.len() {
            return Err(Code::Capacity);
        }
        let key = StorageKey::PrivateActions {
            workspace: copy_string(workspace)?,
        };
        // No fallible semantic operation after this point. Move the original backing.
        let length = payload.len();
        bytes.copy_within(payload, 0);
        bytes.truncate(length);
        let committed = CommittedValue {
            revision: 0,
            value: CachedBytes {
                bytes: std::mem::take(bytes),
                actor: None,
            },
            provenance: Some(stamp),
        };
        cache.entries.insert(
            key,
            Entry {
                value: CurrentValue::Committed,
                document_heads: None,
                committed_document_heads: None,
                revision: 0,
                durable: true,
                error: None,
                committed: Some(committed),
            },
        );
        Ok(())
    }
    pub(crate) fn private_queue(
        &self,
        workspace: &str,
        revision: u64,
        stamp: &crate::host_worker::private_actions::PrivateCommitStamp,
        payload: &mut Vec<u8>,
    ) -> Result<Option<u64>, crate::host_worker::private_actions::SenderStorageCode> {
        use crate::host_worker::private_actions::SenderStorageCode as Code;
        let mut cache = self.cache.try_borrow_mut().map_err(|_| Code::Busy)?;
        let entry = Self::private_entry(&cache, workspace).ok_or(Code::StaleCapture)?;
        if !Self::private_matches(entry, revision, stamp) {
            return Err(Code::StaleCapture);
        }
        if payload.len() > 4 * 1024 * 1024 {
            return Err(Code::Capacity);
        }
        if entry.current().bytes == *payload {
            return Ok(None);
        }
        let next = cache.revision.checked_add(1).ok_or(Code::Capacity)?;
        let entry = cache
            .entries
            .iter_mut()
            .find_map(|(key, entry)| {
                matches!(key,StorageKey::PrivateActions{workspace:w} if w==workspace)
                    .then_some(entry)
            })
            .ok_or(Code::StaleCapture)?;
        entry.value = CurrentValue::Owned {
            value: CachedBytes {
                bytes: std::mem::take(payload),
                actor: None,
            },
        };
        entry.revision = next;
        entry.durable = false;
        entry.error = None;
        entry.document_heads = None;
        cache.revision = next;
        Ok(Some(next))
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod private_physical_current_tests {
    use super::*;
    use crate::host_worker::private_actions::{SenderStorageCode, committed_test_fixture};
    #[test]
    fn private_actions_storage_consuming_borrow_conflict_and_generic_replacement_deny() {
        let (_root, state, cache) = committed_test_fixture(b"original");
        let borrowed = cache.cache.borrow_mut();
        assert!(matches!(
            state.borrow_current(),
            Err(SenderStorageCode::Busy)
        ));
        drop(borrowed);
        let key = StorageKey::PrivateActions {
            workspace: state.workspace().into(),
        };
        cache
            .put(key.clone(), b"replacement".to_vec(), None)
            .unwrap();
        let write = cache
            .pending_writes()
            .into_iter()
            .find(|v| v.key == key)
            .unwrap();
        cache.acknowledge(&write, Ok(())).unwrap();
        assert!(cache.flush(&key).is_ok());
        assert!(matches!(
            state.borrow_current(),
            Err(SenderStorageCode::StaleCapture)
        ));
        let cache_borrow = cache.cache.borrow();
        assert!(
            cache_borrow
                .entries
                .get(&key)
                .unwrap()
                .committed
                .as_ref()
                .unwrap()
                .provenance
                .is_none()
        );
    }
}
