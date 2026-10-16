use super::*;
use crate::join::{sign_membership_proof, verify_membership_proof};
use crate::state::now_ns;
use ed25519_dalek::SigningKey;
use tilo_user_identity::RevocationList;

fn proof(
    peer: &StatePeer,
    cert: &DeviceCert,
    key: &SigningKey,
    now: SystemTime,
) -> crate::join::VerifiedMembership {
    let challenge = peer
        .governance()
        .membership_challenge(
            7,
            cert,
            [10; 32],
            [11; 32],
            now_ns(now).checked_add(1_000_000).unwrap(),
            now,
        )
        .unwrap();
    let proof = sign_membership_proof(challenge.clone(), key).unwrap();
    verify_membership_proof(
        &proof,
        &challenge,
        cert,
        &RevocationList::new(),
        now_ns(now),
    )
    .unwrap()
}

#[test]
fn the_structural_owner_does_not_use_collaborator_refresh() {
    let (peer, _, key, cert, now) = crate::admission_tests::seeded();
    let membership = proof(&peer, &cert, &key, now);
    assert_eq!(
        peer.permissions(cert.device_public_key, now).unwrap().role,
        Role::Owner
    );
    assert_eq!(
        peer.governance()
            .verify_member_refresh(&membership, now)
            .unwrap_err(),
        "structural owner does not use collaborator refresh"
    );
}

#[test]
fn all_collaborator_roles_remain_refreshable_until_removed() {
    use crate::join::*;
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    let root = SigningKey::from_bytes(&[41; 32]);
    let key = SigningKey::from_bytes(&[42; 32]);
    let mut cert = peer.authority().clone();
    cert.user_public_key = root.verifying_key().to_bytes();
    cert.device_public_key = key.verifying_key().to_bytes();
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    let ticket = issue_invitation(
        InviteGrant {
            workspace_id: 7,
            room: peer.room().into(),
            authority: peer.authority().clone(),
            issuer: peer.authority().clone(),
            delegation: None,
            role: MemberRole::Editor,
            nonce: [43; 16],
            recipient_user_public_key: Some(cert.user_public_key),
            invitee_name: None,
            expires_at_ns: now_ns(now) + 1_000_000,
            transport: vec![],
        },
        &host_key,
    )
    .unwrap();
    let join = decode_and_verify_invitation(
        &ticket,
        Some(cert.user_public_key),
        &RevocationList::new(),
        now_ns(now),
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &join,
        &cert,
        [44; 32],
        [45; 32],
        now_ns(now) + 1_000_000,
        now_ns(now),
    )
    .unwrap();
    let signed = sign_join_proof(challenge.clone(), &key).unwrap();
    let redemption = verify_join_proof(
        &signed,
        &challenge,
        &cert,
        &RevocationList::new(),
        now_ns(now),
    )
    .unwrap();
    peer.governance_mut()
        .admit_invited_member(&redemption, &host_key, now)
        .unwrap();
    for role in [
        MemberRole::Viewer,
        MemberRole::Commenter,
        MemberRole::Editor,
    ] {
        peer.governance_mut()
            .set_member_role(cert.user_public_key, role, &host_key, now)
            .unwrap();
        let verified = proof(&peer, &cert, &key, now);
        assert_eq!(
            peer.governance()
                .verify_member_refresh(&verified, now)
                .unwrap(),
            role
        );
    }
    let verified = proof(&peer, &cert, &key, now);
    peer.governance_mut()
        .remove_member(cert.user_public_key, &host_key, now)
        .unwrap();
    assert_eq!(
        peer.governance()
            .verify_member_refresh(&verified, now)
            .unwrap_err(),
        "member has been removed"
    );
}
