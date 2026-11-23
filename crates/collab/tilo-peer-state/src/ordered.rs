//! An ordered map whose copies share what neither changed.
//!
//! A workspace holds thousands of documents, and every write decides in a
//! copy of what it holds, then stores and journals what that copy changed.
//! With an ordinary map, each of those steps cost every document: the copy
//! copied every entry, and telling what changed compared every entry. Here
//! the entries sit in chunks of a few dozen, each shared between copies
//! until one copy changes it: a copy costs a pointer per chunk, a change
//! copies one chunk, and [`Ordered::changes`] skips every chunk two copies
//! still share.

use std::sync::Arc;

/// The most entries a chunk holds; a chunk that grows past it splits in two.
const CHUNK: usize = 64;

/// Entries in key order, shared between copies until one changes them.
type Chunk<K, V> = Arc<Vec<(K, V)>>;

/// An ordered map of `K` to `V` whose copies share their unchanged chunks.
pub(crate) struct Ordered<K, V> {
    /// The chunks in key order, none empty.
    chunks: Arc<Vec<Chunk<K, V>>>,
    len: usize,
}

impl<K, V> Clone for Ordered<K, V> {
    fn clone(&self) -> Self {
        Self {
            chunks: Arc::clone(&self.chunks),
            len: self.len,
        }
    }
}

impl<K, V> Default for Ordered<K, V> {
    fn default() -> Self {
        Self {
            chunks: Arc::default(),
            len: 0,
        }
    }
}

impl<K: std::fmt::Debug, V: std::fmt::Debug> std::fmt::Debug for Ordered<K, V> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

impl<K: Ord, V: PartialEq> PartialEq for Ordered<K, V> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.chunks, &other.chunks)
            || (self.len == other.len && self.iter().eq(other.iter()))
    }
}

impl<K: Ord, V: Eq> Eq for Ordered<K, V> {}

/// What one key holds in two copies of a map, where they differ.
pub(crate) struct Change<'a, K, V> {
    pub(crate) key: &'a K,
    #[cfg_attr(
        target_arch = "wasm32",
        allow(
            dead_code,
            reason = "the journal that reads it is the host's own file record"
        )
    )]
    pub(crate) before: Option<&'a V>,
    pub(crate) after: Option<&'a V>,
}

impl<K, V> Ordered<K, V> {
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The entries in key order.
    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = (&K, &V)> + '_ {
        self.chunks
            .iter()
            .flat_map(|chunk| chunk.iter().map(|(key, value)| (key, value)))
    }

    pub(crate) fn keys(&self) -> impl DoubleEndedIterator<Item = &K> + '_ {
        self.iter().map(|(key, _)| key)
    }

    pub(crate) fn values(&self) -> impl DoubleEndedIterator<Item = &V> + '_ {
        self.iter().map(|(_, value)| value)
    }

    /// Whether `a` and `b` are copies of one map neither changed since.
    pub(crate) fn ptr_eq(a: &Self, b: &Self) -> bool {
        Arc::ptr_eq(&a.chunks, &b.chunks)
    }
}

impl<K: Ord, V> Ordered<K, V> {
    /// The chunk `key` belongs in: the last one whose first key is not past it.
    fn chunk_of(&self, key: &K) -> Option<usize> {
        let after = self
            .chunks
            .partition_point(|chunk| chunk.first().is_some_and(|(first, _)| first <= key));
        if self.chunks.is_empty() {
            None
        } else {
            Some(after.saturating_sub(1))
        }
    }

    pub(crate) fn get_key_value(&self, key: &K) -> Option<(&K, &V)> {
        let chunk = self.chunks.get(self.chunk_of(key)?)?;
        let at = chunk.binary_search_by(|(held, _)| held.cmp(key)).ok()?;
        chunk.get(at).map(|(key, value)| (key, value))
    }

    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        self.get_key_value(key).map(|(_, value)| value)
    }

    pub(crate) fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }

    /// The entries of `before` and `after`, two copies of one map, that
    /// differ, in key order: a chunk both still share is skipped whole, and
    /// within one they do not, `same` tells two values apart.
    pub(crate) fn changes<'a>(
        before: &'a Self,
        after: &'a Self,
        same: impl Fn(&V, &V) -> bool,
    ) -> Vec<Change<'a, K, V>> {
        let mut changes = Vec::new();
        if Self::ptr_eq(before, after) {
            return changes;
        }
        let (old, new) = (&before.chunks, &after.chunks);
        // A cursor: a chunk, and an entry in it.
        let (mut old_chunk, mut old_at) = (0, 0);
        let (mut new_chunk, mut new_at) = (0, 0);
        loop {
            if old_at == 0
                && new_at == 0
                && let (Some(ours), Some(theirs)) = (old.get(old_chunk), new.get(new_chunk))
                && Arc::ptr_eq(ours, theirs)
            {
                old_chunk = old_chunk.saturating_add(1);
                new_chunk = new_chunk.saturating_add(1);
                continue;
            }
            let ours = old.get(old_chunk).and_then(|chunk| chunk.get(old_at));
            let theirs = new.get(new_chunk).and_then(|chunk| chunk.get(new_at));
            let (step_old, step_new) = match (ours, theirs) {
                (None, None) => break,
                (Some((key, value)), None) => {
                    changes.push(Change {
                        key,
                        before: Some(value),
                        after: None,
                    });
                    (true, false)
                }
                (None, Some((key, value))) => {
                    changes.push(Change {
                        key,
                        before: None,
                        after: Some(value),
                    });
                    (false, true)
                }
                (Some((old_key, old_value)), Some((new_key, new_value))) => {
                    match old_key.cmp(new_key) {
                        std::cmp::Ordering::Less => {
                            changes.push(Change {
                                key: old_key,
                                before: Some(old_value),
                                after: None,
                            });
                            (true, false)
                        }
                        std::cmp::Ordering::Greater => {
                            changes.push(Change {
                                key: new_key,
                                before: None,
                                after: Some(new_value),
                            });
                            (false, true)
                        }
                        std::cmp::Ordering::Equal => {
                            if !same(old_value, new_value) {
                                changes.push(Change {
                                    key: new_key,
                                    before: Some(old_value),
                                    after: Some(new_value),
                                });
                            }
                            (true, true)
                        }
                    }
                }
            };
            if step_old {
                old_at = old_at.saturating_add(1);
                if old.get(old_chunk).is_none_or(|chunk| old_at >= chunk.len()) {
                    (old_chunk, old_at) = (old_chunk.saturating_add(1), 0);
                }
            }
            if step_new {
                new_at = new_at.saturating_add(1);
                if new.get(new_chunk).is_none_or(|chunk| new_at >= chunk.len()) {
                    (new_chunk, new_at) = (new_chunk.saturating_add(1), 0);
                }
            }
        }
        changes
    }
}

