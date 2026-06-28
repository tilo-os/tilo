//! Content-conflict resolution.
//!
//! When two peers concurrently set a *different* content hash on the same file,
//! Automerge keeps both values in the `hash` register and resolves a single
//! deterministic winner (the same on every peer). We surface the loser as a
//! `*.sync-conflict-*` copy so no peer's bytes are ever silently dropped — the
//! same semantics Syncthing/Dropbox use. The conflict-copy name is derived
//! purely from the path + losing hash, so every peer agrees on it and the
//! copies converge instead of multiplying.

use iroh_blobs::Hash;

use crate::namespace::Namespace;

/// A concurrent content edit whose loser still needs a conflict copy.
#[derive(Debug, Clone)]
pub struct ContentConflict {
    pub path: String,
    pub loser: Hash,
}

const MARKER: &str = ".sync-conflict-";

/// Deterministic conflict-copy path for a losing version of `path`. Uses a
/// 128-bit (32 hex) prefix of the losing hash — collision-resistant enough that
/// two distinct losers can't alias, while keeping the file name bounded.
pub fn conflict_name(path: &str, loser: &Hash) -> String {
    let hex = hex::encode(loser.as_bytes());
    format!("{path}{MARKER}{}", &hex[..32])
}

fn hash_from_hex(s: &str) -> Option<Hash> {
    let bytes = hex::decode(s).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(Hash::from(arr))
}

/// Detect content conflicts that don't yet have a conflict copy. Deterministic
/// across peers, and idempotent (returns nothing once copies exist).
pub fn detect(ns: &Namespace) -> Vec<ContentConflict> {
    let mut out = Vec::new();
    for path in ns.live_paths() {
        if path.contains(MARKER) {
            continue; // never make conflict copies of conflict copies
        }
        let distinct: std::collections::BTreeSet<String> = ns
            .content_conflicts(&path)
            .into_iter()
            .map(|(_, hex)| hex)
            .collect();
        if distinct.len() < 2 {
            continue;
        }
        let winner = ns
            .entry_at(&path)
            .and_then(|e| e.hash)
            .map(|h| hex::encode(h.as_bytes()));
        for hex in distinct {
            if Some(&hex) == winner.as_ref() {
                continue; // the winner stays in place
            }
            let Some(loser) = hash_from_hex(&hex) else {
                continue;
            };
            if ns.entry_at(&conflict_name(&path, &loser)).is_some() {
                continue; // already resolved
            }
            out.push(ContentConflict {
                path: path.clone(),
                loser,
            });
        }
    }
    out
}

/// Materialize the conflict copy for `c`, returning the CRDT delta to broadcast
/// (empty if it already existed). `loser_len` is the byte length of the losing
/// blob so the copy has a correct file size.
pub fn materialize(ns: &mut Namespace, c: &ContentConflict, loser_len: u64, now: u64) -> Vec<u8> {
    let name = conflict_name(&c.path, &c.loser);
    if ns.insert_conflict_copy(&name, &c.loser, loser_len, now) {
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
        // A and B share one document (B bootstraps from A's save).
        let mut a = Namespace::new("A");
        let ino = a.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
        let snap = a.save();
        let mut b = Namespace::from_bytes("B", &snap).unwrap();
        let bino = b.lookup(ROOT_INO, "doc").unwrap().ino.0;

        // Concurrent, conflicting content writes.
        a.set_content(ino, Some(&h(10)), 10, 5);
        b.set_content(bino, Some(&h(20)), 20, 5);

        a.merge_doc(&mut b).unwrap();

        // Exactly one loser needs a copy.
        let conflicts = detect(&a);
        assert_eq!(conflicts.len(), 1, "one loser must need a conflict copy");
        let loser = conflicts[0].loser;
        assert!(loser == h(10) || loser == h(20));

        let delta = materialize(&mut a, &conflicts[0], 99, 6);
        assert!(!delta.is_empty(), "materialize must produce a delta");

        // Winner stays in place...
        let winner = a.entry_at("doc").unwrap().hash.unwrap();
        assert!(winner == h(10) || winner == h(20));
        assert_ne!(winner, loser);

        // ...and the loser is preserved as a conflict copy (no bytes lost).
        let copy = a
            .entry_at(&conflict_name("doc", &loser))
            .expect("conflict copy must exist");
        assert_eq!(copy.hash, Some(loser));
        assert_eq!(copy.size, 99);

        // Resolution is idempotent.
        assert!(detect(&a).is_empty(), "no new conflicts after resolution");
    }

    #[test]
    fn no_conflict_when_edits_are_sequential() {
        let mut a = Namespace::new("A");
        let ino = a.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
        a.set_content(ino, Some(&h(2)), 2, 2);
        a.set_content(ino, Some(&h(3)), 3, 3);
        assert!(detect(&a).is_empty(), "sequential edits are not conflicts");
    }
}
