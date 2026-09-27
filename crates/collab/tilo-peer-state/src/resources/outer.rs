use serde::de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use std::fmt;

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) allocation: usize,
    pub(crate) collection_entries: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Capacity,
    Invalid,
}

/// Closed crate-internal set, implemented only for audited peer wire DTOs.
/// Native documents and arbitrary user Deserialize code cannot enter here.
pub(crate) trait EncodeSchema: serde::Serialize {}
pub(crate) trait Schema<'de>: serde::Deserialize<'de> + EncodeSchema {}

impl EncodeSchema for crate::NeighbourMsg {}
impl Schema<'_> for crate::NeighbourMsg {}

struct Budget {
    limits: Limits,
    remaining: usize,
    capacity: bool,
    depth: usize,
}
impl Budget {
    fn refuse<E: de::Error>(&mut self) -> E {
        self.capacity = true;
        E::custom("outer allocation capacity")
    }
    fn charge<E: de::Error>(&mut self, amount: Option<usize>) -> Result<(), E> {
        self.remaining = amount
            .and_then(|n| self.remaining.checked_sub(n))
            .ok_or_else(|| self.refuse())?;
        Ok(())
    }
    fn collection<E: de::Error>(&mut self, hint: Option<usize>) -> Result<(), E> {
        // Postcard intentionally hides a sequence's count when fewer input
        // bytes remain than announced elements. Known peer schema collections
        // have nonzero wire elements; an unavailable count is never admitted
        // as permission to begin allocation.
        if hint.is_none_or(|n| n > self.limits.collection_entries) {
            return Err(self.refuse());
        }
        Ok(())
    }
    fn enter<T, E: de::Error>(&mut self) -> Result<(), E> {
        if self.depth == 64 {
            return Err(self.refuse());
        }
        // Include the concrete visited value's layout. This also covers Box<T>
        // wrappers whose serde implementation boxes the visited T afterward.
        self.charge(Some(size_of::<T>()))?;
        // Below 64 here, so the step is exact.
        self.depth = self.depth.saturating_add(1);
        Ok(())
    }
    /// Pairs with a successful `enter`, so the depth is at least one.
    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
    fn map_slot<T, E: de::Error>(&mut self) -> Result<(), E> {
        // The admitted schemas use fresh BTreeMaps. Rust 1.97.1 alloc btree
        // node.rs has 11 K/V slots, 12 child pointers and a small fixed header.
        // At alignment <=16, 11*(K+V)+256 bounds an internal node. Fewer than
        // three nodes per inserted entry, including split/root transient nodes,
        // are live. Charging both seeds gives 48*(K+V)+1024 per wire entry,
        // above 33*(K+V)+768; duplicate keys retain this conservative charge.
        // This is not a claim about an arbitrary map's retained heap storage.
        if align_of::<T>() > 16 {
            return Err(self.refuse());
        }
        self.charge(
            size_of::<T>()
                .checked_mul(48)
                .and_then(|n| n.checked_add(512)),
        )
    }
}

