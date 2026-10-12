use super::{Change, EncryptedBackend, Result, binary_key, key};
use whatsapp_rust::wacore::appstate::hash::HashState;
use whatsapp_rust::wacore::appstate::processor::AppStateMutationMAC;
use whatsapp_rust::wacore::store::traits::{AppStateSyncKey, AppSyncStore};

#[whatsapp_rust::async_trait]
impl AppSyncStore for EncryptedBackend {
    async fn get_sync_key(&self, id: &[u8]) -> Result<Option<AppStateSyncKey>> {
        self.get(binary_key("sync-key", &[], id)?).await
    }
    async fn set_sync_key(&self, id: &[u8], value: AppStateSyncKey) -> Result<()> {
        let changes = vec![
            Change::put(binary_key("sync-key", &[], id)?, &value)?,
            Change::put(key("latest-sync", &[]), &id)?,
        ];
        self.run(move |disk| disk.commit(changes)).await
    }
    async fn get_latest_sync_key_id(&self) -> Result<Option<Vec<u8>>> {
        self.get(key("latest-sync", &[])).await
    }
    async fn get_version(&self, name: &str) -> Result<HashState> {
        Ok(self.get(key("version", &[name])).await?.unwrap_or_default())
    }
    async fn set_version(&self, name: &str, value: HashState) -> Result<()> {
        self.put(key("version", &[name]), &value).await
    }
    async fn put_mutation_macs(
        &self,
        name: &str,
        _version: u64,
        values: &[AppStateMutationMAC],
    ) -> Result<()> {
        let changes = values
            .iter()
            .map(|value| {
                Change::put(
                    binary_key("mutation", &[name], &value.index_mac)?,
                    &value.value_mac,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        self.run(move |disk| disk.commit(changes)).await
    }
    async fn get_mutation_mac(&self, name: &str, id: &[u8]) -> Result<Option<Vec<u8>>> {
        self.get(binary_key("mutation", &[name], id)?).await
    }
    async fn get_mutation_macs(
        &self,
        name: &str,
        ids: &[[u8; 32]],
    ) -> Result<std::collections::HashMap<[u8; 32], Vec<u8>>> {
        let keys = ids
            .iter()
            .map(|id| Ok((*id, binary_key("mutation", &[name], id)?)))
            .collect::<Result<Vec<_>>>()?;
        self.run(move |disk| {
            let mut found = std::collections::HashMap::with_capacity(keys.len());
            for (id, key) in keys {
                if let Some(value) = disk.get(&key)? {
                    found.insert(id, value);
                }
            }
            Ok(found)
        })
        .await
    }
    async fn delete_mutation_macs(&self, name: &str, ids: &[Vec<u8>]) -> Result<()> {
        let changes = ids
            .iter()
            .map(|id| Ok(Change::delete(binary_key("mutation", &[name], id)?)))
            .collect::<Result<Vec<_>>>()?;
        self.run(move |disk| disk.commit(changes)).await
    }
    async fn clear_mutation_macs(&self, name: &str) -> Result<()> {
        let name = name.to_owned();
        self.run(move |disk| {
            let changes = disk
                .rows::<Vec<u8>>("mutation", &[&name])?
                .into_iter()
                .map(|(key, _)| Change::delete(key))
                .collect();
            disk.commit(changes)
        })
        .await
    }
}
