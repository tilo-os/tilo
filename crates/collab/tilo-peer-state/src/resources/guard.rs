//! Budgeted decoding of closed wire schemas: every allocation a visitor makes
//! is charged against the caller's limits before it happens.
//!
//! Only audited peer schemas may reach this adapter. Arbitrary custom
//! `Deserialize` code could allocate outside its visitor and must never obtain
//! an allocation qualifier here.
use super::{OuterLimits, PeerResourceError};
use serde::de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use std::fmt;

/// Decode one postcard value as `T`, refusing trailing bytes.
pub(crate) fn postcard<'de, T: de::Deserialize<'de>>(
    bytes: &'de [u8],
    limits: OuterLimits,
) -> Result<T, PeerResourceError> {
    let mut budget = Budget::new(limits, Format::Postcard);
    let mut decoder = postcard::Deserializer::from_bytes(bytes);
    let value = T::deserialize(Guard {
        inner: &mut decoder,
        budget: &mut budget,
    });
    match value {
        Ok(value) => match decoder.finalize() {
            Ok([]) => Ok(value),
            _ => Err(PeerResourceError::Malformed),
        },
        Err(_) => Err(budget.refused()),
    }
}

/// Decode one JSON document as `T`, refusing trailing input.
pub(crate) fn json<T: de::DeserializeOwned>(
    bytes: &[u8],
    limits: OuterLimits,
) -> Result<T, PeerResourceError> {
    let mut budget = Budget::new(limits, Format::Json);
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = T::deserialize(Guard {
        inner: &mut decoder,
        budget: &mut budget,
    });
    match value {
        Ok(value) => decoder
            .end()
            .map(|()| value)
            .map_err(|_| PeerResourceError::Malformed),
        Err(_) => Err(budget.refused()),
    }
}

/// One refused step. Whether it was capacity or malformed input is recorded on
/// the [`Budget`], so no schema error text is ever formatted.
#[derive(Debug)]
struct Refused;
impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("guarded decode")
    }
}
impl std::error::Error for Refused {}
impl de::Error for Refused {
    fn custom<T: fmt::Display>(_: T) -> Self {
        Self
    }
}
fn boundary<E: de::Error>(_: Refused) -> E {
    E::custom("guarded decode")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    /// Collections announce their length up front and may hold owned
    /// `BTreeMap`s.
    Postcard,
    /// Collections end at a delimiter; closed schemas hold only structs and
    /// externally tagged enums, never owned maps.
    Json,
}

const MAX_DEPTH: usize = 64;

