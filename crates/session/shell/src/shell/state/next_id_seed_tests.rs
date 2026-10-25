//! [`WorkspaceTree::seed_next_id_from_self`] must advance the
//! counter past a loaded tree's highest id. The counter is
//! process-wide, so tests observe `>= max+1`, not an exact value. The salt
//! is the tree's: each tree mints in its own namespace.
use super::*;

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
            let allocated = tree.mint_id();
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
            NEXT_ID.store(1, AtomicOrdering::Relaxed);

            // A tree containing one id under the local salt (counter=5)
            // and a much larger id under a remote salt (counter=1000).
            let local_id_raw = ((0x1111u64) << PEER_ID_SALT_SHIFT) | 5;
            let remote_id_raw = ((0x2222u64) << PEER_ID_SALT_SHIFT) | 0x3E8;
            let mut tree = WorkspaceTree::default();
            tree.set_peer_salt(0x1111);
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
            // Reset the counter so a parallel test that bumped it doesn't
            // leak.
            NEXT_ID.store(1, AtomicOrdering::Relaxed);

            let id = WorkspaceTree::default().mint_id();
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
            let mut tree = WorkspaceTree::default();
            tree.set_peer_salt(0xABCD);

            // Reset the counter so the assertion below sees a small value
            // it can compare cleanly.
            NEXT_ID.store(1, AtomicOrdering::Relaxed);
            let id = tree.mint_id();

            assert_eq!(
                peer_salt_of(id),
                0xABCD,
                "set_peer_salt(0xABCD) must stamp 0xABCD into the high 16 \
                 bits of the ids the tree mints; got id=0x{:016x} (salt=0x{:04x})",
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
            NEXT_ID.store(1, AtomicOrdering::Relaxed);
            let mut tree_a = WorkspaceTree::default();
            tree_a.set_peer_salt(0x1111);
            tree_a.seed_doc_from_cache();
            let base_changes = tree_a.export_changes_since(&[]);
            let mut tree_b = WorkspaceTree::default();
            tree_b.set_peer_salt(0x2222);
            tree_b.apply_changes(&base_changes).expect("seed B from A");
            // Snapshot the heads after the base sync so the post-fork
            // export only ships the *incremental* appends.
            let base_heads = tree_a.current_heads();

            NEXT_ID.store(1, AtomicOrdering::Relaxed);
            let id_b = tree_b.create_hosted_workspace("PeerB").0;

            // Peer A appends concurrently under its own salt. Without the
            // salt, both peers would mint id `1` here and collide.
            NEXT_ID.store(1, AtomicOrdering::Relaxed);
            let id_a = tree_a.create_hosted_workspace("PeerA").0;

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

/// Two trees in one process — two simulated devices — each mint in their
/// own namespace, interleaved, with nothing process-wide set: every id a
/// tree makes, its first workspace's strip included, carries that tree's
/// salt, and no id is made twice.
#[test]
#[serial_test::serial(next_id_statics)]
fn two_trees_in_one_process_mint_in_their_own_namespaces() {
    const ADA: u16 = 0x0A0A;
    const GRACE: u16 = 0x0C0C;
    let mut ada = WorkspaceTree::default();
    ada.set_peer_salt(ADA);
    let mut grace = WorkspaceTree::default();
    grace.set_peer_salt(GRACE);

    let mut minted = std::collections::HashSet::new();
    for _ in 0..4 {
        for (tree, salt) in [(&ada, ADA), (&grace, GRACE)] {
            let ids = [
                tree.mint_id(),
                tree.mint_strip_id().raw(),
                tree.mint_workspace_id().raw(),
                tree.ids().mint_id(),
            ];
            for id in ids {
                assert_eq!(
                    peer_salt_of(id),
                    salt,
                    "{id:#018x} is minted by its own tree"
                );
                assert!(minted.insert(id), "{id:#018x} is minted once");
            }
        }
    }

    let (ada_home, _) = ada.create_hosted_workspace("Ada's");
    let (grace_home, _) = grace.create_hosted_workspace("Grace's");
    for (tree, home, salt) in [(&ada, ada_home, ADA), (&grace, grace_home, GRACE)] {
        let workspace = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == home)
            .expect("the workspace is made");
        assert!(!workspace.strips.is_empty(), "a new workspace has a strip");
        for strip in &workspace.strips {
            assert_eq!(
                peer_salt_of(strip.id.raw()),
                salt,
                "a new workspace's strip is in its device's namespace"
            );
        }
    }
}
