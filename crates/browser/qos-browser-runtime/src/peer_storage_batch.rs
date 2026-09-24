//! Immutable storage transaction capture; lease observations never supply cursor contents.
//! The budget covers retained captures, not PortableStorage's own cache, the
//! temporary pending_writes clone, journal decode, or JS encoder overlap.
use crate::{peer_delivery_cursor::StoredDeliveryCursor, peer_journal::Journal};
use std::collections::BTreeSet;
use ui::portable_storage::{PendingWrite, PortableStorage, ProfileNamespace, StorageKey};

pub(crate) const MAX_PENDING_BATCHES: usize = 2;
pub(crate) const MAX_PENDING_BATCH_BYTES: usize = 128 * 1024 * 1024;

struct CapturedPeerBatch {
    cache: PortableStorage,
    writes: Vec<PendingWrite>,
    admission_basis: Option<u64>,
    lease_epoch: String,
    journal_revision: Option<u64>,
    delivery: Option<StoredDeliveryCursor>,
    transport_generation: u64,
    charge: usize,
}
struct TransferCommit {
    id: String,
    cursor: StoredDeliveryCursor,
    generation: u64,
    captured: Option<String>,
    abandoned: bool,
}
struct CommittedCursor {
    revision: u64,
    transport_generation: u64,
    cursor: StoredDeliveryCursor,
}
pub(crate) struct PeerStorageBatches {
    peer_id: u32,
    next_batch: u64,
    lease_epoch: Option<String>,
    pending: Vec<(String, CapturedPeerBatch)>,
    pending_bytes: usize,
    committed: Option<CommittedCursor>,
    transfer: Option<TransferCommit>,
}
impl PeerStorageBatches {
    pub(crate) fn new(peer_id: u32) -> Self {
        Self {
            peer_id,
            next_batch: 0,
            lease_epoch: None,
            pending: Vec::with_capacity(MAX_PENDING_BATCHES),
            pending_bytes: 0,
            committed: None,
            transfer: None,
        }
    }
    pub(crate) fn bind_lease(&mut self, epoch: &str) -> Result<(), String> {
        if epoch.is_empty() || epoch.len() > 128 {
            return Err("invalid storage lease epoch".into());
        }
        if let Some(current) = &self.lease_epoch {
            if current != epoch {
                return Err("live storage lease cannot be rebound".into());
            }
        } else {
            self.lease_epoch = Some(epoch.to_owned());
        }
        Ok(())
    }
    pub(crate) fn lease_epoch(&self) -> Option<&str> {
        self.lease_epoch.as_deref()
    }

