//! A sorted map from string keys whose clones share it chunk by chunk:
//! [`ChunkMap`], what a [`Map`](crate::Map), a [`Set`](crate::Set) and a
//! [`Grid`](crate::Grid) keep their contents in.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use tilo_sort::CompactSort as _;

use crate::cow::Shared;
use crate::model::ModelError;
use crate::table::{MapEntries, filled, mismatch};

/// Entries a full chunk holds; a chunk that grows past twice this splits.
const CHUNK: usize = 32;
const MAX_CHUNK: usize = 64;

type Chunk<V> = Vec<(String, V)>;

/// Entries sorted by key, in chunks of at most `MAX_CHUNK` that clones of
/// the map share until one of them changes the chunk.
///
/// Cloning copies a pointer per chunk; changing, adding or removing an entry
/// copies its chunk alone when a clone still shares it. A state that a view
/// holds therefore costs the next change one chunk, not the map.
pub(crate) struct ChunkMap<V> {
    /// Never empty, each sorted, and in key order one after the other.
    chunks: Vec<Shared<Chunk<V>>>,
    len: usize,
}

impl<V> Default for ChunkMap<V> {
    fn default() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
        }
    }
}

impl<V: Clone> Clone for ChunkMap<V> {
    fn clone(&self) -> Self {
        Self {
            chunks: self.chunks.iter().map(Shared::share).collect(),
            len: self.len,
        }
    }
}

impl<V: fmt::Debug> fmt::Debug for ChunkMap<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<V: PartialEq> PartialEq for ChunkMap<V> {
    /// A chunk both maps share is equal without a look inside it, so a map
    /// compared with a clone of it costs a pointer per chunk.
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len {
            return false;
        }
        let aligned = self.chunks.len() == other.chunks.len()
            && self
                .chunks
                .iter()
                .zip(&other.chunks)
                .all(|(mine, theirs)| mine.get().len() == theirs.get().len());
        if aligned {
            return self
                .chunks
                .iter()
                .zip(&other.chunks)
                .all(|(mine, theirs)| mine.same(theirs) || mine.get() == theirs.get());
        }
        self.iter().eq(other.iter())
    }
}

impl<V> From<BTreeMap<String, V>> for ChunkMap<V> {
    fn from(entries: BTreeMap<String, V>) -> Self {
        let len = entries.len();
        let mut chunks = Vec::with_capacity(len.div_ceil(CHUNK));
        let mut chunk = Vec::with_capacity(CHUNK.min(len));
        for entry in entries {
            chunk.push(entry);
            if chunk.len() == CHUNK {
                chunks.push(Shared::new(std::mem::take(&mut chunk)));
            }
        }
        if !chunk.is_empty() {
            chunks.push(Shared::new(chunk));
        }
        Self { chunks, len }
    }
}

impl<V> ChunkMap<V> {
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The chunk `key` belongs in: the first whose last key is not below it,
    /// or past the end.
    fn chunk_of(&self, key: &str) -> usize {
        self.chunks.partition_point(|chunk| {
            chunk
                .get()
                .last()
                .is_some_and(|(last, _)| last.as_str() < key)
        })
    }

    fn locate(&self, key: &str) -> Option<(usize, usize)> {
        let index = self.chunk_of(key);
        let at = self
            .chunks
            .get(index)?
            .get()
            .binary_search_by(|(held, _)| held.as_str().cmp(key))
            .ok()?;
        Some((index, at))
    }

