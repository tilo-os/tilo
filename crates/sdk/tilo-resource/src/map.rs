//! A map from string keys whose clones share it chunk by chunk: [`Map`],
//! for a state field keyed by name (where the reader is in each book, the
//! pictures a notebook shows by digest).

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;

use crate::chunk_map::ChunkMap;
use crate::model::{ModelError, leaf_paths};
use crate::path::{Edit, EditOp, Path, Segment, Settable};
use crate::table::{MapEntries, MapIn, ModelTable};
use crate::{Model, Shape};

/// A map from string keys, sorted by key, stored and merged as a
/// `BTreeMap<String, V>` is: one document map, reconciled key by key.
///
/// Clones share the entries in chunks of a few dozen: a copy costs a pointer
/// per chunk, and a change copies only the chunk it touches while another
/// clone still holds it. A state a view holds therefore costs the next
/// change to the map one chunk, and a change elsewhere in the state none.
#[derive(Clone)]
pub struct Map<V> {
    entries: ChunkMap<V>,
}

impl<V> Default for Map<V> {
    fn default() -> Self {
        Self {
            entries: ChunkMap::default(),
        }
    }
}

impl<V: fmt::Debug> fmt::Debug for Map<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.entries.fmt(f)
    }
}

impl<V: PartialEq> PartialEq for Map<V> {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl<V: Eq> Eq for Map<V> {}

impl<V> Map<V> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The value at `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&V> {
        self.entries.get(key)
    }

    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// The entries in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
        self.entries.iter()
    }

    /// The keys in order.
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.entries.iter().map(|(key, _)| key)
    }

    /// The values in key order.
    pub fn values(&self) -> impl ExactSizeIterator<Item = &V> {
        self.entries.values()
    }

    /// The value at `key`, to change. When a clone still shares its chunk,
    /// the chunk is copied first, so the clone keeps seeing it as it was.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.entries.get_mut(key)
    }

    /// Put `value` at `key`; returns the value it replaced.
    pub fn insert(&mut self, key: impl Into<String>, value: V) -> Option<V> {
        self.entries.insert(key.into(), value)
    }

    /// Take the entry `key` out; returns its value.
    pub fn remove(&mut self, key: &str) -> Option<V> {
        self.entries.remove(key)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

impl<V> From<BTreeMap<String, V>> for Map<V> {
    fn from(entries: BTreeMap<String, V>) -> Self {
        Self {
            entries: ChunkMap::from(entries),
        }
    }
}

impl<V: Clone> From<&Map<V>> for BTreeMap<String, V> {
    fn from(map: &Map<V>) -> Self {
        map.iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

impl<K: Into<String>, V> FromIterator<(K, V)> for Map<V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let sorted: BTreeMap<String, V> = iter
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect();
        Self::from(sorted)
    }
}

impl<K: Into<String>, V> Extend<(K, V)> for Map<V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (key, value) in iter {
            self.insert(key, value);
        }
    }
}

impl<T: 'static> MapEntries for Map<T> {
    fn count(&self) -> usize {
        self.entries.count()
    }

    fn each(&self, visit: &mut dyn FnMut(&str, &dyn Any)) {
        self.entries.each(visit);
    }

    fn value(&self, key: &str) -> Option<&dyn Any> {
        self.entries.value(key)
    }

    fn value_mut(&mut self, key: &str) -> Option<&mut dyn Any> {
        self.entries.value_mut(key)
    }

    fn remove_key(&mut self, key: &str) {
        self.entries.remove_key(key);
    }

    fn insert_filled(
        &mut self,
        key: String,
        fill: &mut dyn FnMut(&mut dyn Any) -> Result<(), ModelError>,
    ) -> Result<(), ModelError> {
        self.entries.insert_filled(key, fill)
    }

    fn insert_moved(&mut self, key: String, staged: &mut dyn Any) -> Result<(), ModelError> {
        self.entries.insert_moved(key, staged)
    }
}

/// A map reconciled key by key, as a `BTreeMap<String, T>` is.
impl<T: Model> Settable for Map<T> {}
impl<T: Model> Model for Map<T> {
    const SHAPE: Shape = Shape::Map(&T::SHAPE);
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    const CHECKS: bool = T::CHECKS;
    const OPS: bool = T::OPS;
    // Run by the functions of every map of string keys, over `T`'s table.
    const TABLE: &'static ModelTable = &MapIn::<T, Self>::TABLE;
    leaf_paths!();
    crate::__table_functions!();
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }
}

impl<R, T: Model> Path<R, Map<T>> {
    /// The entry `key`, its own paths rooted here.
    #[must_use]
    pub fn key(&self, key: impl Into<String>) -> T::Paths<R> {
        T::paths_at(self.child(Segment::Key(key.into())))
    }

    /// Remove the entry `key`.
    #[must_use]
    pub fn remove(&self, key: impl Into<String>) -> Edit<R> {
        self.edit(EditOp::RemoveKey { key: key.into() })
    }
}
