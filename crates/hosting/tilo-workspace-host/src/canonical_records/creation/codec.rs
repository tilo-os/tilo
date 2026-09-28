use super::*;
use serde::{
    de::{DeserializeSeed, SeqAccess, Visitor},
    ser::{SerializeSeq, SerializeTuple},
};

struct Rows<I>(I, usize);
impl<'a, I: Iterator<Item = &'a CreationRowV1> + Clone> Serialize for Rows<I> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut sequence = s.serialize_seq(Some(self.1))?;
        for row in self.0.clone() {
            sequence.serialize_element(row)?;
        }
        sequence.end()
    }
}
struct Wire<I>(Rows<I>);
impl<'a, I: Iterator<Item = &'a CreationRowV1> + Clone> Serialize for Wire<I> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut tuple = s.serialize_tuple(2)?;
        tuple.serialize_element(&1u16)?;
        tuple.serialize_element(&self.0)?;
        tuple.end()
    }
}
pub(super) fn count_rows<'a>(
    rows: impl Iterator<Item = &'a CreationRowV1> + Clone,
    len: usize,
) -> Result<usize, String> {
    postcard::serialize_with_flavor(&Wire(Rows(rows, len)), Count(0)).map_err(|e| e.to_string())
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
    fn add(&mut self, len: usize) -> postcard::Result<()> {
        self.0 = self
            .0
            .checked_add(len)
            .filter(|n| *n <= MAX_CREATION_LEDGER_BYTES)
            .ok_or(postcard::Error::SerializeBufferFull)?;
        Ok(())
    }
}
pub(super) fn encode(ledger: &CreationLedgerV1) -> Result<Vec<u8>, String> {
    ledger.validate()?;
    let length = ledger.encoded_len()?;
    let mut bytes = vec![0; length];
    postcard::to_slice(
        &Wire(Rows(ledger.rows.values(), ledger.rows.len())),
        &mut bytes,
    )
    .map_err(|e| e.to_string())?;
    Ok(bytes)
}
pub(super) fn decode(bytes: &[u8]) -> Result<CreationLedgerV1, String> {
    if bytes.len() > MAX_CREATION_LEDGER_BYTES {
        return Err("creation ledger byte limit".into());
    }
    let (ledger, rest) = postcard::take_from_bytes::<Decoded>(bytes).map_err(|e| e.to_string())?;
    if !rest.is_empty() {
        return Err("trailing creation ledger bytes".into());
    }
    let ledger = ledger.0;
    ledger.validate()?;
    // Match the original without allocating another full encoded ledger.
    postcard::serialize_with_flavor(
        &Wire(Rows(ledger.rows.values(), ledger.rows.len())),
        Exact(bytes),
    )
    .map_err(|e| e.to_string())?;
    Ok(ledger)
}
struct Exact<'a>(&'a [u8]);
impl postcard::ser_flavors::Flavor for Exact<'_> {
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
struct Decoded(CreationLedgerV1);
impl<'de> Deserialize<'de> for Decoded {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Ledger;
        impl<'de> Visitor<'de> for Ledger {
            type Value = Decoded;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("creation ledger v1")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let version: u16 = a
                    .next_element()?
                    .ok_or_else(|| serde::de::Error::custom("missing creation version"))?;
                if version != 1 {
                    return Err(serde::de::Error::custom("unsupported creation version"));
                }
                let rows = a
                    .next_element_seed(Entries)?
                    .ok_or_else(|| serde::de::Error::custom("missing creation rows"))?;
                Ok(Decoded(CreationLedgerV1 { rows }))
            }
        }
        d.deserialize_tuple(2, Ledger)
    }
}
struct Entries;
impl<'de> DeserializeSeed<'de> for Entries {
    type Value = BTreeMap<CreationKeyV1, CreationRowV1>;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for Entries {
    type Value = BTreeMap<CreationKeyV1, CreationRowV1>;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded ordered creation rows")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
        if a.size_hint().is_some_and(|n| n > MAX_CREATION_ROWS) {
            return Err(serde::de::Error::custom("creation row limit"));
        }
        let mut rows = BTreeMap::new();
        while let Some(row) = a.next_element::<CreationRowV1>()? {
            if rows.len() >= MAX_CREATION_ROWS
                || rows
                    .last_key_value()
                    .is_some_and(|(key, _)| key >= &row.intent.key)
            {
                return Err(serde::de::Error::custom(
                    "invalid creation row order or count",
                ));
            }
            rows.insert(row.intent.key.clone(), row);
        }
        Ok(rows)
    }
}
