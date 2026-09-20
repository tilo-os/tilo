//! Compatibility for numeric local-view map keys in native flattened state.
use serde::{Deserialize, Deserializer, de};
use std::collections::HashMap;
use std::hash::Hash;

// serde_json normally decodes numeric object keys through its map-key
// deserializer. serde(flatten) buffers them as strings first. Accept both
// representations at this boundary without changing ids or the serialized
// state shape. Binary formats continue to decode their original u64 keys.
#[derive(PartialEq, Eq, Hash)]
struct NumericKey(u64);

impl<'de> Deserialize<'de> for NumericKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if !deserializer.is_human_readable() {
            return u64::deserialize(deserializer).map(Self);
        }
        struct Visitor;
        impl de::Visitor<'_> for Visitor {
            type Value = NumericKey;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an unsigned desktop id or its decimal map key")
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(NumericKey(value))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                value.parse::<u64>().map(NumericKey).map_err(E::custom)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

pub(crate) fn deserialize_id_map<'de, D, K, V>(deserializer: D) -> Result<HashMap<K, V>, D::Error>
where
    D: Deserializer<'de>,
    K: From<u64> + Eq + Hash,
    V: Deserialize<'de>,
{
    HashMap::<NumericKey, V>::deserialize(deserializer).map(|entries| {
        entries
            .into_iter()
            .map(|(key, value)| (K::from(key.0), value))
            .collect()
    })
}
