//! The documents a replica holds, with a digest of all of them that a write
//! updates where it wrote.
//!
//! A commit's receipt names the replica's durable state by a digest
//! ([`super::StatePeer::durable_hash`]). Folding every document into it on
//! every commit made each write cost the size of the workspace, so the
//! documents are folded into a Merkle tree instead: a document's leaf is the
//! digest of its part of the durable encoding, and a change to a document
//! rehashes its leaf and the few nodes above it. The tree's shape depends
//! only on which documents are held, never on the order they changed in, so
//! one state has one digest on every device and after every restart.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::Route;
use super::backend::StoredDocument;
use crate::shared::Shared;

/// Children of a branch of the tree.
const FANOUT: usize = 16;
/// Branches between the root and a bucket: 16^3 buckets, so a bucket holds
/// one document or none up to thousands of documents, and a few hundred at a
/// million.
const DEPTH: usize = 3;

/// The documents a replica holds, by route. Reads see the map; every change
/// goes through a method here, which names the document changed for the next
/// digest.
#[derive(Default)]
pub(super) struct Documents {
    map: Shared<BTreeMap<Route, StoredDocument>>,
    digest: parking_lot::Mutex<Folded>,
}

/// The tree as of the last digest, and the documents changed since; the
/// documents holding sealed history, and the documents changed since they were
/// last counted.
#[derive(Clone, Default)]
struct Folded {
    tree: Option<Node>,
    changed: BTreeSet<Route>,
    sealed: BTreeSet<Route>,
    sealed_changed: BTreeSet<Route>,
}

impl Folded {
    fn changed(&mut self, route: &Route) {
        self.changed.insert(route.clone());
        self.sealed_changed.insert(route.clone());
    }
}

impl Clone for Documents {
    fn clone(&self) -> Self {
        Self {
            map: self.map.clone(),
            digest: parking_lot::Mutex::new(self.digest.lock().clone()),
        }
    }
}

impl std::ops::Deref for Documents {
    type Target = BTreeMap<Route, StoredDocument>;
    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl<'a> IntoIterator for &'a Documents {
    type Item = (&'a Route, &'a StoredDocument);
    type IntoIter = std::collections::btree_map::Iter<'a, Route, StoredDocument>;
    fn into_iter(self) -> Self::IntoIter {
        self.map.iter()
    }
}

impl Documents {
    /// Hold `document` as `route`'s document.
    pub(super) fn insert(
        &mut self,
        route: Route,
        document: StoredDocument,
    ) -> Option<StoredDocument> {
        self.digest.get_mut().changed(&route);
        self.map.insert(route, document)
    }

    /// `route`'s document, to change.
    pub(super) fn get_mut(&mut self, route: &Route) -> Option<&mut StoredDocument> {
        if !self.map.contains_key(route) {
            return None;
        }
        self.digest.get_mut().changed(route);
        self.map.get_mut(route)
    }

    /// Hold no documents.
    #[cfg(test)]
    pub(super) fn clear(&mut self) {
        let folded = self.digest.get_mut();
        for route in self.map.keys() {
            folded.changed(route);
        }
        self.map.clear();
    }

    /// Whether `a` and `b` are copies of one map of documents no copy changed.
    #[cfg(test)]
    pub(super) fn ptr_eq(a: &Self, b: &Self) -> bool {
        Shared::ptr_eq(&a.map, &b.map)
    }

    /// The documents that hold sealed history, in route order.
    pub(super) fn sealed(&self) -> Vec<(&Route, &StoredDocument)> {
        let mut guard = self.digest.lock();
        let folded = &mut *guard;
        for route in std::mem::take(&mut folded.sealed_changed) {
            if self
                .map
                .get(&route)
                .is_some_and(|document| !document.sealed.is_empty())
            {
                folded.sealed.insert(route);
            } else {
                folded.sealed.remove(&route);
            }
        }
        let sealed = folded
            .sealed
            .iter()
            .filter_map(|route| self.map.get_key_value(route))
            .collect();
        drop(guard);
        sealed
    }

