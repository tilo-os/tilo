//! Hydrated, explicitly namespaced browser persistence. Durable writes are acknowledged
//! by the host only after its backing transaction commits.
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_retains_exact_prior_commit_across_newer_pending_and_failed_writes() {
        let cache=PortableStorage::new("profile","device").unwrap();
        let key=StorageKey::Admission{workspace:"room".into()};
        cache.put(key.clone(),vec![1],None).unwrap();let first=cache.pending_writes().remove(0);
        cache.put(key.clone(),vec![2],None).unwrap();
        cache.acknowledge(&first,Ok(())).unwrap();
        let status=cache.status(&key).unwrap();assert!(!status.durable);assert_eq!(status.committed.unwrap().1.bytes,vec![1]);
        let second=cache.pending_writes().remove(0);cache.acknowledge(&second,Err("quota".into())).unwrap();
        let status=cache.status(&key).unwrap();assert_eq!(status.error.as_deref(),Some("quota"));assert_eq!(status.committed.unwrap().0,first.revision);
        cache.acknowledge(&second,Ok(())).unwrap();let status=cache.status(&key).unwrap();assert!(status.durable);assert_eq!(status.committed.unwrap().1.bytes,vec![2]);
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
    Admission { workspace: String },
    Primary { package: String, workspace: String },
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
    PrivateActions { workspace: String },
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
}
#[derive(Clone, Debug)]
pub struct StorageStatus {
    pub revision: u64,
    pub durable: bool,
    pub error: Option<String>,
    pub committed: Option<(u64, CachedBytes)>,
}
struct Entry {
    value: CachedBytes,
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
                committed: Some((0,CachedBytes{bytes:bytes.clone(),actor:actor.clone()})),
                value: CachedBytes { bytes, actor },
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
        let committed=cache.entries.get(&key).and_then(|entry|entry.committed.clone());
        cache.entries.insert(
            key,
            Entry {
                committed,
                value: CachedBytes { bytes, actor },
                revision,
                durable: false,
                error: None,
            },
        );
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
        if write.revision<=entry.revision && result.is_ok() && entry.committed.as_ref().is_none_or(|(revision,_)|*revision<write.revision){
            if write.value.actor!=entry.value.actor{return Err("acknowledgement actor differs from persistent custody".into());}
            if write.revision==entry.revision && write.value.bytes!=entry.value.bytes{return Err("acknowledgement bytes differ from the queued write".into());}
            entry.committed=Some((write.revision,write.value.clone()));
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
    pub fn status(&self,key:&StorageKey)->Option<StorageStatus>{
        self.cache.borrow().entries.get(key).map(|entry|StorageStatus{revision:entry.revision,durable:entry.durable,error:entry.error.clone(),committed:entry.committed.clone()})
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
