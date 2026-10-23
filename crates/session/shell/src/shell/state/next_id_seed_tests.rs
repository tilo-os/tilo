//! [`WorkspaceTree::seed_next_id_from_self`] must advance the
//! counter past a loaded tree's highest id. The counter is
//! process-wide, so tests observe `>= max+1`, not an exact value.
use super::*;

/// Restores the process-wide peer salt at scope-exit, unconditionally
/// (RAII), so a test that flips it doesn't leak into others.
struct SaltGuard(u16);
impl SaltGuard {
    fn new() -> Self {
        Self(NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed))
    }
}
impl Drop for SaltGuard {
    fn drop(&mut self) {
        NEXT_ID_PEER_SALT.store(self.0, AtomicOrdering::Relaxed);
    }
}

#[test]
#[serial_test::serial(next_id_statics)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn next_ids_are_seeded_above_use_and_partitioned_by_peer_salt() {
    // Phase `next_id_is_seeded_above_every_used_id`.
    {
        {
            let _guard = SaltGuard::new();
            NEXT_ID_PEER_SALT.store(0, AtomicOrdering::Relaxed);
            let mut tree = WorkspaceTree::default();
            tree.desktop
                .workspaces
                .push(Workspace::new(WorkspaceId::from(1), "First"));
            tree.desktop
                .workspaces
                .push(Workspace::new(WorkspaceId::from(50), "Fiftieth"));

            tree.seed_next_id_from_self();

            // Mint a fresh id — it must be strictly greater than every
            // persisted id, including the gap-jumping `50`.
            let allocated = next_id();
            assert!(
                allocated > 50,
                "after seeding, next_id() must skip past every persisted \
                 id (max was 50); got {allocated} which would collide with the \
                 existing tree (D-9 regression)"
            );
        }

        // Phase `seed_next_id_from_self_respects_peer_salt`.
        // `seed_next_id_from_self`: the counter advances based ONLY on ids
        // minted under the local salt — a remote peer's ids live in a
        // disjoint range and must not drag it forward.
        {
            let _guard = SaltGuard::new();
            NEXT_ID_PEER_SALT.store(0x1111, AtomicOrdering::Relaxed);
            NEXT_ID.store(1, AtomicOrdering::Relaxed);

            // A tree containing one id under the local salt (counter=5)
            // and a much larger id under a remote salt (counter=1000).
            let local_id_raw = ((0x1111u64) << PEER_ID_SALT_SHIFT) | 5;
            let remote_id_raw = ((0x2222u64) << PEER_ID_SALT_SHIFT) | 0x3E8;
            let mut tree = WorkspaceTree::default();
            tree.desktop
                .workspaces
                .push(Workspace::new(WorkspaceId::from(local_id_raw), "Local"));
            tree.desktop
                .workspaces
                .push(Workspace::new(WorkspaceId::from(remote_id_raw), "Remote"));

            tree.seed_next_id_from_self();

            // The counter must have advanced to 6 (= local max + 1), NOT
            // to 1001 — remote ids do not consume our counter space.
            let next = NEXT_ID.load(AtomicOrdering::Relaxed);
            assert_eq!(
                next, 6,
                "seed_next_id_from_self must only consider ids under the \
                 local salt; got counter={next} but the only local-salt id had \
                 counter=5 (remote-salt id counter=1000 is in a disjoint \
                 range and must be ignored)"
            );
        }
    }

    // Phase `peer_salt_partitions_ids_so_independent_peers_cannot_collide`.
    {
        // Phase `peer_salt_zero_default_keeps_old_ids`.
        // A salt of `0` reproduces the exact id shape every fixture that
        // expects `WorkspaceId(1)` to round-trip depends on.
        {
            let _guard = SaltGuard::new();
            // Reset both the salt and the counter so a parallel test that
            // bumped them doesn't leak.
            NEXT_ID_PEER_SALT.store(0, AtomicOrdering::Relaxed);
            NEXT_ID.store(1, AtomicOrdering::Relaxed);

            let id = next_id();
            assert_eq!(
                id, 1,
                "salt-0 default must mint the bare counter; got 0x{id:016x}"
            );
            assert_eq!(peer_salt_of(id), 0);
            assert_eq!(counter_of(id), 1);
        }

        // Phase `peer_salt_high_bits_are_the_salt`.
        // After `set_peer_salt(0xABCD)`, the top 16 bits of every minted
        // id must equal `0xABCD` — sanity check on the bit-packing.
        {
            let _guard = SaltGuard::new();
            let mut tree = WorkspaceTree::default();
            tree.set_peer_salt(0xABCD);

            // Reset the counter so the assertion below sees a small value
            // it can compare cleanly.
            NEXT_ID.store(1, AtomicOrdering::Relaxed);
            let id = next_id();

            assert_eq!(
                peer_salt_of(id),
                0xABCD,
                "set_peer_salt(0xABCD) must stamp 0xABCD into the high 16 \
                 bits of next_id(); got id=0x{:016x} (salt=0x{:04x})",
                id,
                peer_salt_of(id)
            );
            // Counter portion must remain monotonic and small.
            assert_eq!(counter_of(id), 1);
        }

        // Phase `two_peers_with_independent_next_id_collide_under_sync`.
        // Two peers run independent `next_id` counters in parallel; their
        // new workspaces carry their rooms' ids, so CRDT sync surfaces
        // both workspaces instead of folding them into one row.
        // They share a base doc before appending, so only the minted id
        // differs between the two `append` calls.
        {
            let _guard = SaltGuard::new();

            NEXT_ID_PEER_SALT.store(0x1111, AtomicOrdering::Relaxed);
            NEXT_ID.store(1, AtomicOrdering::Relaxed);
            let mut tree_a = WorkspaceTree::default();
            tree_a.seed_doc_from_cache();
            let base_changes = tree_a.export_changes_since(&[]);
            let mut tree_b = WorkspaceTree::default();
            tree_b.apply_changes(&base_changes).expect("seed B from A");
            // Snapshot the heads after the base sync so the post-fork
            // export only ships the *incremental* appends.
            let base_heads = tree_a.current_heads();

            NEXT_ID_PEER_SALT.store(0x2222, AtomicOrdering::Relaxed);
            NEXT_ID.store(1, AtomicOrdering::Relaxed);
            let id_b = tree_b.append_empty_workspace("PeerB");

            // Peer A appends concurrently under its own salt. Without the
            // salt, both peers would mint id `1` here and collide.
            NEXT_ID_PEER_SALT.store(0x1111, AtomicOrdering::Relaxed);
            NEXT_ID.store(1, AtomicOrdering::Relaxed);
            let id_a = tree_a.append_empty_workspace("PeerA");

            assert_ne!(
                id_a,
                id_b,
                "two peers with distinct salts must mint distinct ids; \
                 got A=0x{:016x} B=0x{:016x} (C-1 regression — pre-patch \
                 both peers would have minted id 1)",
                id_a.raw(),
                id_b.raw()
            );
            // A new workspace's id is its room's own, so two peers'
            // workspaces never share one, whatever their counters.
            assert_eq!(Some(id_a), tree_a.room_of(id_a).map(WorkspaceId::of_room));
            assert_eq!(Some(id_b), tree_b.room_of(id_b).map(WorkspaceId::of_room));

            // Sync the incremental appends in both directions and rebuild
            // each peer's cache from the merged doc.
            let changes_a = tree_a.export_changes_since(&base_heads);
            let changes_b = tree_b.export_changes_since(&base_heads);
            tree_b.apply_changes(&changes_a).expect("apply A→B");
            tree_a.apply_changes(&changes_b).expect("apply B→A");

            // Both peers must observe BOTH workspaces — no collision-fold.
            // A shared id would cause `rebuild_cache_from_doc` to merge the
            // two distinct rows into one (strips collide, names overwrite,
            // grants merge).
            for (label, tree) in [("A", &tree_a), ("B", &tree_b)] {
                let has_a = tree.desktop.workspaces.iter().any(|w| w.id == id_a);
                let has_b = tree.desktop.workspaces.iter().any(|w| w.id == id_b);
                assert!(
                    has_a && has_b,
                    "peer {label} must see both workspaces after sync; \
                     has_a={has_a} has_b={has_b}, workspaces={:?}",
                    tree.desktop
                        .workspaces
                        .iter()
                        .map(|w| (w.id.raw(), &w.name))
                        .collect::<Vec<_>>()
                );
            }
        }
    }
}
