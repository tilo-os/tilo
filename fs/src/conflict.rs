//! Content-conflict resolution for the move-tree namespace.
//!
//! Two cases surface a `<name>.sync-conflict-<hash>` copy so no peer's bytes
//! are silently dropped: **concurrent edit/edit** of one live file (the
//! losing hash becomes a sibling copy), and a **modify/delete race** (delete
//! wins, but the concurrent edit is resurrected as a copy at the pre-deletion location).
//!
//! Copy names derive purely from name + losing hash, so peers converge on them.

use iroh_blobs::Hash;

use crate::namespace::{Namespace, ROOT_ID};

/// A conflict copy that needs to be created (fully resolved location + name).
#[derive(Debug, Clone)]
pub struct ContentConflict {
    pub parent_id: String,
    pub name: String,
    pub loser: Hash,
}

const MARKER: &str = ".sync-conflict-";

/// Deterministic conflict-copy file name for a losing version of `name`, using a 128-bit (32 hex) prefix of the losing hash.
pub fn conflict_name(name: &str, loser: &Hash) -> String {
    let hex = hex::encode(loser.as_bytes());
    // `hex::encode` only ever emits ASCII hex digits, so a char-count prefix is a byte prefix.
    let prefix: String = hex.chars().take(32).collect();
    format!("{name}{MARKER}{prefix}")
}

fn hash_from_hex(s: &str) -> Option<Hash> {
    let bytes = hex::decode(s).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(Hash::from(arr))
}

/// Detect conflict copies that don't yet exist — deterministic across peers, idempotent once present.
pub fn detect(ns: &Namespace) -> Vec<ContentConflict> {
    let mut out = Vec::new();

    // (1) concurrent edits of a live file.
    for id in ns.live_node_ids() {
        let Some((parent, name)) = ns.node_parent_name(&id) else {
            continue;
        };
        if name.contains(MARKER) {
            continue;
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
            push_copy(ns, &mut out, &parent, &name, &hex);
        }
    }

    // (2) modify/delete: an explicitly-trashed file whose content changed concurrently with the deletion.
    for id in ns.directly_trashed() {
        let Some((_, name)) = ns.node_parent_name(&id) else {
            continue;
        };
        if name.contains(MARKER) {
            continue;
        }
        let dh = ns.deleted_hash(&id).map(|h| hex::encode(h.as_bytes()));
        let mut candidates: std::collections::BTreeSet<String> = ns
            .content_conflicts(&id)
            .into_iter()
            .map(|(_, hex)| hex)
            .collect();
        if let Some(h) = ns.node_hash(&id) {
            candidates.insert(hex::encode(h.as_bytes()));
        }
        for hex in candidates {
            if Some(&hex) == dh.as_ref() {
                continue; // content unchanged since deletion → not a concurrent edit
            }
            // Resurrect at the pre-deletion location, or root if that's gone too.
            let mut parent = ns.prev_parent(&id).unwrap_or_else(|| ROOT_ID.to_string());
            if !ns.is_visible(&parent) {
                parent = ROOT_ID.to_string();
            }
            push_copy(ns, &mut out, &parent, &name, &hex);
        }
    }

    out
}

fn push_copy(
    ns: &Namespace,
    out: &mut Vec<ContentConflict>,
    parent: &str,
    name: &str,
    loser_hex: &str,
) {
    let Some(loser) = hash_from_hex(loser_hex) else {
        return;
    };
    let cname = conflict_name(name, &loser);
    if ns.child_id_named(parent, &cname).is_some() {
        return; // already resolved
    }
    if out.iter().any(|c| c.parent_id == parent && c.name == cname) {
        return; // already queued this pass
    }
    out.push(ContentConflict {
        parent_id: parent.to_string(),
        name: cname,
        loser,
    });
}

/// Materialize the conflict copy for `c`; returns the CRDT delta to broadcast (empty if it already existed).
pub fn materialize(ns: &mut Namespace, c: &ContentConflict, loser_len: u64, now: u64) -> Vec<u8> {
    if ns.create_conflict_copy(&c.parent_id, &c.name, &c.loser, loser_len, now) {
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
        let mut a = Namespace::new("A").unwrap();
        let ino = a.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
        let snap = a.save();
        let mut b = Namespace::from_bytes("B", &snap).unwrap();
        let bino = b.lookup(ROOT_INO, "doc").unwrap().ino.0;

        a.set_content(ino, Some(&h(10)), 10, 5);
        b.set_content(bino, Some(&h(20)), 20, 5);
        a.merge_doc(&mut b).unwrap();

        let conflicts = detect(&a);
        assert_eq!(conflicts.len(), 1);
        let loser = conflicts[0].loser;
        assert!(loser == h(10) || loser == h(20));

        let delta = materialize(&mut a, &conflicts[0], 99, 6);
        assert!(!delta.is_empty());

        let winner = a.entry_at("doc").unwrap().hash.unwrap();
        assert_ne!(winner, loser);
        let copy = a
            .entry_at(&conflict_name("doc", &loser))
            .expect("conflict copy");
        assert_eq!(copy.hash, Some(loser));
        assert!(detect(&a).is_empty(), "idempotent");
    }

    #[test]
    fn no_conflict_when_edits_are_sequential() {
        let mut a = Namespace::new("A").unwrap();
        let ino = a.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
        a.set_content(ino, Some(&h(2)), 2, 2);
        a.set_content(ino, Some(&h(3)), 3, 3);
        assert!(detect(&a).is_empty());
    }

    #[test]
    fn plain_delete_does_not_resurrect() {
        // A deletes a file with no concurrent edit → no conflict copy.
        let mut a = Namespace::new("A").unwrap();
        let ino = a.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
        a.remove(ino);
        assert!(detect(&a).is_empty(), "a clean delete must not resurrect");
    }

    #[test]
    fn modify_delete_race_resurrects_the_edit() {
        // A deletes /doc while B edits its content concurrently.
        let mut a = Namespace::new("A").unwrap();
        let ino = a.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
        let snap = a.save();
        let mut b = Namespace::from_bytes("B", &snap).unwrap();

        a.remove(ino);
        let bino = b.lookup(ROOT_INO, "doc").unwrap().ino.0;
        b.set_content(bino, Some(&h(9)), 9, 5);

        a.merge_doc(&mut b).unwrap();
        a.resolve();

        assert!(a.lookup(ROOT_INO, "doc").is_none(), "delete wins");
        let conflicts = detect(&a);
        let c = conflicts
            .iter()
            .find(|c| c.loser == h(9))
            .expect("B's concurrent edit must be surfaced");
        materialize(&mut a, c, 9, 6);

        // The edited content is now visible again as a conflict copy at root.
        let copy = a
            .entry_at(&conflict_name("doc", &h(9)))
            .expect("resurrected conflict copy");
        assert_eq!(copy.hash, Some(h(9)));
        assert!(detect(&a).is_empty(), "idempotent");
    }
}
