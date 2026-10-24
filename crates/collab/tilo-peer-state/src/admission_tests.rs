use super::*;
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::{Signer as _, SigningKey};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::{Duration, SystemTime};
fn fixtures() -> (SigningKey, DeviceCert, SigningKey, DeviceCert, SystemTime) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap();
    let make = |n: u8| {
        let key = SigningKey::from_bytes(&[n; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("device-{n}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        (key, cert)
    };
    let (a, ac) = make(2);
    let (b, bc) = make(3);
    (a, ac, b, bc, now)
}
#[test]
fn unchanged_permission_queries_share_policy_data_across_peer_clones() {
    let (peer, _, _, cert, now) = seeded();
    crate::room::POLICY_READS.with(|count| count.set(0));
    let expected = peer.permissions(cert.device_public_key, now).unwrap();
    for _ in 0..100 {
        let actual = peer
            .clone()
            .permissions(cert.device_public_key, now)
            .unwrap();
        assert_eq!(actual.role, expected.role);
        assert_eq!(
            serde_json::to_vec(&actual.policy).unwrap(),
            serde_json::to_vec(&expected.policy).unwrap()
        );
        assert_eq!(actual.person, expected.person);
    }
    assert!(
        crate::room::POLICY_READS.with(std::cell::Cell::get) <= 1,
        "unchanged policy data is decoded at most once"
    );
}
#[test]
fn admitted_immutable_certificate_does_not_repeat_signature_checks_per_permission_query() {
    let (peer, _, _, cert, now) = seeded();
    crate::state::CERTIFICATE_VERIFICATIONS.with(|count| count.set(0));
    let expected = peer.permissions(cert.device_public_key, now).unwrap();
    for _ in 0..100 {
        let actual = peer
            .clone()
            .permissions(cert.device_public_key, now)
            .unwrap();
        assert_eq!(actual.role, expected.role);
        assert_eq!(
            serde_json::to_vec(&actual.policy).unwrap(),
            serde_json::to_vec(&expected.policy).unwrap()
        );
        assert_eq!(actual.person, expected.person);
    }
    assert_eq!(
        crate::state::CERTIFICATE_VERIFICATIONS.with(std::cell::Cell::get),
        0,
        "immutable signatures were already verified when the certificate entered the roster"
    );
}
#[test]
fn exported_certificate_mutation_cannot_change_verified_roster_or_replace_it_via_presence() {
    let (mut peer, _, _, cert, now) = seeded();
    let old = peer.clone();
    let mut exported = peer
        .certificates()
        .into_iter()
        .find(|c| c.device_public_key == cert.device_public_key)
        .unwrap();
    exported.signature[0] ^= 1;
    assert!(peer.permissions(cert.device_public_key, now).is_ok());
    assert!(old.permissions(cert.device_public_key, now).is_ok());
    let before = peer.durable().unwrap();
    assert!(
        peer.receive_from(
            TransportOrigin::UntrustedPresence(cert.device_public_key),
            NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(exported),
                revocations: vec![],
                successions: Vec::new()
            },
            now
        )
        .is_err()
    );
    assert_eq!(peer.durable().unwrap(), before);
    assert!(peer.certificates().contains(&cert));
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one cached policy followed through expiry, removal and replacement"
)]
fn cached_policy_rechecks_membership_expiry_and_governance_replacement() {
    use automerge::ReadDoc;
    use tilo_action_policy::{ActionKind, Answer, Grant, KindSet, Lifetime, Subject, TreePath};
    let (key, cert, _, other, now) = fixtures();
    let room_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut doc = crate::room::candidate(
        None,
        &seed_room(room_id, &cert, &[cert.clone(), other.clone()]).unwrap(),
    )
    .unwrap();
    doc.put(ROOT, "owner", "another-person").unwrap();
    let members = doc.get(ROOT, "members").unwrap().unwrap().1;
    let person = hex::encode(cert.user_public_key);
    doc.put(
        &members,
        person.as_str(),
        automerge::ScalarValue::Bytes(serde_json::to_vec(&collab_types::Role::Editor).unwrap()),
    )
    .unwrap();
    // A time-limited hold on undoable changes, rechecked at every decision.
    let grants = doc.get(ROOT, "grants").unwrap().unwrap().1;
    let hold = Grant::new(
        "hold",
        Subject::Person(person.clone()),
        TreePath::room(room_id),
        KindSet::UNDOABLE,
        Answer::Deny,
        Subject::person("another-person"),
    )
    .with_lifetime(Lifetime::Until(2_001_000));
    doc.put(
        &grants,
        "hold",
        automerge::ScalarValue::Bytes(serde_json::to_vec(&hold).unwrap()),
    )
    .unwrap();
    let changes = doc
        .get_changes(&[])
        .into_iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    let mut peer = StatePeer::new(
        room_id,
        cert.clone(),
        vec![cert.clone(), other],
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.install_seed(
        SeedState {
            room_changes: changes,
            documents: vec![],
        },
        &key,
        now,
    )
    .unwrap();
    assert_eq!(
        peer.permissions(cert.device_public_key, now).unwrap().role,
        collab_types::Role::Editor
    );
    let room_node = TreePath::room(room_id);
    assert!(
        !peer
            .permissions(cert.device_public_key, now)
            .unwrap()
            .allows(&room_node, ActionKind::Undoable, now)
    );
    assert!(
        peer.permissions(cert.device_public_key, now + Duration::from_secs(2))
            .unwrap()
            .allows(
                &room_node,
                ActionKind::Undoable,
                now + Duration::from_secs(2)
            )
    );
    let old = peer.clone();
    doc.delete(&members, person.as_str()).unwrap();
    let changes = doc
        .get_changes(&[])
        .into_iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    peer.receive(
        cert.device_public_key,
        NeighbourMsg::SyncResp {
            room: room_id.into(),
            changes,
        },
        now,
    )
    .unwrap();
    assert!(
        peer.permissions(cert.device_public_key, now)
            .unwrap_err()
            .contains("membership")
    );
    assert_eq!(
        old.permissions(cert.device_public_key, now).unwrap().role,
        collab_types::Role::Editor
    );
}
#[test]
fn app_waits_for_governance_then_signed_state_converges_without_reexecuting_action() {
    let (a, ac, _, bc, now) = fixtures();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut native = StatePeer::new(
        room,
        ac.clone(),
        vec![ac.clone(), bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "native").unwrap();
    document.commit_edit("fixture", "value");
    let seed = SeedState {
        room_changes: seed_room(room, &ac, &[ac.clone(), bc.clone()]).unwrap(),
        documents: vec![SeedDocument {
            app_id: "test.app".into(),
            record_id: "tab:1".into(),
            kind: "test".into(),
            bytes: document.save(),
        }],
    };
    native.install_seed(seed, &a, now).unwrap();
    let mut browser =
        StatePeer::new(room, ac.clone(), vec![ac, bc], RevocationList::new(), now).unwrap();
    let app = native
        .publication_mut()
        .signed_snapshots(&a, now)
        .unwrap()
        .remove(0);
    assert!(
        browser
            .receive(a.verifying_key().to_bytes(), app.clone(), now)
            .unwrap()
            .deferred
    );
    assert!(browser.rows().is_empty());
    browser
        .receive(a.verifying_key().to_bytes(), native.room_message(), now)
        .unwrap();
    assert_eq!(browser.rows(), native.rows());
    assert!(
        browser
            .receive(a.verifying_key().to_bytes(), app, now)
            .unwrap()
            .duplicate
    );
    let restored = StatePeer::restore(
        &browser.durable().unwrap(),
        room,
        native.authority().clone(),
        vec![native.authority().clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert_eq!(restored.rows(), browser.rows());
}
#[test]
fn source_room_signature_and_aged_checkpoint_fail_closed() {
    let (a, ac, _, bc, now) = fixtures();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut native = StatePeer::new(
        room,
        ac.clone(),
        vec![ac.clone(), bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", 1).unwrap();
    document.commit_edit("fixture", "value");
    native
        .install_seed(
            SeedState {
                room_changes: seed_room(room, &ac, &[ac.clone(), bc.clone()]).unwrap(),
                documents: vec![SeedDocument {
                    app_id: "test.app".into(),
                    record_id: "tab:1".into(),
                    kind: "test".into(),
                    bytes: document.save(),
                }],
            },
            &a,
            now,
        )
        .unwrap();
    let mut receiver = StatePeer::new(
        room,
        ac.clone(),
        vec![ac, bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    receiver
        .receive(a.verifying_key().to_bytes(), native.room_message(), now)
        .unwrap();
    let message = native
        .publication_mut()
        .signed_snapshots(&a, now)
        .unwrap()
        .remove(0);
    assert!(
        receiver
            .receive(bc.device_public_key, message.clone(), now)
            .is_err()
    );
    assert!(
        receiver
            .receive(
                a.verifying_key().to_bytes(),
                message.clone(),
                now + Duration::from_secs(301)
            )
            .is_err()
    );
    let mut forged = message;
    if let NeighbourMsg::AppDocument { cluster_id, .. } = &mut forged {
        *cluster_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into();
    }
    assert!(
        receiver
            .receive(a.verifying_key().to_bytes(), forged, now)
            .is_err()
    );
    assert!(receiver.rows().is_empty());
}
/// [`seeded`], with the room owned by another person and the fixture's
/// person an Editor of it: a member whose grants a governor can narrow.
pub(crate) fn seeded_editor() -> (StatePeer, SigningKey, SigningKey, DeviceCert, SystemTime) {
    let (a, ac, b, bc, now) = fixtures();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut doc = crate::room::candidate(
        None,
        &seed_room(room, &ac, &[ac.clone(), bc.clone()]).unwrap(),
    )
    .unwrap();
    doc.put(ROOT, "owner", "another-person").unwrap();
    let members = doc.get(ROOT, "members").unwrap().unwrap().1;
    doc.put(
        &members,
        hex::encode(bc.user_public_key).as_str(),
        automerge::ScalarValue::Bytes(serde_json::to_vec(&collab_types::Role::Editor).unwrap()),
    )
    .unwrap();
    let room_changes = doc
        .get_changes(&[])
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    let mut native = StatePeer::new(
        room,
        ac.clone(),
        vec![ac, bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    native
        .install_seed(
            SeedState {
                room_changes,
                documents: vec![],
            },
            &a,
            now,
        )
        .unwrap();
    (native, a, b, bc, now)
}

/// Invite a second person into `peer` as an Editor with one device, as the
/// governor `authority`: their device certificate and root.
pub(crate) fn add_editor(
    peer: &mut StatePeer,
    authority: &SigningKey,
    now: SystemTime,
) -> (DeviceCert, [u8; 32]) {
    use crate::join::{InviteGrant, JoinChallenge, MemberRole, issue_invitation, sign_join_proof};
    let root = SigningKey::from_bytes(&[5; 32]);
    let device = SigningKey::from_bytes(&[6; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Editor".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let now_ns = u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
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
            recipient_user_public_key: Some(cert.user_public_key),
            invitee_name: Some("Editor".into()),
            expires_at_ns: now_ns.checked_add(10_000_000_000).unwrap(),
            transport: vec![],
        },
        authority,
    )
    .unwrap();
    let join = crate::join::decode_and_verify_invitation(
        &signed,
        Some(cert.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &join,
        &cert,
        [54; 32],
        [55; 32],
        now_ns.checked_add(1_000_000_000).unwrap(),
        now_ns,
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), &device).unwrap();
    let redemption =
        crate::join::verify_join_proof(&proof, &challenge, &cert, &RevocationList::new(), now_ns)
            .unwrap();
    assert!(
        peer.governance_mut()
            .admit_invited_member(&redemption, authority, now)
            .unwrap()
    );
    (cert, root.verifying_key().to_bytes())
}

pub(crate) fn seeded() -> (StatePeer, SigningKey, SigningKey, DeviceCert, SystemTime) {
    let (a, ac, b, bc, now) = fixtures();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut native = StatePeer::new(
        room,
        ac.clone(),
        vec![ac.clone(), bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "native").unwrap();
    document.commit_edit("fixture", "value");
    native
        .install_seed(
            SeedState {
                room_changes: seed_room(room, &ac, &[ac.clone(), bc.clone()]).unwrap(),
                documents: vec![SeedDocument {
                    app_id: "test.app".into(),
                    record_id: "tab:1".into(),
                    kind: "test".into(),
                    bytes: document.save(),
                }],
            },
            &a,
            now,
        )
        .unwrap();
    (native, a, b, bc, now)
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
fn independent_invite_redemption_is_durable_single_use_and_removal_closes_access() {
    use crate::join::{
        InviteGrant, JoinChallenge, MemberRole, issue_invitation, sign_join_proof,
        verify_join_proof,
    };
    use automerge::ReadDoc;

    let (mut peer, authority_key, other_owner_device, _, now) = seeded();
    let now_ns: u64 = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap();
    let guest_root = SigningKey::from_bytes(&[41; 32]);
    let guest_key = SigningKey::from_bytes(&[42; 32]);
    let mut guest_cert = DeviceCert {
        user_public_key: guest_root.verifying_key().to_bytes(),
        device_public_key: guest_key.verifying_key().to_bytes(),
        device_label: "Independent guest".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut guest_cert, &guest_root).unwrap();
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: 7,
            room: peer.room().into(),
            authority: peer.authority().clone(),
            issuer: peer.authority().clone(),
            delegation: None,
            role: MemberRole::Editor,
            nonce: [43; 16],
            recipient_user_public_key: Some(guest_cert.user_public_key),
            invitee_name: Some("Independent guest".into()),
            expires_at_ns: now_ns + 10_000_000_000,
            transport: vec![],
        },
        &authority_key,
    )
    .unwrap();
    let join = crate::join::decode_and_verify_invitation(
        &signed,
        Some(guest_cert.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &join,
        &guest_cert,
        [44; 32],
        [45; 32],
        now_ns + 1_000_000_000,
        now_ns,
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), &guest_key).unwrap();
    let redemption = verify_join_proof(
        &proof,
        &challenge,
        &guest_cert,
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();

    assert!(
        peer.governance_mut()
            .admit_invited_member(&redemption, &other_owner_device, now)
            .is_err(),
        "only the pinned authority device may mutate governance"
    );
    assert!(
        peer.governance_mut()
            .admit_invited_member(&redemption, &authority_key, now)
            .unwrap()
    );
    assert_eq!(
        peer.permissions(guest_cert.device_public_key, now)
            .unwrap()
            .role,
        collab_types::Role::Editor
    );
    let access = peer.governance().governed_access(now).unwrap();
    assert!(
        access
            .member_devices
            .contains(&guest_cert.device_public_key)
    );
    assert!(
        access
            .member_devices
            .contains(&peer.authority().device_public_key)
    );
    assert!(
        access
            .member_user_roots
            .contains(&guest_cert.user_public_key)
    );
    assert!(
        access
            .member_user_roots
            .contains(&peer.authority().user_public_key)
    );
    assert!(
        !peer
            .governance_mut()
            .admit_invited_member(&redemption, &authority_key, now)
            .unwrap()
    );
    let mut second_grant = redemption.join().grant().clone();
    second_grant.nonce = [46; 16];
    let second_signed = issue_invitation(second_grant, &authority_key).unwrap();
    let second_join = crate::join::decode_and_verify_invitation(
        &second_signed,
        Some(guest_cert.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let second_challenge = JoinChallenge::new(
        &second_join,
        &guest_cert,
        [47; 32],
        [48; 32],
        now_ns + 1_000_000_000,
        now_ns,
    )
    .unwrap();
    let second_proof = sign_join_proof(second_challenge.clone(), &guest_key).unwrap();
    let second_redemption = verify_join_proof(
        &second_proof,
        &second_challenge,
        &guest_cert,
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    assert!(
        peer.governance_mut()
            .admit_invited_member(&second_redemption, &authority_key, now)
            .unwrap()
    );
    assert!(
        !peer
            .governance_mut()
            .admit_invited_member(&redemption, &authority_key, now)
            .unwrap(),
        "a later invite for the same device must not overwrite the first receipt"
    );

    let mut restored = StatePeer::restore(
        &peer.durable().unwrap(),
        peer.room(),
        peer.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(
        !restored
            .governance_mut()
            .admit_invited_member(&redemption, &authority_key, now)
            .unwrap()
    );
    assert!(
        !restored
            .governance_mut()
            .admit_invited_member(&second_redemption, &authority_key, now)
            .unwrap()
    );
    let replay_key = SigningKey::from_bytes(&[54; 32]);
    let mut replay_cert = DeviceCert {
        user_public_key: guest_root.verifying_key().to_bytes(),
        device_public_key: replay_key.verifying_key().to_bytes(),
        device_label: "Replay device".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut replay_cert, &guest_root).unwrap();
    let replay_join = crate::join::decode_and_verify_invitation(
        &signed,
        Some(replay_cert.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let replay_challenge = JoinChallenge::new(
        &replay_join,
        &replay_cert,
        [55; 32],
        [56; 32],
        now_ns + 1_000_000_000,
        now_ns,
    )
    .unwrap();
    let replay_proof = sign_join_proof(replay_challenge.clone(), &replay_key).unwrap();
    let replay_redemption = verify_join_proof(
        &replay_proof,
        &replay_challenge,
        &replay_cert,
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    assert!(
        restored
            .governance_mut()
            .admit_invited_member(&replay_redemption, &authority_key, now)
            .unwrap_err()
            .contains("already redeemed"),
        "invite A remains spent after invite B updates the same original device"
    );
    let NeighbourMsg::SyncResp { changes, room } = restored.room_message() else {
        panic!("unexpected fixture shape")
    };
    let mut room_doc = crate::room::candidate(None, &changes).unwrap();
    let members = room_doc.get(ROOT, "members").unwrap().unwrap().1;
    let before = room_doc.get_heads();
    room_doc.set_actor(automerge::ActorId::from(
        authority_key.verifying_key().to_bytes().to_vec(),
    ));
    room_doc
        .put(
            &members,
            hex::encode(guest_cert.user_public_key),
            automerge::ScalarValue::Bytes(
                serde_json::to_vec(&collab_types::Role::Commenter).unwrap(),
            ),
        )
        .unwrap();
    room_doc.commit_edit("fixture", "fixture");
    restored
        .receive_from(
            TransportOrigin::RelayDelta(authority_key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp {
                changes: room_doc
                    .get_changes(&before)
                    .into_iter()
                    .map(|change| change.raw_bytes().to_vec())
                    .collect(),
                room,
            },
            now,
        )
        .unwrap();
    assert_eq!(
        restored
            .permissions(guest_cert.device_public_key, now)
            .unwrap()
            .role,
        collab_types::Role::Commenter
    );
    assert!(
        restored
            .governance_mut()
            .set_member_role(
                guest_cert.user_public_key,
                MemberRole::Viewer,
                &authority_key,
                now
            )
            .unwrap()
    );
    assert_eq!(
        restored
            .permissions(guest_cert.device_public_key, now)
            .unwrap()
            .role,
        collab_types::Role::Viewer
    );
    restored
        .governance()
        .membership_challenge(
            7,
            &guest_cert,
            [49; 32],
            [50; 32],
            now_ns + 1_000_000_000,
            now,
        )
        .unwrap();
    assert!(
        restored
            .governance_mut()
            .remove_member(guest_cert.user_public_key, &authority_key, now)
            .unwrap()
    );
    assert!(
        restored
            .permissions(guest_cert.device_public_key, now)
            .is_err()
    );
    assert_eq!(
        restored
            .governance()
            .membership_challenge(
                7,
                &guest_cert,
                [51; 32],
                [52; 32],
                now_ns + 1_000_000_000,
                now,
            )
            .unwrap_err(),
        "member has been removed"
    );
    assert!(
        !restored
            .governance_mut()
            .admit_invited_member(&redemption, &authority_key, now)
            .unwrap()
    );
    let access = restored.governance().governed_access(now).unwrap();
    assert!(
        !access
            .member_devices
            .contains(&guest_cert.device_public_key)
    );
    assert!(
        !access
            .member_user_roots
            .contains(&guest_cert.user_public_key)
    );
    assert!(
        restored
            .governance_mut()
            .remove_member(peer.authority().user_public_key, &authority_key, now)
            .is_err(),
        "the owner cannot be removed through member governance"
    );
}

#[test]
fn host_without_device_enrollment_power_needs_explicit_room_delegation() {
    use crate::join::{
        InviteGrant, JoinChallenge, MemberRole, WorkspaceDelegationPermissions,
        decode_and_verify_invitation, issue_invitation, sign_join_proof, sign_workspace_delegation,
        verify_join_proof,
    };
    let (mut peer, authority_key, host_key, _, now) = seeded();
    let now_ns: u64 = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap();
    let host = peer
        .certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == host_key.verifying_key().to_bytes())
        .unwrap();
    assert!(!host.capabilities.can_admit_devices);
    let delegation = sign_workspace_delegation(
        peer.room(),
        9,
        peer.authority(),
        host.clone(),
        WorkspaceDelegationPermissions {
            issue_invitations: true,
            admit_invited_members: true,
        },
        [61; 16],
        Some(now_ns + 20_000_000_000),
        &authority_key,
    )
    .unwrap();
    assert!(
        peer.governance_mut()
            .install_workspace_delegation(delegation.clone(), &authority_key, now)
            .unwrap()
    );

    let guest_root = SigningKey::from_bytes(&[62; 32]);
    let guest_key = SigningKey::from_bytes(&[63; 32]);
    let mut guest = DeviceCert {
        user_public_key: guest_root.verifying_key().to_bytes(),
        device_public_key: guest_key.verifying_key().to_bytes(),
        device_label: "Delegated guest".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut guest, &guest_root).unwrap();
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: 9,
            room: peer.room().into(),
            authority: peer.authority().clone(),
            issuer: host,
            delegation: Some(delegation),
            role: MemberRole::Commenter,
            nonce: [64; 16],
            recipient_user_public_key: Some(guest.user_public_key),
            invitee_name: None,
            expires_at_ns: now_ns + 10_000_000_000,
            transport: vec![],
        },
        &host_key,
    )
    .unwrap();
    let join = decode_and_verify_invitation(
        &signed,
        Some(guest.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &join,
        &guest,
        [65; 32],
        [66; 32],
        now_ns + 1_000_000_000,
        now_ns,
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), &guest_key).unwrap();
    let redemption =
        verify_join_proof(&proof, &challenge, &guest, &RevocationList::new(), now_ns).unwrap();
    assert!(
        peer.governance_mut()
            .admit_invited_member(&redemption, &host_key, now)
            .unwrap()
    );
    assert_eq!(
        peer.permissions(guest.device_public_key, now).unwrap().role,
        collab_types::Role::Commenter
    );
}

#[test]
fn concurrent_redeemers_cannot_merge_two_receipts_for_one_invitation() {
    use crate::join::{
        InviteGrant, JoinChallenge, MemberRole, decode_and_verify_invitation, issue_invitation,
        sign_join_proof, verify_join_proof,
    };
    let (base, authority_key, _, _, now) = seeded();
    let now_ns: u64 = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap();
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: 10,
            room: base.room().into(),
            authority: base.authority().clone(),
            issuer: base.authority().clone(),
            delegation: None,
            role: MemberRole::Editor,
            nonce: [70; 16],
            recipient_user_public_key: None,
            invitee_name: None,
            expires_at_ns: now_ns + 10_000_000_000,
            transport: vec![],
        },
        &authority_key,
    )
    .unwrap();
    let redeem = |root_seed: u8, device_seed: u8, request: u8, challenge_nonce: u8| {
        let root = SigningKey::from_bytes(&[root_seed; 32]);
        let device = SigningKey::from_bytes(&[device_seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: format!("concurrent-{device_seed}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let join = decode_and_verify_invitation(
            &signed,
            Some(cert.user_public_key),
            &RevocationList::new(),
            now_ns,
        )
        .unwrap();
        let challenge = JoinChallenge::new(
            &join,
            &cert,
            [request; 32],
            [challenge_nonce; 32],
            now_ns + 1_000_000_000,
            now_ns,
        )
        .unwrap();
        let proof = sign_join_proof(challenge.clone(), &device).unwrap();
        verify_join_proof(&proof, &challenge, &cert, &RevocationList::new(), now_ns).unwrap()
    };
    let first = redeem(71, 72, 73, 74);
    let second = redeem(75, 76, 77, 78);
    let mut left = base.clone();
    let mut right = base.clone();
    assert!(
        left.governance_mut()
            .admit_invited_member(&first, &authority_key, now)
            .unwrap()
    );
    assert!(
        right
            .governance_mut()
            .admit_invited_member(&second, &authority_key, now)
            .unwrap()
    );

    let authority = base.authority().device_public_key;
    let mut receiver = base;
    receiver
        .receive_from(
            TransportOrigin::RelayDelta(authority),
            left.room_message(),
            now,
        )
        .unwrap();
    let accepted = receiver.durable().unwrap();
    let error = receiver
        .receive_from(
            TransportOrigin::RelayDelta(authority),
            right.room_message(),
            now,
        )
        .unwrap_err();
    assert!(
        error.contains("conflicting redemption") || error.contains("duplicate seq"),
        "concurrent redemption must fail before two receipts merge: {error}"
    );
    assert_eq!(receiver.durable().unwrap(), accepted);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
fn concurrent_distinct_invites_for_one_device_preserve_both_spends_after_restore() {
    use crate::join::{
        InviteGrant, JoinChallenge, MemberRole, WorkspaceDelegationPermissions,
        decode_and_verify_invitation, issue_invitation, sign_join_proof, sign_workspace_delegation,
        verify_join_proof,
    };
    let (mut base, authority_key, delegate_key, _, now) = seeded();
    let NeighbourMsg::SyncResp { changes, room } = base.room_message() else {
        panic!("unexpected fixture shape")
    };
    let mut legacy = crate::room::candidate(None, &changes).unwrap();
    let before = legacy.get_heads();
    legacy.set_actor(automerge::ActorId::from(
        authority_key.verifying_key().to_bytes().to_vec(),
    ));
    legacy.delete(ROOT, "invitation_receipts_v1").unwrap();
    legacy.commit_edit("fixture", "invitation_receipts_v1");
    base.receive_from(
        TransportOrigin::RelayDelta(authority_key.verifying_key().to_bytes()),
        NeighbourMsg::SyncResp {
            changes: legacy
                .get_changes(&before)
                .into_iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect(),
            room,
        },
        now,
    )
    .unwrap();
    let now_ns: u64 = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap();
    let delegate = base
        .certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == delegate_key.verifying_key().to_bytes())
        .unwrap();
    let delegation = sign_workspace_delegation(
        base.room(),
        11,
        base.authority(),
        delegate.clone(),
        WorkspaceDelegationPermissions {
            issue_invitations: true,
            admit_invited_members: true,
        },
        [80; 16],
        Some(now_ns + 20_000_000_000),
        &authority_key,
    )
    .unwrap();
    base.governance_mut()
        .install_workspace_delegation(delegation.clone(), &authority_key, now)
        .unwrap();

    let guest_root = SigningKey::from_bytes(&[81; 32]);
    let guest_key = SigningKey::from_bytes(&[82; 32]);
    let mut guest = DeviceCert {
        user_public_key: guest_root.verifying_key().to_bytes(),
        device_public_key: guest_key.verifying_key().to_bytes(),
        device_label: "same concurrent device".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut guest, &guest_root).unwrap();
    let grant = |nonce, issuer, delegation| InviteGrant {
        workspace_id: 11,
        room: base.room().into(),
        authority: base.authority().clone(),
        issuer,
        delegation,
        role: MemberRole::Editor,
        nonce: [nonce; 16],
        recipient_user_public_key: Some(guest.user_public_key),
        invitee_name: None,
        expires_at_ns: now_ns + 10_000_000_000,
        transport: vec![],
    };
    let signed_a =
        issue_invitation(grant(83, base.authority().clone(), None), &authority_key).unwrap();
    let signed_b = issue_invitation(grant(84, delegate, Some(delegation)), &delegate_key).unwrap();
    let redeem = |signed: &[u8], request: u8, nonce: u8, cert: &DeviceCert, key: &SigningKey| {
        let join = decode_and_verify_invitation(
            signed,
            Some(cert.user_public_key),
            &RevocationList::new(),
            now_ns,
        )
        .unwrap();
        let challenge = JoinChallenge::new(
            &join,
            cert,
            [request; 32],
            [nonce; 32],
            now_ns + 1_000_000_000,
            now_ns,
        )
        .unwrap();
        let proof = sign_join_proof(challenge.clone(), key).unwrap();
        verify_join_proof(&proof, &challenge, cert, &RevocationList::new(), now_ns).unwrap()
    };
    let redemption_a = redeem(&signed_a, 85, 86, &guest, &guest_key);
    let redemption_b = redeem(&signed_b, 87, 88, &guest, &guest_key);
    let mut left = base.clone();
    let mut right = base.clone();
    left.governance_mut()
        .admit_invited_member(&redemption_a, &authority_key, now)
        .unwrap();
    right
        .governance_mut()
        .admit_invited_member(&redemption_b, &delegate_key, now)
        .unwrap();

    let authority = base.authority().device_public_key;
    let mut merged = base;
    merged
        .receive_from(
            TransportOrigin::RelayDelta(authority),
            left.room_message(),
            now,
        )
        .unwrap();
    merged
        .receive_from(
            TransportOrigin::RelayDelta(authority),
            right.room_message(),
            now,
        )
        .unwrap();
    let mut restored = StatePeer::restore(
        &merged.durable().unwrap(),
        merged.room(),
        merged.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();

    let replay_key = SigningKey::from_bytes(&[89; 32]);
    let mut replay = guest.clone();
    replay.device_public_key = replay_key.verifying_key().to_bytes();
    replay.device_label = "different replay device".into();
    sign_device_cert(&mut replay, &guest_root).unwrap();
    let replay_a = redeem(&signed_a, 90, 91, &replay, &replay_key);
    let replay_b = redeem(&signed_b, 92, 93, &replay, &replay_key);
    for (redemption, issuer) in [(&replay_a, &authority_key), (&replay_b, &delegate_key)] {
        assert!(
            restored
                .governance_mut()
                .admit_invited_member(redemption, issuer, now)
                .unwrap_err()
                .contains("already redeemed")
        );
    }
}

#[test]
fn conflicting_crdt_values_for_one_invite_fail_closed() {
    let (base, authority_key, _, _, now) = seeded();
    let NeighbourMsg::SyncResp { changes, room } = base.room_message() else {
        panic!("unexpected fixture shape")
    };
    let mut document = crate::room::candidate(None, &changes).unwrap();
    let base_heads = document.get_heads();
    let mut left = document.fork();
    let mut right = document.fork();
    let invitation_receipts = left.get(ROOT, "invitation_receipts_v1").unwrap().unwrap().1;
    let invite_id = [97; 32];
    let receipt = |device_public_key, request_id| crate::state::RedemptionReceipt {
        invite_id,
        user_public_key: [98; 32],
        device_public_key,
        request_id,
        role: crate::join::MemberRole::Viewer,
    };
    left.set_actor(automerge::ActorId::from(vec![98; 32]));
    left.put(
        &invitation_receipts,
        hex::encode(invite_id),
        automerge::ScalarValue::Bytes(serde_json::to_vec(&receipt([99; 32], [100; 32])).unwrap()),
    )
    .unwrap();
    left.commit_edit("fixture", "fixture");
    right.set_actor(automerge::ActorId::from(vec![101; 32]));
    right
        .put(
            &invitation_receipts,
            hex::encode(invite_id),
            automerge::ScalarValue::Bytes(
                serde_json::to_vec(&receipt([102; 32], [103; 32])).unwrap(),
            ),
        )
        .unwrap();
    right.commit_edit("fixture", "fixture");
    let message = |mut branch: AutoCommit| NeighbourMsg::SyncResp {
        changes: branch
            .get_changes(&base_heads)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
        room: room.clone(),
    };
    let mut receiver = base;
    receiver
        .receive_from(
            TransportOrigin::RelayDelta(authority_key.verifying_key().to_bytes()),
            message(left),
            now,
        )
        .unwrap();
    let durable = receiver.durable().unwrap();
    assert!(
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(authority_key.verifying_key().to_bytes()),
                message(right),
                now,
            )
            .unwrap_err()
            .contains("conflicting redemption")
    );
    assert_eq!(receiver.durable().unwrap(), durable);
}

#[test]
fn retained_relay_replay_accepts_certified_document_after_live_freshness_window() {
    let (mut governor, authority, _, browser_cert, authored_at) = seeded();
    let replayed_at = authored_at + Duration::from_secs(301);
    let mut browser = StatePeer::new(
        governor.room(),
        governor.authority().clone(),
        vec![browser_cert],
        RevocationList::new(),
        replayed_at,
    )
    .unwrap();
    let origin = TransportOrigin::RetainedRelayDelta(authority.verifying_key().to_bytes());

    for message in governor
        .publication_mut()
        .bootstrap_messages(&authority, authored_at)
        .unwrap()
    {
        browser.receive_from(origin, message, replayed_at).unwrap();
    }

    assert_eq!(browser.rows(), governor.rows());
    assert_eq!(
        browser.document_heads("test.app", "tab:1"),
        governor.document_heads("test.app", "tab:1")
    );
}
#[test]
fn fresh_checkpoint_covers_compacted_other_author_and_restore_never_reuses_sequence() {
    let (mut native, a, b, bc, now) = seeded();
    let mut doc = AutoCommit::load(&native.document("test.app", "tab:1").unwrap()).unwrap();
    let before = doc.get_heads();
    doc.put(ROOT, "other", "B").unwrap();
    doc.commit_edit("fixture", "other");
    let old = native
        .publication_mut()
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &b, now)
        .unwrap();
    let bootstrap = native
        .publication_mut()
        .bootstrap_messages(&a, now)
        .unwrap();
    let mut fresh = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    // Retained per-channel replay can arrive in either order.
    assert!(
        fresh
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                bootstrap[1].clone(),
                now
            )
            .unwrap()
            .deferred
    );
    fresh
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            bootstrap[0].clone(),
            now,
        )
        .unwrap();
    assert_eq!(fresh.rows(), native.rows());
    assert!(
        fresh
            .receive_from(
                TransportOrigin::RelayDelta(bc.device_public_key),
                old.clone(),
                now
            )
            .unwrap()
            .duplicate
    );
    let checkpoint = fresh.durable().unwrap();
    let mut reload = StatePeer::restore(
        &checkpoint,
        native.room(),
        native.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(
        reload
            .receive_from(
                TransportOrigin::RelayDelta(b.verifying_key().to_bytes()),
                old,
                now
            )
            .unwrap()
            .duplicate
    );
    let mut doc = AutoCommit::load(&reload.document("test.app", "tab:1").unwrap()).unwrap();
    let before = doc.get_heads();
    doc.put(ROOT, "new", "B2").unwrap();
    doc.commit_edit("fixture", "new");
    let next = reload
        .publication_mut()
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &b, now)
        .unwrap();
    assert!(matches!(next,NeighbourMsg::AppDocument{envelope,..} if envelope.seq==2));
}
#[test]
fn state_on_presence_cannot_install_governance_or_advance_watermarks() {
    let (mut native, a, _, bc, now) = seeded();
    let bootstrap = native
        .publication_mut()
        .bootstrap_messages(&a, now)
        .unwrap();
    let mut fresh = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for message in &bootstrap {
        assert!(
            fresh
                .receive_from(
                    TransportOrigin::UntrustedPresence(a.verifying_key().to_bytes()),
                    message.clone(),
                    now
                )
                .is_err()
        );
    }
    assert!(!fresh.has_governance());
    assert!(fresh.rows().is_empty());
    for message in bootstrap {
        fresh
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
    }
    assert_eq!(fresh.rows(), native.rows());
}
#[test]
fn forged_or_mismatched_coverage_cannot_install_snapshot() {
    let (mut native, a, _, bc, now) = seeded();
    let bootstrap = native
        .publication_mut()
        .bootstrap_messages(&a, now)
        .unwrap();
    let mut fresh = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(
        fresh
            .receive_from(
                TransportOrigin::RelayDelta(bc.device_public_key),
                bootstrap[0].clone(),
                now
            )
            .is_err()
    );
    fresh
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            bootstrap[0].clone(),
            now,
        )
        .unwrap();
    let mut mismatch = bootstrap[1].clone();
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut mismatch {
        let mut replacement = AutoCommit::new();
        replacement.put(ROOT, "attack", true).unwrap();
        replacement.commit_edit("fixture", "attack");
        envelope.payload = collab_types::DocumentMutation::Snapshot(replacement.save())
            .encode()
            .unwrap();
        collab_types::sign_op(envelope, &a).unwrap();
    }
    assert!(
        fresh
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                mismatch,
                now
            )
            .unwrap_err()
            .contains("coverage")
    );
    assert!(fresh.rows().is_empty());
    fresh
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            bootstrap[1].clone(),
            now,
        )
        .unwrap();
    assert_eq!(fresh.rows(), native.rows());
}

#[test]
fn checkpoint_without_coverage_remains_deferred_and_metadata_cannot_regress() {
    let (mut native, a, _, bc, now) = seeded();
    let messages = native
        .publication_mut()
        .bootstrap_messages(&a, now)
        .unwrap();
    let NeighbourMsg::SyncResp { changes, room } = messages[0].clone() else {
        panic!("unexpected fixture shape")
    };
    let mut doc = room::candidate(None, &changes).unwrap();
    doc.delete(ROOT, coverage::FIELD_V2).unwrap();
    doc.commit_edit("fixture", "fixture");
    let legacy = NeighbourMsg::SyncResp {
        room: room.clone(),
        changes: doc
            .get_changes(&[])
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect(),
    };
    let mut receiver = StatePeer::new(
        &room,
        native.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    receiver
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            legacy,
            now,
        )
        .unwrap();
    assert!(
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                messages[1].clone(),
                now
            )
            .unwrap()
            .deferred
    );
    assert!(receiver.rows().is_empty());
    let mut candidate = room::candidate(None, &changes).unwrap();
    let mut rows = coverage::entries(&candidate).unwrap();
    let entry = rows.values_mut().next().unwrap();
    entry.sequence = 0;
    let regressed = entry.clone();
    coverage::put(&mut candidate, regressed).unwrap();
    assert!(
        coverage::monotonic(Some(&room::candidate(None, &changes).unwrap()), &candidate).is_err()
    );
}
#[test]
fn fresh_same_head_checkpoint_is_still_a_durable_watermark_mutation() {
    let (mut native, a, _, bc, now) = seeded();
    let mut receiver = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for message in native
        .publication_mut()
        .bootstrap_messages(&a, now)
        .unwrap()
    {
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
    }
    let heads = receiver.rows();
    let fresh = native
        .publication_mut()
        .bootstrap_messages(&a, now)
        .unwrap();
    receiver
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            fresh[0].clone(),
            now,
        )
        .unwrap();
    let outcome = receiver
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            fresh[1].clone(),
            now,
        )
        .unwrap();
    assert!(!outcome.changed);
    assert!(outcome.metadata_changed);
    assert_eq!(heads, receiver.rows());
}
#[test]
fn authenticated_missing_mls_is_durable_and_later_plaintext_cannot_downgrade() {
    let (mut native, a, _, bc, now) = seeded();
    let mut receiver = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for message in native
        .publication_mut()
        .bootstrap_messages(&a, now)
        .unwrap()
    {
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
    }
    let messages = native
        .publication_mut()
        .bootstrap_messages(&a, now)
        .unwrap();
    let mut encrypted = messages[1].clone();
    if let NeighbourMsg::AppDocument {
        envelope,
        app_id,
        cluster_id,
        record_id,
        ..
    } = &mut encrypted
    {
        envelope.encryption = Some(collab_types::EncryptionMetadata::mls_application(
            1,
            collab_types::document_channel_id(app_id, cluster_id, record_id),
        ));
        envelope.payload = vec![1, 2, 3];
        collab_types::sign_op(envelope, &a).unwrap();
    }
    let outcome = receiver
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            encrypted,
            now,
        )
        .expect("authenticated unsupported encryption must persist its no-downgrade obligation");
    assert!(outcome.metadata_changed);
    let mut restored = StatePeer::restore(
        &receiver.durable().unwrap(),
        native.room(),
        native.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(
        restored
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                messages[1].clone(),
                now
            )
            .unwrap_err()
            .contains("downgrade")
    );
}
#[test]
fn self_issued_identity_presence_cannot_expand_the_admitted_roster() {
    let (mut receiver, _, _, _, now) = seeded();
    let rogue_root = SigningKey::from_bytes(&[99; 32]);
    let rogue = SigningKey::from_bytes(&[98; 32]);
    let mut cert = DeviceCert {
        user_public_key: rogue_root.verifying_key().to_bytes(),
        device_public_key: rogue.verifying_key().to_bytes(),
        device_label: "self issued".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &rogue_root).unwrap();
    assert!(
        receiver
            .receive_from(
                TransportOrigin::UntrustedPresence(cert.device_public_key),
                NeighbourMsg::IdentityHello {
                    protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                    cert: Box::new(cert),
                    revocations: vec![],
                    successions: Vec::new()
                },
                now
            )
            .is_err()
    );
}

#[test]
fn aged_retained_checkpoint_rejects_without_poisoning_fresh_sync_or_durable_ready_state() {
    let (mut native, key, _, browser_cert, created) = seeded();
    let delayed = created + Duration::from_secs(301);
    let stale = native
        .publication_mut()
        .bootstrap_messages(&key, created)
        .unwrap();
    let mut receiver = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![native.authority().clone(), browser_cert],
        RevocationList::new(),
        delayed,
    )
    .unwrap();
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    receiver
        .receive_from(origin, stale[0].clone(), delayed)
        .unwrap();
    let before = receiver.durable().unwrap();
    assert!(
        receiver
            .receive_from(origin, stale[1].clone(), delayed)
            .unwrap_err()
            .contains("timestamp")
    );
    assert_eq!(receiver.durable().unwrap(), before);
    assert!(receiver.rows().is_empty());
    assert_eq!(receiver.incomplete_reason(), None);
    for message in native
        .publication_mut()
        .bootstrap_messages(&key, delayed)
        .unwrap()
    {
        receiver.receive_from(origin, message, delayed).unwrap();
    }
    assert_eq!(receiver.rows(), native.rows());
    let admitted = receiver.durable().unwrap();
    assert!(
        receiver
            .receive_from(origin, stale[1].clone(), delayed)
            .is_err()
    );
    assert_eq!(receiver.durable().unwrap(), admitted);
    assert_eq!(receiver.incomplete_reason(), None);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
fn learned_revocation_and_governed_identity_survive_stale_enrollment_restore() {
    use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
    let (mut native, a, b, bc, now) = seeded();
    let mut receiver = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for msg in native
        .publication_mut()
        .bootstrap_messages(&a, now)
        .unwrap()
    {
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                msg,
                now,
            )
            .unwrap();
    }
    let learned = receiver
        .receive_from(
            TransportOrigin::UntrustedPresence(bc.device_public_key),
            NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(bc.clone()),
                revocations: vec![],
                successions: Vec::new(),
            },
            now,
        )
        .unwrap();
    assert!(
        !learned.metadata_changed,
        "governed SyncResp already imported certificate"
    );
    assert!(receiver.certificates().contains(&bc));
    let restored = StatePeer::restore(
        &receiver.durable().unwrap(),
        native.room(),
        native.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(restored.permissions(bc.device_public_key, now).is_ok());
    assert!(receiver.permissions(bc.device_public_key, now).is_ok());
    let mut revocation = RevocationEntry {
        user_public_key: bc.user_public_key,
        revoked_device_public_key: bc.device_public_key,
        revoked_at_ns: 1,
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut revocation, &SigningKey::from_bytes(&[1; 32])).unwrap();
    let learned = receiver
        .receive_from(
            TransportOrigin::UntrustedPresence(a.verifying_key().to_bytes()),
            NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(native.authority().clone()),
                revocations: vec![revocation],
                successions: Vec::new(),
            },
            now,
        )
        .unwrap();
    assert!(learned.metadata_changed);
    assert!(
        receiver
            .permissions(bc.device_public_key, now)
            .unwrap_err()
            .contains("revoked"),
        "external revocation must take effect with a warm policy cache"
    );
    let mut restored = StatePeer::restore(
        &receiver.durable().unwrap(),
        native.room(),
        native.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(
        restored
            .permissions(b.verifying_key().to_bytes(), now)
            .unwrap_err()
            .contains("revoked")
    );
    let mut doc = AutoCommit::load(&native.document("test.app", "tab:1").unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "new", "forbidden").unwrap();
    doc.commit_edit("fixture", "new");
    let op = native
        .publication_mut()
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &b, now)
        .unwrap();
    assert!(
        restored
            .receive_from(
                TransportOrigin::RelayDelta(b.verifying_key().to_bytes()),
                op,
                now
            )
            .unwrap_err()
            .contains("revoked")
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
fn deferred_encryption_obligation_is_journaled_on_fresh_and_unchanged_governance() {
    for existing in [false, true] {
        let (mut native, a, b, bc, now) = seeded();
        let mut receiver = StatePeer::new(
            native.room(),
            native.authority().clone(),
            vec![],
            RevocationList::new(),
            now,
        )
        .unwrap();
        if existing {
            for msg in native
                .publication_mut()
                .bootstrap_messages(&a, now)
                .unwrap()
            {
                receiver
                    .receive_from(
                        TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                        msg,
                        now,
                    )
                    .unwrap();
            }
        }
        let messages = native
            .publication_mut()
            .bootstrap_messages(&a, now)
            .unwrap();
        // Install exactly the room we will drain again: the existing path then
        // has no changed room heads, only the deferred metadata obligation.
        if existing {
            receiver
                .receive_from(
                    TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                    messages[0].clone(),
                    now,
                )
                .unwrap();
        }
        let mut encrypted = messages[1].clone();
        if let NeighbourMsg::AppDocument {
            envelope,
            app_id,
            cluster_id,
            record_id,
            ..
        } = &mut encrypted
        {
            envelope.encryption = Some(collab_types::EncryptionMetadata::mls_application(
                1,
                collab_types::document_channel_id(app_id, cluster_id, record_id),
            ));
            envelope.payload = vec![1, 2, 3];
            collab_types::sign_op(envelope, &a).unwrap();
        }
        let author = if existing {
            b.verifying_key().to_bytes()
        } else {
            a.verifying_key().to_bytes()
        };
        if existing && let NeighbourMsg::AppDocument { peer, envelope, .. } = &mut encrypted {
            *peer = peer_handle(&author);
            envelope.author = collab_types::Ed25519PublicKey::from_bytes(author);
            envelope.seq = 1;
            collab_types::sign_op(envelope, &b).unwrap();
        }
        let encrypted_outcome = receiver
            .receive_from(TransportOrigin::RelayDelta(author), encrypted, now)
            .unwrap();
        assert_eq!(encrypted_outcome.deferred, !existing);
        if existing {
            assert!(encrypted_outcome.metadata_changed);
            assert!(encrypted_outcome.incomplete.is_some());
        }
        if existing {
            receiver
                .receive_from(
                    TransportOrigin::UntrustedPresence(author),
                    NeighbourMsg::IdentityHello {
                        protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                        cert: Box::new(bc),
                        revocations: vec![],
                        successions: Vec::new(),
                    },
                    now,
                )
                .unwrap();
        }
        let outcome = receiver
            .receive_from(
                TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                messages[0].clone(),
                now,
            )
            .unwrap();
        assert!(
            outcome.metadata_changed || (existing && encrypted_outcome.metadata_changed),
            "immediate or deferred metadata must be journaled"
        );
        assert!(outcome.incomplete.is_some());
        if existing {
            assert!(!outcome.changed);
        }
        let mut restored = StatePeer::restore(
            &receiver.durable().unwrap(),
            native.room(),
            native.authority().clone(),
            vec![],
            RevocationList::new(),
            now,
        )
        .unwrap();
        assert!(restored.incomplete_reason().is_some());
        assert!(
            restored
                .receive_from(
                    TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
                    messages[1].clone(),
                    now
                )
                .unwrap_err()
                .contains("downgrade")
        );
    }
}

#[test]
fn unsupported_durable_version_is_rejected_without_mutating_the_bytes() {
    let (peer, _, _, bc, now) = seeded();
    let mut bytes = peer.durable().unwrap();
    bytes[0] = 3;
    let unchanged = bytes.clone();
    let error = StatePeer::restore(
        &bytes,
        peer.room(),
        peer.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .err()
    .unwrap();
    assert!(error.contains("unsupported durable admission version"));
    assert_eq!(bytes, unchanged);
}

#[test]
fn reordered_incrementals_are_pending_durable_and_drain_after_predecessor() {
    let (receiver, _, author_key, author_cert, now) = seeded();
    let mut author = receiver.clone();
    let app = "test.app";
    let record = "tab:1";
    let mut document = AutoCommit::load(&author.document(app, record).unwrap()).unwrap();
    document.set_actor(automerge::ActorId::from(
        author_key.verifying_key().to_bytes().to_vec(),
    ));

    let before = document.get_heads();
    document.put(ROOT, "first", "A").unwrap();
    document.commit_edit("fixture", "first");
    let first = author
        .publication_mut()
        .publish_incremental(app, record, document.save_after(&before), &author_key, now)
        .unwrap();
    let before = document.get_heads();
    document.put(ROOT, "second", "B").unwrap();
    document.commit_edit("fixture", "second");
    let second = author
        .publication_mut()
        .publish_incremental(app, record, document.save_after(&before), &author_key, now)
        .unwrap();

    let origin = TransportOrigin::RelayDelta(author_key.verifying_key().to_bytes());
    let mut receiver = receiver;
    let outcome = receiver.receive_from(origin, second.clone(), now).unwrap();
    assert!(outcome.deferred);
    assert_eq!(receiver.pending_count(), 1);
    assert!(!receiver.covers(&second));

    let mut receiver = StatePeer::restore(
        &receiver.durable().unwrap(),
        receiver.room(),
        receiver.authority().clone(),
        vec![author_cert],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert_eq!(receiver.pending_count(), 1);
    receiver.receive_from(origin, first, now).unwrap();
    assert_eq!(receiver.pending_count(), 0);
    assert_eq!(
        receiver.document_heads(app, record),
        Some(document.base_heads())
    );
}

#[test]
fn dynamic_governed_identity_import_preserves_history_and_requires_authority() {
    let (mut governor, a, b, _, now) = seeded();
    let root = SigningKey::from_bytes(&[1; 32]);
    let new_key = SigningKey::from_bytes(&[8; 32]);
    let mut cert = governor.authority().clone();
    cert.device_public_key = new_key.verifying_key().to_bytes();
    cert.device_label = "dynamic client".into();
    sign_device_cert(&mut cert, &root).unwrap();
    let mut existing = governor.clone();
    let before = governor.durable().unwrap();
    assert!(governor.admit_certificate(cert.clone(), &b, now).is_err());
    assert_eq!(before, governor.durable().unwrap());
    let rows = governor.rows();
    let floors = governor.replay_floors();
    assert!(governor.admit_certificate(cert.clone(), &a, now).unwrap());
    assert!(!governor.admit_certificate(cert.clone(), &a, now).unwrap());
    assert_eq!(rows, governor.rows());
    assert_eq!(floors, governor.replay_floors());
    let outcome = existing
        .receive_from(
            TransportOrigin::RelayDelta(a.verifying_key().to_bytes()),
            governor.room_message(),
            now,
        )
        .unwrap();
    assert!(outcome.metadata_changed);
    assert!(existing.certificates().contains(&cert));
    assert!(
        existing
            .permissions(new_key.verifying_key().to_bytes(), now)
            .is_ok()
    );
    assert_eq!(rows, existing.rows());
    let mut doc = AutoCommit::load(&existing.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        new_key.verifying_key().to_bytes().to_vec(),
    ));
    let before = doc.get_heads();
    doc.put(ROOT, "joined", true).unwrap();
    doc.commit_edit("fixture", "joined");
    let message = existing
        .publication_mut()
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &new_key, now)
        .unwrap();
    assert!(
        governor
            .receive_from(
                TransportOrigin::RelayDelta(new_key.verifying_key().to_bytes()),
                message,
                now
            )
            .unwrap()
            .changed
    );
    let restored = StatePeer::restore(
        &existing.durable().unwrap(),
        existing.room(),
        existing.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(restored.certificates().contains(&cert));
    let other_root = SigningKey::from_bytes(&[9; 32]);
    cert.user_public_key = other_root.verifying_key().to_bytes();
    sign_device_cert(&mut cert, &other_root).unwrap();
    assert!(governor.admit_certificate(cert, &a, now).is_err());
}

#[test]
fn expired_known_identity_does_not_block_governed_join() {
    let (mut governor, a, _, _, now) = seeded();
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut cert = governor.authority().clone();
    cert.device_public_key = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
    cert.expires_at_ns = Some(
        (now + Duration::from_secs(1))
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .try_into()
            .unwrap(),
    );
    sign_device_cert(&mut cert, &root).unwrap();
    governor.admit_certificate(cert.clone(), &a, now).unwrap();
    let mut existing = governor.clone();
    assert!(existing.permissions(cert.device_public_key, now).is_ok());
    let later = now + Duration::from_secs(2);
    assert!(
        existing
            .permissions(cert.device_public_key, later)
            .unwrap_err()
            .contains("expired"),
        "a warm policy cache must not cache certificate validity"
    );
    let mut next = governor.authority().clone();
    next.device_public_key = SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes();
    sign_device_cert(&mut next, &root).unwrap();
    assert!(governor.admit_certificate(next.clone(), &a, later).unwrap());
    assert!(
        existing
            .receive(a.verifying_key().to_bytes(), governor.room_message(), later)
            .unwrap()
            .metadata_changed
    );
    assert!(
        existing
            .permissions(cert.device_public_key, later)
            .unwrap_err()
            .contains("expired")
    );
    assert!(existing.permissions(next.device_public_key, later).is_ok());
    let restored = StatePeer::restore(
        &existing.durable().unwrap(),
        existing.room(),
        existing.authority().clone(),
        vec![],
        RevocationList::new(),
        later,
    )
    .unwrap();
    assert!(
        restored
            .permissions(cert.device_public_key, later)
            .unwrap_err()
            .contains("expired"),
        "expired historical certificates restore but never regain current access"
    );
    let mut empty = StatePeer::new(
        governor.room(),
        governor.authority().clone(),
        vec![],
        RevocationList::new(),
        later,
    )
    .unwrap();
    assert!(
        empty
            .receive(a.verifying_key().to_bytes(), governor.room_message(), later)
            .is_err(),
        "previously unknown expired certificate cannot be imported"
    );
}

#[test]
fn restore_validates_fresh_roster_not_before_and_interval_while_retaining_history() {
    let (peer, _, _, _, now) = seeded();
    let now_ns: u64 = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap();
    let root = SigningKey::from_bytes(&[94; 32]);
    let make = |device_seed: u8, issued_at_ns: u64, expires_at_ns: Option<u64>| {
        let device = SigningKey::from_bytes(&[device_seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: format!("fresh-{device_seed}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
        };
        if expires_at_ns.is_some_and(|expiry| expiry <= issued_at_ns) {
            let digest = tilo_user_identity::device_cert_signing_digest(&cert).unwrap();
            cert.signature = root.sign(&digest).to_bytes();
        } else {
            sign_device_cert(&mut cert, &root).unwrap();
        }
        cert
    };
    let future = make(95, now_ns + 1_000_000_000, None);
    let error = StatePeer::restore(
        &peer.durable().unwrap(),
        peer.room(),
        peer.authority().clone(),
        vec![future],
        RevocationList::new(),
        now,
    )
    .err()
    .unwrap();
    assert!(error.contains("not yet valid"), "{error}");

    let backwards = make(96, now_ns, Some(now_ns));
    let error = StatePeer::restore(
        &peer.durable().unwrap(),
        peer.room(),
        peer.authority().clone(),
        vec![backwards],
        RevocationList::new(),
        now,
    )
    .err()
    .unwrap();
    assert!(error.contains("invalid validity"), "{error}");
}

#[test]
#[expect(
    clippy::similar_names,
    reason = "`root` and `room` are the domain's own names"
)]
fn expired_pinned_authority_restores_as_history_but_cannot_authorize() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(4_000);
    let later = now + Duration::from_secs(2);
    let root = SigningKey::from_bytes(&[104; 32]);
    let authority_key = SigningKey::from_bytes(&[105; 32]);
    let mut authority = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: authority_key.verifying_key().to_bytes(),
        device_label: "historical authority".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: Some(
            (now + Duration::from_secs(1))
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .try_into()
                .unwrap(),
        ),
        signature: [0; 64],
    };
    sign_device_cert(&mut authority, &root).unwrap();
    let room = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    let mut peer = StatePeer::new(
        room,
        authority.clone(),
        vec![authority.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.install_seed(
        SeedState {
            room_changes: seed_room(room, &authority, &[authority.clone()]).unwrap(),
            documents: vec![],
        },
        &authority_key,
        now,
    )
    .unwrap();

    let restored = StatePeer::restore(
        &peer.durable().unwrap(),
        room,
        authority.clone(),
        vec![],
        RevocationList::new(),
        later,
    )
    .unwrap();
    assert!(restored.certificates().contains(&authority));
    assert!(
        restored
            .permissions(authority.device_public_key, later)
            .unwrap_err()
            .contains("expired")
    );
}
#[test]
fn governed_import_is_atomic_even_for_direct_admission() {
    use automerge::ReadDoc;
    let root = SigningKey::from_bytes(&[1; 32]);
    let (mut source, a, _, _, now) = seeded();
    let mut receiver = source.clone();
    let mut valid = source.authority().clone();
    valid.device_public_key = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
    sign_device_cert(&mut valid, &root).unwrap();
    source.admit_certificate(valid, &a, now).unwrap();
    let NeighbourMsg::SyncResp { changes, room } = source.room_message() else {
        panic!("unexpected fixture shape")
    };
    let mut candidate = room::candidate(None, &changes).unwrap();
    let (_, identities) = candidate.get(ROOT, "peer_identities_v1").unwrap().unwrap();
    candidate
        .put(
            &identities,
            "zz-invalid",
            automerge::ScalarValue::Bytes(vec![0]),
        )
        .unwrap();
    candidate.commit_edit("fixture", "fixture");
    let changes = candidate
        .get_changes(&[])
        .into_iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    let before = receiver.durable().unwrap();
    assert!(
        receiver
            .receive(
                a.verifying_key().to_bytes(),
                NeighbourMsg::SyncResp { changes, room },
                now
            )
            .is_err()
    );
    assert_eq!(
        before,
        receiver.durable().unwrap(),
        "direct receive cannot partly import certificate metadata"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
fn durability_receipt_is_distinct_from_acceptance_and_survives_restore() {
    let (a, ac, b, bc, now) = fixtures();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut native = StatePeer::new(
        room,
        ac.clone(),
        vec![ac.clone(), bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "value", 1).unwrap();
    doc.commit_edit("fixture", "value");
    native
        .install_seed(
            SeedState {
                room_changes: seed_room(room, &ac, &[ac.clone(), bc.clone()]).unwrap(),
                documents: vec![SeedDocument {
                    app_id: "test.app".into(),
                    record_id: "tab:1".into(),
                    kind: "test".into(),
                    bytes: doc.save(),
                }],
            },
            &a,
            now,
        )
        .unwrap();
    let mut browser = StatePeer::new(
        room,
        ac.clone(),
        vec![ac.clone(), bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for message in native
        .publication_mut()
        .bootstrap_messages(&a, now)
        .unwrap()
    {
        browser
            .receive(a.verifying_key().to_bytes(), message, now)
            .unwrap();
    }
    assert!(
        browser
            .durable_coverage("test.app", "tab:1")
            .unwrap()
            .is_none(),
        "legacy accepted coverage is not a durable receipt"
    );
    let messages = native
        .publication_mut()
        .durable_bootstrap_messages(&a, now, 7)
        .unwrap();
    browser
        .receive(a.verifying_key().to_bytes(), messages[0].clone(), now)
        .unwrap();
    assert!(
        browser
            .durable_coverage("test.app", "tab:1")
            .unwrap()
            .is_none(),
        "receipt cannot cover a checkpoint that is not accepted yet"
    );
    for message in messages.into_iter().skip(1) {
        browser
            .receive(a.verifying_key().to_bytes(), message, now)
            .unwrap();
    }
    let receipt = browser
        .durable_coverage("test.app", "tab:1")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.generation, 7);
    assert!(browser.durable_covers_current("test.app", "tab:1").unwrap());
    assert_eq!(
        browser
            .durable_coverage_with_current("test.app", "tab:1")
            .unwrap(),
        Some((receipt.clone(), true))
    );
    let bytes = browser.durable().unwrap();
    let mut browser =
        StatePeer::restore(&bytes, room, ac, vec![bc], RevocationList::new(), now).unwrap();
    crate::state::CHECKPOINT_DECODES.with(|count| count.set(0));
    for _ in 0..100 {
        let fork = browser.clone();
        assert_eq!(
            fork.durable_coverage("test.app", "tab:1").unwrap().unwrap(),
            receipt
        );
        assert_eq!(
            fork.durable_coverage_with_current("test.app", "tab:1")
                .unwrap(),
            Some((receipt.clone(), true))
        );
    }
    assert_eq!(
        crate::state::CHECKPOINT_DECODES.with(std::cell::Cell::get),
        1,
        "peer clones must share immutable checkpoint decoding"
    );
    assert_eq!(
        browser
            .durable_coverage("test.app", "tab:1")
            .unwrap()
            .unwrap(),
        receipt
    );
    let mut doc = AutoCommit::load(&browser.document("test.app", "tab:1").unwrap()).unwrap();
    let heads = browser.document_heads("test.app", "tab:1").unwrap();
    assert_eq!(heads, doc.get_heads());
    doc.put(ROOT, "value", 2).unwrap();
    doc.commit_edit("fixture", "value");
    let local = browser
        .publication_mut()
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &b, now)
        .unwrap();
    assert!(
        !browser.durable_covers_current("test.app", "tab:1").unwrap(),
        "an older receipt never covers a newer local edit"
    );
    assert_eq!(
        browser
            .durable_coverage_with_current("test.app", "tab:1")
            .unwrap(),
        Some((receipt.clone(), false))
    );
    assert!(
        browser
            .durable_coverage_with_current("test.app", "missing")
            .unwrap()
            .is_none()
    );
    let previous = browser.clone();
    native
        .receive(b.verifying_key().to_bytes(), local, now)
        .unwrap();
    for message in native
        .publication_mut()
        .durable_bootstrap_messages(&a, now, 8)
        .unwrap()
    {
        browser
            .receive(a.verifying_key().to_bytes(), message, now)
            .unwrap();
    }
    let (next, current) = browser
        .durable_coverage_with_current("test.app", "tab:1")
        .unwrap()
        .unwrap();
    assert_eq!(next.generation, 8);
    assert!(current, "replacement checkpoint covers the newer edit");
    assert_eq!(
        previous
            .durable_coverage_with_current("test.app", "tab:1")
            .unwrap(),
        Some((receipt, false)),
        "checkpoint replacement must not mutate older peer clones"
    );
}

#[test]
fn nonowner_topology_and_record_route_changes_are_rejected_before_admission() {
    use automerge::{ObjType, ReadDoc};
    let (a, ac, b, bc, now) = fixtures();
    let room = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
    let changes = seed_room(room, &ac, &[ac.clone(), bc.clone()]).unwrap();
    let mut doc = room::candidate(None, &changes).unwrap();
    let owners = doc
        .put_object(ROOT, "contents_owners_v1", ObjType::Map)
        .unwrap();
    doc.put(&owners, "0002", hex::encode(ac.device_public_key))
        .unwrap();
    let contents = doc.put_object(ROOT, "contents", ObjType::Map).unwrap();
    let strip = doc
        .put_object(contents, "0002000000000001", ObjType::Map)
        .unwrap();
    doc.put(&strip, "name", "Shared").unwrap();
    doc.put(&strip, "order", 0_u64).unwrap();
    let groups = doc.put_object(&strip, "groups", ObjType::Map).unwrap();
    let group = doc
        .put_object(groups, "0002000000000002", ObjType::Map)
        .unwrap();
    let tabs = doc.put_object(&group, "tabs", ObjType::Map).unwrap();
    let tab = doc
        .put_object(tabs, "0002000000000003", ObjType::Map)
        .unwrap();
    doc.put(&tab, "title", "Original").unwrap();
    doc.put(&tab, "record_id", "tab:0000000000000001").unwrap();
    doc.commit_edit("fixture", "record_id");
    let mut peer =
        StatePeer::new(room, ac.clone(), vec![ac, bc], RevocationList::new(), now).unwrap();
    peer.install_seed(
        SeedState {
            room_changes: doc
                .get_changes(&[])
                .iter()
                .map(|c| c.raw_bytes().to_vec())
                .collect(),
            documents: vec![],
        },
        &a,
        now,
    )
    .unwrap();
    let before = peer.durable().unwrap();
    doc.set_actor(automerge::ActorId::from(
        b.verifying_key().to_bytes().to_vec(),
    ));
    doc.put(&tab, "title", "Stolen").unwrap();
    doc.put(&tab, "record_id", "tab:0000000000000012").unwrap();
    doc.commit_edit("fixture", "record_id");
    let result = peer.receive(
        b.verifying_key().to_bytes(),
        NeighbourMsg::SyncResp {
            room: room.into(),
            changes: doc
                .get_changes(&[])
                .iter()
                .map(|c| c.raw_bytes().to_vec())
                .collect(),
        },
        now,
    );
    assert!(
        result.is_err(),
        "another certified device must not overwrite owned topology"
    );
    assert_eq!(
        peer.durable().unwrap(),
        before,
        "unadmitted topology must never become visible"
    );
    let _ = doc.get(ROOT, "contents").unwrap();
}

#[path = "copy_on_write_tests.rs"]
pub(crate) mod copy_on_write;
#[path = "delivery_admission_tests.rs"]
mod delivery_admission;
#[path = "selective_publication_tests.rs"]
mod selective_publication;

/// An invitation for `guest_cert` with `nonce`, redeemed by its device.
fn guest_redemption(
    peer: &StatePeer,
    authority_key: &SigningKey,
    guest_key: &SigningKey,
    guest_cert: &DeviceCert,
    nonce: u8,
    now: SystemTime,
) -> crate::join::VerifiedRedemption {
    use crate::join::{
        InviteGrant, JoinChallenge, MemberRole, issue_invitation, sign_join_proof,
        verify_join_proof,
    };
    let now_ns: u64 = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap();
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: 7,
            room: peer.room().into(),
            authority: peer.authority().clone(),
            issuer: peer.authority().clone(),
            delegation: None,
            role: MemberRole::Editor,
            nonce: [nonce; 16],
            recipient_user_public_key: Some(guest_cert.user_public_key),
            invitee_name: Some("Guest".into()),
            expires_at_ns: now_ns.saturating_add(10_000_000_000),
            transport: vec![],
        },
        authority_key,
    )
    .unwrap();
    let join = crate::join::decode_and_verify_invitation(
        &signed,
        Some(guest_cert.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &join,
        guest_cert,
        [nonce.wrapping_add(1); 32],
        [nonce.wrapping_add(2); 32],
        now_ns.saturating_add(1_000_000_000),
        now_ns,
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), guest_key).unwrap();
    verify_join_proof(
        &proof,
        &challenge,
        guest_cert,
        &RevocationList::new(),
        now_ns,
    )
    .unwrap()
}

#[test]
fn a_banned_person_cannot_redeem_an_invitation_until_unbanned() {
    let (mut peer, authority_key, _, _, now) = seeded();
    let guest_root = SigningKey::from_bytes(&[61; 32]);
    let guest_key = SigningKey::from_bytes(&[62; 32]);
    let mut guest_cert = DeviceCert {
        user_public_key: guest_root.verifying_key().to_bytes(),
        device_public_key: guest_key.verifying_key().to_bytes(),
        device_label: "Guest".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut guest_cert, &guest_root).unwrap();
    let first = guest_redemption(&peer, &authority_key, &guest_key, &guest_cert, 70, now);
    let unused = guest_redemption(&peer, &authority_key, &guest_key, &guest_cert, 80, now);
    assert!(
        peer.governance_mut()
            .admit_invited_member(&first, &authority_key, now)
            .unwrap()
    );
    let owner = hex::encode(peer.authority().user_public_key);
    assert!(
        peer.governance_mut()
            .ban_member(guest_cert.user_public_key, &owner, &authority_key, now)
            .unwrap()
    );
    assert!(peer.permissions(guest_cert.device_public_key, now).is_err());
    assert!(
        !peer
            .governance()
            .governed_access(now)
            .unwrap()
            .member_user_roots
            .contains(&guest_cert.user_public_key)
    );
    assert!(
        peer.governance_mut()
            .admit_invited_member(&unused, &authority_key, now)
            .is_err(),
        "an invitation issued before the ban does not let them back"
    );
    assert!(
        peer.governance_mut()
            .unban_member(guest_cert.user_public_key, &authority_key, now)
            .unwrap()
    );
    assert!(
        peer.governance_mut()
            .admit_invited_member(&unused, &authority_key, now)
            .unwrap()
    );
    assert_eq!(
        peer.permissions(guest_cert.device_public_key, now)
            .unwrap()
            .role,
        collab_types::Role::Editor
    );
}

/// The governor records when the room admitted a person, once: a role
/// change keeps it, so the numbers namesakes are told apart by never shift,
/// and removal forgets it.
#[test]
fn admission_records_when_a_member_joined_and_a_role_change_keeps_it() {
    let (mut peer, authority_key, _, _, now) = seeded();
    let (_, root) = add_editor(&mut peer, &authority_key, now);
    let person = hex::encode(root);
    let joined = |peer: &StatePeer| {
        let NeighbourMsg::SyncResp { changes, .. } = peer.room_message() else {
            panic!("unexpected fixture shape")
        };
        let doc = crate::room::candidate(None, &changes).unwrap();
        crate::room::extract_governed_fields(&doc)
            .1
            .joined
            .get(&person)
            .copied()
    };
    let at_ms = u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    assert_eq!(joined(&peer), Some(at_ms));

    let later = now + Duration::from_secs(60);
    assert!(
        peer.governance_mut()
            .set_member_role(root, crate::join::MemberRole::Viewer, &authority_key, later)
            .unwrap()
    );
    assert_eq!(joined(&peer), Some(at_ms), "a role change keeps it");

    assert!(
        peer.governance_mut()
            .remove_member(root, &authority_key, later)
            .unwrap()
    );
    assert_eq!(joined(&peer), None);
}
