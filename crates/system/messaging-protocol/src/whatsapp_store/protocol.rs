use super::{Change, Deserialize, EncryptedBackend, Result, Serialize, key, unavailable};
use whatsapp_rust::wacore::store::traits::{
    DeviceListRecord, LidPnMappingEntry, PendingInboundKey, PendingInboundRow, ProtocolStore,
    TcTokenEntry,
};

#[derive(Serialize, Deserialize)]
struct Payload {
    bytes: Vec<u8>,
    timestamp: i64,
}

fn now() -> Result<i64> {
    tilo_clock::try_unix_now()
        .ok_or_else(unavailable)
        .and_then(|secs| i64::try_from(secs).map_err(|_| unavailable()))
}

impl EncryptedBackend {
    async fn clear_domain(&self, domain: &'static str, prefix: Vec<String>) -> Result<()> {
        self.run(move |disk| {
            let prefix: Vec<_> = prefix.iter().map(String::as_str).collect();
            let changes = disk
                .values
                .keys()
                .filter(|key| {
                    key.domain == domain
                        && key.parts.len() >= prefix.len()
                        && key
                            .parts
                            .iter()
                            .zip(&prefix)
                            .all(|(stored, expected)| stored == expected)
                })
                .cloned()
                .map(Change::delete)
                .collect();
            disk.commit(changes)
        })
        .await
    }
    async fn expire_payloads(&self, domain: &'static str, cutoff: i64) -> Result<u32> {
        self.run(move |disk| {
            let changes: Vec<_> = disk
                .rows::<Payload>(domain, &[])?
                .into_iter()
                .filter(|(_, value)| value.timestamp < cutoff)
                .map(|(key, _)| Change::delete(key))
                .collect();
            let count = u32::try_from(changes.len()).map_err(|_| unavailable())?;
            disk.commit(changes)?;
            Ok(count)
        })
        .await
    }
}

