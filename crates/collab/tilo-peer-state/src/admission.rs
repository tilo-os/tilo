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

/// One writer of one record: whose writes a compaction's mark covers.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct WriterKey {
    pub room: String,
    pub app: String,
    pub record: String,
    pub author: [u8; 32],
}
impl WriterKey {
    fn of(key: &AdmissionKey) -> Self {
        Self {
            room: key.room.clone(),
            app: key.app.clone(),
            record: key.record.clone(),
            author: key.author,
        }
    }
    /// This writer's write at `sequence`.
    pub(crate) fn at(&self, sequence: u64) -> AdmissionKey {
        AdmissionKey {
            room: self.room.clone(),
            app: self.app.clone(),
            record: self.record.clone(),
            author: self.author,
            sequence,
        }
    }
}

/// Compact positive evidence only. Never evicted to meet a quota.
///
/// Every admission copies the peer it is decided in, and the index grows
/// with every write a room keeps: its settled entries are shared between
/// copies, so a copy costs the few entries since they last settled, not the
/// whole index. The settled entries also keep their encoding: a durable
/// copy of the index each governance change re-encodes only the entries
/// since they last settled, and the index is stored as its settled and its
/// recent entries apart, so whoever writes it to disk can tell the large
/// part it already wrote by pointer.
///
/// Once a compaction this peer admitted folds a record's writes into one
/// snapshot, their exact entries give way to one mark per writer: every
/// write of that writer at or below its mark is in the snapshot. A write a
/// mark covers is already decided, whatever its bytes, so it is ignored;
/// above the mark the evidence stays exact. The index then grows with the
/// writes since the last compaction and with the writers, not with every
/// write the room ever kept.
#[derive(Clone, Default)]
pub(crate) struct ExactAdmissions {
    settled: std::sync::Arc<Settled>,
    /// Entries since the last settling, none of them in `settled`.
    recent: BTreeMap<AdmissionKey, [u8; 32]>,
    entry_bytes: usize,
    /// The highest sequence of each writer a compaction folded away.
    marks: std::sync::Arc<BTreeMap<WriterKey, u64>>,
    mark_bytes: usize,
}
/// The settled entries of an [`ExactAdmissions`] and, once asked for, their
/// encoding as the index's first wire map.
#[derive(Default)]
struct Settled {
    entries: BTreeMap<AdmissionKey, [u8; 32]>,
    encoded: std::sync::OnceLock<std::sync::Arc<[u8]>>,
}
impl Clone for Settled {
    /// A copy is only ever made to be changed: it starts unencoded.
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            encoded: std::sync::OnceLock::new(),
        }
    }
}
impl std::ops::Deref for Settled {
    type Target = BTreeMap<AdmissionKey, [u8; 32]>;
    fn deref(&self) -> &Self::Target {
        &self.entries
    }
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
    /// Whether a compaction this index took in already folded the write
    /// `key` names: it is decided, and nothing about it is kept exactly.
    pub(crate) fn covers(&self, key: &AdmissionKey) -> bool {
        self.marks
            .get(&WriterKey::of(key))
            .is_some_and(|mark| key.sequence <= *mark)
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
    /// Every writer's mark, in key order.
    pub(crate) fn marks(&self) -> impl Iterator<Item = (&WriterKey, u64)> {
        self.marks.iter().map(|(writer, mark)| (writer, *mark))
    }
    /// Add an entry `check` measured as `bytes`, settling the recent ones
    /// once there are enough of them.
    pub(super) fn add(&mut self, key: AdmissionKey, operation_id: [u8; 32], bytes: usize) {
        self.recent.insert(key, operation_id);
        // `check` bounded the new total by MAX_EXACT_ADMISSION_BYTES.
        self.entry_bytes = self.entry_bytes.saturating_add(bytes);
        if self.recent.len() >= RECENT_ADMISSIONS {
            let recent = std::mem::take(&mut self.recent);
            self.settled_mut().extend(recent);
        }
    }
    /// Add a decoded entry `check` measured as `bytes` to the settled or
    /// the recent entries, as the wire index placed it.
    fn add_decoded(
        &mut self,
        settled: bool,
        key: AdmissionKey,
        operation_id: [u8; 32],
        bytes: usize,
    ) {
        if settled {
            self.settled_mut().insert(key, operation_id);
        } else {
            self.recent.insert(key, operation_id);
        }
        self.entry_bytes = self.entry_bytes.saturating_add(bytes);
    }
    /// A compaction of `record` of `app` folded every write of each listed
    /// author up to its sequence: their exact entries become that author's
    /// mark. Marks only ever rise.
    ///
    /// # Errors
    ///
    /// [`AdmissionError::ResourceLimit`] if the marks would not fit the
    /// index; it is unchanged then.
    pub(crate) fn cover(
        &mut self,
        (room, app, record): (&str, &str, &str),
        covers: impl IntoIterator<Item = ([u8; 32], u64)>,
    ) -> Result<(), AdmissionError> {
        let mut next = self.clone();
        for (author, sequence) in covers {
            let writer = WriterKey {
                room: room.into(),
                app: app.into(),
                record: record.into(),
                author,
            };
            let prior = next.marks.get(&writer).copied();
            if prior.is_some_and(|mark| mark >= sequence) {
                continue;
            }
            let key = |sequence| AdmissionKey {
                room: room.into(),
                app: app.into(),
                record: record.into(),
                author,
                sequence,
            };
            let range = key(0)..=key(sequence);
            let folded: Vec<(AdmissionKey, [u8; 32])> = next
                .settled
                .range(range.clone())
                .chain(next.recent.range(range))
                .map(|(key, id)| (key.clone(), *id))
                .collect();
            for (key, operation_id) in folded {
                let bytes = resources::entry_bytes(&DocumentAdmission {
                    key: key.clone(),
                    operation_id,
                })?;
                if next.recent.remove(&key).is_none() {
                    next.settled_mut().remove(&key);
                }
                next.entry_bytes = next.entry_bytes.saturating_sub(bytes);
            }
            if let Some(mark) = prior {
                next.mark_bytes = next
                    .mark_bytes
                    .saturating_sub(resources::mark_bytes(&writer, mark)?);
            }
            next.mark_bytes = next
                .mark_bytes
                .saturating_add(resources::mark_bytes(&writer, sequence)?);
            std::sync::Arc::make_mut(&mut next.marks).insert(writer, sequence);
        }
        if next.marks.len() > MAX_EXACT_ADMISSIONS
            || next.total_bytes(0, 0) > MAX_EXACT_ADMISSION_BYTES
        {
            return Err(AdmissionError::ResourceLimit);
        }
        *self = next;
        Ok(())
    }
    /// The settled entries to change: no longer shared, and unencoded.
    fn settled_mut(&mut self) -> &mut BTreeMap<AdmissionKey, [u8; 32]> {
        let settled = std::sync::Arc::make_mut(&mut self.settled);
        settled.encoded = std::sync::OnceLock::new();
        &mut settled.entries
    }
    /// Whether `self` and `before` share their settled entries and marks:
    /// then everything either holds beyond them is in its recent entries.
    pub(crate) fn shares_settled(&self, before: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.settled, &before.settled)
            && std::sync::Arc::ptr_eq(&self.marks, &before.marks)
    }
    /// The entries since the index last settled, in key order.
    pub(crate) fn recent(&self) -> impl Iterator<Item = (&AdmissionKey, &[u8; 32])> {
        self.recent.iter()
    }
    /// Whether the index holds more than half of what it may: its sealed
    /// records' histories are then worth compacting, which folds their
    /// entries into marks.
    pub(crate) fn pressed(&self) -> bool {
        self.len() > MAX_EXACT_ADMISSIONS / 2
            || self.total_bytes(0, 0) > MAX_EXACT_ADMISSION_BYTES / 2
    }
    /// The encoded size with `more` entries of `bytes` in all added.
    fn total_bytes(&self, more: usize, bytes: usize) -> usize {
        self.entry_bytes
            .saturating_add(bytes)
            // One count for each of the two entry maps, at most the whole.
            .saturating_add(count_bytes(self.len().saturating_add(more)).saturating_mul(2))
            .saturating_add(self.mark_bytes)
            .saturating_add(count_bytes(self.marks.len()))
    }
    /// The canonical index: its settled entries, its recent entries (each
    /// map in key order, no key in both), then its marks.
    pub(super) fn serialize_index<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple as _;
        let mut tuple = serializer.serialize_tuple(3)?;
        tuple.serialize_element(&self.settled.entries)?;
        tuple.serialize_element(&self.recent)?;
        tuple.serialize_element(&*self.marks)?;
        tuple.end()
    }
    /// Install decoded `marks`: none of the index's entries may be one they
    /// cover, and they must fit beside the entries.
    pub(super) fn with_marks(
        mut self,
        marks: BTreeMap<WriterKey, u64>,
    ) -> Result<Self, AdmissionError> {
        if marks.len() > MAX_EXACT_ADMISSIONS {
            return Err(AdmissionError::ResourceLimit);
        }
        let mut bytes = 0usize;
        for (writer, mark) in &marks {
            bytes = bytes.saturating_add(resources::mark_bytes(writer, *mark)?);
        }
        self.marks = std::sync::Arc::new(marks);
        self.mark_bytes = bytes;
        if self.iter().any(|(key, _)| self.covers(key)) {
            return Err("exact admission entry at or below its writer's mark".into());
        }
        if self.total_bytes(0, 0) > MAX_EXACT_ADMISSION_BYTES {
            return Err(AdmissionError::ResourceLimit);
        }
        Ok(self)
    }
    pub(crate) fn check(&self, op: &DocumentAdmission) -> Result<usize, AdmissionError> {
        if let Some(prior) = self.get(&op.key) {
            return if prior == &op.operation_id {
                Ok(0)
            } else {
                Err(AdmissionError::ConflictingSequence)
            };
        }
        // Folded into a snapshot this peer holds: already decided.
        if self.covers(&op.key) {
            return Ok(0);
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
        if self.total_bytes(1, bytes) > MAX_EXACT_ADMISSION_BYTES {
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
        let (settled, rest) = self.encode_parts()?;
        let mut bytes = Vec::with_capacity(settled.len().saturating_add(rest.len()));
        bytes.extend_from_slice(&settled);
        bytes.extend_from_slice(&rest);
        Ok(bytes)
    }
    /// [`Self::encode`] in two parts: the settled entries, shared and
    /// encoded once per settling, then the rest.
    pub(crate) fn encode_parts(&self) -> Result<(std::sync::Arc<[u8]>, Vec<u8>), AdmissionError> {
        let rejected = |e: postcard::Error| AdmissionError::Rejected(e.to_string());
        let settled = if let Some(settled) = self.settled.encoded.get() {
            settled.clone()
        } else {
            let encoded: std::sync::Arc<[u8]> = postcard::to_allocvec(&self.settled.entries)
                .map_err(rejected)?
                .into();
            self.settled.encoded.get_or_init(|| encoded).clone()
        };
        let rest = postcard::to_allocvec(&(&self.recent, &*self.marks)).map_err(rejected)?;
        if settled.len().saturating_add(rest.len()) > MAX_EXACT_ADMISSION_BYTES {
            return Err(AdmissionError::ResourceLimit);
        }
        Ok((settled, rest))
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
            ExactAdmissions::decode(&[0, 0, 1]).is_err(),
            "trailing data refused"
        );
        let mut duplicate = vec![2];
        let row = postcard::to_allocvec(&(&second.key, second.operation_id)).unwrap();
        duplicate.extend(&row);
        duplicate.extend(&row);
        duplicate.push(0);
        assert!(ExactAdmissions::decode(&duplicate).is_err());
        assert!(ExactAdmissions::decode(&vec![0; MAX_EXACT_ADMISSION_BYTES + 1]).is_err());
    }

    /// A compaction's cover turns a writer's folded entries into one mark:
    /// below it every write is decided, above it evidence stays exact, and
    /// the encoded index refuses an entry its mark covers.
    #[test]
    fn a_cover_folds_entries_into_a_mark_that_only_rises() {
        let admission = |author: u8, sequence: u64, id: u8| DocumentAdmission {
            key: AdmissionKey {
                room: "room".into(),
                app: "app".into(),
                record: "record".into(),
                author: [author; 32],
                sequence,
            },
            operation_id: [id; 32],
        };
        let mut index = ExactAdmissions::default();
        for sequence in 1..=600 {
            index.insert(&admission(1, sequence, 1)).unwrap();
            index.insert(&admission(2, sequence, 1)).unwrap();
        }
        let full = index.encode().unwrap().len();
        index
            .cover(("room", "app", "record"), [([1; 32], 500)])
            .unwrap();
        assert_eq!(index.len(), 700);
        assert!(index.encode().unwrap().len() < full);
        assert!(index.covers(&admission(1, 500, 9).key));
        assert!(!index.covers(&admission(1, 501, 1).key));
        assert!(!index.covers(&admission(2, 1, 1).key));
        // Folded: any bytes at a covered sequence are already decided.
        assert_eq!(index.check(&admission(1, 7, 9)), Ok(0));
        // Above the mark: a different write at a held sequence conflicts.
        assert_eq!(
            index.check(&admission(1, 501, 9)),
            Err(AdmissionError::ConflictingSequence)
        );
        // A lower mark changes nothing.
        let before = index.encode().unwrap();
        index
            .cover(("room", "app", "record"), [([1; 32], 10)])
            .unwrap();
        assert_eq!(index.encode().unwrap(), before);
        let decoded = ExactAdmissions::decode(&before).unwrap();
        assert_eq!(decoded.encode().unwrap(), before);
        assert_eq!(decoded.marks().count(), 1);

        // An index holding an entry its own mark covers is not canonical.
        let mut entries: BTreeMap<_, _> = index.iter().map(|(k, v)| (k.clone(), *v)).collect();
        entries.insert(admission(1, 3, 1).key, [1; 32]);
        let marks: BTreeMap<_, _> = index.marks().map(|(k, v)| (k.clone(), v)).collect();
        let forged =
            postcard::to_allocvec(&(entries, BTreeMap::<AdmissionKey, [u8; 32]>::new(), marks))
                .unwrap();
        assert!(ExactAdmissions::decode(&forged).is_err());
    }

    /// Copies share the settled entries; the index still reads as one map
    /// in key order, encodes its settled and recent entries apart, and
    /// decodes back to the same bytes.
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
        assert!(copy.shares_settled(&index));
        assert!(copy.recent.len() < RECENT_ADMISSIONS);
        let keys: Vec<_> = index.iter().map(|(key, _)| key.clone()).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert_eq!(keys.len(), RECENT_ADMISSIONS + 40);
        let bytes = index.encode().unwrap();
        let marks = BTreeMap::<WriterKey, u64>::new();
        assert_eq!(
            bytes,
            postcard::to_allocvec(&(&index.settled.entries, &index.recent, marks)).unwrap()
        );
        let decoded = ExactAdmissions::decode(&bytes).unwrap();
        assert_eq!(decoded.encode().unwrap(), bytes);
        assert_eq!(decoded.recent.len(), index.recent.len());

        // The settled part is encoded once and shared by every copy until
        // the entries settle again.
        let (settled, _) = index.encode_parts().unwrap();
        let (again, _) = copy.encode_parts().unwrap();
        assert!(std::sync::Arc::ptr_eq(&settled, &again));
        let mut grown = copy;
        for sequence in 1000..1000 + RECENT_ADMISSIONS as u64 {
            grown.insert(&admission(sequence)).unwrap();
        }
        assert!(!grown.shares_settled(&index));
        let (resettled, _) = grown.encode_parts().unwrap();
        assert!(!std::sync::Arc::ptr_eq(&settled, &resettled));
        assert_eq!(
            ExactAdmissions::decode(&grown.encode().unwrap())
                .unwrap()
                .len(),
            grown.len()
        );
    }
}
