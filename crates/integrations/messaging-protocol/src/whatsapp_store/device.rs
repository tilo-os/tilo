use super::{BTreeMap, Change, EncryptedBackend, Key, Result, key, unavailable};
use whatsapp_rust::wacore::store::Device;
use whatsapp_rust::wacore::store::traits::{DeviceStore, MsgSecretEntry, MsgSecretStore};

#[whatsapp_rust::async_trait]
impl DeviceStore for EncryptedBackend {
    async fn save(&self, device: &Device) -> Result<()> {
        self.put(key("device", &[]), device).await
    }
    async fn load(&self) -> Result<Option<Device>> {
        self.get(key("device", &[])).await
    }
    async fn exists(&self) -> Result<bool> {
        Ok(self.load().await?.is_some())
    }
    async fn create(&self) -> Result<i32> {
        self.run(|disk| {
            let key = key("device", &[]);
            if disk.get::<Device>(&key)?.is_none() {
                disk.commit(vec![Change::put(key, &Device::new())?])?;
            }
            Ok(1)
        })
        .await
    }
    async fn snapshot_db(&self, _name: &str, _content: Option<&[u8]>) -> Result<()> {
        Err(unavailable())
    }
}

#[whatsapp_rust::async_trait]
impl MsgSecretStore for EncryptedBackend {
    async fn put_msg_secrets(&self, entries: Vec<MsgSecretEntry>) -> Result<usize> {
        self.run(move |disk| {
            let count = entries.len();
            let mut merged = BTreeMap::<Key, MsgSecretEntry>::new();
            for mut entry in entries {
                let key = key(
                    "message-secret",
                    &[&entry.chat, &entry.sender, &entry.msg_id],
                );
                if let Some(old) = merged.remove(&key).or(disk.get::<MsgSecretEntry>(&key)?) {
                    entry.expires_at = if old.expires_at == 0 || entry.expires_at == 0 {
                        0
                    } else {
                        old.expires_at.max(entry.expires_at)
                    };
                    entry.message_ts = old.message_ts.max(entry.message_ts);
                }
                merged.insert(key, entry);
            }
            let changes = merged
                .into_iter()
                .map(|(key, value)| Change::put(key, &value))
                .collect::<Result<Vec<_>>>()?;
            disk.commit(changes)?;
            Ok(count)
        })
        .await
    }
    async fn get_msg_secret(&self, chat: &str, sender: &str, id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .get_msg_secret_with_ts(chat, sender, id)
            .await?
            .map(|(secret, _)| secret))
    }
    async fn get_msg_secret_with_ts(
        &self,
        chat: &str,
        sender: &str,
        id: &str,
    ) -> Result<Option<(Vec<u8>, i64)>> {
        Ok(self
            .get::<MsgSecretEntry>(key("message-secret", &[chat, sender, id]))
            .await?
            .map(|entry| (entry.secret.to_vec(), entry.message_ts)))
    }
    async fn delete_expired_msg_secrets(&self, cutoff: i64) -> Result<u32> {
        self.run(move |disk| {
            let changes: Vec<_> = disk
                .rows::<MsgSecretEntry>("message-secret", &[])?
                .into_iter()
                .filter(|(_, entry)| entry.expires_at != 0 && entry.expires_at <= cutoff)
                .map(|(key, _)| Change::delete(key))
                .collect();
            let count = u32::try_from(changes.len()).map_err(|_| unavailable())?;
            disk.commit(changes)?;
            Ok(count)
        })
        .await
    }
}
