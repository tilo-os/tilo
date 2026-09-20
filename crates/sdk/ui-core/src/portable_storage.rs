//! Hydrated, explicitly namespaced browser persistence. Durable writes are acknowledged
//! by the host only after its backing transaction commits.
#[cfg(test)]
thread_local! { static DOCUMENT_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
mod tests {
    use super::*;
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
            Self::Admission { workspace } | Self::PrivateActions { workspace } => vec![workspace],
            Self::Primary { package, workspace } => vec![package, workspace],
            Self::Record {
                package,
                workspace,
                record,
            } => vec![package, workspace, record],
            Self::View { workspace, .. } => vec![workspace],
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
struct Entry {
    value: CachedBytes,
    // Derived only from this entry's immutable bytes; replacement invalidates it.
    document_heads: Option<DocumentHeads>,
    // Independently keyed by `committed`: newer queued bytes are not durable.
    committed_document_heads: Option<Result<Vec<automerge::ChangeHash>, ()>>,
    revision: u64,
    durable: bool,
    error: Option<String>,
    committed: Option<(u64, CachedBytes)>,
}
#[derive(Default)]
struct Cache {
    entries: BTreeMap<StorageKey, Entry>,
    revision: u64,
    owners: BTreeMap<(String, String), std::rc::Weak<RefCell<crate::record::RecordSet>>>,
}
/// Clone one cache throughout a browser host. Hydrate before opening its owners;
/// only acknowledge writes after the external storage transaction commits.
#[derive(Clone)]
pub struct PortableStorage {
    namespace: ProfileNamespace,
    id: uuid::Uuid,
    cache: Rc<RefCell<Cache>>,
}
impl PortableStorage {
    pub fn new(profile: &str, device: &str) -> Result<Self, String> {
        validate(profile)?;
        validate(device)?;
        Ok(Self {
            namespace: ProfileNamespace::Portable {
                profile: profile.into(),
                device: device.into(),
            },
            id: uuid::Uuid::new_v4(),
            cache: Rc::default(),
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
    pub fn namespace(&self) -> &ProfileNamespace {
        &self.namespace
    }
    pub fn get(&self, key: &StorageKey) -> Option<CachedBytes> {
        self.cache
            .borrow()
            .entries
            .get(key)
            .map(|entry| entry.value.clone())
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
            let heads = automerge::AutoCommit::load(&entry.value.bytes)
                .map(|mut doc| doc.get_heads())
                .map_err(|_| ());
            entry.document_heads = Some(DocumentHeads::new(&entry.value.bytes, heads));
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
        let (_, committed) = entry.committed.as_ref()?;
        if entry.committed_document_heads.is_none() {
            let same_bytes = committed.bytes == entry.value.bytes;
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
                    .map(|heads| DocumentHeads::new(&entry.value.bytes, heads));
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
                committed: Some((
                    0,
                    CachedBytes {
                        bytes: bytes.clone(),
                        actor: actor.clone(),
                    },
                )),
                value: CachedBytes { bytes, actor },
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
            if entry.value.actor != actor {
                return Err("persistent actor binding mismatch".into());
            }
            if entry.value.bytes == bytes {
                return Ok(());
            }
        }
        cache.revision = cache
            .revision
            .checked_add(1)
            .ok_or("storage revision exhausted")?;
        let revision = cache.revision;
        let committed = cache
            .entries
            .get(&key)
            .and_then(|entry| entry.committed.clone());
        let committed_document_heads = cache
            .entries
            .get(&key)
            .and_then(|entry| entry.committed_document_heads.clone());
        cache.entries.insert(
            key,
            Entry {
                committed,
                value: CachedBytes { bytes, actor },
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
    pub(crate) fn put_document(
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
        let entry = cache
            .entries
            .get_mut(&key)
            .expect("successful put installs the document");
        if entry.document_heads.is_none() {
            entry.document_heads = Some(DocumentHeads::new(&entry.value.bytes, Ok(heads)));
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
                namespace: self.namespace.clone(),
                key: key.clone(),
                value: entry.value.clone(),
                revision: entry.revision,
                cache_id: self.id,
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
        if write.namespace != self.namespace || write.cache_id != self.id {
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
                .is_none_or(|(revision, _)| *revision < write.revision)
        {
            if write.value.actor != entry.value.actor {
                return Err("acknowledgement actor differs from persistent custody".into());
            }
            if write.revision == entry.revision && write.value.bytes != entry.value.bytes {
                return Err("acknowledgement bytes differ from the queued write".into());
            }
            entry.committed = Some((write.revision, write.value.clone()));
            entry.committed_document_heads = if write.value.bytes == entry.value.bytes {
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
        if write.value.bytes != entry.value.bytes || write.value.actor != entry.value.actor {
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
    pub fn status(&self, key: &StorageKey) -> Option<StorageStatus> {
        self.cache
            .borrow()
            .entries
            .get(key)
            .map(|entry| StorageStatus {
                revision: entry.revision,
                durable: entry.durable,
                error: entry.error.clone(),
                committed: entry.committed.clone(),
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
