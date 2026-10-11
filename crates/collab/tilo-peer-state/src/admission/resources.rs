//! Limited decoding of the existing exact-admission wire index.
use super::{
    AdmissionError, AdmissionKey, Deserialize, DocumentAdmission, ExactAdmissions,
    MAX_EXACT_ADMISSION_BYTES, MAX_EXACT_ADMISSIONS, Serialize,
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
}

// This single visitor is used by legacy restore and the limited decoder.
struct WireIndex(ExactAdmissions);
impl Serialize for WireIndex {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.entries.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for WireIndex {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = WireIndex;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("bounded exact admissions")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<WireIndex, A::Error> {
                use serde::de::Error;
                if map.size_hint().is_some_and(|n| n > MAX_EXACT_ADMISSIONS) {
                    return Err(A::Error::custom("exact admission count exceeds limit"));
                }
                let mut value = ExactAdmissions::default();
                while let Some((key, operation_id)) = map.next_entry::<AdmissionKey, [u8; 32]>()? {
                    if value.entries.contains_key(&key) {
                        return Err(A::Error::custom("duplicate exact admission key"));
                    }
                    let admission = DocumentAdmission { key, operation_id };
                    let bytes = value.check(&admission).map_err(A::Error::custom)?;
                    value.entries.insert(admission.key, admission.operation_id);
                    // `check` bounded the new total by MAX_EXACT_ADMISSION_BYTES.
                    value.entry_bytes = value.entry_bytes.saturating_add(bytes);
                }
                Ok(WireIndex(value))
            }
        }
        deserializer.deserialize_map(Visitor)
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

pub(super) fn entry_bytes(op: &DocumentAdmission) -> Result<usize, AdmissionError> {
    // The closed entry contains only borrowed strings, fixed key/hash bytes,
    // and a u64. Its Serialize code creates no temporary owned allocations.
    postcard::serialize_with_flavor(&(&op.key, op.operation_id), Count(0))
        .map_err(|_| AdmissionError::ResourceLimit)
}
struct Count(usize);
impl postcard::ser_flavors::Flavor for Count {
    type Output = usize;
    fn try_push(&mut self, _: u8) -> postcard::Result<()> {
        self.add(1)
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.add(bytes.len())
    }
    fn finalize(self) -> postcard::Result<usize> {
        Ok(self.0)
    }
}
impl Count {
    fn add(&mut self, bytes: usize) -> postcard::Result<()> {
        self.0 = self
            .0
            .checked_add(bytes)
            .filter(|n| *n <= MAX_EXACT_ADMISSION_BYTES)
            .ok_or(postcard::Error::SerializeBufferFull)?;
        Ok(())
    }
}

// Compare the exact existing canonical map bytes without an encoded Vec.
// No digest substitutes for byte equality and no signed representation changes.
fn matches_wire(value: &WireIndex, bytes: &[u8]) -> bool {
    postcard::serialize_with_flavor(value, Compare(bytes)).is_ok()
}
struct Compare<'a>(&'a [u8]);
impl postcard::ser_flavors::Flavor for Compare<'_> {
    type Output = ();
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.try_extend(&[byte])
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        let Some(tail) = self.0.strip_prefix(bytes) else {
            return Err(postcard::Error::SerializeBufferFull);
        };
        self.0 = tail;
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

#[cfg(test)]
mod tests;