// Only the known peer durable/message schemas may call this. The adapter is
// deliberately private: arbitrary custom Deserialize code could allocate
// outside its visitor and must never obtain an allocation qualifier here.
pub(crate) fn decode<'de, T: Schema<'de>>(bytes: &'de [u8], limits: Limits) -> Result<T, Error> {
    let (value, tail) = decode_guarded(bytes, limits)?;
    if tail.is_empty() {
        Ok(value)
    } else {
        Err(Error::Invalid)
    }
}
fn decode_guarded<'de, T: Schema<'de>>(
    bytes: &'de [u8],
    limits: Limits,
) -> Result<(T, &'de [u8]), Error> {
    let mut budget = Budget {
        limits,
        remaining: limits.allocation,
        capacity: false,
        depth: 0,
    };
    let mut decoder = postcard::Deserializer::from_bytes(bytes);
    let result = T::deserialize(Guard {
        inner: &mut decoder,
        budget: &mut budget,
    });
    match result {
        Ok(value) => decoder
            .finalize()
            .map(|tail| (value, tail))
            .map_err(|_| Error::Invalid),
        _ if budget.capacity => Err(Error::Capacity),
        _ => Err(Error::Invalid),
    }
}
struct Guard<'a, D> {
    inner: D,
    budget: &'a mut Budget,
}
struct Visit<'a, V> {
    inner: V,
    budget: &'a mut Budget,
}
struct Seed<'a, S> {
    inner: S,
    budget: &'a mut Budget,
}
impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for Seed<'_, S> {
    type Value = S::Value;
    fn deserialize<D: de::Deserializer<'de>>(self, decoder: D) -> Result<Self::Value, D::Error> {
        self.inner.deserialize(Guard {
            inner: decoder,
            budget: self.budget,
        })
    }
}
macro_rules! method {
    ($name:ident $(, $arg:ident: $ty:ty)*) => {
        fn $name<V: Visitor<'de>>(self, $($arg:$ty,)* visitor: V) -> Result<V::Value, D::Error> {
            self.inner.$name($($arg,)* Visit { inner: visitor, budget: self.budget })
        }
    };
}
impl<'de, D: de::Deserializer<'de>> de::Deserializer<'de> for Guard<'_, D> {
    type Error = D::Error;
    method!(deserialize_any);
    method!(deserialize_bool);
    method!(deserialize_i8);
    method!(deserialize_i16);
    method!(deserialize_i32);
    method!(deserialize_i64);
    method!(deserialize_i128);
    method!(deserialize_u8);
    method!(deserialize_u16);
    method!(deserialize_u32);
    method!(deserialize_u64);
    method!(deserialize_u128);
    method!(deserialize_f32);
    method!(deserialize_f64);
    method!(deserialize_char);
    method!(deserialize_str);
    method!(deserialize_string);
    method!(deserialize_bytes);
    method!(deserialize_byte_buf);
    method!(deserialize_option);
    method!(deserialize_unit);
    method!(deserialize_unit_struct, name: &'static str);
    method!(deserialize_newtype_struct, name: &'static str);
    method!(deserialize_seq);
    method!(deserialize_tuple, len: usize);
    method!(deserialize_tuple_struct, name: &'static str, len: usize);
    method!(deserialize_map);
    method!(deserialize_struct, name: &'static str, fields: &'static [&'static str]);
    method!(deserialize_enum, name: &'static str, variants: &'static [&'static str]);
    method!(deserialize_identifier);
    method!(deserialize_ignored_any);
    fn is_human_readable(&self) -> bool {
        self.inner.is_human_readable()
    }
}
macro_rules! scalar {
    ($name:ident, $ty:ty) => {
        fn $name<E: de::Error>(self, value: $ty) -> Result<Self::Value, E> {
            self.inner.$name(value)
        }
    };
}
impl<'de, V: Visitor<'de>> Visitor<'de> for Visit<'_, V> {
    type Value = V::Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.expecting(f)
    }
    scalar!(visit_bool, bool);
    scalar!(visit_i8, i8);
    scalar!(visit_i16, i16);
    scalar!(visit_i32, i32);
    scalar!(visit_i64, i64);
    scalar!(visit_i128, i128);
    scalar!(visit_u8, u8);
    scalar!(visit_u16, u16);
    scalar!(visit_u32, u32);
    scalar!(visit_u64, u64);
    scalar!(visit_u128, u128);
    scalar!(visit_f32, f32);
    scalar!(visit_f64, f64);
    scalar!(visit_char, char);
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        self.budget.charge(Some(value.len()))?;
        self.inner.visit_str(value)
    }
    fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Self::Value, E> {
        self.budget.charge(Some(value.len()))?;
        self.inner.visit_borrowed_str(value)
    }
    fn visit_bytes<E: de::Error>(self, value: &[u8]) -> Result<Self::Value, E> {
        self.budget.charge(Some(value.len()))?;
        self.inner.visit_bytes(value)
    }
    fn visit_borrowed_bytes<E: de::Error>(self, value: &'de [u8]) -> Result<Self::Value, E> {
        self.budget.charge(Some(value.len()))?;
        self.inner.visit_borrowed_bytes(value)
    }
    // Postcard's slice flavor lends bytes/str. An owned callback would mean an
    // allocation happened outside the adapter and is never a supported path.
    fn visit_string<E: de::Error>(self, _: String) -> Result<Self::Value, E> {
        Err(E::custom("owned decoder callback"))
    }
    fn visit_byte_buf<E: de::Error>(self, _: Vec<u8>) -> Result<Self::Value, E> {
        Err(E::custom("owned decoder callback"))
    }
    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        self.inner.visit_none()
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        self.inner.visit_unit()
    }
    fn visit_some<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        self.budget.enter::<V::Value, D::Error>()?;
        let result = self.inner.visit_some(Guard {
            inner: d,
            budget: &mut *self.budget,
        });
        self.budget.leave();
        result
    }
    fn visit_newtype_struct<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        self.budget.enter::<V::Value, D::Error>()?;
        let result = self.inner.visit_newtype_struct(Guard {
            inner: d,
            budget: &mut *self.budget,
        });
        self.budget.leave();
        result
    }
    fn visit_seq<A: SeqAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
        self.budget.collection(a.size_hint())?;
        self.budget.enter::<V::Value, A::Error>()?;
        // Rust Vec's minimum allocation is at most eight element slots. Charge
        // eight bytes here and four slots per element before its Deserialize
        // seed runs; this covers minimum non-byte capacity, geometric growth,
        // and simultaneous old/new allocations during growth.
        self.budget.charge(Some(8))?;
        let result = self.inner.visit_seq(Sequence {
            inner: a,
            budget: &mut *self.budget,
            count: 0,
        });
        self.budget.leave();
        result
    }
    fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
        self.budget.collection(a.size_hint())?;
        self.budget.enter::<V::Value, A::Error>()?;
        let result = self.inner.visit_map(Map {
            inner: a,
            budget: &mut *self.budget,
            count: 0,
        });
        self.budget.leave();
        result
    }
    fn visit_enum<A: EnumAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
        self.budget.enter::<V::Value, A::Error>()?;
        let result = self.inner.visit_enum(Enumeration {
            inner: a,
            budget: &mut *self.budget,
        });
        self.budget.leave();
        result
    }
}
struct Sequence<'a, A> {
    inner: A,
    budget: &'a mut Budget,
    count: usize,
}
impl<'de, A: SeqAccess<'de>> SeqAccess<'de> for Sequence<'_, A> {
    type Error = A::Error;
    fn size_hint(&self) -> Option<usize> {
        None
    }
    fn next_element_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, Self::Error> {
        if self.inner.size_hint() == Some(0) {
            return Ok(None);
        }
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| self.budget.refuse())?;
        self.budget.collection(Some(self.count))?;
        self.budget
            .charge(size_of::<S::Value>().max(1).checked_mul(4))?;
        self.inner.next_element_seed(Seed {
            inner: seed,
            budget: &mut *self.budget,
        })
    }
}
struct Map<'a, A> {
    inner: A,
    budget: &'a mut Budget,
    count: usize,
}
impl<'de, A: MapAccess<'de>> MapAccess<'de> for Map<'_, A> {
    type Error = A::Error;
    fn size_hint(&self) -> Option<usize> {
        None
    }
    fn next_key_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, Self::Error> {
        if self.inner.size_hint() == Some(0) {
            return Ok(None);
        }
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| self.budget.refuse())?;
        self.budget.collection(Some(self.count))?;
        self.budget.map_slot::<S::Value, Self::Error>()?;
        self.inner.next_key_seed(Seed {
            inner: seed,
            budget: &mut *self.budget,
        })
    }
    fn next_value_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<S::Value, Self::Error> {
        self.budget.map_slot::<S::Value, Self::Error>()?;
        self.inner.next_value_seed(Seed {
            inner: seed,
            budget: &mut *self.budget,
        })
    }
}
struct Enumeration<'a, A> {
    inner: A,
    budget: &'a mut Budget,
}
struct Variant<'a, A> {
    inner: A,
    budget: &'a mut Budget,
}
impl<'a, 'de, A: EnumAccess<'de>> EnumAccess<'de> for Enumeration<'a, A> {
    type Error = A::Error;
    type Variant = Variant<'a, A::Variant>;
    fn variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Self::Variant), Self::Error> {
        let (value, inner) = self.inner.variant_seed(Seed {
            inner: seed,
            budget: &mut *self.budget,
        })?;
        Ok((
            value,
            Variant {
                inner,
                budget: self.budget,
            },
        ))
    }
}
impl<'de, A: VariantAccess<'de>> VariantAccess<'de> for Variant<'_, A> {
    type Error = A::Error;
    fn unit_variant(self) -> Result<(), Self::Error> {
        self.inner.unit_variant()
    }
    fn newtype_variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<S::Value, Self::Error> {
        self.budget.charge(Some(size_of::<S::Value>()))?;
        self.inner.newtype_variant_seed(Seed {
            inner: seed,
            budget: self.budget,
        })
    }
    fn tuple_variant<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.tuple_variant(
            len,
            Visit {
                inner: visitor,
                budget: self.budget,
            },
        )
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.struct_variant(
            fields,
            Visit {
                inner: visitor,
                budget: self.budget,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeighbourMsg;

    fn limits(allocation: usize, collection_entries: usize) -> Limits {
        Limits {
            allocation,
            collection_entries,
        }
    }

    #[test]
    fn outer_resources_refuse_announced_collection_before_element_decode() {
        // Actual SyncResp discriminant followed by an oversized changes count,
        // with no element bytes. Capacity must win over malformed element EOF.
        let mut bytes = postcard::to_allocvec(&NeighbourMsg::SyncResp {
            changes: vec![],
            room: String::new(),
        })
        .unwrap();
        bytes.truncate(1);
        bytes.extend(postcard::to_allocvec(&4096usize).unwrap());
        assert_eq!(
            decode::<NeighbourMsg>(&bytes, limits(1024 * 1024, 16)),
            Err(Error::Capacity)
        );
    }

    #[test]
    fn outer_resources_bound_owned_buffers_across_all_chunks() {
        let message = NeighbourMsg::SyncResp {
            changes: vec![vec![1; 2048], vec![2; 2048]],
            room: "room".into(),
        };
        let bytes = postcard::to_allocvec(&message).unwrap();
        let original = bytes.clone();
        let one_chunk = NeighbourMsg::SyncResp {
            changes: vec![vec![1; 2048]],
            room: "room".into(),
        };
        assert_eq!(
            decode::<NeighbourMsg>(
                &postcard::to_allocvec(&one_chunk).unwrap(),
                limits(14 * 1024, 4096)
            )
            .unwrap(),
            one_chunk
        );
        assert_eq!(
            decode::<NeighbourMsg>(&bytes, limits(14 * 1024, 4096)),
            Err(Error::Capacity)
        );
        assert_eq!(bytes, original);
        assert_eq!(
            decode::<NeighbourMsg>(&bytes, limits(1024 * 1024, 4096)).unwrap(),
            message
        );
    }

    #[test]
    fn outer_resources_refuse_string_copy_and_preserve_valid_wire() {
        let message = NeighbourMsg::SyncReq {
            since: vec![],
            room: "r".repeat(4096),
            invite_id: None,
        };
        let bytes = postcard::to_allocvec(&message).unwrap();
        assert_eq!(
            decode::<NeighbourMsg>(&bytes, limits(1024, 4096)),
            Err(Error::Capacity)
        );
        assert_eq!(
            decode::<NeighbourMsg>(&bytes, limits(1024 * 1024, 4096)).unwrap(),
            message
        );
    }
}
