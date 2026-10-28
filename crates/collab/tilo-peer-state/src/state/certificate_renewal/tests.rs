//! A member's renewed certificate keeps the device the same member: its
//! role stays, and only its person's root renews it.
use crate::StatePeer;
use crate::admission_tests::seeded;
use crate::join::{
    InviteGrant, JoinChallenge, MemberRole, MembershipChallengeRequest, MembershipCompleteRequest,
    issue_invitation, sign_join_proof, sign_membership_proof,
};
use crate::join_ledger::{Governed, JoinLedger};
use collab_types::Role;
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::{Duration, SystemTime};

const SECOND: u64 = 1_000_000_000;

fn ns(time: SystemTime) -> u64 {
    u64::try_from(
        time.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap()
}

fn cert(root: &SigningKey, device: &SigningKey, issued: u64, expires: Option<u64>) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Editor".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: issued,
        expires_at_ns: expires,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// Admit `member` into `peer` as an Editor, through the governor's
/// invitation, at `at`.
fn admit(
    peer: &mut StatePeer,
    authority: &SigningKey,
    member: &DeviceCert,
    device: &SigningKey,
    at: SystemTime,
) {
    let at_ns = ns(at);
    let owner = peer.authority().clone();
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: 7,
            room: peer.room().into(),
            authority: owner.clone(),
            issuer: owner,
            delegation: None,
            role: MemberRole::Editor,
            nonce: [53; 16],
            recipient_user_public_key: Some(member.user_public_key),
            invitee_name: Some("Editor".into()),
            expires_at_ns: at_ns.saturating_add(10 * SECOND),
            transport: vec![],
        },
        authority,
    )
    .unwrap();
    let join = crate::join::decode_and_verify_invitation(
        &signed,
        Some(member.user_public_key),
        &RevocationList::new(),
        at_ns,
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &join,
        member,
        [54; 32],
        [55; 32],
        at_ns.saturating_add(SECOND),
        at_ns,
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), device).unwrap();
    let redemption =
        crate::join::verify_join_proof(&proof, &challenge, member, &RevocationList::new(), at_ns)
            .unwrap();
    assert!(
        peer.governance_mut()
            .admit_invited_member(&redemption, authority, at)
            .unwrap()
    );
}

/// A room with an Editor whose certificate lasted from 1000s to 1500s, now
/// at 2000s expired; and a renewal of it the person's root issued at 1900s.
struct Fixture {
    peer: StatePeer,
    authority: SigningKey,
    root: SigningKey,
    device: SigningKey,
    held: DeviceCert,
    renewed: DeviceCert,
    now: SystemTime,
}

fn fixture() -> Fixture {
    let (mut peer, authority, _, _, now) = seeded();
    let root = SigningKey::from_bytes(&[5; 32]);
    let device = SigningKey::from_bytes(&[6; 32]);
    let held = cert(&root, &device, 1000 * SECOND, Some(1500 * SECOND));
    admit(
        &mut peer,
        &authority,
        &held,
        &device,
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(1200))
            .unwrap(),
    );
    let renewed = cert(&root, &device, 1900 * SECOND, Some(2900 * SECOND));
    Fixture {
        peer,
        authority,
        root,
        device,
        held,
        renewed,
        now,
    }
}

#[test]
fn a_renewed_certificate_takes_the_place_of_the_expired_one_and_keeps_the_role() {
    let Fixture {
        mut peer,
        authority,
        device,
        held,
        renewed,
        now,
        ..
    } = fixture();
    assert!(
        peer.permissions(held.device_public_key, now).is_err(),
        "an expired certificate acts in nothing"
    );
    assert_eq!(
        peer.governance().renewal_of(&renewed, now).unwrap(),
        Some(held.clone())
    );

    assert!(
        peer.governance_mut()
            .renew_member_certificate(&renewed, &authority, now)
            .unwrap()
    );

    assert!(peer.certificates().contains(&renewed));
    assert!(!peer.certificates().contains(&held));
    assert_eq!(
        peer.permissions(device.verifying_key().to_bytes(), now)
            .unwrap()
            .role,
        Role::Editor,
        "the same member, with the same role"
    );
    assert_eq!(peer.governance().renewal_of(&renewed, now).unwrap(), None);
    assert!(
        !peer
            .governance_mut()
            .renew_member_certificate(&renewed, &authority, now)
            .unwrap(),
        "renewing again changes nothing"
    );
}

