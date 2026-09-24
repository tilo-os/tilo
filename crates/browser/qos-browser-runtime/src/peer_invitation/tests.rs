use super::*;
use automerge::transaction::Transactable;
use ed25519_dalek::SigningKey;
use qos_peer_state::join::{self, InviteGrant, MemberRole};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
const NOW: u64 = 1_000_000_000_000;
const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
fn cert(root: &SigningKey, key: &SigningKey, capabilities: DeviceCapabilities) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "invitation fixture".into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}
struct Fixture {
    root: SigningKey,
    governor_key: SigningKey,
    governor: DeviceCert,
    host_key: SigningKey,
    host: DeviceCert,
}
impl Fixture {
    fn new() -> Self {
        let root = SigningKey::from_bytes(&[1; 32]);
        let governor_key = SigningKey::from_bytes(&[2; 32]);
        let host_key = SigningKey::from_bytes(&[3; 32]);
        let governor = cert(&root, &governor_key, DeviceCapabilities::primary());
        let host = cert(&root, &host_key, DeviceCapabilities::host());
        Self {
            root,
            governor_key,
            governor,
            host_key,
            host,
        }
    }
    fn invitation(&self, recipient: Option<[u8; 32]>) -> String {
        let grant = InviteGrant {
            workspace_id: tilo_relay::room_workspace_id(ROOM).unwrap(),
            room: ROOM.into(),
            authority: self.governor.clone(),
            issuer: self.governor.clone(),
            delegation: None,
            role: MemberRole::Editor,
            nonce: [4; 16],
            recipient_user_public_key: recipient,
            invitee_name: None,
            expires_at_ns: NOW + 600_000_000_000,
            transport: vec![],
        };
        join::encode_invitation_text(&join::issue_invitation(grant, &self.governor_key).unwrap())
    }
}
#[test]
fn invited_material_has_real_distinct_root_signed_device_and_is_stable_on_restore() {
    let f = Fixture::new();
    let text = f.invitation(None);
    let bytes = InvitedIdentity::create(&text, "Browser guest", NOW).unwrap();
    let identity = InvitedIdentity::read(&bytes, NOW).unwrap();
    assert_ne!(
        identity.certificate().user_public_key,
        f.governor.user_public_key
    );
    assert_eq!(
        identity.device_key().verifying_key().to_bytes(),
        identity.certificate().device_public_key
    );
    tilo_user_identity::verify_device_cert_at(identity.certificate(), NOW).unwrap();
    assert_eq!(identity.encode().unwrap(), bytes);
    assert_eq!(
        identity.request_id(),
        InvitedIdentity::read(&bytes, NOW).unwrap().request_id()
    );
}
#[test]
fn invalid_or_recipient_bound_invite_cannot_create_another_person() {
    let f = Fixture::new();
    assert!(InvitedIdentity::create(&f.invitation(Some([7; 32])), "guest", NOW).is_err());
    assert!(InvitedIdentity::create("forged", "guest", NOW).is_err());
    assert!(InvitedIdentity::create(&f.invitation(None), "guest", NOW + 601_000_000_000).is_err());
}
#[test]
fn invited_material_rejects_secret_certificate_scope_and_version_substitution() {
    let f = Fixture::new();
    let bytes = InvitedIdentity::create(&f.invitation(None), "guest", NOW).unwrap();
    for field in ["device_secret", "root_secret", "request_id"] {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value[field] = serde_json::json!(vec![0; 32]);
        assert!(InvitedIdentity::read(&serde_json::to_vec(&value).unwrap(), NOW).is_err());
    }
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["version"] = serde_json::json!(2);
    assert!(InvitedIdentity::read(&serde_json::to_vec(&value).unwrap(), NOW).is_err());
}

