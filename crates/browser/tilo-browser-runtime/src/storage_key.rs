//! The existing named JSON-string key contract, shared by real browser hydration and native tests.
use ui::portable_storage::{PortableStorage, StorageKey};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum DiskKey {
    Admission {
        workspace: String,
    },
    Record {
        package: String,
        workspace: String,
        record: String,
    },
    Primary {
        package: String,
        workspace: String,
    },
    View {
        workspace: String,
        instance: u64,
    },
    PrivateActions {
        workspace: String,
    },
}
impl From<&StorageKey> for DiskKey {
    fn from(k: &StorageKey) -> Self {
        match k {
            StorageKey::PrivateActions { workspace } => Self::PrivateActions {
                workspace: workspace.clone(),
            },
            StorageKey::Admission { workspace } => Self::Admission {
                workspace: workspace.clone(),
            },
            StorageKey::Record {
                package,
                workspace,
                record,
            } => Self::Record {
                package: package.clone(),
                workspace: workspace.clone(),
                record: record.clone(),
            },
            StorageKey::Primary { package, workspace } => Self::Primary {
                package: package.clone(),
                workspace: workspace.clone(),
            },
            StorageKey::View {
                workspace,
                instance,
            } => Self::View {
                workspace: workspace.clone(),
                instance: *instance,
            },
        }
    }
}
impl From<DiskKey> for StorageKey {
    fn from(k: DiskKey) -> Self {
        match k {
            DiskKey::PrivateActions { workspace } => Self::PrivateActions { workspace },
            DiskKey::Admission { workspace } => Self::Admission { workspace },
            DiskKey::Record {
                package,
                workspace,
                record,
            } => Self::Record {
                package,
                workspace,
                record,
            },
            DiskKey::Primary { package, workspace } => Self::Primary { package, workspace },
            DiskKey::View {
                workspace,
                instance,
            } => Self::View {
                workspace,
                instance,
            },
        }
    }
}
pub(crate) fn hydrate_entry(
    storage: &PortableStorage,
    raw_key: Option<&str>,
    bytes: Vec<u8>,
    actor: Option<Vec<u8>>,
) -> Result<(), String> {
    let key: DiskKey = serde_json::from_str(raw_key.ok_or("missing storage key")?)
        .map_err(|error| error.to_string())?;
    let key = StorageKey::from(key);
    if storage.get(&key).is_none() {
        storage.hydrate(key, bytes, actor)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serialized_private_and_existing_keys_reopen_in_actual_cache() {
        let storage = PortableStorage::new("regression-profile", "regression-device").unwrap();
        let keys = [
            StorageKey::Admission {
                workspace: "room".into(),
            },
            StorageKey::Primary {
                workspace: "room".into(),
                package: "chat".into(),
            },
            StorageKey::Record {
                workspace: "room".into(),
                package: "writer".into(),
                record: "tab:1".into(),
            },
            StorageKey::View {
                workspace: "room".into(),
                instance: 1,
            },
            StorageKey::PrivateActions {
                workspace: "room".into(),
            },
        ];
        for (index, key) in keys.iter().enumerate() {
            let serialized = serde_json::to_string(&DiskKey::from(key)).unwrap();
            let actor = (index == 2).then(|| vec![9, 8, 7]);
            hydrate_entry(
                &storage,
                Some(&serialized),
                vec![index as u8],
                actor.clone(),
            )
            .unwrap();
            assert_eq!(storage.get(key).unwrap().bytes, vec![index as u8]);
            assert_eq!(storage.get(key).unwrap().actor, actor);
            assert!(storage.status(key).unwrap().durable);
            // A later desktop mount hydrates the same committed rows without replacing owners.
            hydrate_entry(&storage, Some(&serialized), vec![99], None).unwrap();
            assert_eq!(storage.get(key).unwrap().bytes, vec![index as u8]);
        }
        assert!(storage.pending_writes().is_empty());
        assert_eq!(
            serde_json::to_string(&DiskKey::from(&keys[4])).unwrap(),
            r#"{"type":"privateActions","workspace":"room"}"#
        );
    }
    #[test]
    fn object_key_fails_before_installation_without_silent_migration() {
        let storage = PortableStorage::new("regression-profile", "regression-device").unwrap();
        let object = serde_json::json!({"type":"privateActions","workspace":"room"});
        assert_eq!(
            hydrate_entry(&storage, object.as_str(), vec![1], None).unwrap_err(),
            "missing storage key"
        );
        assert!(
            storage
                .get(&StorageKey::PrivateActions {
                    workspace: "room".into()
                })
                .is_none()
        );
    }
}
