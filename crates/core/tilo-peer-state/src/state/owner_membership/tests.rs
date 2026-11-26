use super::*;
use crate::NeighbourMsg;
use crate::TransportOrigin;
use crate::join::WorkspaceId;
use crate::join::{sign_membership_proof, verify_membership_proof};
use crate::state::now_ns;
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCert, RevocationList};

fn proof(
    peer: &StatePeer,
    cert: &DeviceCert,
    key: &SigningKey,
    now: SystemTime,
) -> crate::join::VerifiedMembership {
    proof_through(peer, &peer.owner_certificates()[0], cert, key, now)
}

/// `cert`'s device proving its membership of `peer`'s workspace, challenged
/// by `host`, the key holder it asked.
fn proof_through(
    peer: &StatePeer,
    host: &DeviceCert,
    cert: &DeviceCert,
    key: &SigningKey,
    now: SystemTime,
) -> crate::join::VerifiedMembership {
    let challenge = peer
        .governance()
        .membership_challenge(
            WorkspaceId::parse(peer.workspace_id()).unwrap(),
            host,
            cert,
            [10; 32],
            [11; 32],
            now_ns(now).checked_add(1_000_000).unwrap(),
            now,
        )
        .unwrap();
    let proof = collab_types::signer::answered_now(sign_membership_proof(challenge.clone(), key))
        .unwrap()
        .unwrap();
    verify_membership_proof(
        &proof,
        &challenge,
        cert,
        &RevocationList::new(),
        now_ns(now),
    )
    .unwrap()
}

/// `key`'s certificate from `root`, a device the person holds themself.
fn certified(root: &SigningKey, key: &SigningKey, _: &str) -> DeviceCert {
    let cert = crate::fixture::cert(
        &root,
        &key,
        tilo_user_identity::DeviceCapabilities::member(),
    );
    cert
}

/// An editor the owner invited, their first desktop, and a second device of
/// theirs the first admitted: a member's further device, and a key holder
/// that is no device of the workspace's owner.
fn member_with_a_further_device(
    peer: &mut StatePeer,
    owner_key: &SigningKey,
    now: SystemTime,
) -> (DeviceCert, SigningKey, DeviceCert, SigningKey) {
    let (editor, _) = crate::admission_tests::add_editor(peer, owner_key, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);
    let root = SigningKey::from_bytes(&[5; 32]);
    let key = SigningKey::from_bytes(&[7; 32]);
    let further = certified(&root, &key, "second desktop");
    peer.governance_mut()
        .admit_member_device(&further, &key, now)
        .unwrap();
    (editor, editor_key, further, key)
}

