use super::*;
use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Role, Subject, TreePath};
fn editor_grant(workspace: &str) -> Grant {
    Grant::new(
        "editor-reads",
        Subject::Role(Role::Editor),
        TreePath::room(workspace),
        KindSet::READ,
        Answer::Allow,
        Subject::Role(Role::Owner),
    )
}
fn fixture() -> crate::StatePeer {
    let (mut peer, _, _, _, _) = crate::admission_tests::seeded();
    let mut workspace = peer.admitted_workspace().unwrap().clone();
    let caps = workspace.get(ROOT, "grants").unwrap().unwrap().1;
    for (index, at) in [8_u64, 7, 7, 3].into_iter().enumerate() {
        let grant = editor_grant(peer.workspace_id())
            .with_lifetime(Lifetime::Until(at.saturating_mul(1000)));
        workspace
            .put(
                &caps,
                index.to_string(),
                ScalarValue::Bytes(serde_json::to_vec(&grant).unwrap()),
            )
            .unwrap();
    }
    workspace.commit();
    // Genuine admitted legacy source; this prerequisite tests reader mechanics.
    let (_, key, _, _, now) = crate::admission_tests::seeded();
    let changes = workspace
        .get_changes(&[])
        .iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    peer.receive_from(
        crate::TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
        crate::NeighbourMsg::SyncResp {
            room: peer.workspace_id().into(),
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
        &LegacyRead(peer.admitted_workspace().unwrap()),
        Metadata {
            workspace: peer.workspace_id(),
            owner: &peer.owner(),
            certificates: certificates.iter(),
            revocations: revocations.iter(),
        },
    )
    .unwrap();
    assert_eq!(
        result.revision,
        crate::legacy_policy::governance_revision(&peer).unwrap()
    );
    assert_eq!(result.expiries, vec![3, 7, 8]);
}
mod parity;
