//! Limited decoding of the existing exact-admission wire index.
use super::{
    AdmissionError, AdmissionKey, Deserialize, DocumentAdmission, ExactAdmissions,
    MAX_EXACT_ADMISSION_BYTES, MAX_EXACT_ADMISSIONS, Serialize, WriterKey,
};
use crate::resources::{OuterLimits, PeerResourceError};

pub(crate) struct BoundedExactAdmissions {
    index: WireIndex,
}
impl BoundedExactAdmissions {
    pub(crate) fn decode(bytes: &[u8], limits: OuterLimits) -> Result<Self, PeerResourceError> {
        if bytes.len() > MAX_EXACT_ADMISSION_BYTES {
            return Err(PeerResourceError::Capacity);
        }
        let index: WireIndex = crate::resources::decode(bytes, limits)?;
        if !matches_wire(&index, bytes) {
            return Err(PeerResourceError::Malformed);
        }
        Ok(Self { index })
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&AdmissionKey, &[u8; 32])> {
        self.index.0.iter()
    }
    pub(crate) fn get(&self, key: &AdmissionKey) -> Option<&[u8; 32]> {
        self.index.0.get(key)
    }
    pub(crate) fn marks(&self) -> impl Iterator<Item = (&WriterKey, u64)> {
        self.index.0.marks()
    }
}

/// An index's entries as their canonical wire map.
pub(super) struct WireEntries<'a>(pub(super) &'a ExactAdmissions);
impl Serialize for WireEntries<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize_entries(serializer)
    }
}

/// A whole index as its canonical wire tuple: entries, then marks.
pub(super) struct WireIndexRef<'a>(pub(super) &'a ExactAdmissions);
impl Serialize for WireIndexRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize_index(serializer)
    }
}

// The entries visitor is used by legacy restore and the limited decoder.
struct Entries(ExactAdmissions);
impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Entries;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("bounded exact admissions")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Entries, A::Error> {
                use serde::de::Error;
                if map.size_hint().is_some_and(|n| n > MAX_EXACT_ADMISSIONS) {
                    return Err(A::Error::custom("exact admission count exceeds limit"));
                }
                let mut value = ExactAdmissions::default();
                while let Some((key, operation_id)) = map.next_entry::<AdmissionKey, [u8; 32]>()? {
                    if value.get(&key).is_some() {
                        return Err(A::Error::custom("duplicate exact admission key"));
                    }
                    let admission = DocumentAdmission { key, operation_id };
                    let bytes = value.check(&admission).map_err(A::Error::custom)?;
                    value.add(admission.key, admission.operation_id, bytes);
                }
                Ok(Entries(value))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

struct WireIndex(ExactAdmissions);
impl Serialize for WireIndex {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize_index(serializer)
    }
}
impl<'de> Deserialize<'de> for WireIndex {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let (Entries(entries), marks) =
            <(Entries, std::collections::BTreeMap<WriterKey, u64>)>::deserialize(deserializer)?;
        entries
            .with_marks(marks)
            .map(WireIndex)
            .map_err(D::Error::custom)
    }
}
impl crate::resources::outer::EncodeSchema for WireIndex {}
impl crate::resources::outer::Schema<'_> for WireIndex {}

pub(super) fn decode_index(bytes: &[u8]) -> Result<ExactAdmissions, AdmissionError> {
    if bytes.len() > MAX_EXACT_ADMISSION_BYTES {
        return Err(AdmissionError::ResourceLimit);
    }
    let (value, tail): (WireIndex, _) = postcard::take_from_bytes(bytes)
        .map_err(|error| AdmissionError::Rejected(error.to_string()))?;
    if !tail.is_empty() || !matches_wire(&value, bytes) {
        return Err("noncanonical exact admission index".into());
    }
    Ok(value.0)
}

/// The encoded size of one writer's mark.
pub(super) fn mark_bytes(writer: &WriterKey, mark: u64) -> Result<usize, AdmissionError> {
    crate::postcard_sink::encoded_len(&(writer, mark), MAX_EXACT_ADMISSION_BYTES)
        .map_err(|_| AdmissionError::ResourceLimit)
}

pub(super) fn entry_bytes(op: &DocumentAdmission) -> Result<usize, AdmissionError> {
    // The closed entry contains only borrowed strings, fixed key/hash bytes,
    // and a u64. Its Serialize code creates no temporary owned allocations.
    crate::postcard_sink::encoded_len(&(&op.key, op.operation_id), MAX_EXACT_ADMISSION_BYTES)
        .map_err(|_| AdmissionError::ResourceLimit)
}

// Compare the exact existing canonical map bytes without an encoded Vec.
// No digest substitutes for byte equality and no signed representation changes.
fn matches_wire(value: &WireIndex, bytes: &[u8]) -> bool {
    crate::postcard_sink::encodes_as(value, bytes)
}

#[cfg(test)]
mod tests;
