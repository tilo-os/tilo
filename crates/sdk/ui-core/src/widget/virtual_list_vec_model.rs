//! The list model for items held whole in memory: [`VecListModel`], which answers a list's
//! rows from a vector keyed once at build.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use super::{DuplicateListKey, ListModel, TreeRow};

pub struct VecListModel<T, K> {
    items: Vec<T>,
    keys: Vec<K>,
    index: HashMap<K, usize>,
    parents: Option<HashMap<K, K>>,
    branches: Option<HashSet<K>>,
}

impl<T, K> VecListModel<T, K>
where
    K: Clone + Eq + Hash,
{
    pub fn new(items: Vec<T>, mut key: impl FnMut(&T) -> K) -> Result<Self, DuplicateListKey> {
        let keys = items.iter().map(&mut key).collect::<Vec<_>>();
        let index = keys
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, key)| (key, index))
            .collect::<HashMap<_, _>>();
        if index.len() != keys.len() {
            return Err(DuplicateListKey);
        }
        Ok(Self {
            items,
            keys,
            index,
            parents: None,
            branches: None,
        })
    }

    #[must_use]
    pub fn empty() -> Self {
        Self {
            items: Vec::new(),
            keys: Vec::new(),
            index: HashMap::new(),
            parents: None,
            branches: None,
        }
    }

    #[must_use]
    pub fn with_parents(mut self, mut parent: impl FnMut(&T) -> Option<K>) -> Self {
        self.parents = Some(
            self.items
                .iter()
                .zip(&self.keys)
                .filter_map(|(item, key)| Some((key.clone(), parent(item)?)))
                .collect(),
        );
        self
    }

    #[must_use]
    pub fn with_branches(mut self, mut branch: impl FnMut(&T) -> bool) -> Self {
        self.branches = Some(
            self.items
                .iter()
                .zip(&self.keys)
                .filter(|(item, _)| branch(item))
                .map(|(_, key)| key.clone())
                .collect(),
        );
        self
    }

    #[must_use]
    pub fn item(&self, index: usize) -> Option<&T> {
        self.items.get(index)
    }

    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    pub fn push(&mut self, key: K, item: T) -> Result<(), DuplicateListKey> {
        if self.index.contains_key(&key) {
            return Err(DuplicateListKey);
        }
        let index = self.items.len();
        self.items.push(item);
        self.keys.push(key.clone());
        self.index.insert(key, index);
        Ok(())
    }

    pub fn remove(&mut self, index: usize) -> Option<T> {
        if index >= self.items.len() {
            return None;
        }
        let key = self.keys.remove(index);
        if let Some(parents) = &mut self.parents {
            parents.remove(&key);
        }
        let item = self.items.remove(index);
        self.rebuild_index();
        Some(item)
    }

    pub fn move_item(&mut self, from: usize, to: usize) -> bool {
        if from >= self.items.len() || to >= self.items.len() || from == to {
            return false;
        }
        let item = self.items.remove(from);
        let key = self.keys.remove(from);
        self.items.insert(to, item);
        self.keys.insert(to, key);
        self.rebuild_index();
        true
    }

    fn rebuild_index(&mut self) {
        self.index.clear();
        self.index.extend(
            self.keys
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, key)| (key, index)),
        );
    }
}

impl<T, K> ListModel for VecListModel<T, K>
where
    K: Clone + Eq + Hash + 'static,
{
    type Key = K;

    fn len(&self) -> usize {
        self.items.len()
    }

    fn key_at(&self, index: usize) -> Option<Self::Key> {
        self.keys.get(index).cloned()
    }

    fn index_of(&self, key: &Self::Key) -> Option<usize> {
        self.index.get(key).copied()
    }

    fn accepts_children(&self, index: usize) -> bool {
        self.has_subtasks(index)
    }

    fn has_subtasks(&self, index: usize) -> bool {
        let Some(branches) = &self.branches else {
            return false;
        };
        self.keys
            .get(index)
            .is_some_and(|key| branches.contains(key))
    }

    fn tree_at(&self, index: usize) -> Option<TreeRow<Self::Key>> {
        let parents = self.parents.as_ref()?;
        Some(TreeRow {
            parent: self
                .keys
                .get(index)
                .and_then(|key| parents.get(key))
                .cloned(),
            depth: None,
            above: None,
        })
    }
}
