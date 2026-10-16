//! Content-conflict resolution for the move-tree namespace.
//!
//! Two cases surface a `<name>.sync-conflict-<hash>` copy so no peer's bytes
//! are silently dropped: **concurrent edit/edit** of one live file (the
//! losing hash becomes a sibling copy), and a **modify/delete race** (delete
//! wins, but the concurrent edit is resurrected as a copy at the pre-deletion location).
//!
//! Copy names derive purely from name + losing hash, so peers converge on them.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use fs_protocol::{FileNameV1, FileRecordId, FileVersionId, MAX_FILE_NAME_BYTES};
use iroh_blobs::Hash;

use crate::namespace::{ContentAuthority, Namespace, ROOT_ID};
use tilo_lock::MutexRecover as _;

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
    #[cfg(test)]
    ns.note_conflict_scan();
    let mut found = Found::default();
    for id in ns.node_ids_after(None, usize::MAX) {
        found.visit(ns, &id);
    }
    found.into_conflicts()
}

/// What [`detect`] has found so far, over any number of steps.
#[derive(Default)]
struct Found {
    /// Case (1), concurrent edits of a live file.
    edits: Vec<ContentConflict>,
    /// Case (2), modify/delete.
    deletes: Vec<ContentConflict>,
}

impl Found {
    /// Look for either case at node `id`.
    fn visit(&mut self, ns: &Namespace, id: &str) {
        if ns.is_visible(id) {
            concurrent_edits(ns, id, &mut self.edits);
        } else if ns.is_directly_trashed(id) {
            modify_delete(ns, id, &mut self.deletes);
        }
    }

    /// Edits first, then each delete whose copy no edit already queued.
    fn into_conflicts(self) -> Vec<ContentConflict> {
        let mut out = self.edits;
        for conflict in self.deletes {
            if !out
                .iter()
                .any(|c| c.parent_id == conflict.parent_id && c.name == conflict.name)
            {
                out.push(conflict);
            }
        }
        out
    }
}

fn has_marker(name: &FileNameV1) -> bool {
    name.as_bytes()
        .windows(MARKER.len())
        .any(|window| window == MARKER)
}

/// (1) concurrent edits of the live file `id`.
fn concurrent_edits(ns: &Namespace, id: &str, out: &mut Vec<ContentConflict>) {
    let Some((parent, name)) = ns.node_parent_name(id) else {
        return;
    };
    if has_marker(&name) {
        return;
    }
    let revisions = ns.content_revision_conflicts(id);
    if revisions.len() >= 2 {
        let Some(winner) = ns.content_revision_for_id(id) else {
            return;
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
                    out,
                    &parent,
                    &name,
                    &hex::encode(hash.0),
                    Some((revision.record_id, winner.version_id, revision.version_id)),
                );
            }
            return;
        }
    }
    // Under signed records a node's hash is the projection of its
    // record, and concurrent edits are the record's own conflict (two
    // heads, kept in its history). Concurrent hash values only say that
    // two devices projected their own heads while apart.
    if ns.content_authority() == ContentAuthority::SignedRecords {
        return;
    }
    let distinct: std::collections::BTreeSet<String> = ns
        .content_conflicts(id)
        .into_iter()
        .map(|(_, hex)| hex)
        .collect();
    if distinct.len() < 2 {
        return;
    }
    let winner = ns.node_hash(id).map(|h| hex::encode(h.as_bytes()));
    for hex in distinct {
        if Some(&hex) == winner.as_ref() {
            continue;
        }
        push_copy(ns, out, &parent, &name, &hex, None);
    }
}

/// (2) modify/delete: the explicitly-trashed file `id`, whose content
/// changed concurrently with the deletion.
fn modify_delete(ns: &Namespace, id: &str, out: &mut Vec<ContentConflict>) {
    let Some((_, name)) = ns.node_parent_name(id) else {
        return;
    };
    if has_marker(&name) {
        return;
    }
    let dh = ns.deleted_hash(id).map(|h| hex::encode(h.as_bytes()));
    let mut candidates: std::collections::BTreeSet<String> = ns
        .content_conflicts(id)
        .into_iter()
        .map(|(_, hex)| hex)
        .collect();
    if let Some(h) = ns.node_hash(id) {
        candidates.insert(hex::encode(h.as_bytes()));
    }
    for hex in candidates {
        if Some(&hex) == dh.as_ref() {
            continue; // content unchanged since deletion → not a concurrent edit
        }
        // Resurrect at the pre-deletion location, or root if that's gone too.
        let mut parent = ns.prev_parent(id).unwrap_or_else(|| ROOT_ID.to_string());
        if !ns.is_visible(&parent) {
            parent = ROOT_ID.to_string();
        }
        push_copy(ns, out, &parent, &name, &hex, None);
    }
}

