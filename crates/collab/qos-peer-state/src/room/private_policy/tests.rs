use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use automerge::resource_limits::{NativeAllocationSet, NativeAllowance};
use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use collab_types::caps::{CapOverride, Expiry, Scope, Subject, Verb};
fn fixture() -> crate::StatePeer {
    let (mut peer, _, _, _, _) = crate::admission_tests::seeded();
    let mut room = peer.admitted_room().unwrap().clone();
    let caps = room.get(ROOT, "capabilities").unwrap().unwrap().1;
    for (index, at) in [8, 7, 7, 3].iter().enumerate() {
        let cap = CapOverride::grant(
            Subject::Role(collab_types::Role::Editor),
            Scope::Workspace,
            Verb::Read,
        )
        .expiring(Expiry::AtUnix(*at));
        room.put(
            &caps,
            index.to_string(),
            ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
        )
        .unwrap();
    }
    room.commit();
    // Genuine admitted legacy source; this prerequisite tests reader mechanics.
    let (_, key, _, _, now) = crate::admission_tests::seeded();
    let changes = room
        .get_changes(&[])
        .iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    peer.receive_from(
        crate::TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
        crate::NeighbourMsg::SyncResp {
            room: peer.room().into(),
            changes,
        },
        now,
    )
    .unwrap();
    peer
}
#[test]
fn shared_private_policy_matches_existing_exact_revision_and_expiry_order() {
    let peer = fixture();
    let certificates = peer.certificates();
    let revocations = peer.revocations();
    let result = legacy(
        &LegacyRead(peer.admitted_room().unwrap()),
        Metadata {
            room: peer.room(),
            authority: peer.authority(),
            certificates: certificates.iter(),
            revocations: revocations.iter(),
        },
    )
    .unwrap();
    assert_eq!(
        result.revision,
        crate::owner_commit::governance_revision(&peer).unwrap()
    );
    assert_eq!(result.expiries, vec![3, 7, 8]);
}
#[test]
fn native_private_policy_pays_actual_current_work_and_retains_result() {
    let peer = fixture();
    let certificates = peer.certificates();
    let revocations = peer.revocations();
    let mut allocations = NativeAllocationSet::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let baseline = current.retained_bytes();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let candidate = peer
        .admitted_room()
        .unwrap()
        .clone_bounded(&mut allocations, &turn)
        .unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let result = assert_precharged(&current, || {
        native(
            &read,
            Metadata {
                room: peer.room(),
                authority: peer.authority(),
                certificates: certificates.iter(),
                revocations: revocations.iter(),
            },
            &turn,
            Limits {
                collections: 4 * 1024 * 1024,
                json: JsonLimits {
                    allocation_bytes: 128 * 1024,
                    collection_entries: 65536,
                },
            },
        )
    })
    .unwrap();
    assert_eq!(
        result.revision(),
        crate::owner_commit::governance_revision(&peer).unwrap()
    );
    assert_eq!(result.next_expiry_after(3), Some(7));
    drop(read);
    drop(candidate);
    drop(turn);
    allocations.reconcile();
    drop(allocations);
    assert_eq!(result.next_expiry_after(7), Some(8));
    let before = current.retained_bytes();
    drop(result);
    assert!(current.retained_bytes() < before);
    assert_eq!(current.retained_bytes(), baseline);
}

mod parity;
#[test]
fn native_private_policy_refuses_small_output_budget_and_sealed_turn() {
    let peer = fixture();
    let certificates = peer.certificates();
    let revocations = peer.revocations();
    let mut allocations = NativeAllocationSet::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let candidate = peer
        .admitted_room()
        .unwrap()
        .clone_bounded(&mut allocations, &turn)
        .unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let run = |limit| {
        native(
            &read,
            Metadata {
                room: peer.room(),
                authority: peer.authority(),
                certificates: certificates.iter(),
                revocations: revocations.iter(),
            },
            &turn,
            Limits {
                collections: limit,
                json: JsonLimits {
                    allocation_bytes: 128 * 1024,
                    collection_entries: 65536,
                },
            },
        )
    };
    let before = current.retained_bytes();
    assert!(matches!(
        assert_precharged(&current, || run(1)),
        Err(Error::Resource(NativeResourceError::WorkBytes))
    ));
    assert_eq!(current.retained_bytes(), before);
    current.finish_turn();
    assert!(matches!(run(0), Err(Error::Resource(_))));
}