#[test]
fn typed_completion_is_stable_across_restore_and_rejects_another_challenge() {
    let f = Fixture::new();
    let bytes = InvitedIdentity::create(&f.invitation(None), "guest", NOW).unwrap();
    let mut identity = InvitedIdentity::read(&bytes, NOW).unwrap();
    let verified = identity.invitation(NOW).unwrap();
    let challenge = join::JoinChallenge::new(
        &verified,
        identity.certificate(),
        identity.request_id(),
        [9; 32],
        NOW + 30_000_000_000,
        NOW,
    )
    .unwrap();
    let checkpoint = identity
        .checkpoint_challenge(challenge.clone(), NOW)
        .unwrap();
    let restored = InvitedIdentity::read(&checkpoint, NOW + 31_000_000_000).unwrap();
    assert_eq!(
        identity.completion().unwrap(),
        restored.completion().unwrap()
    );
    let complete: join::JoinCompleteRequest =
        postcard::from_bytes(identity.completion().unwrap()).unwrap();
    join::verify_join_proof(
        &complete.proof,
        &challenge,
        identity.certificate(),
        &tilo_user_identity::RevocationList::new(),
        NOW,
    )
    .unwrap();
    let mut wrong = challenge.clone();
    wrong.nonce = [10; 32];
    assert!(identity.checkpoint_challenge(wrong, NOW).is_err());
    for mutation in 0..5 {
        let mut identity = InvitedIdentity::read(&bytes, NOW).unwrap();
        let mut wrong = challenge.clone();
        match mutation {
            0 => wrong.request_id = [2; 32],
            1 => wrong.device_public_key = [2; 32],
            2 => wrong.user_public_key = [2; 32],
            3 => wrong.room = "foreign".into(),
            _ => wrong.expires_at_ns = NOW,
        };
        assert!(identity.checkpoint_challenge(wrong, NOW).is_err());
    }
}

fn admitted_fixture() -> (InvitedIdentity, join::JoinBootstrap) {
    admitted_fixture_with_delivery(0, false)
}
fn admitted_fixture_with_delivery(
    locators: usize,
    appointed: bool,
) -> (InvitedIdentity, join::JoinBootstrap) {
    let (identity, bootstrap, _) = admitted_fixture_with_document(locators, appointed, false);
    (identity, bootstrap)
}
fn admitted_fixture_with_document(
    locators: usize,
    appointed: bool,
    document: bool,
) -> (
    InvitedIdentity,
    join::JoinBootstrap,
    qos_peer_state::StatePeer,
) {
    let f = Fixture::new();
    let original = join::decode_and_verify_invitation_text(
        &f.invitation(None),
        None,
        &RevocationList::new(),
        NOW,
    )
    .unwrap();
    let mut grant = original.grant().clone();
    let locator = tilo_relay::workspace_join::DeliveryOriginLocatorV1 {
        version: 1,
        origin: "https://delivery.example".into(),
    }
    .transport_hint(&f.host, &f.host_key)
    .unwrap();
    grant.transport = vec![locator; locators];
    let text =
        join::encode_invitation_text(&join::issue_invitation(grant, &f.governor_key).unwrap());
    let mut identity =
        InvitedIdentity::read(&InvitedIdentity::create(&text, "guest", NOW).unwrap(), NOW).unwrap();
    let challenge = join::JoinChallenge::new(
        &identity.invitation(NOW).unwrap(),
        identity.certificate(),
        identity.request_id(),
        [9; 32],
        NOW + 30_000_000_000,
        NOW,
    )
    .unwrap();
    identity
        .checkpoint_challenge(challenge.clone(), NOW)
        .unwrap();
    let complete: join::JoinCompleteRequest =
        postcard::from_bytes(identity.completion().unwrap()).unwrap();
    let redemption = join::verify_join_proof(
        &complete.proof,
        &challenge,
        identity.certificate(),
        &tilo_user_identity::RevocationList::new(),
        NOW,
    )
    .unwrap();
    let time = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(NOW);
    let mut roster = vec![f.governor.clone()];
    if appointed {
        roster.push(f.host.clone());
    }
    let mut native = qos_peer_state::StatePeer::new(
        ROOM,
        f.governor.clone(),
        roster.clone(),
        tilo_user_identity::RevocationList::new(),
        time,
    )
    .unwrap();
    native
        .install_seed(
            qos_peer_state::SeedState {
                room_changes: qos_peer_state::seed_room(ROOM, &f.governor, &roster).unwrap(),
                documents: if document {
                    let mut doc = automerge::AutoCommit::new();
                    doc.put(automerge::ROOT, "value", "retained native document")
                        .unwrap();
                    doc.commit();
                    vec![qos_peer_state::SeedDocument {
                        app_id: "test.app".into(),
                        record_id: "tab:1".into(),
                        kind: "test".into(),
                        bytes: doc.save(),
                    }]
                } else {
                    vec![]
                },
            },
            &f.governor_key,
            time,
        )
        .unwrap();
    native
        .admit_invited_member(&redemption, &f.governor_key, time)
        .unwrap();
    let bootstrap = join::JoinBootstrap {
        version: 1,
        invite_id: redemption.join().invite_id(),
        device_cert: identity.certificate().clone(),
        role: MemberRole::Editor,
        room: ROOM.into(),
        workspace_id: tilo_relay::room_workspace_id(ROOM).unwrap(),
        authority: f.governor,
        certified_roster: vec![identity.certificate().clone()],
        revocations: vec![],
        records: vec![],
        governance_messages: native
            .bootstrap_messages(&f.governor_key, time)
            .unwrap()
            .iter()
            .map(|m| postcard::to_stdvec(m).unwrap())
            .collect(),
        generation: 1,
    };
    (identity, bootstrap, native)
}

