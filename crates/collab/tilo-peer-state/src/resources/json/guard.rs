//! Known-schema DTO guard: decodes a closed serde type while counting every
//! allocation its visitors make against the caller's limits.
use serde::de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use std::{alloc::Layout, fmt};

/// Why a guarded decode refused its input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardError {
    /// The input needs more allocation, entries or depth than its limits allow.
    Capacity,
    /// The input does not decode as the requested type.
    Malformed,
}
impl fmt::Display for GuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("guarded DTO")
    }
}
impl std::error::Error for GuardError {}
impl de::Error for GuardError {
    // Never format arbitrary schema errors/expected field lists.
    fn custom<T: fmt::Display>(_: T) -> Self {
        Self::Malformed
    }
}
fn boundary<E: de::Error>(_: GuardError) -> E {
    E::custom("guarded DTO")
}
/// The budget one guarded decode may spend.
#[derive(Clone, Copy, Debug)]
pub struct GuardLimits {
    /// Bytes the decoded value and its transient visitor state may allocate.
    pub allocation_bytes: usize,
    /// Entries any one sequence or map may hold.
    pub collection_entries: usize,
    /// Deepest nesting admitted.
    pub max_depth: usize,
    /// Extra bytes each map or struct value pays before it is decoded, from
    /// the key and value layouts (for example owned map node storage);
    /// `None` refuses the input as over capacity.
    pub map_value: fn(Layout, Layout) -> Option<usize>,
}
impl GuardLimits {
    /// Limits for closed schemas of structs and externally tagged enums: 64
    /// levels deep, with no per-entry map charge.
    #[must_use]
    pub const fn closed(allocation_bytes: usize, collection_entries: usize) -> Self {
        Self {
            allocation_bytes,
            collection_entries,
            max_depth: 64,
            map_value: |_, _| Some(0),
        }
    }
}
struct Budget {
    remaining: usize,
    entries: usize,
    depth: usize,
    max_depth: usize,
    map_value: fn(Layout, Layout) -> Option<usize>,
    capacity: bool,
}
impl Budget {
    fn refuse(&mut self) -> GuardError {
        self.capacity = true;
        GuardError::Capacity
    }
    fn charge(&mut self, bytes: Option<usize>) -> Result<(), GuardError> {
        self.remaining = bytes
            .and_then(|n| self.remaining.checked_sub(n))
            .ok_or_else(|| self.refuse())?;
        Ok(())
    }
    fn enter<T>(&mut self) -> Result<(), GuardError> {
        if self.depth >= self.max_depth {
            return Err(self.refuse());
        }
        self.charge(Some(size_of::<T>()))?;
        // Below `max_depth` here, so the step is exact.
        self.depth = self.depth.saturating_add(1);
        Ok(())
    }
    /// Pairs with a successful `enter`, so the depth is at least one.
    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}
