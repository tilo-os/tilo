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

/// Decodes one entry map of the wire index into `index`: its settled
/// entries, or its recent ones. Every entry is checked against the whole
/// index so far, so no key is in both maps and the bounds hold for both.
struct EntriesInto<'a> {
    index: &'a mut ExactAdmissions,
    settled: bool,
}
impl<'de> serde::de::DeserializeSeed<'de> for EntriesInto<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_map(self)
    }
}
impl<'de> serde::de::Visitor<'de> for EntriesInto<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded exact admissions")
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        use serde::de::Error;
        if map
            .size_hint()
            .is_some_and(|n| n.saturating_add(self.index.len()) > MAX_EXACT_ADMISSIONS)
        {
            return Err(A::Error::custom("exact admission count exceeds limit"));
        }
        while let Some((key, operation_id)) = map.next_entry::<AdmissionKey, [u8; 32]>()? {
            if self.index.get(&key).is_some() {
                return Err(A::Error::custom("duplicate exact admission key"));
            }
            let admission = DocumentAdmission { key, operation_id };
            let bytes = self.index.check(&admission).map_err(A::Error::custom)?;
            self.index
                .add_decoded(self.settled, admission.key, admission.operation_id, bytes);
        }
        Ok(())
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
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = WireIndex;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an exact admission index")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<WireIndex, A::Error> {
                use serde::de::Error;
                let missing = || A::Error::custom("exact admission index is incomplete");
                let mut index = ExactAdmissions::default();
                for settled in [true, false] {
                    seq.next_element_seed(EntriesInto {
                        index: &mut index,
                        settled,
                    })?
                    .ok_or_else(missing)?;
                }
                let marks: std::collections::BTreeMap<WriterKey, u64> =
                    seq.next_element()?.ok_or_else(missing)?;
                index
                    .with_marks(marks)
                    .map(WireIndex)
                    .map_err(A::Error::custom)
            }
        }
        deserializer.deserialize_tuple(3, Visitor)
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
