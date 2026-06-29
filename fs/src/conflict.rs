//! Content-conflict resolution for the move-tree namespace.
//!
//! When two peers concurrently set a *different* content hash on the same file
//! node, Automerge keeps both values in the node's `hash` register and resolves
//! one deterministic winner. We surface the loser as a sibling
//! `<name>.sync-conflict-<hash>` node so no peer's bytes are silently dropped.
//! The copy name is derived purely from name + losing hash, so peers converge
//! on it instead of multiplying copies.

use iroh_blobs::Hash;

use crate::namespace::Namespace;

/// A concurrent content edit whose loser still needs a conflict copy.
#[derive(Debug, Clone)]
pub struct ContentConflict {
    pub node_id: String,
    pub loser: Hash,
}

const MARKER: &str = ".sync-conflict-";

/// Deterministic conflict-copy file name for a losing version of `name`. Uses a
/// 128-bit (32 hex) prefix of the losing hash.
pub fn conflict_name(name: &str, loser: &Hash) -> String {
    let hex = hex::encode(loser.as_bytes());
    format!("{name}{MARKER}{}", &hex[..32])
}

fn hash_from_hex(s: &str) -> Option<Hash> {
    let bytes = hex::decode(s).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(Hash::from(arr))
}

/// Detect content conflicts that don't yet have a conflict copy. Deterministic
/// across peers, idempotent once copies exist.
pub fn detect(ns: &Namespace) -> Vec<ContentConflict> {
    let mut out = Vec::new();
    for id in ns.live_node_ids() {
        let Some((parent, name)) = ns.node_parent_name(&id) else {
            continue;
        };
        if name.contains(MARKER) {
            continue; // never make conflict copies of conflict copies
        }
        let distinct: std::collections::BTreeSet<String> = ns
            .content_conflicts(&id)
            .into_iter()
            .map(|(_, hex)| hex)
            .collect();
        if distinct.len() < 2 {
            continue;
        }
        let winner = ns.node_hash(&id).map(|h| hex::encode(h.as_bytes()));
        for hex in distinct {
            if Some(&hex) == winner.as_ref() {
                continue;
            }
            let Some(loser) = hash_from_hex(&hex) else {
                continue;
            };
            if ns.child_id_named(&parent, &conflict_name(&name, &loser)).is_some() {
                continue; // already resolved
            }
            out.push(ContentConflict { node_id: id.clone(), loser });
        }
    }
    out
}

/// Materialize the conflict copy for `c` and return the CRDT delta to broadcast
/// (empty if it already existed). `loser_len` sizes the copy.
pub fn materialize(ns: &mut Namespace, c: &ContentConflict, loser_len: u64, now: u64) -> Vec<u8> {
    let Some((parent, name)) = ns.node_parent_name(&c.node_id) else {
        return Vec::new();
    };
    let cname = conflict_name(&name, &c.loser);
    if ns.create_conflict_copy(&parent, &cname, &c.loser, loser_len, now) {
        ns.save_incremental()
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::{Namespace, ROOT_INO};

    fn h(b: u8) -> Hash {
        Hash::from([b; 32])
    }

    #[test]
    fn concurrent_content_edits_produce_a_conflict_copy() {
        let mut a = Namespace::new("A");
        let ino = a.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
        let snap = a.save();
        let mut b = Namespace::from_bytes("B", &snap).unwrap();
        let bino = b.lookup(ROOT_INO, "doc").unwrap().ino.0;

        a.set_content(ino, Some(&h(10)), 10, 5);
        b.set_content(bino, Some(&h(20)), 20, 5);
        a.merge_doc(&mut b).unwrap();

        let conflicts = detect(&a);
        assert_eq!(conflicts.len(), 1, "one loser must need a conflict copy");
        let loser = conflicts[0].loser;
        assert!(loser == h(10) || loser == h(20));

        let delta = materialize(&mut a, &conflicts[0], 99, 6);
        assert!(!delta.is_empty());

        let winner = a.entry_at("doc").unwrap().hash.unwrap();
        assert!(winner == h(10) || winner == h(20));
        assert_ne!(winner, loser);

        let copy = a.entry_at(&conflict_name("doc", &loser)).expect("conflict copy exists");
        assert_eq!(copy.hash, Some(loser));
        assert_eq!(copy.size, 99);

        assert!(detect(&a).is_empty(), "resolution is idempotent");
    }

    #[test]
    fn no_conflict_when_edits_are_sequential() {
        let mut a = Namespace::new("A");
        let ino = a.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
        a.set_content(ino, Some(&h(2)), 2, 2);
        a.set_content(ino, Some(&h(3)), 3, 3);
        assert!(detect(&a).is_empty());
    }
}