/// Nodes [`NamespaceScan`] examines per hold of the namespace lock.
pub const SCAN_STEP_NODES: usize = 64;

/// The work one [`NamespaceScan::due`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConflictPassStats {
    /// Nodes examined.
    pub nodes: usize,
    /// Times the namespace lock was taken.
    pub lock_holds: usize,
    /// The longest of those holds.
    pub longest_hold: Duration,
    /// All of them together.
    pub total_hold: Duration,
}

impl ConflictPassStats {
    fn held(&mut self, hold: Duration) {
        self.lock_holds = self.lock_holds.saturating_add(1);
        self.longest_hold = self.longest_hold.max(hold);
        self.total_hold = self.total_hold.saturating_add(hold);
    }
}

/// Scans a namespace other threads share for content conflicts, and only
/// when changes another replica made were merged since the last scan
/// ([`Namespace::merge_generation`]): concurrent edits arrive no other way.
///
/// A pass with nothing merged reads that generation under one short hold
/// of the namespace lock and examines no node. A scan examines every node,
/// in steps of [`SCAN_STEP_NODES`] with the lock released between them, so
/// a FUSE request waits for one step, never for the walk.
#[derive(Debug, Default)]
pub struct NamespaceScan {
    /// The merge generation the last scan began at; `None` scans.
    scanned: Option<u64>,
    /// Scans so far.
    scans: u64,
    last: ConflictPassStats,
}

impl NamespaceScan {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The conflict copies still to create: a [`detect`] in steps if
    /// changes were merged since the last scan (or [`Self::rescan`] asked
    /// for one), otherwise none.
    pub fn due(&mut self, namespace: &Mutex<Namespace>) -> Vec<ContentConflict> {
        let mut stats = ConflictPassStats::default();
        let generation = {
            let ns = namespace.lock_recover();
            let held = Instant::now();
            let generation = ns.merge_generation();
            drop(ns);
            stats.held(held.elapsed());
            generation
        };
        if self.scanned == Some(generation) {
            self.last = stats;
            return Vec::new();
        }
        // A merge from here on moves the generation past this one, so the
        // next pass scans again.
        self.scanned = Some(generation);
        let mut found = Found::default();
        let mut after: Option<String> = None;
        loop {
            let ns = namespace.lock_recover();
            let held = Instant::now();
            #[cfg(test)]
            if after.is_none() {
                ns.note_conflict_scan();
            }
            let ids = ns.node_ids_after(after.as_deref(), SCAN_STEP_NODES);
            for id in &ids {
                found.visit(&ns, id);
            }
            drop(ns);
            stats.held(held.elapsed());
            stats.nodes = stats.nodes.saturating_add(ids.len());
            if ids.len() < SCAN_STEP_NODES {
                break;
            }
            after = ids.into_iter().last();
        }
        self.scans = self.scans.saturating_add(1);
        self.last = stats;
        found.into_conflicts()
    }

    /// Scan on the next [`Self::due`] whatever was merged: a periodic
    /// sweep, or a conflict that could not be materialised yet.
    pub fn rescan(&mut self) {
        self.scanned = None;
    }

    /// The work of the last [`Self::due`].
    #[must_use]
    pub fn last_pass(&self) -> ConflictPassStats {
        self.last
    }

