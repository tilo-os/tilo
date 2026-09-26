//! Content-conflict resolution for the move-tree namespace.
//!
//! Two cases surface a `<name>.sync-conflict-<hash>` copy so no peer's bytes
//! are silently dropped: **concurrent edit/edit** of one live file (the
//! losing hash becomes a sibling copy), and a **modify/delete race** (delete
//! wins, but the concurrent edit is resurrected as a copy at the pre-deletion location).
//!
//! Copy names derive purely from name + losing hash, so peers converge on them.

use collab_types::{FileNameV1, FileRecordId, FileVersionId, MAX_FILE_NAME_BYTES};
use iroh_blobs::Hash;

use crate::namespace::{ContentAuthority, Namespace, ROOT_ID};

/// A conflict copy that needs to be created (fully resolved location + name).
#[derive(Debug, Clone)]
pub struct ContentConflict {
    pub parent_id: String,
    pub name: FileNameV1,
    pub loser: Hash,
    /// Present for revision-aware conflicts. Legacy hash-only conflicts keep
    /// these empty and cannot be projected as first-class signed conflicts.
    pub record_id: Option<FileRecordId>,
    pub winner_version: Option<FileVersionId>,
    pub loser_version: Option<FileVersionId>,
}

const MARKER: &[u8] = b".sync-conflict-";

/// Deterministic conflict-copy file name for a losing version of `name`, using a 128-bit (32 hex) prefix of the losing hash.
pub fn conflict_name(name: impl AsRef<[u8]>, loser: &Hash) -> FileNameV1 {
    let hex = hex::encode(loser.as_bytes());
    let hash_prefix = hex.as_bytes().get(..32).unwrap_or(hex.as_bytes());
    let suffix = [MARKER, hash_prefix].concat();
    let name = name.as_ref();
    let prefix_len = name
        .len()
        .min(MAX_FILE_NAME_BYTES.saturating_sub(suffix.len()));
    let mut out = name.get(..prefix_len).unwrap_or(name).to_vec();
    out.extend_from_slice(&suffix);
    out.into()
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
        if name
            .as_bytes()
            .windows(MARKER.len())
            .any(|window| window == MARKER)
        {
            continue;
        }
        let revisions = ns.content_revision_conflicts(&id);
        if revisions.len() >= 2 {
            let Some(winner) = ns.content_revision_for_id(&id) else {
                continue;
            };
            let mut distinct = std::collections::BTreeMap::new();
            for (_, revision) in revisions {
                distinct.entry(revision.version_id).or_insert(revision);
            }
            if distinct.len() >= 2 {
                for revision in distinct.values() {
                    if revision.version_id == winner.version_id {
                        continue;
                    }
                    let Some(hash) = revision.content_hash else {
                        continue;
                    };
                    push_copy(
                        ns,
                        &mut out,
                        &parent,
                        &name,
                        &hex::encode(hash.0),
                        Some((revision.record_id, winner.version_id, revision.version_id)),
                    );
                }
                continue;
            }
        }
        // Under signed records a node's hash is the projection of its
        // record, and concurrent edits are the record's own conflict (two
        // heads, kept in its history). Concurrent hash values only say that
        // two devices projected their own heads while apart.
        if ns.content_authority() == ContentAuthority::SignedRecords {
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
            push_copy(ns, &mut out, &parent, &name, &hex, None);
        }
    }

    // (2) modify/delete: an explicitly-trashed file whose content changed concurrently with the deletion.
    for id in ns.directly_trashed() {
        let Some((_, name)) = ns.node_parent_name(&id) else {
            continue;
        };
        if name
            .as_bytes()
            .windows(MARKER.len())
            .any(|window| window == MARKER)
        {
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
            push_copy(ns, &mut out, &parent, &name, &hex, None);
        }
    }

    out
}

