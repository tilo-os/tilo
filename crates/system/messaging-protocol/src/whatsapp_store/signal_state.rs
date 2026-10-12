use super::{Arc, Change, EncryptedBackend, Result, key, unavailable};
use whatsapp_rust::bytes::Bytes;
use whatsapp_rust::wacore::store::traits::SignalStore;

#[whatsapp_rust::async_trait]
impl SignalStore for EncryptedBackend {
    async fn put_identity(&self, address: &str, value: [u8; 32]) -> Result<()> {
        self.put(key("identity", &[address]), &value).await
    }
    async fn load_identity(&self, address: &str) -> Result<Option<[u8; 32]>> {
        self.get(key("identity", &[address])).await
    }
    async fn delete_identity(&self, address: &str) -> Result<()> {
        self.delete(key("identity", &[address])).await
    }
    async fn put_identities_batch(&self, values: &[(Arc<str>, [u8; 32])]) -> Result<()> {
        let changes = values
            .iter()
            .map(|(address, value)| Change::put(key("identity", &[address]), value))
            .collect::<Result<Vec<_>>>()?;
        self.run(move |disk| disk.commit(changes)).await
    }
    async fn get_session(&self, address: &str) -> Result<Option<Bytes>> {
        Ok(self
            .get::<Vec<u8>>(key("session", &[address]))
            .await?
            .map(Bytes::from))
    }
    async fn put_session(&self, address: &str, value: &[u8]) -> Result<()> {
        self.put(key("session", &[address]), &value).await
    }
    async fn delete_session(&self, address: &str) -> Result<()> {
        self.delete(key("session", &[address])).await
    }
    async fn put_sessions_batch(&self, values: &[(Arc<str>, Bytes)]) -> Result<()> {
        let changes = values
            .iter()
            .map(|(address, value)| Change::put(key("session", &[address]), &value.as_ref()))
            .collect::<Result<Vec<_>>>()?;
        self.run(move |disk| disk.commit(changes)).await
    }
    async fn has_signal_state_for_user(&self, user: &str) -> Result<bool> {
        let user = user.to_owned();
        self.run(move |disk| {
            Ok(disk.values.keys().any(|key| {
                ["identity", "session"].contains(&key.domain.as_str())
                    && key.parts.first().is_some_and(|address| {
                        address
                            .split('@')
                            .next()
                            .is_some_and(|address| address.split(':').next() == Some(&user))
                    })
            }))
        })
        .await
    }
    async fn store_prekey(&self, id: u32, value: &[u8], uploaded: bool) -> Result<()> {
        self.put(key("prekey", &[&id.to_string()]), &(value, uploaded))
            .await
    }
    async fn load_prekey(&self, id: u32) -> Result<Option<Bytes>> {
        Ok(self
            .get::<(Vec<u8>, bool)>(key("prekey", &[&id.to_string()]))
            .await?
            .map(|(bytes, _)| Bytes::from(bytes)))
    }
    async fn load_prekeys_batch(&self, ids: &[u32]) -> Result<Vec<(u32, Bytes)>> {
        let ids = ids.to_vec();
        self.run(move |disk| {
            let mut found = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some((bytes, _)) =
                    disk.get::<(Vec<u8>, bool)>(&key("prekey", &[&id.to_string()]))?
                {
                    found.push((id, Bytes::from(bytes)));
                }
            }
            Ok(found)
        })
        .await
    }
    async fn store_prekeys_batch(&self, values: &[(u32, Bytes)], uploaded: bool) -> Result<()> {
        let changes = values
            .iter()
            .map(|(id, value)| {
                Change::put(
                    key("prekey", &[&id.to_string()]),
                    &(value.as_ref(), uploaded),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        self.run(move |disk| disk.commit(changes)).await
    }
    async fn mark_prekeys_uploaded(&self, ids: &[u32]) -> Result<()> {
        let ids = ids.to_vec();
        self.run(move |disk| {
            let mut changes = Vec::new();
            for id in ids {
                let key = key("prekey", &[&id.to_string()]);
                if let Some((bytes, _)) = disk.get::<(Vec<u8>, bool)>(&key)? {
                    changes.push(Change::put(key, &(bytes, true))?);
                }
            }
            disk.commit(changes)
        })
        .await
    }
    async fn remove_prekey(&self, id: u32) -> Result<()> {
        self.delete(key("prekey", &[&id.to_string()])).await
    }
    async fn get_max_prekey_id(&self) -> Result<u32> {
        self.run(|disk| {
            disk.values
                .keys()
                .filter(|key| key.domain == "prekey")
                .try_fold(0, |max, key| {
                    Ok(max.max(
                        key.parts
                            .first()
                            .ok_or_else(unavailable)?
                            .parse::<u32>()
                            .map_err(|_| unavailable())?,
                    ))
                })
        })
        .await
    }
    async fn store_signed_prekey(&self, id: u32, value: &[u8]) -> Result<()> {
        self.put(key("signed-prekey", &[&id.to_string()]), &value)
            .await
    }
    async fn load_signed_prekey(&self, id: u32) -> Result<Option<Vec<u8>>> {
        self.get(key("signed-prekey", &[&id.to_string()])).await
    }
    async fn load_all_signed_prekeys(&self) -> Result<Vec<(u32, Vec<u8>)>> {
        self.run(|disk| {
            disk.rows("signed-prekey", &[])?
                .into_iter()
                .map(|(key, value)| {
                    Ok((
                        key.parts
                            .first()
                            .ok_or_else(unavailable)?
                            .parse()
                            .map_err(|_| unavailable())?,
                        value,
                    ))
                })
                .collect()
        })
        .await
    }
    async fn remove_signed_prekey(&self, id: u32) -> Result<()> {
        self.delete(key("signed-prekey", &[&id.to_string()])).await
    }
    async fn put_sender_key(&self, address: &str, value: &[u8]) -> Result<()> {
        self.put(key("sender-key", &[address]), &value).await
    }
    async fn get_sender_key(&self, address: &str) -> Result<Option<Vec<u8>>> {
        self.get(key("sender-key", &[address])).await
    }
    async fn delete_sender_key(&self, address: &str) -> Result<()> {
        self.delete(key("sender-key", &[address])).await
    }
    async fn put_sender_keys_batch(&self, values: &[(Arc<str>, Bytes)]) -> Result<()> {
        let changes = values
            .iter()
            .map(|(address, value)| Change::put(key("sender-key", &[address]), &value.as_ref()))
            .collect::<Result<Vec<_>>>()?;
        self.run(move |disk| disk.commit(changes)).await
    }
}
