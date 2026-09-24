use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

use crate::exid::ExId;
use crate::types::Prop;
use crate::{PatchAction, ScalarValue, TextEncoding};

use super::{HydrateError, Value};

#[derive(Clone)]
pub struct Map(HashMap<String, MapValue>, Option<usize>);

impl Default for Map {
    fn default() -> Self { Self(HashMap::new(), Some(0)) }
}
impl std::fmt::Debug for Map {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_tuple("Map").field(&self.0).finish() }
}
impl PartialEq for Map {
    fn eq(&self, other: &Self) -> bool { self.0 == other.0 }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapValue {
    pub value: Value,
    pub conflict: bool,
}

impl Map {
    pub fn iter(&self) -> impl Iterator<Item = (&String, &MapValue)> {
        self.0.iter()
    }

    pub(crate) fn apply(
        &mut self,
        text_encoding: TextEncoding,
        patch: PatchAction,
    ) -> Result<(), HydrateError> {
        match patch {
            PatchAction::DeleteMap { key } => {
                self.0.remove(&key);
                Ok(())
            }
            PatchAction::PutMap {
                key,
                value,
                conflict,
            } => {
                let h_value = Value::new(value.0, text_encoding);
                self.0
                    .insert(key, MapValue::new(h_value, value.1, conflict));
                if let Some(high_water) = &mut self.1 { *high_water = (*high_water).max(self.0.capacity()); }
                Ok(())
            }
            PatchAction::Increment {
                prop: Prop::Map(key),
                value,
            } => {
                self.0
                    .get_mut(&key)
                    .ok_or(HydrateError::InvalidKey(key))?
                    .increment(value)?;
                Ok(())
            }
            _ => Err(HydrateError::InvalidMapOp),
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key).map(|mv| &mv.value)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.0.get_mut(key).map(|mv| &mut mv.value)
    }

    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl MapValue {
    pub(crate) fn new(value: Value, _id: ExId, conflict: bool) -> Self {
        Self { value, conflict }
    }

    pub(crate) fn increment(&mut self, n: i64) -> Result<(), HydrateError> {
        if let Value::Scalar(ScalarValue::Counter(c)) = &mut self.value {
            c.increment(n);
            Ok(())
        } else {
            Err(HydrateError::BadIncrement)
        }
    }
}

impl Deref for Map {
    type Target = HashMap<String, MapValue>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Map {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Public map mutation can reserve, erase and shrink without observing
        // intermediate bucket allocations. Never qualify its capacity later.
        self.1 = None;
        &mut self.0
    }
}

impl From<HashMap<&str, Value>> for Map {
    fn from(value: HashMap<&str, Value>) -> Self {
        let map: HashMap<_, _> = value
            .into_iter()
            .map(|(k, value)| {
                (
                    k.to_string(),
                    MapValue {
                        value,
                        conflict: false,
                    },
                )
            })
            .collect();
        let capacity = map.capacity();
        Map(map, Some(capacity))
    }
}

impl From<HashMap<String, Value>> for Map {
    fn from(value: HashMap<String, Value>) -> Self {
        let map: HashMap<_, _> = value
            .into_iter()
            .map(|(k, value)| {
                (
                    k,
                    MapValue {
                        value,
                        conflict: false,
                    },
                )
            })
            .collect();
        let capacity = map.capacity();
        Map(map, Some(capacity))
    }
}

// Native allocation traversal is read-only and does not populate caches.
use crate::resource_limits::retained::{NativeVisit, Visitor};
impl NativeVisit for Map {
    fn native_visit(
        &self,
        visitor: &mut Visitor<'_>,
    ) -> Result<(), crate::resource_limits::NativeResourceError> {
        let high_water = self
            .1
            .ok_or(crate::resource_limits::NativeResourceError::Unaccounted)?;
        visitor.hash_map(&self.0, high_water)
    }
}

impl NativeVisit for MapValue {
    fn native_visit(
        &self,
        visitor: &mut Visitor<'_>,
    ) -> Result<(), crate::resource_limits::NativeResourceError> {
        let Self { value, conflict: _ } = self;
        value.native_visit(visitor)
    }
}