    /// The root of the documents' tree, each held document's leaf as `leaf`
    /// takes it: only the documents changed since the last root are taken
    /// again. `[0; 32]` while no document is held.
    pub(super) fn root(
        &self,
        mut leaf: impl FnMut(&Route, &StoredDocument) -> Result<[u8; 32], String>,
    ) -> Result<[u8; 32], String> {
        let mut folded = self.digest.lock();
        let changed = std::mem::take(&mut folded.changed);
        for route in &changed {
            let key = key_of(route)?;
            let value = match self.map.get(route) {
                Some(document) => {
                    #[cfg(test)]
                    super::DOCUMENTS_WALKED
                        .with(|walked| walked.set(walked.get().saturating_add(1)));
                    Some(leaf(route, document)?)
                }
                None => None,
            };
            if let Err(error) = set(&mut folded.tree, &key, 0, value) {
                // Nothing of the tree is kept that the next root would miss.
                folded.changed = changed;
                return Err(error);
            }
        }
        Ok(folded.tree.as_ref().map_or([0; 32], Node::digest))
    }
}

/// The root of a tree of `leaves`, made whole: what [`Documents::root`] must answer.
#[cfg(test)]
pub(super) fn root_of(
    leaves: impl IntoIterator<Item = (Route, [u8; 32])>,
) -> Result<[u8; 32], String> {
    let mut tree = None;
    for (route, leaf) in leaves {
        set(&mut tree, &key_of(&route)?, 0, Some(leaf))?;
    }
    Ok(tree.as_ref().map_or([0; 32], Node::digest))
}

/// A document's place in the tree: the digest of its route.
fn key_of(route: &Route) -> Result<[u8; 32], String> {
    let encoded = postcard::to_allocvec(route).map_err(|error| error.to_string())?;
    Ok(
        *blake3::Hasher::new_derive_key("Tilo durable document key v1")
            .update(&encoded)
            .finalize()
            .as_bytes(),
    )
}

/// A subtree, never empty: an empty one is `None` where it hangs.
#[derive(Clone)]
enum Node {
    Branch(Arc<Branch>),
    Bucket(Arc<Bucket>),
}

#[derive(Clone)]
struct Branch {
    children: [Option<Node>; FANOUT],
    digest: [u8; 32],
}

#[derive(Clone)]
struct Bucket {
    leaves: BTreeMap<[u8; 32], [u8; 32]>,
    digest: [u8; 32],
}

impl Node {
    fn digest(&self) -> [u8; 32] {
        match self {
            Self::Branch(branch) => branch.digest,
            Self::Bucket(bucket) => bucket.digest,
        }
    }
}

/// The child of a branch at `depth` that `key` goes under.
fn nibble(key: &[u8; 32], depth: usize) -> usize {
    let byte = key.get(depth / 2).copied().unwrap_or(0);
    usize::from(if depth.is_multiple_of(2) {
        byte >> 4
    } else {
        byte & 0x0f
    })
}

/// Set `key`'s leaf to `value`, or take it out with `None`, in the subtree
/// at `depth` that `node` holds, and digest again what is above it.
fn set(
    node: &mut Option<Node>,
    key: &[u8; 32],
    depth: usize,
    value: Option<[u8; 32]>,
) -> Result<(), String> {
    if depth == DEPTH {
        let mut bucket = match node.take() {
            Some(Node::Bucket(bucket)) => bucket,
            Some(Node::Branch(_)) => return Err("a document tree's bucket is a branch".into()),
            None => Arc::new(Bucket {
                leaves: BTreeMap::new(),
                digest: [0; 32],
            }),
        };
        let held = Arc::make_mut(&mut bucket);
        match value {
            Some(value) => held.leaves.insert(*key, value),
            None => held.leaves.remove(key),
        };
        if held.leaves.is_empty() {
            return Ok(());
        }
        let mut hasher = blake3::Hasher::new_derive_key("Tilo durable document bucket v1");
        for (key, leaf) in &held.leaves {
            hasher.update(key);
            hasher.update(leaf);
        }
        held.digest = *hasher.finalize().as_bytes();
        *node = Some(Node::Bucket(bucket));
        return Ok(());
    }
    let mut branch = match node.take() {
        Some(Node::Branch(branch)) => branch,
        Some(Node::Bucket(_)) => return Err("a document tree's branch is a bucket".into()),
        None => Arc::new(Branch {
            children: Default::default(),
            digest: [0; 32],
        }),
    };
    let held = Arc::make_mut(&mut branch);
    let child = held
        .children
        .get_mut(nibble(key, depth))
        .ok_or("a document tree's branch has no such child")?;
    set(child, key, depth.saturating_add(1), value)?;
    if held.children.iter().all(Option::is_none) {
        return Ok(());
    }
    let mut hasher = blake3::Hasher::new_derive_key("Tilo durable document branch v1");
    for child in &held.children {
        hasher.update(&child.as_ref().map_or([0; 32], Node::digest));
    }
    held.digest = *hasher.finalize().as_bytes();
    *node = Some(Node::Branch(branch));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: usize) -> [u8; 32] {
        *blake3::hash(&n.to_le_bytes()).as_bytes()
    }

    fn root_of(leaves: &BTreeMap<usize, [u8; 32]>) -> [u8; 32] {
        let mut tree = None;
        for (n, leaf) in leaves {
            set(&mut tree, &key(*n), 0, Some(*leaf)).unwrap();
        }
        tree.as_ref().map_or([0; 32], Node::digest)
    }

    #[test]
    fn a_tree_digests_what_it_holds_whatever_order_it_changed_in() {
        let leaves: BTreeMap<usize, [u8; 32]> = (0..500_usize)
            .map(|n| (n, key(n.saturating_add(1_000))))
            .collect();
        let whole = root_of(&leaves);
        // Built in another order, with documents taken out again and leaves
        // changed back: the same documents, the same digest.
        let mut tree = None;
        for n in (0..700).rev() {
            set(&mut tree, &key(n), 0, Some([7; 32])).unwrap();
        }
        for n in 500..700 {
            set(&mut tree, &key(n), 0, None).unwrap();
        }
        for (n, leaf) in &leaves {
            set(&mut tree, &key(*n), 0, Some(*leaf)).unwrap();
        }
        assert_eq!(tree.as_ref().map_or([0; 32], Node::digest), whole);
        // One leaf changed changes the digest; every document taken out leaves
        // the empty tree's.
        let mut changed = leaves;
        changed.insert(42, [1; 32]);
        assert_ne!(root_of(&changed), whole);
        for n in 0..500 {
            set(&mut tree, &key(n), 0, None).unwrap();
        }
        assert!(tree.is_none());
    }
}
