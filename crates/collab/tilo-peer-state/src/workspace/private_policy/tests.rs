use super::*;
use tilo_action_policy::{Answer, Grant, Grantee, KindSet, Lifetime, Role, TreePath};
fn editor_grant(workspace: &str) -> Grant {
    Grant::new(
        "editor-reads",
        Grantee::role(Role::Editor),
        TreePath::workspace(workspace),
        KindSet::READ,
        Answer::Allow,
        Grantee::role(Role::Owner),
    )
}
fn fixture() -> crate::StatePeer {
    let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
    let mut workspace = peer.admitted_workspace().unwrap().clone();
    let owner = peer
        .certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == key.verifying_key().to_bytes())
        .unwrap();
    let changes = [8_u64, 7, 7, 3]
        .into_iter()
        .enumerate()
        .map(|(index, at)| {
            let mut grant = editor_grant(peer.workspace_id())
                .with_lifetime(Lifetime::Until(at.saturating_mul(1000)));
            grant.id = tilo_action_policy::GrantId::new(format!("editor-reads-{index}"));
            tilo_action_policy::Change::Grant(grant)
        })
        .collect();
    let before = workspace.get_heads();
    crate::members::op::tests_support::sign_into(
        &mut workspace,
        &(key.clone(), owner),
        crate::state::now_ns(now),
        changes,
    );
    workspace.commit();
    let changes = workspace
        .get_changes(&before)
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
