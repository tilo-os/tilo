//! Exact canonical admission observations. These are not durable commit receipts.
use crate::ReceiveOutcome;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
mod resources;
pub(crate) use resources::BoundedExactAdmissions;

pub const MAX_EXACT_ADMISSIONS: usize = 65_536;
pub const MAX_EXACT_ADMISSION_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmissionError {
    WriterSelectionRequired,
    ConflictingSequence,
    ResourceLimit,
    Rejected(String),
}
impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WriterSelectionRequired => {
                f.write_str("exact admission requires explicit native v5 writer selection")
            }
            Self::ConflictingSequence => {
                f.write_str("signed operation conflicts with exact admitted sequence")
            }
            Self::ResourceLimit => f.write_str("exact admission index quota exceeded"),
            Self::Rejected(reason) => f.write_str(reason),
        }
    }
}
impl std::error::Error for AdmissionError {}
impl From<String> for AdmissionError {
    fn from(value: String) -> Self {
        Self::Rejected(value)
    }
}
impl From<&str> for AdmissionError {
    fn from(value: &str) -> Self {
        Self::Rejected(value.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct AdmissionKey {
    pub room: String,
    pub app: String,
    pub record: String,
    pub author: [u8; 32],
    pub sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentAdmission {
    pub(crate) key: AdmissionKey,
    pub(crate) operation_id: [u8; 32],
}
impl DocumentAdmission {
    pub fn room(&self) -> &str {
        &self.key.room
    }
    pub fn app(&self) -> &str {
        &self.key.app
    }
    pub fn record(&self) -> &str {
        &self.key.record
    }
    pub fn author(&self) -> [u8; 32] {
        self.key.author
    }
    pub fn sequence(&self) -> u64 {
        self.key.sequence
    }
    pub fn operation_id(&self) -> [u8; 32] {
        self.operation_id
    }
    pub(crate) fn from_op(
        room: &str,
        app: &str,
        record: &str,
        op: &collab_types::SignedOp,
    ) -> Result<Self, AdmissionError> {
        let bytes = collab_types::canonical_bytes(op)
            .map_err(|e| AdmissionError::Rejected(e.to_string()))?;
        Ok(Self {
            key: AdmissionKey {
                room: room.into(),
                app: app.into(),
                record: record.into(),
                author: *op.author.as_bytes(),
                sequence: op.seq,
            },
            operation_id: *blake3::hash(&bytes).as_bytes(),
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MissingPrerequisite {
    Governance,
    Certificate,
    RecordCheckpoint,
    CheckpointCoverage,
    CausalDependencies,
    PersistedMls,
    ExactHistoricalEvidence,
    /// The app surface the op names is not here yet: the op is decided
    /// once it is.
    AppSurface,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputDisposition {
    Accepted(DocumentAdmission),
    Duplicate(DocumentAdmission),
    RetryLater(MissingPrerequisite),
    MetadataOnly,
    UnsupportedFamily,
}
#[derive(Clone, Debug)]
pub struct AdmissionReport {
    pub(crate) primary: InputDisposition,
    pub(crate) settled: Vec<DocumentAdmission>,
    pub(crate) summary: ReceiveOutcome,
}
impl AdmissionReport {
    pub fn primary(&self) -> &InputDisposition {
        &self.primary
    }
    pub fn settled(&self) -> &[DocumentAdmission] {
        &self.settled
    }
    pub fn summary(&self) -> &ReceiveOutcome {
        &self.summary
    }
    pub(crate) fn new(primary: InputDisposition, summary: ReceiveOutcome) -> Self {
        let settled = match &primary {
            InputDisposition::Accepted(op) => vec![op.clone()],
            _ => vec![],
        };
        Self {
            primary,
            settled,
            summary,
        }
    }
    pub(crate) fn metadata(summary: ReceiveOutcome) -> Self {
        Self::new(InputDisposition::MetadataOnly, summary)
    }
}
impl std::ops::Deref for AdmissionReport {
    type Target = ReceiveOutcome;
    fn deref(&self) -> &ReceiveOutcome {
        &self.summary
    }
}
impl std::ops::DerefMut for AdmissionReport {
    fn deref_mut(&mut self) -> &mut ReceiveOutcome {
        &mut self.summary
    }
}

/// Compact positive evidence only. Never evicted to meet a quota.
///
/// Every admission copies the peer it is decided in, and the index grows
/// with every write a room keeps: its settled entries are shared between
/// copies, so a copy costs the few entries since they last settled, not the
/// whole index.
#[derive(Clone, Default)]
pub(crate) struct ExactAdmissions {
    settled: std::sync::Arc<BTreeMap<AdmissionKey, [u8; 32]>>,
    /// Entries since the last settling, none of them in `settled`.
    recent: BTreeMap<AdmissionKey, [u8; 32]>,
    entry_bytes: usize,
}
/// Entries an index keeps apart before it settles them.
const RECENT_ADMISSIONS: usize = 256;
fn count_bytes(mut count: usize) -> usize {
    let mut bytes = 1usize;
    while count >= 128 {
        count >>= 7;
        // At most ten iterations for a usize.
        bytes = bytes.saturating_add(1);
    }
    bytes
}
impl ExactAdmissions {
    pub(crate) fn get(&self, key: &AdmissionKey) -> Option<&[u8; 32]> {
        self.recent.get(key).or_else(|| self.settled.get(key))
    }
    pub(crate) fn len(&self) -> usize {
        self.settled.len().saturating_add(self.recent.len())
    }
    /// Every entry, in key order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&AdmissionKey, &[u8; 32])> {
        let mut settled = self.settled.iter().peekable();
        let mut recent = self.recent.iter().peekable();
        std::iter::from_fn(move || match (settled.peek(), recent.peek()) {
            (Some(old), Some(new)) if old.0 < new.0 => settled.next(),
            (Some(_), None) => settled.next(),
            _ => recent.next(),
        })
    }
    /// Add an entry `check` measured as `bytes`, settling the recent ones
    /// once there are enough of them.
    pub(super) fn add(&mut self, key: AdmissionKey, operation_id: [u8; 32], bytes: usize) {
        self.recent.insert(key, operation_id);
        // `check` bounded the new total by MAX_EXACT_ADMISSION_BYTES.
        self.entry_bytes = self.entry_bytes.saturating_add(bytes);
        if self.recent.len() >= RECENT_ADMISSIONS {
            let recent = std::mem::take(&mut self.recent);
            std::sync::Arc::make_mut(&mut self.settled).extend(recent);
        }
    }
    /// The entries as the canonical index map: in key order, every one once.
    pub(super) fn serialize_entries<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (key, operation_id) in self.iter() {
            map.serialize_entry(key, operation_id)?;
        }
        map.end()
    }
    pub(crate) fn check(&self, op: &DocumentAdmission) -> Result<usize, AdmissionError> {
        if let Some(prior) = self.get(&op.key) {
            return if prior == &op.operation_id {
                Ok(0)
            } else {
                Err(AdmissionError::ConflictingSequence)
            };
        }
        if self.len() >= MAX_EXACT_ADMISSIONS {
            return Err(AdmissionError::ResourceLimit);
        }
        // Bound string lengths before counting the unchanged canonical key.
        let names = op
            .room()
            .len()
            .saturating_add(op.app().len())
            .saturating_add(op.record().len());
        if names > MAX_EXACT_ADMISSION_BYTES {
            return Err(AdmissionError::ResourceLimit);
        }
        let bytes = resources::entry_bytes(op)?;
        let total = self
            .entry_bytes
            .saturating_add(bytes)
            .saturating_add(count_bytes(self.len().saturating_add(1)));
        if total > MAX_EXACT_ADMISSION_BYTES {
            return Err(AdmissionError::ResourceLimit);
        }
        Ok(bytes)
    }
    pub(crate) fn insert(&mut self, op: &DocumentAdmission) -> Result<(), AdmissionError> {
        let bytes = self.check(op)?;
        if bytes != 0 {
            self.add(op.key.clone(), op.operation_id, bytes);
        }
        Ok(())
    }
    pub(crate) fn encode(&self) -> Result<Vec<u8>, AdmissionError> {
        let bytes = postcard::to_allocvec(&resources::WireEntries(self))
            .map_err(|e| AdmissionError::Rejected(e.to_string()))?;
        if bytes.len() > MAX_EXACT_ADMISSION_BYTES {
            return Err(AdmissionError::ResourceLimit);
        }
        Ok(bytes)
    }
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, AdmissionError> {
        resources::decode_index(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delivery_admission_index_bounds_and_duplicate_decode_are_fail_closed() {
        let fact = DocumentAdmission {
            key: AdmissionKey {
                room: String::new(),
                app: String::new(),
                record: "x".repeat(MAX_EXACT_ADMISSION_BYTES - 100),
                author: [0; 32],
                sequence: 0,
            },
            operation_id: [1; 32],
        };
        let mut index = ExactAdmissions::default();
        index.insert(&fact).unwrap();
        let before = index.encode().unwrap();
        let mut second = fact.clone();
        second.key.record = "different".into();
        assert_eq!(index.insert(&second), Err(AdmissionError::ResourceLimit));
        assert_eq!(index.encode().unwrap(), before);
        assert!(
            ExactAdmissions::decode(&[0x81, 0x80, 0x04]).is_err(),
            "65537 entries refused before reading any key"
        );
        assert!(
            ExactAdmissions::decode(&[0, 1]).is_err(),
            "trailing data refused"
        );
        let mut duplicate = vec![2];
        let row = postcard::to_allocvec(&(&second.key, second.operation_id)).unwrap();
        duplicate.extend(&row);
        duplicate.extend(&row);
        assert!(ExactAdmissions::decode(&duplicate).is_err());
        assert!(ExactAdmissions::decode(&vec![0; MAX_EXACT_ADMISSION_BYTES + 1]).is_err());
    }

    /// Copies share the settled entries; the index still reads and encodes
    /// as one map in key order, and decodes back to the same bytes.
    #[test]
    fn a_copy_shares_settled_admissions_and_encodes_one_ordered_index() {
        let admission = |sequence: u64| DocumentAdmission {
            key: AdmissionKey {
                room: "room".into(),
                app: "app".into(),
                record: "record".into(),
                author: [u8::try_from(sequence % 7).unwrap(); 32],
                sequence,
            },
            operation_id: [1; 32],
        };
        let mut index = ExactAdmissions::default();
        // Out of key order, past one settling.
        for sequence in (0..RECENT_ADMISSIONS as u64 + 40).rev() {
            index.insert(&admission(sequence)).unwrap();
        }
        let copy = index.clone();
        assert!(std::sync::Arc::ptr_eq(&index.settled, &copy.settled));
        assert!(copy.recent.len() < RECENT_ADMISSIONS);
        let keys: Vec<_> = index.iter().map(|(key, _)| key.clone()).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert_eq!(keys.len(), RECENT_ADMISSIONS + 40);
        let bytes = index.encode().unwrap();
        let whole: BTreeMap<_, _> = index.iter().map(|(k, v)| (k.clone(), *v)).collect();
        assert_eq!(bytes, postcard::to_allocvec(&whole).unwrap());
        assert_eq!(
            ExactAdmissions::decode(&bytes).unwrap().encode().unwrap(),
            bytes
        );
    }
}