fn push_copy(
    ns: &Namespace,
    out: &mut Vec<ContentConflict>,
    parent: &str,
    name: &FileNameV1,
    loser_hex: &str,
    revision_binding: Option<(FileRecordId, FileVersionId, FileVersionId)>,
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
        record_id: revision_binding.map(|binding| binding.0),
        winner_version: revision_binding.map(|binding| binding.1),
        loser_version: revision_binding.map(|binding| binding.2),
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
    fn concurrent_edits_produce_bound_conflict_copies_and_sequential_edits_do_not() {
        // Phase `conflict_suffix_preserves_invalid_utf8_name_bytes`.
        {
            let original = [b'f', 0xff, b'o'];
            let name = conflict_name(original, &h(7));
            assert!(name.as_bytes().starts_with(&original));
            assert!(
                name.as_bytes()
                    .ends_with(b".sync-conflict-07070707070707070707070707070707")
            );
            assert!(name.as_bytes().len() <= MAX_FILE_NAME_BYTES);
        }
        // Phase `concurrent_content_edits_produce_a_conflict_copy`.
        {
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
            let copy_name =
                String::from_utf8(conflict_name("doc", &loser).as_bytes().to_vec()).unwrap();
            let copy = a.entry_at(&copy_name).expect("conflict copy");
            assert_eq!(copy.hash, Some(loser));
            assert!(detect(&a).is_empty(), "idempotent");
        }
        // Phase `concurrent_atomic_revisions_preserve_record_and_version_binding`.
        {
            let mut a = Namespace::new("A").unwrap();
            let ino = a.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
            let record_id = a.record_id_for(ino).unwrap();
            let snap = a.save();
            let mut b = Namespace::from_bytes("B", &snap).unwrap();
            let bino = b.lookup(ROOT_INO, "doc").unwrap().ino.0;
            let revision_a = collab_types::ContentRevisionV1 {
                schema_version: collab_types::CONTENT_REVISION_VERSION,
                record_id,
                version_id: collab_types::FileVersionId::new(),
                content_hash: Some(collab_types::ContentHash([10; 32])),
                byte_len: 10,
                metadata_hash: collab_types::ContentHash([11; 32]),
            };
            let revision_b = collab_types::ContentRevisionV1 {
                schema_version: collab_types::CONTENT_REVISION_VERSION,
                record_id,
                version_id: collab_types::FileVersionId::new(),
                content_hash: Some(collab_types::ContentHash([20; 32])),
                byte_len: 20,
                metadata_hash: collab_types::ContentHash([21; 32]),
            };
            assert!(a.set_content_revision(ino, &revision_a, 5));
            assert!(b.set_content_revision(bino, &revision_b, 5));
            a.merge_doc(&mut b).unwrap();

            let winner = a.content_revision_for(ino).unwrap();
            let loser = if winner == revision_a {
                revision_b
            } else {
                revision_a
            };
            let conflicts = detect(&a);
            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].record_id, Some(record_id));
            assert_eq!(conflicts[0].winner_version, Some(winner.version_id));
            assert_eq!(conflicts[0].loser_version, Some(loser.version_id));
            assert_eq!(
                conflicts[0].loser,
                Hash::from(loser.content_hash.unwrap().0)
            );
        }
        // Phase `no_conflict_when_edits_are_sequential`.
        {
            let mut a = Namespace::new("A").unwrap();
            let ino = a.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
            a.set_content(ino, Some(&h(2)), 2, 2);
            a.set_content(ino, Some(&h(3)), 3, 3);
            assert!(detect(&a).is_empty());
        }
    }

    #[test]
    fn deletes_resurrect_only_when_racing_an_edit() {
        // Phase `plain_delete_does_not_resurrect`.
        {
            // A deletes a file with no concurrent edit → no conflict copy.
            let mut a = Namespace::new("A").unwrap();
            let ino = a.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
            a.remove(ino);
            assert!(detect(&a).is_empty(), "a clean delete must not resurrect");
        }
        // Phase `modify_delete_race_resurrects_the_edit`.
        {
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
            let copy_name =
                String::from_utf8(conflict_name("doc", &h(9)).as_bytes().to_vec()).unwrap();
            let copy = a.entry_at(&copy_name).expect("resurrected conflict copy");
            assert_eq!(copy.hash, Some(h(9)));
            assert!(detect(&a).is_empty(), "idempotent");
        }
    }
}
