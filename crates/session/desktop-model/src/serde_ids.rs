//! Deserializing maps keyed by numeric ids.
//!
//! JSON object keys are strings, so a map keyed by an id is written as
//! `{"7": …}`. `serde_json` parses such keys back into integers only when
//! it deserializes the map directly; once the map sits inside a
//! `#[serde(flatten)]`ed struct it is buffered first and the key arrives as
//! a plain string. These helpers accept both spellings.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::hash::Hash;
use std::marker::PhantomData;

use serde::de::{self, Deserialize, Deserializer, MapAccess, Visitor};

/// A map key that is a `u64`, whether it arrives as a number or as the
/// decimal string a JSON object key carries.
struct IdKey(u64);

impl<'de> Deserialize<'de> for IdKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeyVisitor;
        impl Visitor<'_> for KeyVisitor {
            type Value = IdKey;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an unsigned integer id, or its decimal string")
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<IdKey, E> {
                Ok(IdKey(v))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<IdKey, E> {
                u64::try_from(v)
                    .map(IdKey)
                    .map_err(|_| E::invalid_value(de::Unexpected::Signed(v), &self))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<IdKey, E> {
                v.parse()
                    .map(IdKey)
                    .map_err(|_| E::invalid_value(de::Unexpected::Str(v), &self))
            }
        }
        deserializer.deserialize_any(KeyVisitor)
    }
}

/// Deserialize a `HashMap` keyed by a numeric id newtype (or a bare
/// `u64`), accepting keys written as numbers or decimal strings.
pub fn deserialize_id_map<'de, D, K, V>(deserializer: D) -> Result<HashMap<K, V>, D::Error>
where
    D: Deserializer<'de>,
    K: From<u64> + Eq + Hash,
    V: Deserialize<'de>,
{
    struct MapVisitor<K, V>(PhantomData<(K, V)>);
    impl<'de, K, V> Visitor<'de> for MapVisitor<K, V>
    where
        K: From<u64> + Eq + Hash,
        V: Deserialize<'de>,
    {
        type Value = HashMap<K, V>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a map keyed by numeric ids")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
            let mut out = HashMap::with_capacity(access.size_hint().unwrap_or(0));
            while let Some((IdKey(key), value)) = access.next_entry::<IdKey, V>()? {
                out.insert(K::from(key), value);
            }
            Ok(out)
        }
    }
    deserializer.deserialize_map(MapVisitor(PhantomData))
}

/// As [`deserialize_id_map`], into an ordered map.
///
/// # Errors
///
/// As [`deserialize_id_map`].
pub fn deserialize_id_btree_map<'de, D, K, V>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
where
    D: Deserializer<'de>,
    K: From<u64> + Eq + Hash + Ord,
    V: Deserialize<'de>,
{
    deserialize_id_map::<D, K, V>(deserializer).map(|map| map.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{StripId, WorkspaceId};

    #[derive(serde::Deserialize)]
    struct Inner {
        #[serde(deserialize_with = "deserialize_id_map")]
        map: HashMap<WorkspaceId, StripId>,
    }

    #[derive(serde::Deserialize)]
    struct Outer {
        #[serde(flatten)]
        inner: Inner,
    }

    #[test]
    fn id_keyed_maps_flatten_to_numeric_string_keys_only() {
        {
            let outer: Outer = serde_json::from_str(r#"{"map":{"7":9}}"#).unwrap();
            assert_eq!(outer.inner.map[&WorkspaceId(7)], StripId(9));
        }
        {
            assert!(serde_json::from_str::<Outer>(r#"{"map":{"seven":9}}"#).is_err());
        }
    }
}
