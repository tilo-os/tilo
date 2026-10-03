use super::*;
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
fn limits(collections: usize) -> Limits {
    Limits {
        collections,
        json: JsonLimits {
            allocation_bytes: 128 * 1024,
            collection_entries: 65536,
        },
    }
}
#[test]
fn bounded_private_policy_matches_governance_revision_and_expiries() {
    let peer = fixture();
    let certificates = peer.certificates();
    let revocations = peer.revocations();
    let result = native(
        &LegacyRead(peer.admitted_room().unwrap()),
        Metadata {
            room: peer.room(),
            authority: peer.authority(),
            certificates: certificates.iter(),
            revocations: revocations.iter(),
        },
        limits(4 * 1024 * 1024),
    )
    .unwrap();
    assert_eq!(
        result.revision(),
        crate::owner_commit::governance_revision(&peer).unwrap()
    );
    assert_eq!(result.next_expiry_after(3), Some(7));
    assert_eq!(result.next_expiry_after(7), Some(8));
    assert_eq!(result.next_expiry_after(8), None);
}

mod parity;
#[test]
fn bounded_private_policy_refuses_small_collection_limits() {
    let peer = fixture();
    let certificates = peer.certificates();
    let revocations = peer.revocations();
    let run = |collections| {
        native(
            &LegacyRead(peer.admitted_room().unwrap()),
            Metadata {
                room: peer.room(),
                authority: peer.authority(),
                certificates: certificates.iter(),
                revocations: revocations.iter(),
            },
            limits(collections),
        )
    };
    assert!(matches!(run(0), Err(Error::Capacity)));
    assert!(matches!(run(1), Err(Error::Capacity)));
    assert!(run(4 * 1024 * 1024).is_ok());
}
