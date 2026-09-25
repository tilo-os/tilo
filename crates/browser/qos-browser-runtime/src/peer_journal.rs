//! Browser journal framing: preserve the exact old postcard body on migration.
use crate::peer_delivery_cursor::{CursorIdentity, StoredDeliveryCursor};
#[cfg(test)]
use automerge::transaction::Transactable;
use qos_peer_state::{DocumentRow, NeighbourMsg, StatePeer};
use serde::{Deserialize, Serialize};
const MAGIC: &[u8] = b"QOSJ";
#[derive(Serialize, Deserialize)]
struct LegacyJournal {
    state: Vec<u8>,
    outbox: Vec<NeighbourMsg>,
}
// Frozen v2 wire layout. Never append optional fields to this postcard struct.
#[derive(Serialize, Deserialize)]
struct JournalV2<S, O, R, C> {
    state: S,
    outbox: O,
    records: R,
    receipts: C,
}
type OwnedV2 = JournalV2<
    Vec<u8>,
    Vec<NeighbourMsg>,
    Vec<DocumentRow>,
    std::collections::BTreeMap<String, String>,
>;
#[derive(Serialize, Deserialize)]
struct JournalV3<B, D> {
    base: B,
    delivery: D,
}
// Explicit wrapper: V2 and V3 remain byte-for-byte frozen.
#[derive(Serialize, Deserialize)]
struct JournalV4<B, F> {
    base: B,
    descriptor: F,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DescriptorFloorV1 {
    pub version: u8,
    pub identity: CursorIdentity,
    pub generation: u64,
    pub descriptor_digest: [u8; 32],
}
/// Constructible only from exact PortableStorage committed Admission bytes.
/// Staged journal metadata and JavaScript acknowledgements cannot create this value.
pub(crate) struct AcknowledgedDescriptorFloor(DescriptorFloorV1);
impl AcknowledgedDescriptorFloor {
    pub fn read(
        cache: &ui::portable_storage::PortableStorage,
        identity: &CursorIdentity,
    ) -> Result<Self, String> {
        let floor = committed_storage(cache, &identity.room)
            .ok_or("descriptor floor not acknowledged")??
            .descriptor
            .ok_or("descriptor floor not acknowledged")?;
        floor.validate()?;
        if &floor.identity != identity {
            return Err("committed descriptor scope mismatch".into());
        }
        Ok(Self(floor))
    }
    pub fn floor(&self) -> &DescriptorFloorV1 {
        &self.0
    }
}
impl DescriptorFloorV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.generation == 0
            || self.descriptor_digest == [0; 32]
            || self.identity.workspace == 0
            || self.identity.room.trim().is_empty()
            || self.identity.room.len() > 256
            || self.identity.room.chars().any(char::is_control)
            || [
                self.identity.workspace_root,
                self.identity.device,
                self.identity.device_root,
                self.identity.host,
                self.identity.host_root,
            ]
            .contains(&[0; 32])
        {
            return Err("invalid descriptor floor".into());
        }
        Ok(())
    }
    pub fn expectation(&self) -> tilo_relay::delivery_v3::descriptor::GenerationExpectation {
        tilo_relay::delivery_v3::descriptor::GenerationExpectation::Current {
            generation: self.generation,
            descriptor_digest: self.descriptor_digest,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum JournalFormat {
    #[default]
    V2,
    V3,
    V4,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct JournalStorage {
    pub format: JournalFormat,
    pub delivery: Option<StoredDeliveryCursor>,
    pub descriptor: Option<DescriptorFloorV1>,
}
impl JournalStorage {
    pub fn advance_descriptor(&mut self, floor: DescriptorFloorV1) -> Result<(), String> {
        floor.validate()?;
        if self
            .delivery
            .as_ref()
            .is_some_and(|c| c.identity != floor.identity)
        {
            return Err("descriptor floor cursor identity mismatch".into());
        }
        if let Some(old) = &self.descriptor {
            if old.identity != floor.identity
                || floor.generation < old.generation
                || (floor.generation == old.generation
                    && floor.descriptor_digest != old.descriptor_digest)
            {
                return Err("descriptor floor rollback or scope mismatch".into());
            }
        }
        self.format = JournalFormat::V4;
        self.descriptor = Some(floor);
        Ok(())
    }
}
#[derive(Debug)]
pub(crate) struct Journal {
    pub state: Vec<u8>,
    pub outbox: Vec<NeighbourMsg>,
    pub records: Vec<DocumentRow>,
    pub receipts: std::collections::BTreeMap<String, String>,
    pub storage: JournalStorage,
}
impl Journal {
    pub fn from_state(state: &StatePeer, outbox: Vec<NeighbourMsg>) -> Result<Self, String> {
        Ok(Self {
            storage: JournalStorage::default(),
            state: state.durable()?,
            outbox,
            records: state.rows(),
            receipts: state
                .rows()
                .iter()
                .filter_map(|row| {
                    state
                        .durable_coverage(&row.app_id, &row.record_id)
                        .ok()
                        .flatten()
                        .map(|receipt| {
                            (
                                qos_peer_state::coverage::route_key(&row.app_id, &row.record_id),
                                receipt.receipt_id,
                            )
                        })
                })
                .collect(),
        })
    }

    pub fn into_unretired_outbox(self, state: &StatePeer) -> Vec<NeighbourMsg> {
        self.outbox
            .into_iter()
            .filter(|message| !state.outbox_message_retired(message))
            .collect()
    }

    /// Only matching committed record bytes and committed journal metadata prove a local basis.
    #[cfg(test)]
    pub fn durable_heads(&self, app: &str, record: &str, bytes: &[u8]) -> Option<Vec<String>> {
        let document = automerge::AutoCommit::load(bytes).ok()?;
        let heads = document
            .base_heads()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        self.match_document_heads(app, record, &heads)
    }

    /// Storage supplies heads of its exact committed bytes; the current journal
    /// must independently agree before they can prove an atomic durable basis.
    pub fn match_document_heads(
        &self,
        app: &str,
        record: &str,
        heads: &[String],
    ) -> Option<Vec<String>> {
        self.records
            .iter()
            .any(|row| row.app_id == app && row.record_id == record && row.heads == heads)
            .then(|| heads.to_vec())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let (base, storage) = if bytes.starts_with(MAGIC) {
            let body = bytes
                .get(MAGIC.len() + 1..)
                .ok_or("missing browser journal version")?;
            match bytes.get(MAGIC.len()) {
                Some(2) => (decode_exact::<OwnedV2>(body)?, JournalStorage::default()),
                Some(3) => {
                    let decoded: JournalV3<OwnedV2, Option<StoredDeliveryCursor>> =
                        decode_exact(body)?;
                    (
                        decoded.base,
                        JournalStorage {
                            format: JournalFormat::V3,
                            delivery: decoded.delivery,
                            descriptor: None,
                        },
                    )
                }
                Some(4) => {
                    let decoded: JournalV4<
                        JournalV3<OwnedV2, Option<StoredDeliveryCursor>>,
                        DescriptorFloorV1,
                    > = decode_exact(body)?;
                    decoded.descriptor.validate()?;
                    if decoded
                        .base
                        .delivery
                        .as_ref()
                        .is_some_and(|cursor| cursor.identity != decoded.descriptor.identity)
                    {
                        return Err("descriptor floor cursor identity mismatch".into());
                    }
                    (
                        decoded.base.base,
                        JournalStorage {
                            format: JournalFormat::V4,
                            delivery: decoded.base.delivery,
                            descriptor: Some(decoded.descriptor),
                        },
                    )
                }
                _ => return Err("unsupported browser journal version".into()),
            }
        } else {
            let old: LegacyJournal = decode_exact(bytes)?;
            (
                JournalV2 {
                    state: old.state,
                    outbox: old.outbox,
                    records: vec![],
                    receipts: Default::default(),
                },
                JournalStorage::default(),
            )
        };
        Ok(Self {
            state: base.state,
            outbox: base.outbox,
            records: base.records,
            receipts: base.receipts,
            storage,
        })
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.encode_for(self.storage.format)
    }
    pub fn encode_for(&self, format: JournalFormat) -> Result<Vec<u8>, String> {
        if let Some(cursor) = &self.storage.delivery {
            cursor.validate_storage_shape()?;
        }
        if self.storage.format == JournalFormat::V4 && format != JournalFormat::V4
            || self.storage.descriptor.is_some() && format != JournalFormat::V4
        {
            return Err("browser journal migration cannot discard descriptor floor".into());
        }
        let base = JournalV2 {
            state: &self.state,
            outbox: &self.outbox,
            records: &self.records,
            receipts: &self.receipts,
        };
        let mut bytes = MAGIC.to_vec();
        match format {
            JournalFormat::V2 => {
                if self.storage.format == JournalFormat::V3 || self.storage.delivery.is_some() {
                    return Err("browser journal migration cannot discard delivery metadata".into());
                }
                bytes.push(2);
                bytes.extend(postcard::to_allocvec(&base).map_err(|e| e.to_string())?);
            }
            JournalFormat::V4 => {
                let floor = self
                    .storage
                    .descriptor
                    .as_ref()
                    .ok_or("missing descriptor floor")?;
                floor.validate()?;
                if self
                    .storage
                    .delivery
                    .as_ref()
                    .is_some_and(|c| c.identity != floor.identity)
                {
                    return Err("descriptor floor cursor identity mismatch".into());
                }
                bytes.push(4);
                bytes.extend(
                    postcard::to_allocvec(&JournalV4 {
                        base: JournalV3 {
                            base,
                            delivery: &self.storage.delivery,
                        },
                        descriptor: floor,
                    })
                    .map_err(|e| e.to_string())?,
                );
            }
            JournalFormat::V3 => {
                bytes.push(3);
                bytes.extend(
                    postcard::to_allocvec(&JournalV3 {
                        base,
                        delivery: &self.storage.delivery,
                    })
                    .map_err(|e| e.to_string())?,
                );
            }
        }
        Ok(bytes)
    }
}
/// Validate canonical bytes without allocating another complete encoded journal.
fn decode_exact<T: serde::de::DeserializeOwned + Serialize>(bytes: &[u8]) -> Result<T, String> {
    let (value, trailing) = postcard::take_from_bytes::<T>(bytes).map_err(|e| e.to_string())?;
    if !trailing.is_empty() {
        return Err("trailing browser journal bytes".into());
    }
    postcard::serialize_with_flavor(&value, ExactBytes(bytes))
        .map_err(|_| "noncanonical browser journal bytes".to_string())?;
    Ok(value)
}
struct ExactBytes<'a>(&'a [u8]);
impl postcard::ser_flavors::Flavor for ExactBytes<'_> {
    type Output = ();
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.try_extend(&[byte])
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        if !self.0.starts_with(bytes) {
            return Err(postcard::Error::SerializeBufferFull);
        }
        self.0 = &self.0[bytes.len()..];
        Ok(())
    }
    fn finalize(self) -> postcard::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(postcard::Error::SerializeBufferFull)
        }
    }
}
/// Storage metadata of the committed Admission journal (delivery cursor and
/// descriptor floor), decoded at most once per exact committed bytes. Every
/// delivery step reads it (often several times), and a full journal decode
/// copies the whole growing state and outbox. Keyed by the exact bytes, like
/// `HealthProjection`: revisions can restart when storage is rehydrated. The
/// bytes are borrowed from the cache, never copied, on a hit.
pub(crate) fn committed_storage(
    cache: &ui::portable_storage::PortableStorage,
    room: &str,
) -> Option<Result<JournalStorage, String>> {
    thread_local! {
        static COMMITTED: std::cell::RefCell<Option<(Vec<u8>, Result<JournalStorage, String>)>> =
            const { std::cell::RefCell::new(None) };
    }
    let key = ui::portable_storage::StorageKey::Admission {
        workspace: room.to_owned(),
    };
    cache.with_committed(&key, |_, value| {
        COMMITTED.with_borrow_mut(|memo| {
            if let Some((bytes, decoded)) = memo.as_ref()
                && bytes.as_slice() == value.bytes.as_slice()
            {
                return decoded.clone();
            }
            let decoded = Journal::decode(&value.bytes).map(|journal| journal.storage);
            if value.bytes.len() <= HEALTH_CACHE_BYTES {
                *memo = Some((value.bytes.clone(), decoded.clone()));
            } else {
                *memo = None;
            }
            decoded
        })
    })
}
/// Cache decoding only, never admission or time-dependent permission decisions.
/// Exact bytes are the key: revisions can restart when storage is rehydrated.
#[derive(Default)]
pub(crate) struct HealthProjection {
    journal: DecodedBytes<Journal>,
}
const HEALTH_CACHE_BYTES: usize = 8 * 1024 * 1024;
struct DecodedBytes<T> {
    cached: Option<(Vec<u8>, Option<std::rc::Rc<T>>)>,
}
impl<T> Default for DecodedBytes<T> {
    fn default() -> Self {
        Self { cached: None }
    }
}
impl<T> DecodedBytes<T> {
    fn get(
        &mut self,
        bytes: &[u8],
        decode: impl FnOnce(&[u8]) -> Option<T>,
    ) -> Option<std::rc::Rc<T>> {
        if let Some((previous, value)) = &self.cached
            && previous == bytes
        {
            return value.clone();
        }
        if bytes.len() > HEALTH_CACHE_BYTES {
            self.cached = None;
            return decode(bytes).map(std::rc::Rc::new);
        }
        self.cached = Some((bytes.to_vec(), decode(bytes).map(std::rc::Rc::new)));
        self.cached.as_ref().and_then(|(_, value)| value.clone())
    }
}
impl HealthProjection {
    pub fn journal(&mut self, bytes: &[u8]) -> Option<std::rc::Rc<Journal>> {
        self.journal.get(bytes, |bytes| Journal::decode(bytes).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn floor(generation: u64) -> DescriptorFloorV1 {
        DescriptorFloorV1 {
            version: 1,
            identity: crate::peer_delivery_cursor::tests::cursor(1).identity,
            generation,
            descriptor_digest: [generation as u8; 32],
        }
    }
    #[test]
    fn floor_rejects_rollback_equivocation_scope_change_and_malformed_reload() {
        let mut storage = JournalStorage::default();
        storage.advance_descriptor(floor(2)).unwrap();
        let previous = storage.clone();
        assert!(storage.advance_descriptor(floor(1)).is_err());
        let mut altered = floor(2);
        altered.descriptor_digest = [8; 32];
        assert!(storage.advance_descriptor(altered).is_err());
        let mut altered = floor(3);
        altered.identity.host = [8; 32];
        assert!(storage.advance_descriptor(altered).is_err());
        assert_eq!(storage, previous);
        storage.advance_descriptor(floor(2)).unwrap();
        storage.advance_descriptor(floor(3)).unwrap();
        for invalid in [0_u64, 4] {
            let mut f = floor(invalid);
            if invalid == 4 {
                f.descriptor_digest = [0; 32];
            }
            let mut bytes = b"QOSJ\x04\x01\x09\x00\x00\x00\x00".to_vec();
            bytes.extend(postcard::to_allocvec(&f).unwrap());
            assert!(Journal::decode(&bytes).is_err());
        }
    }
    #[test]
    fn descriptor_signing_basis_requires_exact_committed_admission_bytes() {
        use ui::portable_storage::{PortableStorage, StorageKey};
        let cache = PortableStorage::new("profile", "device").unwrap();
        let first = floor(1);
        let key = StorageKey::Admission {
            workspace: first.identity.room.clone(),
        };
        let mut journal = Journal::decode(b"QOSJ\x02\x01\x09\x00\x00\x00").unwrap();
        journal.storage.advance_descriptor(first.clone()).unwrap();
        cache
            .put(key.clone(), journal.encode().unwrap(), None)
            .unwrap();
        assert!(AcknowledgedDescriptorFloor::read(&cache, &first.identity).is_err());
        let writes = cache.pending_writes();
        for write in writes {
            cache.acknowledge(&write, Err("quota".into())).unwrap();
        }
        assert!(AcknowledgedDescriptorFloor::read(&cache, &first.identity).is_err());
        for write in cache.pending_writes() {
            cache.acknowledge(&write, Ok(())).unwrap();
        }
        assert_eq!(
            AcknowledgedDescriptorFloor::read(&cache, &first.identity)
                .unwrap()
                .floor(),
            &first
        );
        journal.storage.advance_descriptor(floor(2)).unwrap();
        cache
            .put(key.clone(), journal.encode().unwrap(), None)
            .unwrap();
        assert_eq!(
            AcknowledgedDescriptorFloor::read(&cache, &first.identity)
                .unwrap()
                .floor(),
            &first,
            "dirty memory cannot advance the durable floor"
        );
        let mut foreign = first.identity.clone();
        foreign.host = [42; 32];
        assert!(AcknowledgedDescriptorFloor::read(&cache, &foreign).is_err());
        let saved = cache.status(&key).unwrap().committed.unwrap().1.bytes;
        let restored = Journal::decode(&saved).unwrap();
        assert_eq!(restored.storage.descriptor, Some(first));
        assert!(restored.encode_for(JournalFormat::V3).is_err());
    }

    #[test]
    fn descriptor_floor_v4_preserves_old_body_and_rejects_downgrade() {
        let identity = crate::peer_delivery_cursor::tests::cursor(1).identity;
        // Literal v2 body and absent v3 cursor, followed by the explicit v1 floor.
        let mut bytes = b"QOSJ\x04\x01\x09\x00\x00\x00\x00".to_vec();
        bytes.extend(postcard::to_allocvec(&(1_u8, &identity, 7_u64, [9_u8; 32])).unwrap());
        let decoded = Journal::decode(&bytes).unwrap();
        assert_eq!(decoded.state, [9]);
        assert_eq!(decoded.encode().unwrap(), bytes);
        assert!(decoded.encode_for(JournalFormat::V2).is_err());
        assert!(decoded.encode_for(JournalFormat::V3).is_err());
    }

    #[test]
    fn reader_preserves_v3_without_activating_v3_for_legacy_workspaces() {
        // Hand-encoded postcard: state=[9], outbox=[], records=[], receipts={};
        // v3 adds an absent cursor. Its writer floor must survive hydration.
        let v2 = b"QOSJ\x02\x01\x09\x00\x00\x00";
        let v3 = b"QOSJ\x03\x01\x09\x00\x00\x00\x00";
        assert_eq!(Journal::decode(v2).unwrap().encode().unwrap(), v2);
        assert_eq!(Journal::decode(v3).unwrap().encode().unwrap(), v3);
        assert_eq!(Journal::decode(&[1, 9, 0]).unwrap().encode().unwrap(), v2);
    }

    #[test]
    fn journal_rejects_noncanonical_postcard_before_rewriting_evidence() {
        // Overlong encoding of the state vector's length (one).
        assert!(Journal::decode(b"QOSJ\x02\x81\x00\x09\x00\x00\x00").is_err());
    }

    #[test]
    fn cursor_journal_preserves_scope_and_refuses_downgrade() {
        let mut journal = Journal::decode(b"QOSJ\x02\x01\x09\x00\x00\x00").unwrap();
        let cursor = crate::peer_delivery_cursor::tests::cursor(11);
        journal.storage = JournalStorage {
            format: JournalFormat::V3,
            delivery: Some(cursor.clone()),
            descriptor: None,
        };
        let bytes = journal.encode().unwrap();
        let decoded = Journal::decode(&bytes).unwrap();
        assert_eq!(decoded.storage.delivery, Some(cursor));
        assert_eq!(decoded.encode().unwrap(), bytes);
        assert!(decoded.encode_for(JournalFormat::V2).is_err());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(Journal::decode(&trailing).is_err());
    }

    #[test]
    fn unchanged_exact_bytes_decode_once_and_changed_invalid_bytes_invalidate() {
        let mut cache = DecodedBytes::default();
        let mut calls = 0;
        for _ in 0..100 {
            assert_eq!(
                *cache
                    .get(b"valid", |_| {
                        calls += 1;
                        Some(42)
                    })
                    .unwrap(),
                42
            );
        }
        assert_eq!(
            calls, 1,
            "unchanged committed bytes must not be reparsed per status report"
        );
        assert!(
            cache
                .get(b"bad", |_| {
                    calls += 1;
                    None
                })
                .is_none()
        );
        assert!(
            cache
                .get(b"bad", |_| panic!("invalid result should also be cached"))
                .is_none()
        );
        assert_eq!(
            *cache
                .get(b"valid", |_| {
                    calls += 1;
                    Some(43)
                })
                .unwrap(),
            43
        );
        assert_eq!(calls, 3);
    }
    #[test]
    fn oversized_health_bytes_are_decoded_without_retention() {
        let mut cache = DecodedBytes::default();
        cache.get(b"small", |_| Some(1));
        let large = vec![0; HEALTH_CACHE_BYTES + 1];
        let mut calls = 0;
        for _ in 0..2 {
            assert_eq!(
                *cache
                    .get(&large, |_| {
                        calls += 1;
                        Some(2)
                    })
                    .unwrap(),
                2
            );
        }
        assert_eq!(calls, 2);
        assert!(cache.cached.is_none());
    }
    #[test]
    fn known_heads_never_reuse_a_different_committed_journal_basis() {
        use automerge::{AutoCommit, ROOT};
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "text", "A").unwrap();
        doc.save();
        let heads_a = doc
            .base_heads()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let mut journal = Journal {
            state: vec![],
            outbox: vec![],
            records: vec![DocumentRow {
                app_id: "app".into(),
                record_id: "record".into(),
                heads: heads_a.clone(),
            }],
            receipts: Default::default(),
            storage: JournalStorage::default(),
        };
        let mut projection = HealthProjection::default();
        assert_eq!(
            journal.match_document_heads("app", "record", &heads_a),
            Some(heads_a.clone())
        );
        doc.put(ROOT, "text", "B").unwrap();
        doc.save();
        let heads_b = doc
            .base_heads()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        journal.records[0].heads = heads_b.clone();
        assert!(
            journal
                .match_document_heads("app", "record", &heads_a)
                .is_none()
        );
        assert_eq!(
            journal.match_document_heads("app", "record", &heads_b),
            Some(heads_b.clone())
        );
        assert!(
            journal
                .match_document_heads("other-app", "record", &heads_b)
                .is_none()
        );
        assert!(
            journal
                .match_document_heads("app", "other-record", &heads_b)
                .is_none()
        );
        let encoded = journal.encode().unwrap();
        let first = projection.journal(&encoded).unwrap();
        assert!(std::rc::Rc::ptr_eq(
            &first,
            &projection.journal(&encoded).unwrap()
        ));
        assert!(projection.journal(b"corrupt").is_none());
    }
    #[test]
    fn older_atomic_commit_never_marks_newer_canonical_record_durable() {
        use automerge::{AutoCommit, ROOT, transaction::Transactable};
        use ui::portable_storage::{PortableStorage, StorageKey};
        let cache = PortableStorage::new("profile", "device").unwrap();
        let record = StorageKey::Record {
            workspace: "room".into(),
            package: "app".into(),
            record: "record".into(),
        };
        let admission = StorageKey::Admission {
            workspace: "room".into(),
        };
        let mut document = AutoCommit::new();
        document.put(ROOT, "text", "A").unwrap();
        document.commit();
        let heads_a = document
            .get_heads()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let actor = document.get_actor().to_bytes().to_vec();
        let journal = |heads| Journal {
            state: vec![1],
            outbox: vec![NeighbourMsg::SyncReq {
                room: "immutable-operation".into(),
                since: vec![],
                invite_id: None,
            }],
            records: vec![DocumentRow {
                app_id: "app".into(),
                record_id: "record".into(),
                heads,
            }],
            receipts: Default::default(),
            storage: JournalStorage::default(),
        };
        cache
            .put(record.clone(), document.save(), Some(actor.clone()))
            .unwrap();
        cache
            .put(
                admission.clone(),
                journal(heads_a.clone()).encode().unwrap(),
                None,
            )
            .unwrap();
        let batch_a = cache.pending_writes();
        document.put(ROOT, "text", "A+B").unwrap();
        document.commit();
        let heads_b = document
            .get_heads()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        cache
            .put(record.clone(), document.save(), Some(actor.clone()))
            .unwrap();
        cache
            .put(
                admission.clone(),
                journal(heads_b.clone()).encode().unwrap(),
                None,
            )
            .unwrap();
        for write in batch_a {
            cache.acknowledge(&write, Ok(())).unwrap();
        }
        let old_record = cache.status(&record).unwrap().committed.unwrap().1;
        let old_journal =
            Journal::decode(&cache.status(&admission).unwrap().committed.unwrap().1.bytes).unwrap();
        assert_eq!(
            old_journal.durable_heads("app", "record", &old_record.bytes),
            Some(heads_a.clone())
        );
        let committed_heads = cache
            .committed_document_heads(&record)
            .unwrap()
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(
            old_journal.match_document_heads("app", "record", &committed_heads),
            Some(heads_a.clone())
        );
        assert!(
            journal(heads_b.clone())
                .match_document_heads("app", "record", &committed_heads)
                .is_none(),
            "new journal metadata cannot turn an old record acknowledgement into a new durable basis"
        );
        assert_ne!(heads_a, heads_b);
        assert!(cache.flush(&record).is_err());
        assert!(cache.flush(&admission).is_err());
        assert!(
            old_journal
                .durable_heads("app", "other", &old_record.bytes)
                .is_none()
        );
        for write in cache.pending_writes() {
            cache.acknowledge(&write, Err("abort".into())).unwrap();
        }
        assert_eq!(cache.get(&record).unwrap().actor, Some(actor));
        assert_eq!(
            cache.status(&record).unwrap().committed.unwrap().1.bytes,
            old_record.bytes
        );
        assert_eq!(
            cache
                .committed_document_heads(&record)
                .unwrap()
                .unwrap()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            committed_heads
        );
    }
    #[test]
    fn legacy_bytes_and_operation_identity_survive_versioned_roundtrip() {
        let outbox = vec![NeighbourMsg::SyncReq {
            since: vec![],
            room: "room".into(),
            invite_id: None,
        }];
        let bytes = postcard::to_allocvec(&LegacyJournal {
            state: vec![9, 8, 7],
            outbox: outbox.clone(),
        })
        .unwrap();
        let journal = Journal::decode(&bytes).unwrap();
        assert_eq!(journal.state, vec![9, 8, 7]);
        assert_eq!(
            postcard::to_allocvec(&journal.outbox).unwrap(),
            postcard::to_allocvec(&outbox).unwrap()
        );
        let current = journal.encode().unwrap();
        assert!(current.starts_with(b"QOSJ\x02"));
        let restored = Journal::decode(&current).unwrap();
        assert_eq!(restored.state, journal.state);
        assert_eq!(
            postcard::to_allocvec(&restored.outbox).unwrap(),
            postcard::to_allocvec(&outbox).unwrap()
        );
    }
    #[test]
    fn unknown_version_and_trailing_data_fail_explicitly() {
        assert!(
            Journal::decode(b"QOSJ\x05")
                .unwrap_err()
                .contains("version")
        );
        let mut bytes = Journal {
            state: vec![1],
            outbox: vec![],
            records: vec![],
            receipts: Default::default(),
            storage: JournalStorage::default(),
        }
        .encode()
        .unwrap();
        bytes.push(1);
        assert!(Journal::decode(&bytes).is_err());
    }

    #[test]
    fn restored_journal_retires_only_exact_durable_receipts_and_normalizes_roundtrip() {
        use automerge::{AutoCommit, ROOT};
        use ed25519_dalek::SigningKey;
        use qos_peer_state::{SeedDocument, SeedState, StatePeer, TransportOrigin, seed_room};
        use tilo_user_identity::{
            DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert,
        };
        use web_time::{Duration, SystemTime};

        fn certificate(root: &SigningKey, key: &SigningKey, label: &str) -> DeviceCert {
            let mut certificate = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: key.verifying_key().to_bytes(),
                device_label: label.into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut certificate, root).unwrap();
            certificate
        }

        fn seed_document(record_id: &str) -> SeedDocument {
            let mut document = AutoCommit::new();
            document.put(ROOT, "value", "seed").unwrap();
            document.commit();
            SeedDocument {
                app_id: "test.app".into(),
                record_id: record_id.into(),
                kind: "test".into(),
                bytes: document.save(),
            }
        }

        fn publish(
            state: &mut StatePeer,
            record: &str,
            key: &SigningKey,
            now: SystemTime,
        ) -> NeighbourMsg {
            let mut document =
                AutoCommit::load(&state.document("test.app", record).unwrap()).unwrap();
            let before = document.get_heads();
            document.put(ROOT, "saved", record).unwrap();
            document.commit();
            state
                .publish_incremental("test.app", record, document.save_after(&before), key, now)
                .unwrap()
        }

        let root = SigningKey::from_bytes(&[71; 32]);
        let authority_key = SigningKey::from_bytes(&[72; 32]);
        let member_key = SigningKey::from_bytes(&[73; 32]);
        let authority = certificate(&root, &authority_key, "authority");
        let member = certificate(&root, &member_key, "member");
        let roster = vec![authority.clone(), member.clone()];
        let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000);
        let mut client = StatePeer::new(
            room,
            authority.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        client
            .install_seed(
                SeedState {
                    room_changes: seed_room(room, &authority, &roster).unwrap(),
                    documents: vec![
                        seed_document("retired"),
                        seed_document("causal-only"),
                        seed_document("unacknowledged"),
                    ],
                },
                &authority_key,
                now,
            )
            .unwrap();
        let base = client;

        let mut causal_client = base.clone();
        let mut causal_governor = base.clone();
        let causal_only = publish(&mut causal_client, "causal-only", &member_key, now);
        causal_governor
            .receive_from(
                TransportOrigin::RelayDelta(member_key.verifying_key().to_bytes()),
                causal_only.clone(),
                now,
            )
            .unwrap();
        for message in causal_governor
            .bootstrap_messages(&authority_key, now)
            .unwrap()
        {
            causal_client
                .receive_from(
                    TransportOrigin::RelayDelta(authority.device_public_key),
                    message,
                    now,
                )
                .unwrap();
        }
        assert!(causal_client.covers(&causal_only));
        assert!(!causal_client.outbox_message_retired(&causal_only));
        let causal_journal = Journal::from_state(&causal_client, vec![causal_only.clone()])
            .unwrap()
            .encode()
            .unwrap();
        let causal_journal = Journal::decode(&causal_journal).unwrap();
        let causal_restored = StatePeer::restore(
            &causal_journal.state,
            room,
            authority.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        assert_eq!(
            postcard::to_allocvec(&causal_journal.into_unretired_outbox(&causal_restored)).unwrap(),
            postcard::to_allocvec(&vec![causal_only]).unwrap(),
            "causal coverage without its designated receipt remains in custody"
        );

        let mut client = base.clone();
        let mut governor = base.clone();
        let retired = publish(&mut client, "retired", &member_key, now);
        governor
            .receive_from(
                TransportOrigin::RelayDelta(member_key.verifying_key().to_bytes()),
                retired.clone(),
                now,
            )
            .unwrap();
        for message in governor
            .durable_bootstrap_messages(&authority_key, now, 5)
            .unwrap()
        {
            client
                .receive_from(
                    TransportOrigin::RelayDelta(authority.device_public_key),
                    message,
                    now,
                )
                .unwrap();
        }
        let unacknowledged = publish(&mut client, "unacknowledged", &member_key, now);
        assert!(client.outbox_message_retired(&retired));
        assert!(!client.covers(&unacknowledged));

        let saved = Journal::from_state(&client, vec![retired, unacknowledged.clone()])
            .unwrap()
            .encode()
            .unwrap();
        let mut journal = Journal::decode(&saved).unwrap();
        let exact_outbox = postcard::to_allocvec(&journal.outbox).unwrap();
        let exact_state = journal.state.clone();
        journal.storage = JournalStorage {
            format: JournalFormat::V3,
            delivery: Some(crate::peer_delivery_cursor::tests::cursor(17)),
            descriptor: None,
        };
        let migrated = journal.encode().unwrap();
        let journal = Journal::decode(&migrated).unwrap();
        assert_eq!(
            postcard::to_allocvec(&journal.outbox).unwrap(),
            exact_outbox
        );
        assert_eq!(journal.state, exact_state);
        assert!(journal.encode_for(JournalFormat::V2).is_err());
        let restored = StatePeer::restore(
            &journal.state,
            room,
            authority.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        let outbox = journal.into_unretired_outbox(&restored);
        assert_eq!(
            postcard::to_allocvec(&outbox).unwrap(),
            postcard::to_allocvec(&vec![unacknowledged.clone()]).unwrap()
        );
        assert!(
            outbox
                .iter()
                .all(|message| !restored.outbox_message_retired(message))
        );

        let incomplete = StatePeer::new(
            room,
            authority.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        assert!(!incomplete.has_governance());
        let incomplete = Journal::from_state(&incomplete, vec![unacknowledged.clone()])
            .unwrap()
            .encode()
            .unwrap();
        let incomplete = Journal::decode(&incomplete).unwrap();
        let incomplete_restored = StatePeer::restore(
            &incomplete.state,
            room,
            authority.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        assert!(!incomplete_restored.has_governance());
        assert_eq!(
            postcard::to_allocvec(&incomplete.into_unretired_outbox(&incomplete_restored)).unwrap(),
            postcard::to_allocvec(&vec![unacknowledged.clone()]).unwrap(),
            "an incomplete restored owner retains its unacknowledged evidence"
        );

        let normalized = Journal::from_state(&restored, outbox.clone())
            .unwrap()
            .encode()
            .unwrap();
        let normalized = Journal::decode(&normalized).unwrap();
        assert_eq!(
            postcard::to_allocvec(&normalized.outbox).unwrap(),
            postcard::to_allocvec(&outbox).unwrap()
        );
        let restored_again = StatePeer::restore(
            &normalized.state,
            room,
            authority,
            roster,
            RevocationList::new(),
            now,
        )
        .unwrap();
        assert_eq!(
            postcard::to_allocvec(&normalized.into_unretired_outbox(&restored_again)).unwrap(),
            postcard::to_allocvec(&outbox).unwrap(),
            "normalization is stable across another restart"
        );
    }
}