#[test]
fn a_certificate_from_another_root_is_not_a_renewal() {
    let Fixture {
        mut peer,
        authority,
        device,
        now,
        ..
    } = fixture();
    let stranger = SigningKey::from_bytes(&[9; 32]);
    let forged = cert(&stranger, &device, 1900 * SECOND, Some(2900 * SECOND));
    assert!(peer.governance().renewal_of(&forged, now).is_err());
    assert!(
        peer.governance_mut()
            .renew_member_certificate(&forged, &authority, now)
            .is_err()
    );
}

#[test]
fn an_older_certificate_is_refused_after_a_newer_one() {
    let Fixture {
        mut peer,
        authority,
        root,
        device,
        held,
        renewed,
        now,
    } = fixture();
    peer.governance_mut()
        .renew_member_certificate(&renewed, &authority, now)
        .unwrap();
    // The certificate it replaced, and one issued between the two.
    let between = cert(&root, &device, 1800 * SECOND, Some(2800 * SECOND));
    for stale in [&held, &between] {
        let refused = peer.governance().renewal_of(stale, now).unwrap_err();
        assert!(refused.contains("newer"), "{refused}");
    }
    assert!(peer.certificates().contains(&renewed));
}

#[test]
fn a_renewal_grants_no_more_than_the_certificate_it_replaces() {
    let Fixture {
        peer,
        root,
        device,
        now,
        ..
    } = fixture();
    let mut wider = cert(&root, &device, 1900 * SECOND, Some(2900 * SECOND));
    wider.capabilities.can_admit_devices = true;
    sign_device_cert(&mut wider, &root).unwrap();
    assert!(peer.governance().renewal_of(&wider, now).is_err());
}

/// The membership exchange is how a member brings its renewal: challenged
/// as the device it renews, it is held in the old one's place before the
/// governor answers, and the answer is the room with the member's role.
#[test]
fn a_member_proving_with_its_renewed_certificate_is_answered_as_the_same_member() {
    let Fixture {
        mut peer,
        authority,
        device,
        renewed,
        now,
        ..
    } = fixture();
    let now_ns = ns(now);
    let messages = || vec![b"governance".to_vec()];
    let mut ledger = JoinLedger::default();
    let challenge = {
        let room = Governed {
            peer: &peer,
            workspace_id: 7,
            generation: 1,
            governance_messages: &messages,
        };
        ledger
            .membership_challenge(
                &room,
                renewed.device_public_key,
                MembershipChallengeRequest {
                    room: peer.room().into(),
                    workspace_id: 7,
                    device_cert: renewed.clone(),
                    request_id: [60; 32],
                },
                [61; 32],
                now_ns,
            )
            .unwrap()
    };
    let request = MembershipCompleteRequest {
        proof: sign_membership_proof(challenge, &device).unwrap(),
    };
    let renewal = {
        let room = Governed {
            peer: &peer,
            workspace_id: 7,
            generation: 1,
            governance_messages: &messages,
        };
        ledger
            .membership_renewal(&room, renewed.device_public_key, &request, now_ns)
            .unwrap()
    };
    assert_eq!(renewal.as_ref(), Some(&renewed));
    peer.governance_mut()
        .renew_member_certificate(&renewed, &authority, now)
        .unwrap();
    let room = Governed {
        peer: &peer,
        workspace_id: 7,
        generation: 2,
        governance_messages: &messages,
    };
    assert_eq!(
        ledger
            .membership_renewal(&room, renewed.device_public_key, &request, now_ns)
            .unwrap(),
        None,
        "the room holds it now"
    );
    let bootstrap = ledger
        .membership_complete(&room, renewed.device_public_key, &request, now_ns)
        .unwrap();
    assert_eq!(bootstrap.device_cert, renewed);
    assert_eq!(bootstrap.role, MemberRole::Editor);
    assert!(bootstrap.certified_roster.contains(&renewed));
}