struct Budget {
    remaining: usize,
    entries: usize,
    depth: usize,
    format: Format,
    capacity: bool,
}
impl Budget {
    fn new(limits: OuterLimits, format: Format) -> Self {
        Self {
            remaining: limits.allocation_bytes,
            entries: limits.collection_entries,
            depth: 0,
            format,
            capacity: false,
        }
    }
    fn refused(&self) -> PeerResourceError {
        if self.capacity {
            PeerResourceError::Capacity
        } else {
            PeerResourceError::Malformed
        }
    }
    fn refuse(&mut self) -> Refused {
        self.capacity = true;
        Refused
    }
    fn charge(&mut self, bytes: Option<usize>) -> Result<(), Refused> {
        self.remaining = bytes
            .and_then(|n| self.remaining.checked_sub(n))
            .ok_or_else(|| self.refuse())?;
        Ok(())
    }
    fn enter<T>(&mut self) -> Result<(), Refused> {
        if self.depth >= MAX_DEPTH {
            return Err(self.refuse());
        }
        // Include the concrete visited value's layout. This also covers Box<T>
        // wrappers whose serde implementation boxes the visited T afterward.
        self.charge(Some(size_of::<T>()))?;
        // Below `MAX_DEPTH` here, so the step is exact.
        self.depth = self.depth.saturating_add(1);
        Ok(())
    }
    /// Pairs with a successful `enter`, so the depth is at least one.
    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
    fn announced(&mut self, hint: Option<usize>) -> Result<(), Refused> {
        // Postcard intentionally hides a sequence's count when fewer input
        // bytes remain than announced elements. Known peer schema collections
        // have nonzero wire elements; an unavailable count is never admitted
        // as permission to begin allocation.
        if self.format == Format::Postcard && hint.is_none_or(|n| n > self.entries) {
            return Err(self.refuse());
        }
        Ok(())
    }
    fn map_slot<T>(&mut self) -> Result<(), Refused> {
        if self.format == Format::Json {
            return Ok(());
        }
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
    // Increment only when the parser actually invokes the seed. EOF probes
    // at an exact collection limit do not spend entries or allocation bytes.
    count: Option<&'a mut usize>,
    vector: bool,
}
impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for Seed<'_, S> {
    type Value = S::Value;
    fn deserialize<D: de::Deserializer<'de>>(self, decoder: D) -> Result<Self::Value, D::Error> {
        if let Some(count) = self.count {
            if *count == self.budget.entries {
                return Err(boundary(self.budget.refuse()));
            }
            // Below `entries` here, so the step is exact.
            *count = count.saturating_add(1);
        }
        // Vec's minimum allocation is at most eight element slots; four slots
        // per element cover geometric growth with old and new buffers live.
        let slots = if self.vector { 4 } else { 1 };
        self.budget
            .charge(size_of::<S::Value>().max(1).checked_mul(slots))
            .map_err(boundary)?;
        self.inner
            .deserialize(Guard {
                inner: decoder,
                budget: self.budget,
            })
            .map_err(boundary)
    }
}
macro_rules! method {
    ($name:ident $(, $arg:ident: $ty:ty)*) => {
        fn $name<V: Visitor<'de>>(self, $($arg:$ty,)* visitor: V) -> Result<V::Value, Refused> {
            self.inner.$name($($arg,)* Visit { inner: visitor, budget: self.budget }).map_err(|_| Refused)
        }
    };
}
impl<'de, D: de::Deserializer<'de>> de::Deserializer<'de> for Guard<'_, D> {
    type Error = Refused;
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
            self.inner.$name::<Refused>(value).map_err(boundary)
        }
    };
}
macro_rules! lent {
    ($name:ident, $ty:ty) => {
        fn $name<E: de::Error>(self, value: $ty) -> Result<Self::Value, E> {
            self.budget.charge(Some(value.len())).map_err(boundary)?;
            self.inner.$name::<Refused>(value).map_err(boundary)
        }
    };
}
macro_rules! nested {
    ($name:ident, $access:ident, $wrap:expr) => {
        fn $name<A: $access<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
            self.budget.enter::<V::Value>().map_err(boundary)?;
            let result = self.inner.$name($wrap(a, &mut *self.budget));
            self.budget.leave();
            result.map_err(boundary)
        }
    };
}
impl<'de, V: Visitor<'de>> Visitor<'de> for Visit<'_, V> {
    type Value = V::Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("guarded value")
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
    lent!(visit_str, &str);
    lent!(visit_borrowed_str, &'de str);
    lent!(visit_bytes, &[u8]);
    lent!(visit_borrowed_bytes, &'de [u8]);
    // Both slice readers lend bytes and strings. An owned callback would mean
    // an allocation happened outside the adapter and is never supported.
    fn visit_string<E: de::Error>(self, _: String) -> Result<Self::Value, E> {
        Err(boundary(Refused))
    }
    fn visit_byte_buf<E: de::Error>(self, _: Vec<u8>) -> Result<Self::Value, E> {
        Err(boundary(Refused))
    }
    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        self.inner.visit_none::<Refused>().map_err(boundary)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        self.inner.visit_unit::<Refused>().map_err(boundary)
    }
    nested!(visit_some, Deserializer, guard);
    nested!(visit_newtype_struct, Deserializer, guard);
    fn visit_seq<A: SeqAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
        self.budget.announced(a.size_hint()).map_err(boundary)?;
        self.budget.enter::<V::Value>().map_err(boundary)?;
        // The eight-byte Vec minimum for byte elements; per-element seeds
        // charge the rest.
        self.budget.charge(Some(8)).map_err(boundary)?;
        let result = self.inner.visit_seq(Sequence {
            inner: a,
            budget: &mut *self.budget,
            count: 0,
        });
        self.budget.leave();
        result.map_err(boundary)
    }
    fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
        self.budget.announced(a.size_hint()).map_err(boundary)?;
        self.budget.enter::<V::Value>().map_err(boundary)?;
        let result = self.inner.visit_map(Map {
            inner: a,
            budget: &mut *self.budget,
            count: 0,
        });
        self.budget.leave();
        result.map_err(boundary)
    }
    nested!(visit_enum, EnumAccess, enumeration);
}
use de::Deserializer;
fn guard<D>(inner: D, budget: &mut Budget) -> Guard<'_, D> {
    Guard { inner, budget }
}
fn enumeration<A>(inner: A, budget: &mut Budget) -> Enumeration<'_, A> {
    Enumeration { inner, budget }
}
struct Sequence<'a, A> {
    inner: A,
    budget: &'a mut Budget,
    count: usize,
}
impl<'de, A: SeqAccess<'de>> SeqAccess<'de> for Sequence<'_, A> {
    type Error = Refused;
    fn size_hint(&self) -> Option<usize> {
        None
    }
    fn next_element_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, Refused> {
        self.inner
            .next_element_seed(Seed {
                inner: seed,
                budget: &mut *self.budget,
                count: Some(&mut self.count),
                vector: true,
            })
            .map_err(|_| Refused)
    }
}
struct Map<'a, A> {
    inner: A,
    budget: &'a mut Budget,
    count: usize,
}
impl<'de, A: MapAccess<'de>> MapAccess<'de> for Map<'_, A> {
    type Error = Refused;
    fn size_hint(&self) -> Option<usize> {
        None
    }
    fn next_key_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, Refused> {
        // Postcard announces the remaining entries; charge the slot only for
        // one that follows.
        if self.inner.size_hint() != Some(0) {
            self.budget.map_slot::<S::Value>()?;
        }
        self.inner
            .next_key_seed(Seed {
                inner: seed,
                budget: &mut *self.budget,
                count: Some(&mut self.count),
                vector: false,
            })
            .map_err(|_| Refused)
    }
    fn next_value_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<S::Value, Refused> {
        self.budget.map_slot::<S::Value>()?;
        self.inner
            .next_value_seed(Seed {
                inner: seed,
                budget: &mut *self.budget,
                count: None,
                vector: false,
            })
            .map_err(|_| Refused)
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
    type Error = Refused;
    type Variant = Variant<'a, A::Variant>;
    fn variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Self::Variant), Refused> {
        let (value, inner) = self
            .inner
            .variant_seed(Seed {
                inner: seed,
                budget: &mut *self.budget,
                count: None,
                vector: false,
            })
            .map_err(|_| Refused)?;
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
    type Error = Refused;
    fn unit_variant(self) -> Result<(), Refused> {
        self.inner.unit_variant().map_err(|_| Refused)
    }
    fn newtype_variant_seed<S: DeserializeSeed<'de>>(self, seed: S) -> Result<S::Value, Refused> {
        self.inner
            .newtype_variant_seed(Seed {
                inner: seed,
                budget: self.budget,
                count: None,
                vector: false,
            })
            .map_err(|_| Refused)
    }
    fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, Refused> {
        self.inner
            .tuple_variant(
                len,
                Visit {
                    inner: visitor,
                    budget: self.budget,
                },
            )
            .map_err(|_| Refused)
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Refused> {
        self.inner
            .struct_variant(
                fields,
                Visit {
                    inner: visitor,
                    budget: self.budget,
                },
            )
            .map_err(|_| Refused)
    }
}