#[whatsapp_rust::async_trait]
impl ProtocolStore for EncryptedBackend {
    async fn get_sender_key_devices(&self, group: &str) -> Result<Vec<(String, bool)>> {
        let group = group.to_owned();
        self.run(move |disk| {
            disk.rows("skdm", &[&group])?
                .into_iter()
                .map(|(key, value)| Ok((key.parts.get(1).ok_or_else(unavailable)?.clone(), value)))
                .collect()
        })
        .await
    }
    async fn set_sender_key_status(&self, group: &str, entries: &[(&str, bool)]) -> Result<()> {
        let changes = entries
            .iter()
            .map(|(device, value)| Change::put(key("skdm", &[group, device]), value))
            .collect::<Result<Vec<_>>>()?;
        self.run(move |disk| disk.commit(changes)).await
    }
    async fn clear_sender_key_devices(&self, group: &str) -> Result<()> {
        self.clear_domain("skdm", vec![group.into()]).await
    }
    async fn clear_all_sender_key_devices(&self) -> Result<()> {
        self.clear_domain("skdm", vec![]).await
    }
    async fn delete_sender_key_device_rows(&self, devices: &[&str]) -> Result<()> {
        let devices: Vec<String> = devices.iter().map(|value| (*value).into()).collect();
        self.run(move |disk| {
            let changes = disk
                .values
                .keys()
                .filter(|key| {
                    key.domain == "skdm"
                        && key.parts.get(1).is_some_and(|part| devices.contains(part))
                })
                .cloned()
                .map(Change::delete)
                .collect();
            disk.commit(changes)
        })
        .await
    }
    async fn get_lid_mapping(&self, lid: &str) -> Result<Option<LidPnMappingEntry>> {
        self.get(key("lid", &[lid])).await
    }
    async fn get_pn_mapping(&self, phone: &str) -> Result<Option<LidPnMappingEntry>> {
        let phone = phone.to_owned();
        self.run(move |disk| {
            Ok(disk
                .rows::<LidPnMappingEntry>("lid", &[])?
                .into_iter()
                .map(|(_, value)| value)
                .filter(|value| value.phone_number == phone)
                .max_by_key(|value| (value.updated_at, value.lid.clone())))
        })
        .await
    }
    async fn put_lid_mapping(&self, entry: &LidPnMappingEntry) -> Result<()> {
        self.put(key("lid", &[&entry.lid]), entry).await
    }
    async fn put_lid_mappings(&self, entries: &[LidPnMappingEntry]) -> Result<()> {
        let changes = entries
            .iter()
            .map(|entry| Change::put(key("lid", &[&entry.lid]), entry))
            .collect::<Result<Vec<_>>>()?;
        self.run(move |disk| disk.commit(changes)).await
    }
    async fn get_all_lid_mappings(&self) -> Result<Vec<LidPnMappingEntry>> {
        self.run(|disk| {
            Ok(disk
                .rows("lid", &[])?
                .into_iter()
                .map(|(_, value)| value)
                .collect())
        })
        .await
    }
    async fn save_base_key(&self, address: &str, id: &str, bytes: &[u8]) -> Result<()> {
        self.put(key("base", &[address, id]), &bytes).await
    }
    async fn has_same_base_key(&self, address: &str, id: &str, bytes: &[u8]) -> Result<bool> {
        Ok(self
            .get::<Vec<u8>>(key("base", &[address, id]))
            .await?
            .is_some_and(|stored| stored == bytes))
    }
    async fn delete_base_key(&self, address: &str, id: &str) -> Result<()> {
        self.delete(key("base", &[address, id])).await
    }
    async fn update_device_list(&self, record: DeviceListRecord) -> Result<()> {
        self.put(key("devices", &[&record.user]), &record).await
    }
    async fn update_device_lists(&self, records: Vec<DeviceListRecord>) -> Result<()> {
        let changes = records
            .iter()
            .map(|entry| Change::put(key("devices", &[&entry.user]), entry))
            .collect::<Result<Vec<_>>>()?;
        self.run(move |disk| disk.commit(changes)).await
    }
    async fn get_devices(&self, user: &str) -> Result<Option<DeviceListRecord>> {
        self.get(key("devices", &[user])).await
    }
    async fn delete_devices(&self, user: &str) -> Result<()> {
        self.delete(key("devices", &[user])).await
    }
    async fn get_group_metadata(&self, group: &str) -> Result<Option<Vec<u8>>> {
        self.get(key("group", &[group])).await
    }
    async fn put_group_metadata(&self, group: &str, bytes: &[u8]) -> Result<()> {
        self.put(key("group", &[group]), &bytes).await
    }
    async fn delete_group_metadata(&self, group: &str) -> Result<()> {
        self.delete(key("group", &[group])).await
    }
    async fn get_tc_token(&self, jid: &str) -> Result<Option<TcTokenEntry>> {
        self.get(key("token", &[jid])).await
    }
    async fn put_tc_token(&self, jid: &str, entry: &TcTokenEntry) -> Result<()> {
        self.put(key("token", &[jid]), entry).await
    }
    async fn delete_tc_token(&self, jid: &str) -> Result<()> {
        self.delete(key("token", &[jid])).await
    }
    async fn get_all_tc_token_jids(&self) -> Result<Vec<String>> {
        self.run(|disk| {
            disk.rows::<TcTokenEntry>("token", &[])?
                .into_iter()
                .map(|(key, _)| key.parts.first().cloned().ok_or_else(unavailable))
                .collect()
        })
        .await
    }
    async fn delete_expired_tc_tokens(&self, token_cutoff: i64, sender_cutoff: i64) -> Result<u32> {
        self.run(move |disk| {
            let changes: Vec<_> = disk
                .rows::<TcTokenEntry>("token", &[])?
                .into_iter()
                .filter(|(_, value)| {
                    (value.token.is_empty() || value.token_timestamp < token_cutoff)
                        && value
                            .sender_timestamp
                            .is_none_or(|time| time < sender_cutoff)
                })
                .map(|(key, _)| Change::delete(key))
                .collect();
            let count = u32::try_from(changes.len()).map_err(|_| unavailable())?;
            disk.commit(changes)?;
            Ok(count)
        })
        .await
    }
    async fn touch_tc_token_sender_timestamp(&self, jid: &str, timestamp: i64) -> Result<()> {
        let key = key("token", &[jid]);
        self.run(move |disk| {
            let mut value = disk.get::<TcTokenEntry>(&key)?.unwrap_or(TcTokenEntry {
                token: vec![],
                token_timestamp: timestamp,
                sender_timestamp: None,
            });
            value.sender_timestamp = Some(
                value
                    .sender_timestamp
                    .map_or(timestamp, |old| old.max(timestamp)),
            );
            disk.commit(vec![Change::put(key, &value)?])
        })
        .await
    }
    async fn store_received_tc_token(&self, jid: &str, bytes: &[u8], timestamp: i64) -> Result<()> {
        let key = key("token", &[jid]);
        let bytes = bytes.to_vec();
        self.run(move |disk| {
            let old = disk.get::<TcTokenEntry>(&key)?;
            if old
                .as_ref()
                .is_some_and(|old| !old.token.is_empty() && old.token_timestamp > timestamp)
            {
                return Ok(());
            }
            let value = TcTokenEntry {
                token: bytes,
                token_timestamp: timestamp,
                sender_timestamp: old.and_then(|old| old.sender_timestamp),
            };
            disk.commit(vec![Change::put(key, &value)?])
        })
        .await
    }
    async fn store_sent_message(&self, chat: &str, id: &str, bytes: &[u8]) -> Result<()> {
        self.put(
            key("sent", &[chat, id]),
            &Payload {
                bytes: bytes.to_vec(),
                timestamp: now()?,
            },
        )
        .await
    }
    async fn take_sent_message(&self, chat: &str, id: &str) -> Result<Option<Vec<u8>>> {
        let key = key("sent", &[chat, id]);
        self.run(move |disk| {
            let value = disk.get::<Payload>(&key)?;
            disk.commit(vec![Change::delete(key)])?;
            Ok(value.map(|value| value.bytes))
        })
        .await
    }
    async fn delete_expired_sent_messages(&self, cutoff: i64) -> Result<u32> {
        self.expire_payloads("sent", cutoff).await
    }
    async fn store_pending_inbound(
        &self,
        chat: &str,
        sender: &str,
        id: &str,
        message: &[u8],
    ) -> Result<()> {
        self.store_pending_inbound_batch(&[PendingInboundRow {
            chat,
            sender,
            id,
            message,
        }])
        .await
    }
    async fn get_pending_inbound(
        &self,
        chat: &str,
        sender: &str,
        id: &str,
    ) -> Result<Option<Vec<u8>>> {
        Ok(self
            .get::<Payload>(key("pending", &[chat, sender, id]))
            .await?
            .map(|value| value.bytes))
    }
    async fn delete_pending_inbound(&self, chat: &str, sender: &str, id: &str) -> Result<()> {
        self.delete(key("pending", &[chat, sender, id])).await
    }
    async fn delete_expired_pending_inbound(&self, cutoff: i64) -> Result<u32> {
        self.expire_payloads("pending", cutoff).await
    }
    async fn store_pending_inbound_batch(&self, rows: &[PendingInboundRow<'_>]) -> Result<()> {
        let timestamp = now()?;
        let changes = rows
            .iter()
            .map(|row| {
                Change::put(
                    key("pending", &[row.chat, row.sender, row.id]),
                    &Payload {
                        bytes: row.message.to_vec(),
                        timestamp,
                    },
                )
            })
            .collect::<Result<Vec<_>>>()?;
        self.run(move |disk| disk.commit(changes)).await
    }
    async fn delete_pending_inbound_batch(&self, keys: &[PendingInboundKey<'_>]) -> Result<()> {
        let changes = keys
            .iter()
            .map(|row| Change::delete(key("pending", &[row.chat, row.sender, row.id])))
            .collect();
        self.run(move |disk| disk.commit(changes)).await
    }
}