#[test]
fn historical_authenticated_bootstrap_accepts_old_signed_documents_but_live_relay_does_not() {
    let (mut identity, bootstrap, _) = admitted_fixture_with_document(0, false, true);
    let later = NOW + 301_000_000_000;
    let time = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(later);
    // The invitation and current certificates remain valid. Only the retained
    // document's inner timestamp is older than the live relay's five-minute limit.
    identity.invitation(later).unwrap();
    tilo_user_identity::verify_device_cert_at(identity.certificate(), later).unwrap();
    let mut live = qos_peer_state::StatePeer::new(
        ROOM,
        bootstrap.authority.clone(),
        vec![bootstrap.authority.clone()],
        RevocationList::new(),
        time,
    )
    .unwrap();
    let mut old_document = false;
    for bytes in &bootstrap.governance_messages {
        let message: qos_peer_state::NeighbourMsg = postcard::from_bytes(bytes).unwrap();
        let document = matches!(&message, qos_peer_state::NeighbourMsg::AppDocument { .. });
        let result = live.receive_from(
            qos_peer_state::TransportOrigin::RelayDelta(bootstrap.authority.device_public_key),
            message,
            time,
        );
        if document {
            old_document = true;
            assert!(
                result.is_err(),
                "ongoing relay must retain live freshness checks"
            );
        } else {
            result.unwrap();
        }
    }
    assert!(
        old_document,
        "fixture must contain an actual signed document"
    );
    let retained = bootstrap.governance_messages.clone();
    let state = identity
        .admit_bootstrap(&bootstrap, later)
        .expect("authenticated retained bootstrap");
    assert!(state.document("test.app", "tab:1").is_some());
    assert_eq!(
        bootstrap.governance_messages, retained,
        "do not re-sign historical bytes"
    );
    assert!(identity.admitted());
}

#[test]
fn historical_bootstrap_still_requires_current_signed_membership_and_role() {
    let f = Fixture::new();
    let later = NOW + 301_000_000_000;
    let time = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(later);
    for remove in [false, true] {
        let (mut identity, mut bootstrap, mut native) =
            admitted_fixture_with_document(0, false, true);
        if remove {
            native
                .remove_member(
                    identity.certificate().user_public_key,
                    &f.governor_key,
                    time,
                )
                .unwrap();
        } else {
            native
                .set_member_role(
                    identity.certificate().user_public_key,
                    MemberRole::Viewer,
                    &f.governor_key,
                    time,
                )
                .unwrap();
        }
        // Preserve old document bytes, but carry the actual newly committed room
        // governance. Neither the stale bootstrap role nor its roster is authority.
        bootstrap.governance_messages[0] = postcard::to_stdvec(&native.room_message()).unwrap();
        assert!(identity.admit_bootstrap(&bootstrap, later).is_err());
        assert!(!identity.admitted());
        if !remove {
            bootstrap.role = MemberRole::Viewer;
            identity
                .admit_bootstrap(&bootstrap, later)
                .expect("current signed viewer role");
        }
    }
}