impl<K: Ord + Clone, V: Clone> Ordered<K, V> {
    /// The chunk at `index`, to change: no longer shared with any copy.
    fn chunk_mut(&mut self, index: usize) -> Option<&mut Vec<(K, V)>> {
        Arc::make_mut(&mut self.chunks)
            .get_mut(index)
            .map(Arc::make_mut)
    }

    pub(crate) fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let index = self.chunk_of(key)?;
        let at = self
            .chunks
            .get(index)?
            .binary_search_by(|(held, _)| held.cmp(key))
            .ok()?;
        self.chunk_mut(index)?.get_mut(at).map(|(_, value)| value)
    }

    /// Hold `value` for `key`, answering what it held before.
    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V> {
        let Some(index) = self.chunk_of(&key) else {
            Arc::make_mut(&mut self.chunks).push(Arc::new(vec![(key, value)]));
            self.len = 1;
            return None;
        };
        let chunk = self.chunk_mut(index)?;
        match chunk.binary_search_by(|(held, _)| held.cmp(&key)) {
            Ok(at) => {
                let held = chunk.get_mut(at)?;
                Some(std::mem::replace(&mut held.1, value))
            }
            Err(at) => {
                chunk.insert(at, (key, value));
                if chunk.len() > CHUNK {
                    let rest = chunk.split_off(CHUNK / 2);
                    Arc::make_mut(&mut self.chunks).insert(index.saturating_add(1), Arc::new(rest));
                }
                self.len = self.len.saturating_add(1);
                None
            }
        }
    }

    /// Hold nothing for `key`, answering what it held.
    pub(crate) fn remove(&mut self, key: &K) -> Option<V> {
        let index = self.chunk_of(key)?;
        let at = self
            .chunks
            .get(index)?
            .binary_search_by(|(held, _)| held.cmp(key))
            .ok()?;
        let chunk = self.chunk_mut(index)?;
        let (_, value) = chunk.remove(at);
        if chunk.is_empty() {
            Arc::make_mut(&mut self.chunks).remove(index);
        }
        self.len = self.len.saturating_sub(1);
        Some(value)
    }
}

impl<K: Ord + Clone, V: Clone> FromIterator<(K, V)> for Ordered<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(entries: I) -> Self {
        let mut map = Self::default();
        for (key, value) in entries {
            map.insert(key, value);
        }
        map
    }
}

impl<K: Ord, V> std::ops::Index<&K> for Ordered<K, V> {
    type Output = V;
    #[allow(
        clippy::panic,
        reason = "`Index` panics on a key the map lacks, as the standard maps do"
    )]
    fn index(&self, key: &K) -> &V {
        self.get(key)
            .unwrap_or_else(|| panic!("no entry for the key"))
    }
}

impl<'a, K, V> IntoIterator for &'a Ordered<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Box<dyn DoubleEndedIterator<Item = (&'a K, &'a V)> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn a_map_holds_what_a_btree_map_holds_and_tells_its_changes() {
        let mut map = Ordered::default();
        let mut expected = BTreeMap::new();
        // Inserted out of order, many times a chunk.
        for n in 0..1_000_u32 {
            let key = n.wrapping_mul(7_919).wrapping_rem(1_000);
            map.insert(key, n);
            expected.insert(key, n);
        }
        assert_eq!(map.len(), expected.len());
        assert!(map.iter().map(|(k, v)| (*k, *v)).eq(expected.clone()));
        let before = map.clone();
        *map.get_mut(&500).unwrap() = 1;
        map.remove(&3);
        map.insert(1_500, 2);
        map.insert(2, 3);
        assert_eq!(map.get(&500), Some(&1));
        assert_eq!(
            before.get(&500),
            expected.get(&500),
            "a copy keeps what it held"
        );
        let changes: Vec<_> = Ordered::changes(&before, &map, |a, b| a == b)
            .into_iter()
            .map(|change| (*change.key, change.before.copied(), change.after.copied()))
            .collect();
        assert_eq!(
            changes,
            vec![
                (2, expected.get(&2).copied(), Some(3)),
                (3, expected.get(&3).copied(), None),
                (500, expected.get(&500).copied(), Some(1)),
                (1_500, None, Some(2)),
            ]
        );
        assert!(Ordered::changes(&map, &map.clone(), |a, b| a == b).is_empty());
    }
}