    pub(crate) fn get(&self, key: &str) -> Option<&V> {
        let (index, at) = self.locate(key)?;
        let (_, value) = self.chunks.get(index)?.get().get(at)?;
        Some(value)
    }

    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.locate(key).is_some()
    }

    /// The entries in key order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
        self.chunks
            .iter()
            .flat_map(|chunk| chunk.get().iter().map(|(key, value)| (key, value)))
    }

    pub(crate) fn values(&self) -> impl ExactSizeIterator<Item = &V> {
        Values {
            inner: self.iter(),
            left: self.len,
        }
    }

    /// The value at `key`, to change: its chunk is copied first when a clone
    /// still shares it.
    pub(crate) fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        let (index, at) = self.locate(key)?;
        let (_, value) = self.chunks.get_mut(index)?.make_mut().get_mut(at)?;
        Some(value)
    }

    /// Put `value` at `key`; returns the value it replaced.
    pub(crate) fn insert(&mut self, key: String, value: V) -> Option<V> {
        let index = self.chunk_of(&key).min(self.chunks.len().saturating_sub(1));
        let Some(chunk) = self.chunks.get_mut(index) else {
            self.chunks.push(Shared::new(vec![(key, value)]));
            self.len = 1;
            return None;
        };
        let entries = chunk.make_mut();
        let at = match entries.binary_search_by(|(held, _)| held.as_str().cmp(&key)) {
            Ok(at) => {
                let (_, held) = entries.get_mut(at)?;
                return Some(std::mem::replace(held, value));
            }
            Err(at) => at,
        };
        entries.insert(at, (key, value));
        self.len = self.len.saturating_add(1);
        if entries.len() > MAX_CHUNK {
            let rest = entries.split_off(CHUNK);
            self.chunks
                .insert(index.saturating_add(1), Shared::new(rest));
        }
        None
    }

    /// The value at `key`, put there by `make` when there was none.
    pub(crate) fn get_or_insert_with(
        &mut self,
        key: &str,
        make: impl FnOnce() -> V,
    ) -> Option<&mut V> {
        if !self.contains_key(key) {
            self.insert(key.to_owned(), make());
        }
        self.get_mut(key)
    }

    pub(crate) fn remove(&mut self, key: &str) -> Option<V> {
        let (index, at) = self.locate(key)?;
        let entries = self.chunks.get_mut(index)?.make_mut();
        if at >= entries.len() {
            return None;
        }
        let (_, value) = entries.remove(at);
        let emptied = entries.is_empty();
        self.len = self.len.saturating_sub(1);
        if emptied {
            self.chunks.remove(index);
        }
        Some(value)
    }

    pub(crate) fn clear(&mut self) {
        self.chunks.clear();
        self.len = 0;
    }

    /// Keep the entries `keep` says to, given each to change, visiting only
    /// the chunks holding an entry `affected` names: the others are neither
    /// copied nor changed.
    pub(crate) fn retain_where(
        &mut self,
        affected: impl Fn(&V) -> bool,
        mut keep: impl FnMut(&mut V) -> bool,
    ) {
        for chunk in &mut self.chunks {
            if chunk.get().iter().any(|(_, value)| affected(value)) {
                chunk.make_mut().retain_mut(|(_, value)| keep(value));
            }
        }
        self.chunks.retain(|chunk| !chunk.get().is_empty());
        self.len = self.chunks.iter().map(|chunk| chunk.get().len()).sum();
    }

    /// The keys whose entries may differ between `self` and `before`, in
    /// key order, once each: the keys of the chunks the two do not share. A
    /// chunk both hold ([`Shared::same`]) holds the same entries in both, so
    /// a map changed an entry at a time since it was cloned differs only in
    /// the chunks those changes copied, however many entries it holds.
    pub(crate) fn differing_keys<'a>(&'a self, before: &'a Self) -> Vec<&'a str> {
        let mut keys: Vec<&'a str> = Vec::new();
        let mut mine = self.chunks.iter().peekable();
        let mut theirs = before.chunks.iter().peekable();
        loop {
            // A shared chunk sits on both sides with the same keys, and every
            // chunk before it on either side ends below its first key: the
            // chunk whose keys end first is shared with no later chunk of the
            // other side, so its keys differ, and the walk meets both copies
            // of a shared chunk together.
            let unshared = match (mine.peek(), theirs.peek()) {
                (None, None) => break,
                (Some(held), Some(was)) if held.same(was) => {
                    mine.next();
                    theirs.next();
                    continue;
                }
                (Some(held), Some(was)) if last_key(held) <= last_key(was) => mine.next(),
                (Some(_) | None, Some(_)) => theirs.next(),
                (Some(_), None) => mine.next(),
            };
            if let Some(chunk) = unshared {
                keys.extend(chunk.get().iter().map(|(key, _)| key.as_str()));
            }
        }
        keys.compact_sort();
        keys.dedup();
        keys
    }

    /// Whether `self` and `other` hold the chunk with `key` in common, not
    /// an equal copy of it.
    #[cfg(test)]
    pub(crate) fn shares_chunk_of(&self, other: &Self, key: &str) -> bool {
        match (
            self.chunks.get(self.chunk_of(key)),
            other.chunks.get(other.chunk_of(key)),
        ) {
            (Some(mine), Some(theirs)) => mine.same(theirs),
            _ => false,
        }
    }
}

fn last_key<V>(chunk: &Shared<Chunk<V>>) -> Option<&str> {
    chunk.get().last().map(|(key, _)| key.as_str())
}

/// The values of a [`ChunkMap`], in key order.
struct Values<I> {
    inner: I,
    left: usize,
}

