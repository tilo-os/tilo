//! Browser journal framing: preserve the exact old postcard body on migration.
use automerge::transaction::Transactable;
use qos_peer_state::{DocumentRow, NeighbourMsg};
use serde::{Deserialize, Serialize};
const MAGIC: &[u8] = b"QOSJ";
#[derive(Serialize, Deserialize)]
struct LegacyJournal {
    state: Vec<u8>,
    outbox: Vec<NeighbourMsg>,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Journal {
    pub state: Vec<u8>,
    pub outbox: Vec<NeighbourMsg>,
    pub records: Vec<DocumentRow>,
    pub receipts: std::collections::BTreeMap<String, String>,
}
impl Journal {
    /// Only matching committed record bytes and committed journal metadata prove a local basis.
    pub fn durable_heads(&self, app: &str, record: &str, bytes: &[u8]) -> Option<Vec<String>> {
        let document = automerge::AutoCommit::load(bytes).ok()?;
        let heads = document
            .base_heads()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        self.records
            .iter()
            .any(|row| row.app_id == app && row.record_id == record && row.heads == heads)
            .then_some(heads)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.starts_with(MAGIC) {
            if bytes.get(MAGIC.len()) != Some(&2) {
                return Err("unsupported browser journal version".into());
            }
            let (journal, tail) = postcard::take_from_bytes::<Self>(&bytes[MAGIC.len() + 1..])
                .map_err(|e| e.to_string())?;
            if !tail.is_empty() {
                return Err("trailing browser journal bytes".into());
            }
            return Ok(journal);
        }
        let (old, tail) =
            postcard::take_from_bytes::<LegacyJournal>(bytes).map_err(|e| e.to_string())?;
        if !tail.is_empty() {
            return Err("trailing legacy browser journal bytes".into());
        }
        Ok(Self {
            state: old.state,
            outbox: old.outbox,
            records: vec![],
            receipts: Default::default(),
        })
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut bytes = MAGIC.to_vec();
        bytes.push(2);
        bytes.extend(postcard::to_allocvec(self).map_err(|e| e.to_string())?);
        Ok(bytes)
    }
}
/// Cache decoding only, never admission or time-dependent permission decisions.
/// Exact bytes are the key: revisions can restart when storage is rehydrated.
#[derive(Default)]
pub(crate) struct HealthProjection {
    journal: DecodedBytes<Journal>,
    records: std::collections::BTreeMap<(String, String), DecodedBytes<Vec<String>>>,
}
const HEALTH_CACHE_BYTES: usize = 8 * 1024 * 1024;
struct DecodedBytes<T> {
    cached: Option<(Vec<u8>, Option<std::rc::Rc<T>>)>,
}
impl<T> Default for DecodedBytes<T> {
    fn default() -> Self { Self { cached: None } }
}
impl<T> DecodedBytes<T> {
    fn get(&mut self, bytes: &[u8], decode: impl FnOnce(&[u8]) -> Option<T>) -> Option<std::rc::Rc<T>> {
        if let Some((previous, value)) = &self.cached
            && previous == bytes { return value.clone(); }
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
    pub fn durable_heads(&mut self, journal: &Journal, app: &str, record: &str, bytes: &[u8]) -> Option<Vec<String>> {
        let key = (app.to_owned(), record.to_owned());
        // At most 64 routes and 8 MiB of encoded record snapshots per peer.
        // Oversized snapshots are decoded without retention by DecodedBytes.
        let retained_except_current = |records: &std::collections::BTreeMap<(String, String), DecodedBytes<Vec<String>>>| {
            records.iter().filter(|(route, _)| **route != key)
                .map(|(_, entry)| entry.cached.as_ref().map_or(0, |(bytes, _)| bytes.len())).sum::<usize>()
        };
        while !self.records.is_empty()
            && ((self.records.len() >= 64 && !self.records.contains_key(&key))
                || retained_except_current(&self.records) + bytes.len().min(HEALTH_CACHE_BYTES) > HEALTH_CACHE_BYTES) {
            self.records.pop_first();
        }
        let heads = self.records.entry(key).or_default().get(bytes, |bytes| {
            let document = automerge::AutoCommit::load(bytes).ok()?;
            Some(document.base_heads().iter().map(ToString::to_string).collect())
        })?;
        // Recheck the CURRENT committed journal every time; decoded bytes alone
        // cannot establish an atomic durable record/journal basis.
        journal.records.iter().any(|row| row.app_id == app && row.record_id == record && row.heads == *heads)
            .then(|| (*heads).clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unchanged_exact_bytes_decode_once_and_changed_invalid_bytes_invalidate() {
        let mut cache = DecodedBytes::default();
        let mut calls = 0;
        for _ in 0..100 {
            assert_eq!(*cache.get(b"valid", |_| { calls += 1; Some(42) }).unwrap(), 42);
        }
        assert_eq!(calls, 1, "unchanged committed bytes must not be reparsed per status report");
        assert!(cache.get(b"bad", |_| { calls += 1; None }).is_none());
        assert!(cache.get(b"bad", |_| panic!("invalid result should also be cached")).is_none());
        assert_eq!(*cache.get(b"valid", |_| { calls += 1; Some(43) }).unwrap(), 43);
        assert_eq!(calls, 3);
    }
    #[test]
    fn oversized_health_bytes_are_decoded_without_retention() {
        let mut cache = DecodedBytes::default();
        cache.get(b"small", |_| Some(1));
        let large = vec![0; HEALTH_CACHE_BYTES + 1];
        let mut calls = 0;
        for _ in 0..2 { assert_eq!(*cache.get(&large, |_| { calls += 1; Some(2) }).unwrap(), 2); }
        assert_eq!(calls, 2);
        assert!(cache.cached.is_none());
    }
    #[test]
    fn decoded_heads_never_reuse_a_different_committed_journal_basis() {
        use automerge::{AutoCommit, ROOT};
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "text", "A").unwrap();
        let bytes_a = doc.save();
        let heads_a = doc.base_heads().iter().map(ToString::to_string).collect::<Vec<_>>();
        let mut journal = Journal { state: vec![], outbox: vec![], records: vec![DocumentRow {
            app_id: "app".into(), record_id: "record".into(), heads: heads_a.clone(),
        }], receipts: Default::default() };
        let mut projection = HealthProjection::default();
        assert_eq!(projection.durable_heads(&journal, "app", "record", &bytes_a), Some(heads_a.clone()));
        doc.put(ROOT, "text", "B").unwrap();
        let bytes_b = doc.save();
        let heads_b = doc.base_heads().iter().map(ToString::to_string).collect::<Vec<_>>();
        journal.records[0].heads = heads_b.clone();
        assert!(projection.durable_heads(&journal, "app", "record", &bytes_a).is_none());
        assert_eq!(projection.durable_heads(&journal, "app", "record", &bytes_b), Some(heads_b));
        assert!(projection.durable_heads(&journal, "other-app", "record", &bytes_b).is_none());
        assert!(projection.durable_heads(&journal, "app", "other-record", &bytes_b).is_none());
        let encoded = journal.encode().unwrap();
        let first = projection.journal(&encoded).unwrap();
        assert!(std::rc::Rc::ptr_eq(&first, &projection.journal(&encoded).unwrap()));
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
            Journal::decode(b"QOSJ\x03")
                .unwrap_err()
                .contains("version")
        );
        let mut bytes = Journal {
            state: vec![1],
            outbox: vec![],
            records: vec![],
            receipts: Default::default(),
        }
        .encode()
        .unwrap();
        bytes.push(1);
        assert!(Journal::decode(&bytes).is_err());
    }
}