#[test]
fn delivery_host_is_independently_appointed_and_need_not_equal_data_governor() {
    let (mut identity, bootstrap) = admitted_fixture_with_delivery(1, true);
    let state = identity.admit_bootstrap(&bootstrap, NOW).unwrap();
    let (host, origin) = identity.delivery_pin(&state, NOW).unwrap().unwrap();
    assert_ne!(
        host.device_public_key,
        bootstrap.authority.device_public_key
    );
    assert_eq!(origin, "https://delivery.example");
    assert!(host.capabilities.can_act_as_host);
    let (mut identity, bootstrap) = admitted_fixture_with_delivery(1, false);
    let state = identity.admit_bootstrap(&bootstrap, NOW).unwrap();
    assert!(
        identity.delivery_pin(&state, NOW).is_err(),
        "a signed locator cannot appoint an absent host"
    );
    let (mut identity, bootstrap) = admitted_fixture_with_delivery(2, true);
    let state = identity.admit_bootstrap(&bootstrap, NOW).unwrap();
    assert!(
        identity.delivery_pin(&state, NOW).is_err(),
        "ambiguous locators must fail closed"
    );
}

#[test]
fn current_membership_is_admitted_only_from_signed_governance_not_the_bootstrap_roster() {
    let (mut identity, bootstrap) = admitted_fixture();
    let mut forged = bootstrap.clone();
    forged.governance_messages.clear();
    assert!(identity.admit_bootstrap(&forged, NOW).is_err());
    assert!(!identity.admitted());
    for mutation in 0..5 {
        let mut changed = bootstrap.clone();
        match mutation {
            0 => changed.room = "foreign".into(),
            1 => changed.authority.device_public_key = [4; 32],
            2 => changed.device_cert.user_public_key = [4; 32],
            3 => changed.invite_id = [4; 32],
            _ => changed.governance_messages[0][0] ^= 128,
        };
        assert!(identity.admit_bootstrap(&changed, NOW).is_err());
    }
    let state = identity.admit_bootstrap(&bootstrap, NOW).unwrap();
    assert!(state.certificates().contains(identity.certificate()));
    assert!(identity.admitted());
    assert!(
        InvitedIdentity::read(&identity.encode().unwrap(), NOW + 700_000_000_000)
            .unwrap()
            .admitted()
    );
}

#[test]
fn restored_expired_invitation_keeps_exact_completion_but_cannot_create_fresh_proof() {
    let (identity, _) = admitted_fixture();
    let restored =
        InvitedIdentity::read(&identity.encode().unwrap(), NOW + 700_000_000_000).unwrap();
    assert_eq!(restored.completion(), identity.completion());
    assert!(restored.invitation(NOW + 700_000_000_000).is_err());
    assert!(restored.retained_invitation().is_ok());
}

#[test]
fn invited_identity_retains_shared_incoming_credit_and_refuses_exhausted_decode() {
    use crate::peer_delivery::incoming::IncomingBudget;
    let f = Fixture::new();
    let bytes = InvitedIdentity::create(&f.invitation(None), "guest", NOW).unwrap();
    let budget = IncomingBudget::new(8 * 1024 * 1024).unwrap();
    let before = budget.available();
    let identity = InvitedIdentity::read_with_budget(&bytes, NOW, &budget).unwrap();
    assert!(
        budget.available() < before,
        "owned identity must keep its incoming credit"
    );
    let remaining = budget.reserve(budget.available()).unwrap();
    assert!(InvitedIdentity::read_with_budget(&bytes, NOW, &budget).is_err());
    drop(remaining);
    drop(identity);
    assert_eq!(budget.available(), before);
    assert!(InvitedIdentity::read_with_budget(b"{", NOW, &budget).is_err());
    assert_eq!(
        budget.available(),
        before,
        "invalid material must release its reservation"
    );
}

