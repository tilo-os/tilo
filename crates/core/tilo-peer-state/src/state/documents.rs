//! The documents a replica holds, with what is made of all of them kept up
//! to date where a write wrote.
//!
//! A commit's receipt names the replica's durable state by a digest
//! ([`super::StatePeer::durable_hash`]), and its publication stores the
//! replica's durable encoding in parts ([`super::StatePeer::durable_layout`]).
//! Making either from every document on every commit made each write cost
//! the size of the workspace. So the documents are held in an [`Ordered`]
//! map, whose copies share what neither changed, and both are made again
//! only for the documents that changed since they were last made: the digest
//! folds the documents into a Merkle tree, a document's leaf the digest of
//! its part of the durable encoding, and a change rehashes its leaf and the
//! few nodes above it. The tree's shape depends only on which documents are
//! held, never on the order they changed in, so one state has one digest on
//! every device and after every restart.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::Route;
use super::backend::StoredDocument;
use super::durable::DocumentParts;
use crate::ordered::Ordered;

/// Children of a branch of the tree.
const FANOUT: usize = 16;
/// Branches between the root and a bucket: 16^3 buckets, so a bucket holds
/// one document or none up to thousands of documents, and a few hundred at a
/// million.
const DEPTH: usize = 3;

/// The documents a replica holds, by route.
#[derive(Default)]
pub(super) struct Documents {
    map: Ordered<Route, StoredDocument>,
    made: parking_lot::Mutex<Made>,
}

/// What was made of the documents, each with the documents it was made of.
#[derive(Clone, Default)]
struct Made {
    tree_of: Ordered<Route, StoredDocument>,
    tree: Option<Node>,
    sealed_of: Ordered<Route, StoredDocument>,
    sealed: Ordered<Route, ()>,
    parts_of: Ordered<Route, StoredDocument>,
    parts: Ordered<Route, DocumentParts>,
}

impl Clone for Documents {
    fn clone(&self) -> Self {
        Self {
            map: self.map.clone(),
            made: parking_lot::Mutex::new(self.made.lock().clone()),
        }
    }
}

impl std::ops::Deref for Documents {
    type Target = Ordered<Route, StoredDocument>;
    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl std::ops::DerefMut for Documents {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.map
    }
}

impl<'a> IntoIterator for &'a Documents {
    type Item = (&'a Route, &'a StoredDocument);
    type IntoIter = Box<dyn DoubleEndedIterator<Item = (&'a Route, &'a StoredDocument)> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.map.iter())
    }
}

/// Counts a document taken again, for the tests that assert a write takes
/// only the documents it changed.
fn walked() {
    #[cfg(test)]
    super::DOCUMENTS_WALKED.with(|walked| walked.set(walked.get().saturating_add(1)));
}

impl Documents {
    /// Whether `a` and `b` are copies of one map of documents no copy changed.
    #[cfg(test)]
    pub(super) fn ptr_eq(a: &Self, b: &Self) -> bool {
        Ordered::ptr_eq(&a.map, &b.map)
    }

    /// How many documents hold sealed history.
    pub(super) fn sealed_count(&self) -> usize {
        let mut guard = self.made.lock();
        let made = &mut *guard;
        for change in Ordered::changes(&made.sealed_of, &self.map, StoredDocument::same) {
            if change.after.is_some_and(|document| document.keeps_sealed()) {
                made.sealed.insert(change.key.clone(), ());
            } else {
                made.sealed.remove(change.key);
            }
        }
        made.sealed_of = self.map.clone();
        let count = made.sealed.len();
        drop(guard);
        count
    }

    /// Each document's part of the durable encoding, as `encode` makes it:
    /// only the documents changed since the parts were last made are
    /// encoded again.
    pub(super) fn parts(
        &self,
        mut encode: impl FnMut(&Route, &StoredDocument) -> Result<DocumentParts, String>,
    ) -> Result<Ordered<Route, DocumentParts>, String> {
        let mut guard = self.made.lock();
        let made = &mut *guard;
        let mut parts = made.parts.clone();
        for change in Ordered::changes(&made.parts_of, &self.map, StoredDocument::same) {
            match change.after {
                Some(document) => {
                    walked();
                    parts.insert(change.key.clone(), encode(change.key, document)?);
                }
                None => {
                    parts.remove(change.key);
                }
            }
        }
        made.parts_of = self.map.clone();
        made.parts = parts.clone();
        drop(guard);
        Ok(parts)
    }

    /// The root of the documents' tree, each held document's leaf as `leaf`
    /// takes it: only the documents changed since the last root are taken
    /// again. `[0; 32]` while no document is held.
    pub(super) fn root(
        &self,
        mut leaf: impl FnMut(&Route, &StoredDocument) -> Result<[u8; 32], String>,
    ) -> Result<[u8; 32], String> {
        let mut guard = self.made.lock();
        let made = &mut *guard;
        let mut tree = made.tree.clone();
        for change in Ordered::changes(&made.tree_of, &self.map, StoredDocument::same) {
            let value = match change.after {
                Some(document) => {
                    walked();
                    Some(leaf(change.key, document)?)
                }
                None => None,
            };
            set(&mut tree, &key_of(change.key)?, 0, value)?;
        }
        let root = tree.as_ref().map_or([0; 32], Node::digest);
        made.tree_of = self.map.clone();
        made.tree = tree;
        drop(guard);
        Ok(root)
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