impl<'a, K: 'a, V: 'a, I: Iterator<Item = (K, &'a V)>> Iterator for Values<I> {
    type Item = &'a V;
    fn next(&mut self) -> Option<&'a V> {
        let (_, value) = self.inner.next()?;
        self.left = self.left.saturating_sub(1);
        Some(value)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.left, Some(self.left))
    }
}

impl<'a, K: 'a, V: 'a, I: Iterator<Item = (K, &'a V)>> ExactSizeIterator for Values<I> {}

impl<T: 'static> MapEntries for ChunkMap<T> {
    fn count(&self) -> usize {
        self.len
    }

    fn each(&self, visit: &mut dyn FnMut(&str, &dyn Any)) {
        for (key, value) in self.iter() {
            visit(key, value);
        }
    }

    fn value(&self, key: &str) -> Option<&dyn Any> {
        self.get(key).map(|value| value as &dyn Any)
    }

    fn value_mut(&mut self, key: &str) -> Option<&mut dyn Any> {
        self.get_mut(key).map(|value| value as &mut dyn Any)
    }

    fn remove_key(&mut self, key: &str) {
        self.remove(key);
    }

    fn insert_filled(
        &mut self,
        key: String,
        fill: &mut dyn FnMut(&mut dyn Any) -> Result<(), ModelError>,
    ) -> Result<(), ModelError> {
        let mut slot: Option<T> = None;
        fill(&mut slot)?;
        self.insert(key, filled(slot)?);
        Ok(())
    }

    fn insert_moved(&mut self, key: String, staged: &mut dyn Any) -> Result<(), ModelError> {
        let value = staged
            .downcast_mut::<Option<T>>()
            .and_then(Option::take)
            .ok_or_else(mismatch)?;
        self.insert(key, value);
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "a test generator and counters"
)]
mod tests {
    use super::{ChunkMap, MAX_CHUNK};
    use std::collections::BTreeMap;

    /// A deterministic sequence, as the collection tests use.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) % n
        }
    }

    fn check(map: &ChunkMap<u64>, model: &BTreeMap<String, u64>) {
        assert_eq!(map.len(), model.len());
        let held: Vec<(&String, &u64)> = map.iter().collect();
        let expected: Vec<(&String, &u64)> = model.iter().collect();
        assert_eq!(held, expected);
        for chunk in &map.chunks {
            assert!(!chunk.get().is_empty(), "no chunk is empty");
            assert!(chunk.get().len() <= MAX_CHUNK, "no chunk is over full");
        }
        for (key, value) in model {
            assert_eq!(map.get(key), Some(value));
        }
    }

    #[test]
    fn it_reads_as_a_sorted_map_through_random_changes_while_clones_hold_it() {
        let mut rng = Lcg(7);
        let mut map = ChunkMap::default();
        let mut model = BTreeMap::new();
        let mut held: Vec<(ChunkMap<u64>, BTreeMap<String, u64>)> = Vec::new();
        for step in 0..4_000 {
            let key = format!("k{:04}", rng.below(600));
            match rng.below(10) {
                0..=4 => {
                    assert_eq!(map.insert(key.clone(), step), model.insert(key, step));
                }
                5..=6 => {
                    assert_eq!(map.remove(&key), model.remove(&key));
                }
                7 => {
                    if let Some(value) = map.get_mut(&key) {
                        *value += 1;
                    }
                    if let Some(value) = model.get_mut(&key) {
                        *value += 1;
                    }
                }
                8 => {
                    let cut = rng.below(4);
                    map.retain_where(|value| value % 4 == cut, |value| *value % 4 != cut);
                    model.retain(|_, value| *value % 4 != cut);
                }
                _ => held.push((map.clone(), model.clone())),
            }
            check(&map, &model);
        }
        for (map, model) in &held {
            check(map, model);
        }
        assert_eq!(ChunkMap::from(model.clone()), map);
    }

    #[test]
    fn a_change_copies_one_chunk_of_a_map_a_clone_holds() {
        let model: BTreeMap<String, u64> = (0..1_000).map(|n| (format!("k{n:04}"), n)).collect();
        let mut map = ChunkMap::from(model);
        let held = map.clone();
        *map.get_mut("k0500").unwrap() = 0;
        assert_eq!(held.get("k0500"), Some(&500));
        assert!(!map.shares_chunk_of(&held, "k0500"));
        let shared = (0..1_000)
            .filter(|n| map.shares_chunk_of(&held, &format!("k{n:04}")))
            .count();
        assert!(shared >= 1_000 - MAX_CHUNK, "{shared} entries still shared");
    }
}