tilo_test_env::scenario! {
    fn a_member_s_further_device_is_answered_by_any_key_holder;

    phase a_device_admitted_through_a_key_holder_that_is_not_the_owner_is_answered_its_role;
    // A member's further device asks a key holder, which need not be a
    // device of the workspace's owner: its own person's root certified it,
    // and its person is a member.
    {
        let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
        let (editor, _, further, key) =
            member_with_a_further_device(&mut peer, &owner_key, now);
        let membership = proof_through(&peer, &editor, &further, &key, now);
        assert_eq!(
            peer.governance().member_role(&membership, now).unwrap(),
            Role::Editor
        );
    }

    phase every_receiver_answers_the_same_role;
    // The answer comes from the membership, which every key holder holds
    // alike, never from the device that answered: the owner's own device and
    // a replica restored from the same ops decide the same.
    {
        let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
        let (editor, _, further, key) =
            member_with_a_further_device(&mut peer, &owner_key, now);
        let through_member = proof_through(&peer, &editor, &further, &key, now);
        let through_owner = proof(&peer, &further, &key, now);
        let role = peer.governance().member_role(&through_owner, now).unwrap();
        assert_eq!(role, Role::Editor);
        assert_eq!(
            peer.governance().member_role(&through_member, now).unwrap(),
            role
        );
        let elsewhere = StatePeer::restore(
            &peer.durable().unwrap(),
            peer.workspace_id(),
            peer.owner_certificates()[0].clone(),
            vec![],
            RevocationList::new(),
            now,
        )
        .unwrap();
        assert_eq!(
            elsewhere
                .governance()
                .member_role(&through_member, now)
                .unwrap(),
            role
        );
    }

    phase a_stranger_s_device_is_refused;
    // A device its person's root certified is nobody's device here when its
    // person is no member.
    {
        let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
        let (editor, _) = crate::admission_tests::add_editor(&mut peer, &owner_key, now);
        let stranger_key = SigningKey::from_bytes(&[10; 32]);
        let stranger = certified(&SigningKey::from_bytes(&[9; 32]), &stranger_key, "stranger");
        let refused = peer
            .governance_mut()
            .admit_member_device(&stranger, &stranger_key, now)
            .unwrap_err();
        assert!(
            refused.contains("not a member"),
            "a stranger's device is refused as one: {refused}"
        );
        assert!(
            peer.governance()
                .membership_challenge(
                    WorkspaceId::parse(peer.workspace_id()).unwrap(),
                    &editor,
                    &stranger,
                    [70; 32],
                    [71; 32],
                    now_ns(now).checked_add(1_000_000).unwrap(),
                    now,
                )
                .is_err(),
            "a stranger is challenged for no membership"
        );
    }

    phase a_revoked_device_is_refused;
    // Its person's root revokes a further device: it is refused everywhere,
    // and an answer given before the revocation reaches a key holder no
    // longer stands at it.
    {
        let (mut peer, owner_key, _, _, now) = crate::admission_tests::seeded();
        let (editor, editor_key, further, key) =
            member_with_a_further_device(&mut peer, &owner_key, now);
        let membership = proof_through(&peer, &editor, &further, &key, now);
        assert!(peer.governance().member_role(&membership, now).is_ok());
        let root = SigningKey::from_bytes(&[5; 32]);
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: key.verifying_key().to_bytes(),
            revoked_at_ns: now_ns(now).saturating_sub(1),
            reason: tilo_user_identity::RevocationReason::Compromised,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        peer.receive_from(
            TransportOrigin::UntrustedPresence(editor_key.verifying_key().to_bytes()),
            NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(editor.clone()),
                revocations: vec![entry],
            },
            now,
        )
        .unwrap();
        assert!(
            peer.governance()
                .membership_challenge(
                    WorkspaceId::parse(peer.workspace_id()).unwrap(),
                    &editor,
                    &further,
                    [72; 32],
                    [73; 32],
                    now_ns(now).checked_add(1_000_000).unwrap(),
                    now,
                )
                .is_err(),
            "a revoked device is challenged for no membership"
        );
        assert!(
            peer.governance().member_role(&membership, now).is_err(),
            "an answer given before the revocation does not stand"
        );
    }
}

tilo_test_env::scenario! {
    fn the_owner_and_every_member_role_are_answered_until_removed;

    phase the_owner_s_own_device_is_answered_the_strongest_member_role;
    // The owner's own device is answered as the workspace's owner, which a
    // member's answer cannot say: it names the strongest member role, and
    // its authority stays governance's to decide.
    {
        let (peer, _, key, cert, now) = crate::admission_tests::seeded();
        let membership = proof(&peer, &cert, &key, now);
        assert_eq!(
            peer.role(&cert.device_public_key, now).unwrap(),
            Role::Owner
        );
        assert_eq!(
            peer.governance().member_role(&membership, now).unwrap(),
            crate::join::Role::Moderator
        );
    }

    phase all_member_roles_remain_refreshable_until_removed;
    {
        use crate::join::*;
        let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
        let root = SigningKey::from_bytes(&[41; 32]);
        let key = SigningKey::from_bytes(&[42; 32]);
        let mut cert = peer.owner_certificates()[0].clone();
        cert.user_public_key = root.verifying_key().to_bytes();
        cert.device_public_key = key.verifying_key().to_bytes();
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        let ticket = crate::fixture::invitation(&peer, &host_key, now, |grant| {
            grant.nonce = [43; 16];
            grant.recipient_user_public_key = Some(cert.user_public_key);
            grant.expires_at_ns = now_ns(now) + 1_000_000;
        });
        let redemption = crate::fixture::redeem(&ticket, (&key, &cert), 44, now);
        peer.governance_mut()
            .admit_invited_member(&redemption, &key, now)
            .unwrap();
        for role in [Role::Viewer, Role::Commenter, Role::Editor] {
            peer.governance_mut()
                .set_member_role(cert.user_public_key, role, &host_key, now)
                .unwrap();
            let verified = proof(&peer, &cert, &key, now);
            assert_eq!(peer.governance().member_role(&verified, now).unwrap(), role);
        }
        let verified = proof(&peer, &cert, &key, now);
        peer.governance_mut()
            .remove_member(cert.user_public_key, &host_key, now)
            .unwrap();
        assert_eq!(
            peer.governance().member_role(&verified, now).unwrap_err(),
            "member has been removed"
        );
    }
}
