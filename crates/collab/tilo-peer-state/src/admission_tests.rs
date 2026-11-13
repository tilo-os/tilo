use super::*;
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::{Signer as _, SigningKey};
use tilo_action_policy::Change;

mod editor;
use crate::join::WorkspaceId;
use editor::owner_signs;
pub(crate) use editor::{other_owner, seeded_editor};
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
            issuer: None,
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
    crate::workspace::POLICY_READS.with(|count| count.set(0));
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
        crate::workspace::POLICY_READS.with(std::cell::Cell::get) <= 1,
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
    use tilo_action_policy::{ActionKind, Answer, Grant, Grantee, KindSet, Lifetime, TreePath};
    let (key, cert, _, other, now) = fixtures();
    let workspace_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut doc = crate::workspace::candidate(
        None,
        &seed_workspace(workspace_id, &cert, &[cert.clone(), other.clone()]).unwrap(),
    )
    .unwrap();
    let owner = other_owner();
    doc.put(ROOT, "owner", hex::encode(owner.1.user_public_key))
        .unwrap();
    let person = hex::encode(cert.user_public_key);
    // A time-limited hold on undoable changes, rechecked at every decision.
    let hold = Grant::new(
        "hold",
        Grantee::person(person.clone()),
        TreePath::workspace(workspace_id),
        KindSet::UNDOABLE,
        Answer::Deny,
        Grantee::person(hex::encode(other_owner().1.user_public_key)),
    )
    .with_lifetime(Lifetime::Until(2_001_000));
    let editor = collab_types::Role::Editor;
    let changes = vec![
        Change::AddMember {
            person: person.clone(),
            role: editor,
        },
        Change::Grant(hold),
    ];
    owner_signs(&mut doc, &owner, now, changes);
    let changes = doc
        .get_changes(&[])
        .into_iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    let mut peer = StatePeer::new(
        workspace_id,
        cert.clone(),
        vec![cert.clone(), other],
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.install_seed(
        SeedState {
            workspace_changes: changes,
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
    let workspace_node = TreePath::workspace(workspace_id);
    assert!(
        !peer
            .permissions(cert.device_public_key, now)
            .unwrap()
            .allows(&workspace_node, ActionKind::Undoable, now)
    );
    assert!(
        peer.permissions(cert.device_public_key, now + Duration::from_secs(2))
            .unwrap()
            .allows(
                &workspace_node,
                ActionKind::Undoable,
                now + Duration::from_secs(2)
            )
    );
    let old = peer.clone();
    owner_signs(&mut doc, &owner, now, vec![Change::RemoveMember { person }]);
    let changes = doc
        .get_changes(&[])
        .into_iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    peer.receive(
        cert.device_public_key,
        NeighbourMsg::SyncResp {
            changes,
            room: peer.workspace_id().to_owned(),
        },
        now,
    )
    .unwrap();
    assert!(matches!(
        peer.permissions(cert.device_public_key, now),
        Err(MembershipError::NoMembership)
    ));
    assert_eq!(
        old.permissions(cert.device_public_key, now).unwrap().role,
        collab_types::Role::Editor
    );
}
#[test]
fn app_waits_for_governance_then_signed_state_converges_without_reexecuting_action() {
    let (a, ac, _, bc, now) = fixtures();
    let workspace = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut native = StatePeer::new(
        workspace,
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
        workspace_changes: seed_workspace(workspace, &ac, &[ac.clone(), bc.clone()]).unwrap(),
        documents: vec![SeedDocument {
            app_id: "test.app".into(),
            record_id: "tab:1".into(),
            kind: "test".into(),
            bytes: document.save(),
        }],
    };
    native.install_seed(seed, &a, now).unwrap();
    let mut browser = StatePeer::new(
        workspace,
        ac.clone(),
        vec![ac, bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
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
        .receive(
            a.verifying_key().to_bytes(),
            native.workspace_message(),
            now,
        )
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
        workspace,
        native.owner_certificates()[0].clone(),
        vec![native.owner_certificates()[0].clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert_eq!(restored.rows(), browser.rows());
}
#[test]
fn source_workspace_signature_and_aged_checkpoint_fail_closed() {
    let (a, ac, _, bc, now) = fixtures();
    let workspace = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut native = StatePeer::new(
        workspace,
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
                workspace_changes: seed_workspace(workspace, &ac, &[ac.clone(), bc.clone()])
                    .unwrap(),
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
        workspace,
        ac.clone(),
        vec![ac, bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    receiver
        .receive(
            a.verifying_key().to_bytes(),
            native.workspace_message(),
            now,
        )
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

/// Invite a second person into `peer` as an Editor with one device, as the
/// governor `authority`: their device certificate and root.
pub(crate) fn add_editor(
    peer: &mut StatePeer,
    authority: &SigningKey,
    now: SystemTime,
) -> (DeviceCert, [u8; 32]) {
    let (_, cert, person) = add_member_with(peer, authority, now, 5, 6);
    (cert, person)
}

/// Invite the person rooted at `root_seed`, with one device from
/// `device_seed`, into `peer` as an Editor, as the governor `authority`: their
/// device key, certificate and person root.
pub(crate) fn add_member_with(
    peer: &mut StatePeer,
    authority: &SigningKey,
    now: SystemTime,
    root_seed: u8,
    device_seed: u8,
) -> (SigningKey, DeviceCert, [u8; 32]) {
    use crate::join::{InviteGrant, JoinChallenge, Role, issue_invitation, sign_join_proof};
    let root = SigningKey::from_bytes(&[root_seed; 32]);
    let device = SigningKey::from_bytes(&[device_seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: format!("Editor {root_seed}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let now_ns = u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
    let owner = peer
        .owner_certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == authority.verifying_key().to_bytes())
        .unwrap();
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
            issuer: owner,
            role: Role::Editor,
            nonce: [device_seed.wrapping_add(1); 16],
            recipient_user_public_key: Some(cert.user_public_key),
            invitee_name: Some(format!("Editor {root_seed}")),
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
        [device_seed.wrapping_add(2); 32],
        [device_seed.wrapping_add(3); 32],
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
    (device, cert, root.verifying_key().to_bytes())
}

pub(crate) fn seeded() -> (StatePeer, SigningKey, SigningKey, DeviceCert, SystemTime) {
    let (a, ac, b, bc, now) = fixtures();
    let workspace = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut native = StatePeer::new(
        workspace,
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
                workspace_changes: seed_workspace(workspace, &ac, &[ac.clone(), bc.clone()])
                    .unwrap(),
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
        InviteGrant, JoinChallenge, Role, issue_invitation, sign_join_proof, verify_join_proof,
    };

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
        issuer: None,
    };
    sign_device_cert(&mut guest_cert, &guest_root).unwrap();
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
            issuer: peer.owner_certificates()[0].clone(),
            role: Role::Editor,
            nonce: [43; 16],
            recipient_user_public_key: Some(guest_cert.user_public_key),
            invitee_name: Some("Independent guest".into()),
            expires_at_ns: now_ns.saturating_add(10_000_000_000),
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
        now_ns.saturating_add(1_000_000_000),
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
            .unwrap(),
        "any of the owner's devices admits, not only the one that issued"
    );
    assert!(
        !peer
            .governance_mut()
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
            .contains(&peer.owner_certificates()[0].device_public_key)
    );
    assert!(
        access
            .member_user_roots
            .contains(&guest_cert.user_public_key)
    );
    assert!(access.member_user_roots.contains(&peer.owner()));
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
        now_ns.saturating_add(1_000_000_000),
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
        peer.workspace_id(),
        peer.owner_certificates()[0].clone(),
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
        issuer: None,
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
        now_ns.saturating_add(1_000_000_000),
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
    assert!(
        restored
            .governance_mut()
            .set_member_role(
                guest_cert.user_public_key,
                Role::Commenter,
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
        collab_types::Role::Commenter
    );
    assert!(
        restored
            .governance_mut()
            .set_member_role(
                guest_cert.user_public_key,
                Role::Viewer,
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
            WorkspaceId::parse(restored.workspace_id()).unwrap(),
            &restored.owner_certificates()[0],
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
                WorkspaceId::parse(restored.workspace_id()).unwrap(),
                &restored.owner_certificates()[0],
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
            .remove_member(peer.owner(), &authority_key, now)
            .is_err(),
        "the owner cannot be removed through member governance"
    );
}

#[test]
fn concurrent_redeemers_cannot_merge_two_receipts_for_one_invitation() {
    use crate::join::{
        InviteGrant, JoinChallenge, Role, decode_and_verify_invitation, issue_invitation,
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
            workspace_id: WorkspaceId::parse(base.workspace_id()).unwrap(),
            issuer: base.owner_certificates()[0].clone(),
            role: Role::Editor,
            nonce: [70; 16],
            recipient_user_public_key: None,
            invitee_name: None,
            expires_at_ns: now_ns.saturating_add(10_000_000_000),
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
            issuer: None,
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

    let authority = base.owner_certificates()[0].device_public_key;
    let mut receiver = base;
    receiver
        .receive_from(
            TransportOrigin::RelayDelta(authority),
            left.workspace_message(),
            now,
        )
        .unwrap();
    let accepted = receiver.durable().unwrap();
    let error = receiver
        .receive_from(
            TransportOrigin::RelayDelta(authority),
            right.workspace_message(),
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
        InviteGrant, JoinChallenge, Role, decode_and_verify_invitation, issue_invitation,
        sign_join_proof, verify_join_proof,
    };
    let (mut base, authority_key, delegate_key, _, now) = seeded();
    let NeighbourMsg::SyncResp { changes, room } = base.workspace_message() else {
        panic!("unexpected fixture shape")
    };
    let mut legacy = crate::workspace::candidate(None, &changes).unwrap();
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
        issuer: None,
    };
    sign_device_cert(&mut guest, &guest_root).unwrap();
    let grant = |nonce, issuer| InviteGrant {
        workspace_id: WorkspaceId::parse(base.workspace_id()).unwrap(),
        issuer,
        role: Role::Editor,
        nonce: [nonce; 16],
        recipient_user_public_key: Some(guest.user_public_key),
        invitee_name: None,
        expires_at_ns: now_ns.saturating_add(10_000_000_000),
        transport: vec![],
    };
    let signed_a = issue_invitation(
        grant(83, base.owner_certificates()[0].clone()),
        &authority_key,
    )
    .unwrap();
    let signed_b = issue_invitation(grant(84, delegate), &delegate_key).unwrap();
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

    let authority = base.owner_certificates()[0].device_public_key;
    let mut merged = base;
    merged
        .receive_from(
            TransportOrigin::RelayDelta(authority),
            left.workspace_message(),
            now,
        )
        .unwrap();
    merged
        .receive_from(
            TransportOrigin::RelayDelta(authority),
            right.workspace_message(),
            now,
        )
        .unwrap();
    let mut restored = StatePeer::restore(
        &merged.durable().unwrap(),
        merged.workspace_id(),
        merged.owner_certificates()[0].clone(),
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

/// A governance change is its writer's role's, not its person's: a member the
/// workspace makes an administrator writes a host, their device records the
/// change as the operation naming the rows it wrote, and another receiver takes
/// it as it takes the owner's. A change the writer's role does not permit is
/// refused and lands nowhere.
#[test]
fn a_members_governance_change_is_its_roles_and_another_receiver_takes_it() {
    use crate::join::TransportHint;
    use crate::members::MembershipOpCommand;
    use crate::workspace::GovernanceRow;
    use tilo_action_policy::{Grantee, roles};

    let (mut peer, authority_key, _, _, now) = seeded();
    let (editor_key, editor_cert, editor_person) =
        add_member_with(&mut peer, &authority_key, now, 5, 6);
    let hint = TransportHint {
        kind: "iroh".into(),
        opaque_json: "an address".into(),
        host: editor_cert.clone(),
        service_public_key: [3; 32],
        host_signature: [4; 64],
    };
    let held = crate::members::ops(peer.admitted_workspace().unwrap()).len();
    assert!(
        peer.governance_mut()
            .author_workspace_host(&hint, &editor_key, now)
            .is_err(),
        "an editor holds no governance grant"
    );
    assert!(peer.governance().workspace_hosts().is_empty());
    assert_eq!(
        crate::members::ops(peer.admitted_workspace().unwrap()).len(),
        held,
        "a refused change lands nowhere"
    );
    let person = hex::encode(peer.owner());
    let workspace = peer.workspace_id().to_owned();
    assert!(
        peer.governance_mut()
            .put_governed_grants(
                &[roles::administrator_grant(
                    &workspace,
                    &hex::encode(editor_person),
                    Grantee::person(person),
                )],
                &authority_key,
                now,
            )
            .unwrap()
    );
    let mut receiver = peer.clone();
    let heads = peer.admitted_workspace().unwrap().clone().get_heads();
    assert!(
        peer.governance_mut()
            .author_workspace_host(&hint, &editor_key, now)
            .unwrap()
    );
    let named = crate::members::ops(peer.admitted_workspace().unwrap())
        .into_values()
        .find_map(|op| match &op.command {
            MembershipOpCommand::Governance { rows } if rows == &[GovernanceRow::Hosts] => {
                Some(op.person())
            }
            _ => None,
        })
        .expect("the change names the rows it wrote");
    assert_eq!(named, hex::encode(editor_person));
    let changed = peer
        .admitted_workspace()
        .unwrap()
        .clone()
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    receiver
        .receive_from(
            TransportOrigin::RelayDelta(editor_cert.device_public_key),
            NeighbourMsg::SyncResp {
                changes: changed,
                room: workspace,
            },
            now,
        )
        .unwrap();
    assert_eq!(
        receiver.governance().workspace_hosts(),
        peer.governance().workspace_hosts(),
        "a receiver takes the member's governance change as it takes the owner's"
    );
}

/// Two participants may redeem one invitation at once, each before it holds
/// the other's receipt. An invitation admits one person: every receiver keeps
/// the redemption that reached it first and refuses the other, whichever of
/// the two arrives first.
#[test]
fn concurrent_receipts_for_one_invite_keep_the_redemption_that_reached_it_first() {
    let (base, authority_key, _, _, now) = seeded();
    let NeighbourMsg::SyncResp {
        changes,
        room: workspace,
    } = base.workspace_message()
    else {
        panic!("unexpected fixture shape")
    };
    let mut document = crate::workspace::candidate(None, &changes).unwrap();
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
        role: crate::join::Role::Viewer,
    };
    let ledgered = |receipt| crate::state::LedgerReceipt {
        receipt,
        invitation: vec![],
    };
    let ledgered = |receipt| ledgered(receipt).encode().unwrap();
    left.set_actor(automerge::ActorId::from(vec![98; 32]));
    left.put(
        &invitation_receipts,
        hex::encode(invite_id),
        automerge::ScalarValue::Bytes(ledgered(receipt([99; 32], [100; 32]))),
    )
    .unwrap();
    left.commit_edit("fixture", "fixture");
    right.set_actor(automerge::ActorId::from(vec![101; 32]));
    right
        .put(
            &invitation_receipts,
            hex::encode(invite_id),
            automerge::ScalarValue::Bytes(ledgered(receipt([102; 32], [103; 32]))),
        )
        .unwrap();
    right.commit_edit("fixture", "fixture");
    let message = |mut branch: AutoCommit| NeighbourMsg::SyncResp {
        changes: branch
            .get_changes(&base_heads)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
        room: workspace.clone(),
    };
    let from = TransportOrigin::RelayDelta(authority_key.verifying_key().to_bytes());
    let receive = |first: NeighbourMsg, second: NeighbourMsg| {
        let mut receiver = base.clone();
        receiver.receive_from(from, first, now).unwrap();
        let refused = receiver.receive_from(from, second, now).unwrap_err();
        assert!(
            refused.contains("conflicting redemption"),
            "the second redemption of one invitation must be refused: {refused}"
        );
        let held = receiver.workspace_message();
        let NeighbourMsg::SyncResp { changes, .. } = held else {
            panic!("workspace message is a SyncResp")
        };
        crate::workspace::candidate(None, &changes).unwrap()
    };
    for redemption in [
        receive(message(left.clone()), message(right.clone())),
        receive(message(right), message(left)),
    ] {
        let held = crate::workspace::invitation_receipts(&redemption).unwrap();
        assert_eq!(held[&hex::encode(invite_id)].len(), 1);
    }
}

#[test]
fn retained_relay_replay_accepts_certified_document_after_live_freshness_window() {
    let (mut governor, authority, _, browser_cert, authored_at) = seeded();
    let replayed_at = authored_at + Duration::from_secs(301);
    let mut browser = StatePeer::new(
        governor.workspace_id(),
        governor.owner_certificates()[0].clone(),
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
        native.workspace_id(),
        native.owner_certificates()[0].clone(),
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
        native.workspace_id(),
        native.owner_certificates()[0].clone(),
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
        native.workspace_id(),
        native.owner_certificates()[0].clone(),
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
        native.workspace_id(),
        native.owner_certificates()[0].clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
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
    let NeighbourMsg::SyncResp {
        changes,
        room: workspace,
    } = messages[0].clone()
    else {
        panic!("unexpected fixture shape")
    };
    let mut doc = workspace::candidate(None, &changes).unwrap();
    doc.delete(ROOT, coverage::FIELD_V2).unwrap();
    doc.commit_edit("fixture", "fixture");
    let legacy = NeighbourMsg::SyncResp {
        room: workspace.clone(),
        changes: doc
            .get_changes(&[])
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect(),
    };
    let mut receiver = StatePeer::new(
        &workspace,
        native.owner_certificates()[0].clone(),
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
    let mut candidate = workspace::candidate(None, &changes).unwrap();
    let mut rows = coverage::entries(&candidate).unwrap();
    let entry = rows.values_mut().next().unwrap();
    entry.sequence = 0;
    let regressed = entry.clone();
    coverage::put(&mut candidate, regressed).unwrap();
    assert!(
        coverage::monotonic(
            Some(&workspace::candidate(None, &changes).unwrap()),
            &candidate
        )
        .is_err()
    );
}
#[test]
fn fresh_same_head_checkpoint_is_still_a_durable_watermark_mutation() {
    let (mut native, a, _, bc, now) = seeded();
    let mut receiver = StatePeer::new(
        native.workspace_id(),
        native.owner_certificates()[0].clone(),
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
        native.workspace_id(),
        native.owner_certificates()[0].clone(),
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
        native.workspace_id(),
        native.owner_certificates()[0].clone(),
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
        issuer: None,
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
        native.workspace_id(),
        native.owner_certificates()[0].clone(),
        vec![native.owner_certificates()[0].clone(), browser_cert],
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
        native.workspace_id(),
        native.owner_certificates()[0].clone(),
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
        native.workspace_id(),
        native.owner_certificates()[0].clone(),
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
                cert: Box::new(native.owner_certificates()[0].clone()),
                revocations: vec![revocation],
            },
            now,
        )
        .unwrap();
    assert!(learned.metadata_changed);
    assert!(
        matches!(
            receiver.permissions(bc.device_public_key, now),
            Err(MembershipError::Revoked)
        ),
        "external revocation must take effect with a warm policy cache"
    );
    let mut restored = StatePeer::restore(
        &receiver.durable().unwrap(),
        native.workspace_id(),
        native.owner_certificates()[0].clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(matches!(
        restored.permissions(b.verifying_key().to_bytes(), now),
        Err(MembershipError::Revoked)
    ));
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
            native.workspace_id(),
            native.owner_certificates()[0].clone(),
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
        // Install exactly the workspace we will drain again: the existing path then
        // has no changed workspace heads, only the deferred metadata obligation.
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
            native.workspace_id(),
            native.owner_certificates()[0].clone(),
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
        peer.workspace_id(),
        peer.owner_certificates()[0].clone(),
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
        receiver.workspace_id(),
        receiver.owner_certificates()[0].clone(),
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
    let (mut governor, a, _, _, now) = seeded();
    let root = SigningKey::from_bytes(&[1; 32]);
    let new_key = SigningKey::from_bytes(&[8; 32]);
    let stranger = SigningKey::from_bytes(&[0x5a; 32]);
    let mut cert = governor.owner_certificates()[0].clone();
    cert.device_public_key = new_key.verifying_key().to_bytes();
    cert.device_label = "dynamic client".into();
    sign_device_cert(&mut cert, &root).unwrap();
    let mut existing = governor.clone();
    let before = governor.durable().unwrap();
    assert!(
        governor
            .admit_certificate(cert.clone(), &stranger, now)
            .is_err()
    );
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
            governor.workspace_message(),
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
        existing.workspace_id(),
        existing.owner_certificates()[0].clone(),
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
    let mut cert = governor.owner_certificates()[0].clone();
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
        matches!(
            existing.permissions(cert.device_public_key, later),
            Err(MembershipError::Certificate(
                tilo_user_identity::DeviceCertError::Expired
            ))
        ),
        "a warm policy cache must not cache certificate validity"
    );
    let mut next = governor
        .owner_certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == a.verifying_key().to_bytes())
        .unwrap();
    next.device_public_key = SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes();
    sign_device_cert(&mut next, &root).unwrap();
    assert!(governor.admit_certificate(next.clone(), &a, later).unwrap());
    assert!(
        existing
            .receive(
                a.verifying_key().to_bytes(),
                governor.workspace_message(),
                later
            )
            .unwrap()
            .metadata_changed
    );
    assert!(matches!(
        existing.permissions(cert.device_public_key, later),
        Err(MembershipError::Certificate(
            tilo_user_identity::DeviceCertError::Expired
        ))
    ));
    assert!(existing.permissions(next.device_public_key, later).is_ok());
    let restored = StatePeer::restore(
        &existing.durable().unwrap(),
        existing.workspace_id(),
        existing.owner_certificates()[0].clone(),
        vec![],
        RevocationList::new(),
        later,
    )
    .unwrap();
    assert!(
        matches!(
            restored.permissions(cert.device_public_key, later),
            Err(MembershipError::Certificate(
                tilo_user_identity::DeviceCertError::Expired
            ))
        ),
        "expired historical certificates restore but never regain current access"
    );
    let mut empty = StatePeer::new(
        governor.workspace_id(),
        next.clone(),
        vec![],
        RevocationList::new(),
        later,
    )
    .unwrap();
    assert!(
        empty
            .receive(
                a.verifying_key().to_bytes(),
                governor.workspace_message(),
                later
            )
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
            issuer: None,
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
        peer.workspace_id(),
        peer.owner_certificates()[0].clone(),
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
        peer.workspace_id(),
        peer.owner_certificates()[0].clone(),
        vec![backwards],
        RevocationList::new(),
        now,
    )
    .err()
    .unwrap();
    assert!(error.contains("invalid validity"), "{error}");
}

#[test]
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
        issuer: None,
    };
    sign_device_cert(&mut authority, &root).unwrap();
    let workspace = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    let mut peer = StatePeer::new(
        workspace,
        authority.clone(),
        vec![authority.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(workspace, &authority, &[authority.clone()]).unwrap(),
            documents: vec![],
        },
        &authority_key,
        now,
    )
    .unwrap();

    let restored = StatePeer::restore(
        &peer.durable().unwrap(),
        workspace,
        authority.clone(),
        vec![],
        RevocationList::new(),
        later,
    )
    .unwrap();
    assert!(restored.certificates().contains(&authority));
    assert!(matches!(
        restored.permissions(authority.device_public_key, later),
        Err(MembershipError::Certificate(
            tilo_user_identity::DeviceCertError::Expired
        ))
    ));
}
#[test]
fn governed_import_is_atomic_even_for_direct_admission() {
    use automerge::ReadDoc;
    let root = SigningKey::from_bytes(&[1; 32]);
    let (mut source, a, _, _, now) = seeded();
    let mut receiver = source.clone();
    let mut valid = source.owner_certificates()[0].clone();
    valid.device_public_key = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
    sign_device_cert(&mut valid, &root).unwrap();
    source.admit_certificate(valid, &a, now).unwrap();
    let NeighbourMsg::SyncResp { changes, room } = source.workspace_message() else {
        panic!("unexpected fixture shape")
    };
    let mut candidate = workspace::candidate(None, &changes).unwrap();
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
    let workspace = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut native = StatePeer::new(
        workspace,
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
                workspace_changes: seed_workspace(workspace, &ac, &[ac.clone(), bc.clone()])
                    .unwrap(),
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
        workspace,
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
        StatePeer::restore(&bytes, workspace, ac, vec![bc], RevocationList::new(), now).unwrap();
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

#[path = "copy_on_write_tests.rs"]
pub(crate) mod copy_on_write;
#[path = "delivery_admission_tests.rs"]
mod delivery_admission;
#[path = "workspace_refusal_tests.rs"]
mod refusal;
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
        InviteGrant, JoinChallenge, Role, issue_invitation, sign_join_proof, verify_join_proof,
    };
    let now_ns: u64 = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap();
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
            issuer: peer.owner_certificates()[0].clone(),
            role: Role::Editor,
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
        issuer: None,
    };
    sign_device_cert(&mut guest_cert, &guest_root).unwrap();
    let first = guest_redemption(&peer, &authority_key, &guest_key, &guest_cert, 70, now);
    let unused = guest_redemption(&peer, &authority_key, &guest_key, &guest_cert, 80, now);
    assert!(
        peer.governance_mut()
            .admit_invited_member(&first, &authority_key, now)
            .unwrap()
    );
    let owner = hex::encode(peer.owner());
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

/// The governor records when the workspace admitted a person, once: a role
/// change keeps it, so the numbers namesakes are told apart by never shift,
/// and removal forgets it.
#[test]
fn admission_records_when_a_member_joined_and_a_role_change_keeps_it() {
    let (mut peer, authority_key, _, _, now) = seeded();
    let (_, root) = add_editor(&mut peer, &authority_key, now);
    let person = hex::encode(root);
    let joined = |peer: &StatePeer| {
        let NeighbourMsg::SyncResp { changes, .. } = peer.workspace_message() else {
            panic!("unexpected fixture shape")
        };
        let doc = crate::workspace::candidate(None, &changes).unwrap();
        crate::workspace::extract_governed_fields(&doc)
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
            .set_member_role(root, crate::join::Role::Viewer, &authority_key, later)
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

/// A record the workspace covers as empty is held by the replica the
/// governance reaches, though no one can send it: an empty record carries no
/// change to replay. The coverage row names exactly that empty checkpoint, so
/// the replica holds the record and a member's write builds on it.
#[test]
fn a_record_covered_as_empty_is_held_from_its_coverage() {
    let (mut native, a_key, b_key, b_cert, now) = seeded();
    let empty = AutoCommit::new().save();
    // The governor checkpoints an empty record: the coverage row it writes
    // names the route with no heads and the hash of that empty snapshot.
    native
        .publication_mut()
        .publish_document("test.app", "tab:2", empty.clone(), &a_key, now)
        .unwrap();
    let messages = native
        .publication_mut()
        .bootstrap_messages(&a_key, now)
        .unwrap();
    let NeighbourMsg::SyncResp { changes, room } = messages[0].clone() else {
        panic!("unexpected fixture shape")
    };
    let mut joiner = StatePeer::new(
        &room,
        native.owner_certificates()[0].clone(),
        vec![b_cert],
        RevocationList::new(),
        now,
    )
    .unwrap();
    // Only the governance arrives.
    joiner
        .receive_from(
            TransportOrigin::RelayDelta(a_key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp { changes, room },
            now,
        )
        .unwrap();
    assert_eq!(
        joiner.document("test.app", "tab:2").as_deref(),
        Some(empty.as_slice()),
        "the replica holds the empty record the governance covers"
    );
    assert!(
        joiner
            .document_heads("test.app", "tab:2")
            .unwrap()
            .is_empty()
    );
    assert!(!joiner.checkpoint_pending("test.app", "tab:2"));
    assert_eq!(
        joiner.document("test.app", "tab:1"),
        None,
        "a covered record with changes is not made up"
    );
    // Held, not carved in stone: the member's write builds on it.
    let mut doc = AutoCommit::load(&joiner.document("test.app", "tab:2").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        b_key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, "note", "mine").unwrap();
    doc.commit_edit("fixture", "note");
    joiner
        .publication_mut()
        .publish_incremental("test.app", "tab:2", doc.save_after(&heads), &b_key, now)
        .unwrap();
    assert_eq!(
        joiner.document_heads("test.app", "tab:2").unwrap().len(),
        1,
        "the member's write lands on the record"
    );
}

/// A frame `author` signs as `key`: one change to its record, as a device
/// publishes it.
fn frame(author: &mut StatePeer, key: &SigningKey, now: SystemTime, name: &str) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&author.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, name, name).unwrap();
    doc.commit_edit("fixture", "fixture");
    author
        .publication_mut()
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), key, now)
        .unwrap()
}

/// A guest of the workspace, admitted through an invitation the owner's device
/// issued: their root and the device key it signs.
fn admitted_guest(
    peer: &mut StatePeer,
    authority_key: &SigningKey,
    now: SystemTime,
) -> (SigningKey, DeviceCert) {
    use crate::join::{
        InviteGrant, JoinChallenge, Role, issue_invitation, sign_join_proof, verify_join_proof,
    };
    let now_ns = ns_since_epoch(now);
    let root = SigningKey::from_bytes(&[61; 32]);
    let key = SigningKey::from_bytes(&[62; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Guest".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
            issuer: peer.owner_certificates()[0].clone(),
            role: Role::Editor,
            nonce: [63; 16],
            recipient_user_public_key: Some(cert.user_public_key),
            invitee_name: Some("Guest".into()),
            expires_at_ns: now_ns.saturating_add(10_000_000_000),
            transport: vec![],
        },
        authority_key,
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
        [64; 32],
        [65; 32],
        now_ns.saturating_add(1_000_000_000),
        now_ns,
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), &key).unwrap();
    let redemption =
        verify_join_proof(&proof, &challenge, &cert, &RevocationList::new(), now_ns).unwrap();
    assert!(
        peer.governance_mut()
            .admit_invited_member(&redemption, authority_key, now)
            .unwrap()
    );
    (key, cert)
}

fn ns_since_epoch(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap()
}

/// A removed member's device keeps its keys, and nothing it writes after the
/// removal counts: a frame it publishes is refused however it signs it, and a
/// membership operation it writes has no effect even where the workspace's own
/// devices relay it. What it wrote before the removal stays.
#[test]
fn a_removed_members_device_writes_nothing_after_its_removal() {
    let (mut peer, authority_key, _, _, now) = seeded();
    let (guest_key, guest) = admitted_guest(&mut peer, &authority_key, now);
    let origin = TransportOrigin::RelayDelta(guest_key.verifying_key().to_bytes());

    // The guest's device publishes a frame while it is a member.
    let mut guest_peer = peer.clone();
    let before = frame(&mut guest_peer, &guest_key, now, "before");
    peer.receive_from(origin, before, now)
        .expect("a member's frame is admitted");

    assert!(
        peer.governance_mut()
            .remove_member(guest.user_public_key, &authority_key, now)
            .unwrap()
    );

    // The same device, with the same keys, writes a frame after the removal.
    let later = now + Duration::from_secs(30);
    let after = frame(&mut guest_peer, &guest_key, later, "after");
    assert!(
        peer.receive_from(origin, after, later).is_err(),
        "a removed device's frame is refused"
    );
    let held = AutoCommit::load(&peer.document("test.app", "tab:1").unwrap()).unwrap();
    assert!(
        held.get(ROOT, "after").unwrap().is_none(),
        "the refused frame wrote nothing"
    );
    assert!(
        held.get(ROOT, "before").unwrap().is_some(),
        "the frame admitted before the removal stays"
    );

    // A membership operation the removed device writes is relayed by the
    // workspace's own devices and kept: replay gives it no effect, because its
    // author is no longer a member.
    let newcomer = SigningKey::from_bytes(&[66; 32]);
    let mut newcomer_cert = DeviceCert {
        user_public_key: newcomer.verifying_key().to_bytes(),
        device_public_key: SigningKey::from_bytes(&[67; 32]).verifying_key().to_bytes(),
        device_label: "Newcomer".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut newcomer_cert, &newcomer).unwrap();
    let NeighbourMsg::SyncResp { changes, room } = peer.workspace_message() else {
        panic!("unexpected fixture shape");
    };
    let mut workspace_doc = crate::workspace::candidate(None, &changes).unwrap();
    let op = crate::members::SignedMembershipOp::signed(
        &guest,
        &guest_key,
        crate::members::heads(&crate::members::ops(&workspace_doc)),
        ns_since_epoch(later),
        crate::members::MembershipOpCommand::Project {
            changes: vec![tilo_action_policy::Change::AddMember {
                person: hex::encode(newcomer_cert.user_public_key),
                role: tilo_action_policy::Role::Viewer,
            }],
        },
    )
    .unwrap();
    let heads = workspace_doc.get_heads();
    crate::members::put_op(&mut workspace_doc, &op).unwrap();
    peer.receive_from(
        TransportOrigin::RelayDelta(authority_key.verifying_key().to_bytes()),
        NeighbourMsg::SyncResp {
            changes: workspace_doc
                .get_changes(&heads)
                .into_iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect(),
            room,
        },
        later,
    )
    .expect("the workspace's device relays the operation");
    let held = peer.workspace_message();
    let NeighbourMsg::SyncResp { changes, .. } = held else {
        panic!("unexpected fixture shape");
    };
    let held = crate::workspace::candidate(None, &changes).unwrap();
    assert!(
        crate::members::ops(&held)
            .values()
            .any(|held| held.id().unwrap() == op.id().unwrap()),
        "the operation is kept"
    );
    assert_eq!(
        crate::members::replay(&held, peer.workspace_id())
            .role_of(&hex::encode(newcomer_cert.user_public_key)),
        None,
        "and has no effect"
    );
    assert!(
        peer.permissions(newcomer_cert.device_public_key, later)
            .is_err()
    );
}