impl Budget {
    fn new(limits: GuardLimits) -> Self {
        Self {
            remaining: limits.allocation_bytes,
            entries: limits.collection_entries,
            depth: 0,
            max_depth: limits.max_depth,
            map_value: limits.map_value,
            capacity: false,
        }
    }
    fn refused(&self) -> GuardError {
        if self.capacity {
            GuardError::Capacity
        } else {
            GuardError::Malformed
        }
    }
}
/// Decode one JSON document as `T`.
///
/// # Errors
///
/// [`GuardError::Capacity`] when `limits` would be exceeded, otherwise
/// [`GuardError::Malformed`] for input that is not exactly one `T`.
pub fn json<T: de::DeserializeOwned>(bytes: &[u8], limits: GuardLimits) -> Result<T, GuardError> {
    let mut budget = Budget::new(limits);
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = T::deserialize(Guard {
        inner: &mut decoder,
        budget: &mut budget,
    });
    match value {
        Ok(value) => decoder
            .end()
            .map(|()| value)
            .map_err(|_| GuardError::Malformed),
        Err(_) => Err(budget.refused()),
    }
}
/// Decode one postcard value as `T`, refusing trailing bytes.
///
/// # Errors
///
/// As [`json`].
pub fn postcard<T: de::DeserializeOwned>(
    bytes: &[u8],
    limits: GuardLimits,
) -> Result<T, GuardError> {
    let mut budget = Budget::new(limits);
    let mut decoder = postcard::Deserializer::from_bytes(bytes);
    let value = T::deserialize(Guard {
        inner: &mut decoder,
        budget: &mut budget,
    });
    match value {
        Ok(value) => match decoder.finalize() {
            Ok([]) => Ok(value),
            _ => Err(GuardError::Malformed),
        },
        Err(_) => Err(budget.refused()),
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
        fn $name<V: Visitor<'de>>(self, $($arg:$ty,)* visitor: V) -> Result<V::Value, GuardError> {
            self.inner.$name($($arg,)* Visit { inner: visitor, budget: self.budget }).map_err(|_| GuardError::Malformed)
        }
    };
}
impl<'de, D: de::Deserializer<'de>> de::Deserializer<'de> for Guard<'_, D> {
    type Error = GuardError;
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
            self.inner.$name::<GuardError>(value).map_err(boundary)
        }
    };
}
impl<'de, V: Visitor<'de>> Visitor<'de> for Visit<'_, V> {
    type Value = V::Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("guarded DTO value")
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
        self.budget.charge(Some(value.len())).map_err(boundary)?;
        self.inner.visit_str::<GuardError>(value).map_err(boundary)
    }
    fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Self::Value, E> {
        self.budget.charge(Some(value.len())).map_err(boundary)?;
        self.inner
            .visit_borrowed_str::<GuardError>(value)
            .map_err(boundary)
    }
    fn visit_bytes<E: de::Error>(self, value: &[u8]) -> Result<Self::Value, E> {
        self.budget.charge(Some(value.len())).map_err(boundary)?;
        self.inner
            .visit_bytes::<GuardError>(value)
            .map_err(boundary)
    }
    fn visit_borrowed_bytes<E: de::Error>(self, value: &'de [u8]) -> Result<Self::Value, E> {
        self.budget.charge(Some(value.len())).map_err(boundary)?;
        self.inner
            .visit_borrowed_bytes::<GuardError>(value)
            .map_err(boundary)
    }
    // SliceRead lends scratch/unescaped strings. Any owned callback (including
    // arbitrary_precision's private Number machinery) stays unsupported.
    fn visit_string<E: de::Error>(self, _: String) -> Result<Self::Value, E> {
        Err(boundary(GuardError::Malformed))
    }
    fn visit_byte_buf<E: de::Error>(self, _: Vec<u8>) -> Result<Self::Value, E> {
        Err(boundary(GuardError::Malformed))
    }
    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        self.inner.visit_none::<GuardError>().map_err(boundary)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        self.inner.visit_unit::<GuardError>().map_err(boundary)
    }
    fn visit_some<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        self.budget.enter::<V::Value>().map_err(boundary)?;
        let result = self.inner.visit_some(Guard {
            inner: d,
            budget: &mut *self.budget,
        });
        self.budget.leave();
        result.map_err(boundary)
    }
    fn visit_newtype_struct<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        self.budget.enter::<V::Value>().map_err(boundary)?;
        let result = self.inner.visit_newtype_struct(Guard {
            inner: d,
            budget: &mut *self.budget,
        });
        self.budget.leave();
        result.map_err(boundary)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
        self.budget.enter::<V::Value>().map_err(boundary)?;
        // Pinned Vec minimum: eight bytes for byte elements, four slots for
        // other small elements. Per-actual-seed four slots cover geometric
        // old+new overlap; this minimum charge covers its initial byte buffer.
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
        self.budget.enter::<V::Value>().map_err(boundary)?;
        // Closed schemas have structs/externally tagged enums, no owned maps.
        let result = self.inner.visit_map(Map {
            inner: a,
            budget: &mut *self.budget,
            count: 0,
            key_layout: None,
        });
        self.budget.leave();
        result.map_err(boundary)
    }
    fn visit_enum<A: EnumAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
        self.budget.enter::<V::Value>().map_err(boundary)?;
        let result = self.inner.visit_enum(Enumeration {
            inner: a,
            budget: &mut *self.budget,
        });
        self.budget.leave();
        result.map_err(boundary)
    }
}
struct Sequence<'a, A> {
    inner: A,
    budget: &'a mut Budget,
    count: usize,
}
impl<'de, A: SeqAccess<'de>> SeqAccess<'de> for Sequence<'_, A> {
    type Error = GuardError;
    fn size_hint(&self) -> Option<usize> {
        None
    }
    fn next_element_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, GuardError> {
        self.inner
            .next_element_seed(Seed {
                inner: seed,
                budget: &mut *self.budget,
                count: Some(&mut self.count),
                vector: true,
            })
            .map_err(|_| GuardError::Malformed)
    }
}
struct Map<'a, A> {
    inner: A,
    budget: &'a mut Budget,
    count: usize,
    key_layout: Option<Layout>,
}
impl<'de, A: MapAccess<'de>> MapAccess<'de> for Map<'_, A> {
    type Error = GuardError;
    fn size_hint(&self) -> Option<usize> {
        None
    }
    fn next_key_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, GuardError> {
        self.key_layout = Some(Layout::new::<S::Value>());
        self.inner
            .next_key_seed(Seed {
                inner: seed,
                budget: &mut *self.budget,
                count: Some(&mut self.count),
                vector: false,
            })
            .map_err(|_| GuardError::Malformed)
    }
    fn next_value_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<S::Value, GuardError> {
        let map_value = self.budget.map_value;
        let charge = self
            .key_layout
            .and_then(|key| map_value(key, Layout::new::<S::Value>()));
        self.budget.charge(charge)?;
        self.inner
            .next_value_seed(Seed {
                inner: seed,
                budget: &mut *self.budget,
                count: None,
                vector: false,
            })
            .map_err(|_| GuardError::Malformed)
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
    type Error = GuardError;
    type Variant = Variant<'a, A::Variant>;
    fn variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Self::Variant), GuardError> {
        let (value, inner) = self
            .inner
            .variant_seed(Seed {
                inner: seed,
                budget: &mut *self.budget,
                count: None,
                vector: false,
            })
            .map_err(|_| GuardError::Malformed)?;
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
    type Error = GuardError;
    fn unit_variant(self) -> Result<(), GuardError> {
        self.inner.unit_variant().map_err(|_| GuardError::Malformed)
    }
    fn newtype_variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<S::Value, GuardError> {
        self.inner
            .newtype_variant_seed(Seed {
                inner: seed,
                budget: self.budget,
                count: None,
                vector: false,
            })
            .map_err(|_| GuardError::Malformed)
    }
    fn tuple_variant<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, GuardError> {
        self.inner
            .tuple_variant(
                len,
                Visit {
                    inner: visitor,
                    budget: self.budget,
                },
            )
            .map_err(|_| GuardError::Malformed)
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, GuardError> {
        self.inner
            .struct_variant(
                fields,
                Visit {
                    inner: visitor,
                    budget: self.budget,
                },
            )
            .map_err(|_| GuardError::Malformed)
    }
}