    /// Called only by the private verified-cut owner immediately before it
    /// installs that cursor into the canonical journal. No JS setter exists.
    pub(crate) fn bind_transfer(
        &mut self,
        id: &str,
        cursor: StoredDeliveryCursor,
        generation: u64,
    ) -> Result<(), String> {
        if self.transfer.is_some()
            || generation == 0
            || id.len() != 32
            || id
                .bytes()
                .any(|b| !b.is_ascii_digit() && !(b'a'..=b'f').contains(&b))
            || id.bytes().all(|b| b == b'0')
        {
            return Err("invalid or occupied transfer commit custody".into());
        }
        cursor.validate_storage_shape()?;
        self.transfer = Some(TransferCommit {
            id: id.to_owned(),
            cursor,
            generation,
            captured: None,
            abandoned: false,
        });
        Ok(())
    }
    /// The delivery session that bound this cut stopped. Its staging handle is
    /// gone, so later captures commit the same journal/cursor without consuming
    /// the stage (a later scan renews or discards it). An in-flight capture
    /// keeps its binding until its transaction settles either way.
    pub(crate) fn abandon_transfer(&mut self) {
        match &mut self.transfer {
            Some(transfer) if transfer.captured.is_some() => transfer.abandoned = true,
            _ => self.transfer = None,
        }
    }
    /// Roll back a synchronous failed candidate installation, before capture.
    pub(crate) fn cancel_transfer(&mut self, id: &str) -> Result<(), String> {
        let transfer = self.transfer.as_ref().ok_or("transfer custody absent")?;
        if transfer.id != id || transfer.captured.is_some() {
            return Err("transfer already captured or replaced".into());
        }
        self.transfer = None;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn capture<T>(
        &mut self,
        cache: &PortableStorage,
        required: &[StorageKey],
        transport_generation: u64,
        basis: impl FnOnce(Option<u64>) -> Option<u64>,
        encode: impl FnOnce(&str, &[PendingWrite]) -> Result<T, String>,
    ) -> Result<T, String> {
        self.capture_transfer(
            cache,
            required,
            transport_generation,
            basis,
            |id, writes, _| encode(id, writes),
        )
    }
    /// Capture directly from this cache, so caller-created/foreign PendingWrites
    /// cannot enter a batch. Encoding succeeds before the capture is registered.
    pub(crate) fn capture_transfer<T>(
        &mut self,
        cache: &PortableStorage,
        required: &[StorageKey],
        transport_generation: u64,
        basis: impl FnOnce(Option<u64>) -> Option<u64>,
        encode: impl FnOnce(&str, &[PendingWrite], Option<&str>) -> Result<T, String>,
    ) -> Result<T, String> {
        if self
            .transfer
            .as_ref()
            .is_some_and(|transfer| transfer.captured.is_some())
        {
            return Err(
                "storage-pressure: settle the captured transfer cut before another capture".into(),
            );
        }
        let epoch = self
            .lease_epoch
            .as_ref()
            .ok_or("storage lease must be bound before batch capture")?;
        if self.pending.len() >= MAX_PENDING_BATCHES {
            return Err(
                "storage-pressure: settle existing storage batches before capturing more work"
                    .into(),
            );
        }
        // Count check precedes the existing API's temporary owned clone.
        let writes = cache.pending_writes();
        if writes.is_empty() {
            return encode("0", &[], None);
        }
        let next = self
            .next_batch
            .checked_add(1)
            .ok_or("batch sequence exhausted")?;
        let id = format!("{}:{next}", self.peer_id);
        // Charge the entire bounded lookup backing store for every capture;
        // this conservatively includes spare slots even with only one capture.
        let bookkeeping = self
            .pending
            .capacity()
            .checked_mul(std::mem::size_of::<(String, CapturedPeerBatch)>())
            .and_then(|bytes| bytes.checked_add(id.capacity()))
            .and_then(|bytes| bytes.checked_add(epoch.len()))
            .ok_or("storage-pressure: storage batch size overflow")?;
        // PortableStorage::clone shares its cache but owns a cloned namespace.
        let cache_namespace = match cache.namespace() {
            ProfileNamespace::Native(path) => Some(path.capacity()),
            ProfileNamespace::Portable { profile, device } => {
                profile.capacity().checked_add(device.capacity())
            }
        }
        .ok_or("storage-pressure: storage batch size overflow")?;
        let overhead = (writes.capacity() - writes.len())
            .checked_mul(std::mem::size_of::<PendingWrite>())
            .and_then(|bytes| bytes.checked_add(bookkeeping))
            .and_then(|bytes| bytes.checked_add(cache_namespace))
            .ok_or("storage-pressure: storage batch size overflow")?;
        let mut charge = writes
            .iter()
            .try_fold(overhead, |total, write| {
                total.checked_add(write.retained_bytes()?)
            })
            .ok_or("storage-pressure: storage batch size overflow")?;
        // Reserve bounded cursor ownership before decoding any v3 journal.
        let cursor_reserve = std::mem::size_of::<StoredDeliveryCursor>() + 4096 + 4 * 256;
        charge = charge
            .checked_add(cursor_reserve)
            .ok_or("storage-pressure: storage batch size overflow")?;
        let admitted_bytes = self
            .pending_bytes
            .checked_add(charge)
            .ok_or("storage-pressure: storage batch size overflow")?;
        if admitted_bytes > MAX_PENDING_BATCH_BYTES {
            return Err("storage-pressure: pending snapshot capture exceeds 128 MiB; retain the unsaved work and reduce pending work or export before retrying".into());
        }
        let mut keys = BTreeSet::new();
        for write in &writes {
            if write.namespace != *cache.namespace() || !keys.insert(&write.key) {
                return Err("invalid storage batch namespace or duplicate key".into());
            }
        }
        let journals = writes
            .iter()
            .filter(|write| matches!(write.key, StorageKey::Admission { .. }))
            .collect::<Vec<_>>();
        if journals.len() > 1 {
            return Err("storage batch spans multiple admission journals".into());
        }
        let journal_revision = journals.first().map(|write| write.revision);
        let delivery = if let Some(write) = journals.first().filter(|write| {
            write.value.bytes.starts_with(b"QOSJ\x03") || write.value.bytes.starts_with(b"QOSJ\x04")
        }) {
            let journal = Journal::decode(&write.value.bytes)?;
            if let Some(cursor) = &journal.storage.delivery {
                let StorageKey::Admission { workspace } = &write.key else {
                    unreachable!()
                };
                if workspace != &cursor.identity.room {
                    return Err("cursor journal workspace mismatch".into());
                }
                validate_records(cache, &writes, required, &journal, workspace)?;
            }
            journal.storage.delivery
        } else {
            None
        };
        let admission_basis = basis(journal_revision);
        let transfer_id = if let Some(transfer) = &self.transfer {
            if transfer.generation != transport_generation
                || delivery.as_ref() != Some(&transfer.cursor)
            {
                return Err(
                    "transfer does not match captured canonical cursor and generation".into(),
                );
            }
            Some(transfer.id.as_str())
        } else {
            None
        };
        let result = encode(&id, &writes, transfer_id)?;
        if let Some(transfer) = &mut self.transfer {
            transfer.captured = Some(id.clone());
        }
        self.next_batch = next;
        self.pending.push((
            id,
            CapturedPeerBatch {
                cache: cache.clone(),
                writes,
                admission_basis,
                lease_epoch: epoch.clone(),
                journal_revision,
                delivery,
                transport_generation,
                charge,
            },
        ));
        self.pending_bytes = admitted_bytes;
        Ok(result)
    }

    pub(crate) fn settle(
        &mut self,
        id: &str,
        epoch: &str,
        result: Result<(), String>,
    ) -> Result<Option<u64>, String> {
        if self.lease_epoch.as_deref() != Some(epoch) {
            return Err("foreign storage epoch".into());
        }
        let index = self
            .pending
            .iter()
            .position(|(batch_id, _)| batch_id == id)
            .ok_or("unknown or settled storage batch")?;
        let batch = &self.pending[index].1;
        if batch.lease_epoch != epoch {
            return Err("foreign captured storage epoch".into());
        }
        // Capture owns its cache and exact immutable writes. Do not remove it
        // until every acknowledgement succeeds, so an unexpected failure is recoverable.
        for write in &batch.writes {
            batch.cache.acknowledge(write, result.clone())?;
        }
        let basis = result.is_ok().then_some(batch.admission_basis).flatten();
        if result.is_ok()
            && let (Some(revision), Some(cursor)) = (batch.journal_revision, &batch.delivery)
            && self
                .committed
                .as_ref()
                .is_none_or(|prior| prior.revision < revision)
        {
            let write = batch
                .writes
                .iter()
                .find(|write| matches!(write.key, StorageKey::Admission { .. }))
                .ok_or("captured cursor journal missing")?;
            let exact = batch
                .cache
                .status(&write.key)
                .and_then(|status| status.committed)
                .is_some_and(|(saved_revision, saved)| {
                    saved_revision == revision && saved.bytes == write.value.bytes
                });
            if exact {
                self.committed = Some(CommittedCursor {
                    revision,
                    transport_generation: batch.transport_generation,
                    cursor: cursor.clone(),
                });
            }
        }
        if self
            .transfer
            .as_ref()
            .is_some_and(|transfer| transfer.captured.as_deref() == Some(id))
        {
            if result.is_ok()
                || self
                    .transfer
                    .as_ref()
                    .is_some_and(|transfer| transfer.abandoned)
            {
                self.transfer = None;
            } else if let Some(transfer) = &mut self.transfer {
                transfer.captured = None;
            }
        }
        let (_, settled) = self.pending.swap_remove(index);
        self.pending_bytes -= settled.charge;
        Ok(basis)
    }

    // The real delivery scheduler will consume this evidence only after its
    // authenticated boundary API exists. It is never exposed as a JS setter.
    pub(crate) fn committed_cursor(
        &self,
        current_generation: u64,
    ) -> Option<&StoredDeliveryCursor> {
        self.committed
            .as_ref()
            .filter(|saved| saved.transport_generation == current_generation)
            .map(|saved| &saved.cursor)
    }
}

fn validate_records(
    cache: &PortableStorage,
    writes: &[PendingWrite],
    required: &[StorageKey],
    journal: &Journal,
    room: &str,
) -> Result<(), String> {
    for key in required {
        let (workspace, app, record) = match key {
            StorageKey::Record {
                workspace,
                package,
                record,
            } => (workspace.as_str(), package.as_str(), record.as_str()),
            StorageKey::Primary { workspace, package } => {
                (workspace.as_str(), package.as_str(), "")
            }
            _ => return Err("cursor requires canonical record keys".into()),
        };
        if workspace != room {
            return Err("cursor record workspace mismatch".into());
        }
        let heads = if let Some(write) = writes.iter().find(|write| write.key == *key) {
            // Heads recorded by the canonical owner for exactly these bytes;
            // decode only when the capture carries none.
            write
                .document_heads()
                .unwrap_or_else(|| {
                    automerge::AutoCommit::load(&write.value.bytes)
                        .map(|mut doc| doc.get_heads())
                        .map_err(|_| ())
                })
                .map_err(|_| "invalid captured record bytes")?
        } else {
            cache
                .committed_document_heads(key)
                .ok_or("cursor record has no committed basis")?
                .map_err(|_| "invalid committed record bytes")?
        }
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
        if journal.match_document_heads(app, record, &heads).is_none() {
            return Err("cursor journal does not match exact record basis".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        peer_delivery_cursor::tests::cursor,
        peer_journal::{Journal, JournalFormat, JournalStorage},
    };
    use automerge::{AutoCommit, ROOT, transaction::Transactable};
    use qos_peer_state::{DocumentRow, NeighbourMsg};

    fn record() -> StorageKey {
        StorageKey::Record {
            workspace: "room".into(),
            package: "app".into(),
            record: "record".into(),
        }
    }
    fn admission() -> StorageKey {
        StorageKey::Admission {
            workspace: "room".into(),
        }
    }
    fn stage(cache: &PortableStorage, doc: &mut AutoCommit, value: u8) {
        doc.put(ROOT, "value", u64::from(value)).unwrap();
        doc.commit();
        let heads = doc.get_heads().iter().map(ToString::to_string).collect();
        cache
            .put(
                record(),
                doc.save(),
                Some(doc.get_actor().to_bytes().to_vec()),
            )
            .unwrap();
        let journal = Journal {
            state: vec![value],
            outbox: vec![NeighbourMsg::SyncReq {
                room: "original-signed-operation".into(),
                since: vec![],
                invite_id: None,
            }],
            records: vec![DocumentRow {
                app_id: "app".into(),
                record_id: "record".into(),
                heads,
            }],
            receipts: Default::default(),
            storage: JournalStorage {
                format: JournalFormat::V3,
                delivery: Some(cursor(value)),
                descriptor: None,
            },
        };
        cache
            .put(admission(), journal.encode().unwrap(), None)
            .unwrap();
    }
    fn capture(
        batches: &mut PeerStorageBatches,
        cache: &PortableStorage,
        generation: u64,
    ) -> Result<String, String> {
        batches.capture(
            cache,
            &[record()],
            generation,
            |revision| revision,
            |id, _| Ok(id.to_owned()),
        )
    }
    fn setup() -> (PortableStorage, AutoCommit, PeerStorageBatches) {
        let cache = PortableStorage::new("p", "d").unwrap();
        let mut batches = PeerStorageBatches::new(1);
        batches.bind_lease("lease").unwrap();
        (cache, AutoCommit::new(), batches)
    }
    #[test]
    fn failed_and_late_batches_never_acknowledge_newer_dirty_record_or_cursor() {
        let (cache, mut doc, mut batches) = setup();
        stage(&cache, &mut doc, 1);
        let actor = cache.get(&record()).unwrap().actor;
        let a = capture(&mut batches, &cache, 1).unwrap();
        stage(&cache, &mut doc, 2);
        let b = capture(&mut batches, &cache, 1).unwrap();
        batches.settle(&a, "lease", Ok(())).unwrap();
        assert_eq!(batches.committed_cursor(1).unwrap().token, [1]);
        assert!(!cache.status(&record()).unwrap().durable);
        let saved = cache.status(&admission()).unwrap().committed.unwrap().1;
        assert_eq!(
            Journal::decode(&saved.bytes)
                .unwrap()
                .storage
                .delivery
                .unwrap()
                .token,
            [1]
        );
        batches.settle(&b, "lease", Err("quota".into())).unwrap();
        assert_eq!(batches.committed_cursor(1).unwrap().token, [1]);
        assert!(!cache.status(&record()).unwrap().durable);
        assert_eq!(cache.get(&record()).unwrap().actor, actor);
        let retry = capture(&mut batches, &cache, 1).unwrap();
        batches.settle(&retry, "lease", Ok(())).unwrap();
        assert_eq!(batches.committed_cursor(1).unwrap().token, [2]);
        assert!(cache.status(&record()).unwrap().durable);
        assert!(batches.settle(&a, "lease", Ok(())).is_err());
        assert_eq!(batches.pending_bytes, 0);
    }
    #[test]
    fn newer_commit_wins_over_late_success_and_old_transport_cannot_ack() {
        let (cache, mut doc, mut batches) = setup();
        stage(&cache, &mut doc, 1);
        let a = capture(&mut batches, &cache, 1).unwrap();
        stage(&cache, &mut doc, 2);
        let b = capture(&mut batches, &cache, 2).unwrap();
        batches.settle(&b, "lease", Ok(())).unwrap();
        batches.settle(&a, "lease", Ok(())).unwrap();
        assert_eq!(batches.committed_cursor(2).unwrap().token, [2]);
        assert!(batches.committed_cursor(1).is_none());
        assert!(batches.committed_cursor(3).is_none());
        assert!(cache.status(&record()).unwrap().durable);
        let saved = cache.status(&admission()).unwrap().committed.unwrap().1;
        assert_eq!(
            Journal::decode(&saved.bytes)
                .unwrap()
                .storage
                .delivery
                .unwrap()
                .token,
            [2]
        );
    }
    #[test]
    fn lease_is_bound_before_capture_and_foreign_ack_cannot_consume_it() {
        let cache = PortableStorage::new("p", "d").unwrap();
        let mut doc = AutoCommit::new();
        stage(&cache, &mut doc, 1);
        let mut batches = PeerStorageBatches::new(1);
        assert!(capture(&mut batches, &cache, 1).is_err());
        assert!(batches.bind_lease("").is_err());
        assert!(batches.bind_lease(&"e".repeat(129)).is_err());
        batches.bind_lease("lease").unwrap();
        batches.bind_lease("lease").unwrap();
        let a = capture(&mut batches, &cache, 1).unwrap();
        let bytes = batches.pending_bytes;
        assert!(batches.bind_lease("other").is_err());
        assert!(batches.settle(&a, "other", Ok(())).is_err());
        assert_eq!(batches.pending_bytes, bytes);
        assert!(batches.committed_cursor(1).is_none());
        batches.settle(&a, "lease", Ok(())).unwrap();
        assert_eq!(batches.committed_cursor(1).unwrap().token, [1]);
    }
    #[test]
    fn pressure_and_encoder_failure_preserve_unsettled_captures() {
        let (cache, mut doc, mut batches) = setup();
        stage(&cache, &mut doc, 1);
        let a = capture(&mut batches, &cache, 1).unwrap();
        let used = batches.pending_bytes;
        assert!(
            batches
                .capture::<()>(&cache, &[record()], 1, |v| v, |_, _| Err("encoder".into()))
                .is_err()
        );
        assert_eq!(batches.pending_bytes, used);
        let b = capture(&mut batches, &cache, 1).unwrap();
        let used = batches.pending_bytes;
        assert!(
            capture(&mut batches, &cache, 1)
                .unwrap_err()
                .contains("storage-pressure")
        );
        assert_eq!(batches.pending_bytes, used);
        batches.settle(&a, "lease", Ok(())).unwrap();
        batches.settle(&b, "lease", Ok(())).unwrap();
        assert_eq!(batches.pending_bytes, 0);
    }
    #[test]
    fn oversized_capture_is_rejected_before_encoding_and_keeps_prior_evidence() {
        let (cache, mut doc, mut batches) = setup();
        stage(&cache, &mut doc, 1);
        let a = capture(&mut batches, &cache, 1).unwrap();
        let used = batches.pending_bytes;
        cache
            .put(
                StorageKey::View {
                    workspace: "room".into(),
                    instance: 7,
                },
                vec![0; MAX_PENDING_BATCH_BYTES],
                None,
            )
            .unwrap();
        let result = batches.capture::<()>(
            &cache,
            &[record()],
            1,
            |v| v,
            |_, _| panic!("over-budget batch reached encoder"),
        );
        assert!(result.unwrap_err().contains("storage-pressure"));
        assert_eq!(batches.pending_bytes, used);
        batches.settle(&a, "lease", Ok(())).unwrap();
        assert_eq!(batches.committed_cursor(1).unwrap().token, [1]);
        assert!(
            cache
                .pending_writes()
                .iter()
                .any(|write| matches!(write.key, StorageKey::View { .. }))
        );
    }
    #[test]
    fn cursor_requires_exact_records_and_unmodified_namespace() {
        let (cache, mut doc, mut batches) = setup();
        stage(&cache, &mut doc, 1);
        let missing = StorageKey::Record {
            workspace: "room".into(),
            package: "app".into(),
            record: "missing".into(),
        };
        assert!(
            batches
                .capture(&cache, &[missing], 1, |v| v, |id, _| Ok(id.to_owned()))
                .is_err()
        );
        let raw = cache.get(&admission()).unwrap().bytes;
        let mut wrong = Journal::decode(&raw).unwrap();
        wrong.records[0].heads = vec!["wrong".into()];
        cache
            .put(admission(), wrong.encode().unwrap(), None)
            .unwrap();
        assert!(capture(&mut batches, &cache, 1).is_err());
        cache.put(admission(), raw, None).unwrap();
        let a = capture(&mut batches, &cache, 1).unwrap();
        batches.settle(&a, "lease", Ok(())).unwrap();
        // Journal-only duplicate boundary may refer to the already committed record.
        let mut next = Journal::decode(&cache.get(&admission()).unwrap().bytes).unwrap();
        next.storage.delivery = Some(cursor(2));
        cache
            .put(admission(), next.encode().unwrap(), None)
            .unwrap();
        let b = capture(&mut batches, &cache, 1).unwrap();
        batches.settle(&b, "lease", Ok(())).unwrap();
        assert_eq!(batches.committed_cursor(1).unwrap().token, [2]);
    }
    #[test]
    fn exact_cut_capture_keeps_staging_atomic_across_older_drain_and_failed_retry() {
        let (cache, mut doc, mut batches) = setup();
        stage(&cache, &mut doc, 1);
        let older = capture(&mut batches, &cache, 1).unwrap();
        let transfer = "0123456789abcdef0123456789abcdef";
        batches.bind_transfer(transfer, cursor(2), 1).unwrap();
        stage(&cache, &mut doc, 2);
        batches.settle(&older, "lease", Ok(())).unwrap();
        let capture_cut = |batches: &mut PeerStorageBatches| {
            batches
                .capture_transfer(
                    &cache,
                    &[record()],
                    1,
                    |v| v,
                    |id, _, tag| {
                        assert_eq!(tag, Some(transfer));
                        Ok(id.to_owned())
                    },
                )
                .unwrap()
        };
        let first = capture_cut(&mut batches);
        assert!(
            capture(&mut batches, &cache, 1).is_err(),
            "a second capture must not reuse a consumed staging handle"
        );
        assert!(batches.settle(&first, "other", Ok(())).is_err());
        batches
            .settle(&first, "lease", Err("quota".into()))
            .unwrap();
        assert_eq!(batches.committed_cursor(1).unwrap().token, [1]);
        let retry = capture_cut(&mut batches);
        batches.settle(&retry, "lease", Ok(())).unwrap();
        assert_eq!(batches.committed_cursor(1).unwrap().token, [2]);
        stage(&cache, &mut doc, 3);
        batches
            .capture_transfer(
                &cache,
                &[record()],
                1,
                |v| v,
                |_, _, tag| {
                    assert_eq!(tag, None);
                    Ok(())
                },
            )
            .unwrap();
    }
    #[test]
    fn transfer_capture_rejects_wrong_cut_generation_and_encoding_failure_retains_binding() {
        let (cache, mut doc, mut batches) = setup();
        assert!(batches.bind_transfer("arbitrary", cursor(1), 1).is_err());
        let transfer = "0123456789abcdef0123456789abcdef";
        batches.bind_transfer(transfer, cursor(2), 1).unwrap();
        assert!(batches.bind_transfer(transfer, cursor(2), 1).is_err());
        stage(&cache, &mut doc, 1);
        assert!(capture(&mut batches, &cache, 1).is_err());
        stage(&cache, &mut doc, 2);
        assert!(capture(&mut batches, &cache, 2).is_err());
        assert!(
            batches
                .capture_transfer::<()>(
                    &cache,
                    &[record()],
                    1,
                    |v| v,
                    |_, _, _| Err("encoder".into())
                )
                .is_err()
        );
        let id = batches
            .capture_transfer(
                &cache,
                &[record()],
                1,
                |v| v,
                |id, _, tag| {
                    assert_eq!(tag, Some(transfer));
                    Ok(id.to_owned())
                },
            )
            .unwrap();
        batches.settle(&id, "lease", Ok(())).unwrap();
    }
}