    /// Scans so far.
    #[must_use]
    pub fn scans(&self) -> u64 {
        self.scans
    }
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
            let revision_a = fs_protocol::ContentRevisionV1 {
                schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                record_id,
                version_id: fs_protocol::FileVersionId::new(),
                content_hash: Some(fs_protocol::ContentHash([10; 32])),
                byte_len: 10,
                metadata_hash: fs_protocol::ContentHash([11; 32]),
            };
            let revision_b = fs_protocol::ContentRevisionV1 {
                schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                record_id,
                version_id: fs_protocol::FileVersionId::new(),
                content_hash: Some(fs_protocol::ContentHash([20; 32])),
                byte_len: 20,
                metadata_hash: fs_protocol::ContentHash([21; 32]),
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

    fn summary(conflicts: &[ContentConflict]) -> Vec<(String, FileNameV1, Hash)> {
        conflicts
            .iter()
            .map(|c| (c.parent_id.clone(), c.name.clone(), c.loser))
            .collect()
    }

    /// #3985: the reconciliation loop asks for conflicts every pass.
    /// With nothing merged it examines no node and holds the lock once; a
    /// scan holds it a step at a time; a concurrent edit that arrives is
    /// found on the next pass.
    #[test]
    fn scan_examines_nodes_only_after_a_merge_and_a_step_per_lock_hold() {
        let mut a = Namespace::new("A").unwrap();
        for d in 0..10 {
            let dir = a.create_dir(ROOT_INO, format!("d{d}"), 1).unwrap();
            for f in 0..100 {
                a.create_file(dir, format!("f{f}"), Some(&h(1)), 1, 1)
                    .unwrap();
            }
        }
        let doc = a.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
        let mut b = Namespace::from_bytes("B", &a.save()).unwrap();
        let nodes = a.node_ids_after(None, usize::MAX).len();
        assert!(nodes > 1_000);
        let a = Mutex::new(a);
        let mut scan = NamespaceScan::new();

        // The first pass scans every node, a step per lock hold.
        assert!(scan.due(&a).is_empty());
        let first = scan.last_pass();
        assert_eq!(scan.scans(), 1);
        assert_eq!(first.nodes, nodes);
        // The generation, then each step; the last step comes up short.
        assert_eq!(first.lock_holds, 2 + nodes / SCAN_STEP_NODES);

        // Idle passes, and a local edit, examine nothing.
        a.lock_recover().set_content(doc, Some(&h(10)), 10, 5);
        for _ in 0..3 {
            assert!(scan.due(&a).is_empty());
            let idle = scan.last_pass();
            assert_eq!((idle.nodes, idle.lock_holds), (0, 1));
        }
        assert_eq!(scan.scans(), 1);

        // B edited `doc` meanwhile; its edit arrives.
        let bino = b.lookup(ROOT_INO, "doc").unwrap().ino.0;
        b.set_content(bino, Some(&h(20)), 20, 5);
        a.lock_recover().merge_doc(&mut b).unwrap();
        let due = scan.due(&a);
        assert_eq!(scan.scans(), 2);
        assert_eq!(due.len(), 1);
        assert_eq!(summary(&due), summary(&detect(&a.lock_recover())));

        // Not materialised yet: the loop asks for another scan.
        scan.rescan();
        assert_eq!(summary(&scan.due(&a)), summary(&due));
        assert_eq!(scan.scans(), 3);
        assert!(!materialize(&mut a.lock_recover(), &due[0], 10, 6).is_empty());
        assert!(scan.due(&a).is_empty());
        assert_eq!(scan.scans(), 3, "a materialised copy is no merge");
        assert!(detect(&a.lock_recover()).is_empty(), "exactly one copy");
    }

    /// A scan in steps, with the lock released between them, finds what
    /// one walk under one lock finds: both cases, in the same order.
    #[test]
    fn stepwise_scan_finds_what_one_walk_finds() {
        let mut a = Namespace::new("A").unwrap();
        for f in 0..3 * SCAN_STEP_NODES {
            a.create_file(ROOT_INO, format!("f{f:04}"), Some(&h(1)), 1, 1)
                .unwrap();
        }
        let mut b = Namespace::from_bytes("B", &a.save()).unwrap();
        let ino = |ns: &mut Namespace, name: &str| ns.lookup(ROOT_INO, name).unwrap().ino.0;
        // An edit/edit conflict and a modify/delete race.
        let (edited, deleted) = ("f0003", "f0150");
        let (a_edited, b_edited) = (ino(&mut a, edited), ino(&mut b, edited));
        let (a_deleted, b_deleted) = (ino(&mut a, deleted), ino(&mut b, deleted));
        a.set_content(a_edited, Some(&h(10)), 10, 5);
        b.set_content(b_edited, Some(&h(20)), 20, 5);
        a.remove(a_deleted);
        b.set_content(b_deleted, Some(&h(9)), 9, 5);
        a.merge_doc(&mut b).unwrap();
        a.resolve();

        let expected = detect(&a);
        assert_eq!(expected.len(), 2);
        let a = Mutex::new(a);
        let mut scan = NamespaceScan::new();
        assert_eq!(summary(&scan.due(&a)), summary(&expected));
        assert!(scan.last_pass().lock_holds > 3);
    }
}