#[test]
fn retained_completion_preflight_and_owned_decode_share_identity_budget() {
    use crate::peer_delivery::incoming::IncomingBudget;
    let f = Fixture::new();
    let bytes = InvitedIdentity::create(&f.invitation(None), "guest", NOW).unwrap();
    let budget = IncomingBudget::new(8 * 1024 * 1024).unwrap();
    let before = budget.available();
    let mut identity = InvitedIdentity::read_with_budget(&bytes, NOW, &budget).unwrap();
    let challenge = join::JoinChallenge::new(
        &identity.invitation(NOW).unwrap(),
        identity.certificate(),
        identity.request_id(),
        [9; 32],
        NOW + 30_000_000_000,
        NOW,
    )
    .unwrap();
    let checkpoint = identity
        .checkpoint_challenge(challenge.clone(), NOW)
        .unwrap();
    drop(identity);
    let mut restored =
        InvitedIdentity::read_with_budget(&checkpoint, NOW + 31_000_000_000, &budget).unwrap();
    let remaining = budget.reserve(budget.available()).unwrap();
    assert!(
        restored
            .checkpoint_challenge(challenge.clone(), NOW + 31_000_000_000)
            .is_err(),
        "retained completion owned decode requires shared credit"
    );
    drop(remaining);
    assert_eq!(
        restored
            .checkpoint_challenge(challenge, NOW + 31_000_000_000)
            .unwrap(),
        checkpoint
    );
    drop(restored);
    assert_eq!(budget.available(), before);
}

#[test]
fn authenticated_bootstrap_requires_one_complete_cut_of_original_member_publications() {
    let (mut identity, mut bootstrap, mut native) = admitted_fixture_with_document(0, false, true);
    let time = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(NOW);
    let member = identity.device_key();
    assert_ne!(
        member.verifying_key().to_bytes(),
        bootstrap.authority.device_public_key
    );
    let mut document =
        automerge::AutoCommit::load(&native.document("test.app", "tab:1").unwrap()).unwrap();
    let heads = document.get_heads();
    document
        .put(automerge::ROOT, "member-parent", true)
        .unwrap();
    document.commit();
    let parent = native
        .publish_incremental(
            "test.app",
            "tab:1",
            document.save_after(&heads),
            &member,
            time,
        )
        .unwrap();
    let heads = document.get_heads();
    document.put(automerge::ROOT, "member-child", true).unwrap();
    document.commit();
    let child = native
        .publish_incremental(
            "test.app",
            "tab:1",
            document.save_after(&heads),
            &member,
            time,
        )
        .unwrap();
    for message in [&parent, &child] {
        let qos_peer_state::NeighbourMsg::AppDocument { envelope, .. } = message else {
            panic!("member fixture must publish an actual signed document")
        };
        assert_eq!(
            *envelope.author.as_bytes(),
            member.verifying_key().to_bytes()
        );
    }
    let parent = postcard::to_stdvec(&parent).unwrap();
    let child = postcard::to_stdvec(&child).unwrap();
    // Retain the exact originals and force a deferred dependency within this
    // authenticated cut. No member envelope is re-signed as the governor.
    bootstrap.governance_messages.push(child);
    let mut incomplete = InvitedIdentity::read(&identity.encode().unwrap(), NOW).unwrap();
    assert!(incomplete.admit_bootstrap(&bootstrap, NOW).is_err());
    bootstrap.governance_messages.push(parent);
    let original = bootstrap.governance_messages.clone();
    let installed = identity
        .admit_bootstrap(&bootstrap, NOW)
        .expect("complete original-member cut requires genuine bounded native admission");
    assert_eq!(installed.pending_count(), 0);
    assert_eq!(
        installed.document_heads("test.app", "tab:1"),
        native.document_heads("test.app", "tab:1")
    );
    assert_eq!(bootstrap.governance_messages, original);
}

// Legacy v2 enrollment material stays readable by the enrollment identity
// source, and is never accepted as an invited identity.
#[test]
fn literal_v2_enrollment_material_remains_accepted_only_by_the_existing_owner_source() {
    let legacy = br#"{"version":2,"secret":[7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7],"request_id":[9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9],"scope":null}"#;
    let request: serde_json::Value =
        serde_json::from_str(&crate::identity::request(legacy).unwrap()).unwrap();
    assert_eq!(
        request["device_public_key"],
        "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"
    );
    assert_eq!(
        request["request_id"],
        "0909090909090909090909090909090909090909090909090909090909090909"
    );
    assert!(super::InvitedIdentity::read(legacy, 1_000_000_000_000).is_err());
}
