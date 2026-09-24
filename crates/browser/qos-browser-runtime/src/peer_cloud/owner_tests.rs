use super::*;
use ed25519_dalek::SigningKey;
use qos_peer_state::join::JoinTransportRequest as Request;

const NOW: u64 = 1_000_000_000_000;
const ROOM: &str = "6d3c0042-a32d-4c3d-9616-03e621da0f47";
const ACCOUNT: &str = "account";
const MACHINE: &str = "machine";

fn budget() -> IncomingBudget {
    IncomingBudget::new(64 * 1024 * 1024).unwrap()
}
fn time(now: u64) -> web_time::SystemTime {
    web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now)
}
fn host_key() -> SigningKey {
    SigningKey::from_bytes(&[91; 32])
}
fn fresh_challenge(now: u64) -> identity::cloud_enrollment::Challenge {
    identity::cloud_enrollment::Challenge {
        version: 1,
        machine_id: MACHINE.into(),
        room: ROOM.into(),
        workspace_id: tilo_relay::room_workspace_id(ROOM).unwrap().to_string(),
        host_public_key: host_key().verifying_key().to_bytes(),
        nonce: [92; 32],
        expires_at_ns: now + 300_000_000_000,
        root_public_key: None,
        host_certificate: None,
    }
}
fn json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap()
}
fn enrollment(payload: &str) -> identity::cloud_enrollment::EnrollmentRequest {
    serde_json::from_str(payload).unwrap()
}
fn apply_payload(owner: &CloudOwner) -> (String, String) {
    match owner.step().unwrap() {
        CloudStep::Apply {
            operation_key,
            payload,
        } => (operation_key, payload),
        _ => panic!("expected apply"),
    }
}

/// The Store.apply reply shape, signed by the challenged host key.
fn store_reply(request: &identity::cloud_enrollment::EnrollmentRequest, now: u64) -> String {
    let host = &request.host_certificate;
    let browser = &request.browser_certificate;
    let workspace = tilo_relay::room_workspace_id(ROOM).unwrap();
    let mut view = identity::WorkspaceDirectory::new(host.user_public_key, host.device_public_key);
    view.add_workspace(workspace, "My Tilo workspace").unwrap();
    view.set_member_devices(
        workspace,
        &[host.device_public_key, browser.device_public_key],
    )
    .unwrap();
    view.set_host_devices(workspace, &[host.device_public_key])
        .unwrap();
    let directory_update = view.sync_incremental(&host_key(), now).unwrap();
    serde_json::to_string(&serde_json::json!({
        "version": 1, "action_address": null, "certificate": browser,
        "root_public_key": browser.user_public_key, "host_certificate": host,
        "relay_address": null, "packet_relay": null, "revocations": [],
        "bootstrap": {"room": ROOM, "workspace_id": workspace.to_string(),
            "directory_update": directory_update, "directory_author": host,
            "certified_roster": [host, browser]}
    }))
    .unwrap()
}

/// Fresh identity through inspect → durable checkpoint → apply → verified reply.
fn enrolled_owner(budget: &IncomingBudget) -> (CloudOwner, Vec<u8>) {
    let material = create(ACCOUNT, MACHINE, "Owner browser", budget, NOW).unwrap();
    let mut owner = CloudOwner::restore(&material, ACCOUNT, MACHINE, budget, NOW).unwrap();
    assert!(matches!(owner.step().unwrap(), CloudStep::Inspect));
    let checkpoint = owner.enroll(&json(&fresh_challenge(NOW)), NOW).unwrap();
    owner.acknowledge(&checkpoint, NOW).unwrap();
    let (_, payload) = apply_payload(&owner);
    let reply = store_reply(&enrollment(&payload), NOW + 1);
    let checkpoint = owner.accept(&reply, NOW + 2).unwrap();
    owner.acknowledge(&checkpoint, NOW + 2).unwrap();
    assert!(matches!(owner.step().unwrap(), CloudStep::Enrolled));
    (owner, checkpoint)
}

/// Host-side governance after Store.apply admitted the browser certificate.
fn owner_bootstrap(scope: &EnrolledOwner, admit: bool, now: u64) -> join::OwnerBootstrapV1 {
    let host = scope.host().clone();
    let mut native = qos_peer_state::StatePeer::new(
        ROOM,
        host.clone(),
        vec![host.clone()],
        identity::RevocationList::new(),
        time(now),
    )
    .unwrap();
    native
        .install_seed(
            qos_peer_state::SeedState {
                room_changes: qos_peer_state::seed_room(ROOM, &host, &[host.clone()]).unwrap(),
                documents: vec![],
            },
            &host_key(),
            time(now),
        )
        .unwrap();
    if admit {
        native
            .admit_certificate(scope.certificate().clone(), &host_key(), time(now))
            .unwrap();
    }
    join::OwnerBootstrapV1 {
        version: 1,
        device_cert: scope.certificate().clone(),
        room: ROOM.into(),
        workspace_id: scope.workspace(),
        authority: host,
        certified_roster: native.certificates(),
        revocations: vec![],
        records: vec![],
        governance_messages: native
            .bootstrap_messages(&host_key(), time(now))
            .unwrap()
            .iter()
            .map(|message| postcard::to_stdvec(message).unwrap())
            .collect(),
        generation: 1,
    }
}

#[test]
fn fresh_owner_material_is_first_claim_custody_with_a_stable_public_binding() {
    let budget = budget();
    let material = create(ACCOUNT, MACHINE, "Owner browser", &budget, NOW).unwrap();
    let text: serde_json::Value = serde_json::from_slice(&material).unwrap();
    assert_eq!(text["first_claim"], true);
    assert_eq!(text["kind"], "cloud-owner");
    let owner = CloudOwner::restore(&material, ACCOUNT, MACHINE, &budget, NOW).unwrap();
    let public: serde_json::Value = serde_json::from_str(&owner.public().unwrap()).unwrap();
    assert_eq!(public["scope"], scope(ACCOUNT, MACHINE).unwrap());
    assert_eq!(public["step"], "inspect");
    assert!(public.get("root_secret").is_none() && public.get("device_secret").is_none());
    assert!(CloudOwner::restore(&material, ACCOUNT, "other", &budget, NOW).is_err());
    assert!(CloudOwner::restore(&material, "other", MACHINE, &budget, NOW).is_err());
    let other = create(ACCOUNT, MACHINE, "Owner browser", &budget, NOW).unwrap();
    assert_ne!(
        serde_json::from_slice::<serde_json::Value>(&other).unwrap()["root_secret"],
        text["root_secret"]
    );
    assert!(create(ACCOUNT, MACHINE, " ", &budget, NOW).is_err());
    assert!(create("", MACHINE, "Owner", &budget, NOW).is_err());
}

#[test]
fn first_claim_root_signs_the_challenged_host_and_store_verification_accepts_it() {
    let budget = budget();
    let material = create(ACCOUNT, MACHINE, "Owner browser", &budget, NOW).unwrap();
    let mut owner = CloudOwner::restore(&material, ACCOUNT, MACHINE, &budget, NOW).unwrap();
    let challenge = fresh_challenge(NOW);
    let checkpoint = owner.enroll(&json(&challenge), NOW).unwrap();
    // Nothing may be applied before the exact checkpoint is acknowledged.
    assert!(owner.step().is_err());
    owner.acknowledge(&checkpoint, NOW).unwrap();
    let (operation_key, payload) = apply_payload(&owner);
    assert!(uuid::Uuid::parse_str(&operation_key).is_ok());
    let request = enrollment(&payload);
    let host = &request.host_certificate;
    assert_eq!(host.user_public_key, owner.certificate().user_public_key);
    assert_eq!(host.device_public_key, challenge.host_public_key);
    assert_eq!(
        host.capabilities,
        identity::cloud_enrollment::host_capabilities()
    );
    assert_eq!(host.expires_at_ns, None);
    identity::cloud_enrollment::verify_request(&request, &challenge, NOW + 1).unwrap();
    // A restored checkpoint reproduces the identical signed request.
    let restored = CloudOwner::restore(&checkpoint, ACCOUNT, MACHINE, &budget, NOW + 5).unwrap();
    assert_eq!(apply_payload(&restored), (operation_key, payload));
}

#[test]
fn first_claim_refuses_foreign_bound_and_mixed_store_challenges() {
    let budget = budget();
    let material = create(ACCOUNT, MACHINE, "Owner browser", &budget, NOW).unwrap();
    let foreign = SigningKey::from_bytes(&[93; 32]);
    let mut foreign_host = identity::DeviceCert {
        user_public_key: foreign.verifying_key().to_bytes(),
        device_public_key: host_key().verifying_key().to_bytes(),
        device_label: "Host".into(),
        capabilities: identity::cloud_enrollment::host_capabilities(),
        issued_at_ns: 10,
        expires_at_ns: None,
        signature: [0; 64],
    };
    identity::sign_device_cert(&mut foreign_host, &foreign).unwrap();
    let mut bound = fresh_challenge(NOW);
    bound.root_public_key = Some(foreign.verifying_key().to_bytes());
    bound.host_certificate = Some(foreign_host.clone());
    let mut root_only = fresh_challenge(NOW);
    root_only.root_public_key = Some(foreign.verifying_key().to_bytes());
    let mut host_only = fresh_challenge(NOW);
    host_only.host_certificate = Some(foreign_host);
    let mut machine = fresh_challenge(NOW);
    machine.machine_id = "other".into();
    for challenge in [bound, root_only, host_only, machine] {
        let mut owner = CloudOwner::restore(&material, ACCOUNT, MACHINE, &budget, NOW).unwrap();
        assert!(owner.enroll(&json(&challenge), NOW).is_err());
        assert!(matches!(owner.step().unwrap(), CloudStep::Inspect));
    }
}

#[test]
fn legacy_material_without_first_claim_still_refuses_an_unbound_store() {
    let budget = budget();
    let material = create(ACCOUNT, MACHINE, "Owner browser", &budget, NOW).unwrap();
    let mut legacy: serde_json::Value = serde_json::from_slice(&material).unwrap();
    legacy.as_object_mut().unwrap().remove("first_claim");
    let legacy = serde_json::to_vec(&legacy).unwrap();
    let mut owner = CloudOwner::restore(&legacy, ACCOUNT, MACHINE, &budget, NOW).unwrap();
    assert!(owner.enroll(&json(&fresh_challenge(NOW)), NOW).is_err());
    // A tampered first-claim checkpoint whose host certificate is not root
    // signed fails restore validation rather than being trusted.
    let mut first = CloudOwner::restore(&material, ACCOUNT, MACHINE, &budget, NOW).unwrap();
    let checkpoint = first.enroll(&json(&fresh_challenge(NOW)), NOW).unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(&checkpoint).unwrap();
    let bytes: Vec<u8> = serde_json::from_value(changed["phase"]["enrollment"].clone()).unwrap();
    let mut request: identity::cloud_enrollment::EnrollmentRequest =
        serde_json::from_slice(&bytes).unwrap();
    request.host_certificate.signature[0] ^= 1;
    changed["phase"]["enrollment"] = serde_json::json!(serde_json::to_vec(&request).unwrap());
    assert!(
        CloudOwner::restore(
            &serde_json::to_vec(&changed).unwrap(),
            ACCOUNT,
            MACHINE,
            &budget,
            NOW
        )
        .is_err()
    );
}

#[test]
fn verified_store_reply_enrolls_and_owner_refresh_uses_fresh_request_ids() {
    let budget = budget();
    let (owner, checkpoint) = enrolled_owner(&budget);
    let scope = owner.enrolled().unwrap();
    assert_eq!(scope.room(), ROOM);
    assert_eq!(
        scope.host().device_public_key,
        host_key().verifying_key().to_bytes()
    );
    assert!(owner.refresh_challenge([0; 32]).is_err());
    let Request::MembershipChallenge(request) = owner.refresh_challenge([5; 32]).unwrap() else {
        panic!("wrong request")
    };
    assert_eq!(request.device_cert, *owner.certificate());
    assert_eq!(request.request_id, [5; 32]);
    let challenge = join::MembershipChallenge::new(
        ROOM,
        scope.workspace(),
        scope.host(),
        owner.certificate(),
        [5; 32],
        [6; 32],
        NOW + 60_000_000_000,
        NOW + 3,
    )
    .unwrap();
    assert!(owner.refresh_proof(&challenge, [7; 32], NOW + 3).is_err());
    let Request::MembershipComplete(proof) =
        owner.refresh_proof(&challenge, [5; 32], NOW + 3).unwrap()
    else {
        panic!("wrong proof")
    };
    join::verify_membership_proof(
        &proof.proof,
        &challenge,
        owner.certificate(),
        &identity::RevocationList::new(),
        NOW + 3,
    )
    .unwrap();
    // Reload much later: no inspect/apply; the retained identity refreshes.
    let later = NOW + 86_400_000_000_000;
    let restored = CloudOwner::restore(&checkpoint, ACCOUNT, MACHINE, &budget, later).unwrap();
    assert!(matches!(restored.step().unwrap(), CloudStep::Enrolled));
    assert!(restored.refresh_challenge([8; 32]).is_ok());
}

#[test]
fn store_reply_for_another_device_or_host_is_refused() {
    let budget = budget();
    let material = create(ACCOUNT, MACHINE, "Owner browser", &budget, NOW).unwrap();
    let mut owner = CloudOwner::restore(&material, ACCOUNT, MACHINE, &budget, NOW).unwrap();
    let checkpoint = owner.enroll(&json(&fresh_challenge(NOW)), NOW).unwrap();
    owner.acknowledge(&checkpoint, NOW).unwrap();
    let (_, payload) = apply_payload(&owner);
    let mut reply: serde_json::Value =
        serde_json::from_str(&store_reply(&enrollment(&payload), NOW + 1)).unwrap();
    reply["bootstrap"]["certified_roster"] = serde_json::json!([reply["host_certificate"]]);
    assert!(owner.accept(&reply.to_string(), NOW + 2).is_err());
    assert!(matches!(owner.step().unwrap(), CloudStep::Apply { .. }));
}

#[test]
fn owner_bootstrap_admits_signed_governance_and_requires_the_structural_owner() {
    let budget = budget();
    let (owner, _) = enrolled_owner(&budget);
    let scope = owner.enrolled().unwrap();
    let bootstrap = owner_bootstrap(&scope, true, NOW + 3);
    let state = owner.admit(&scope, &bootstrap, NOW + 4).unwrap();
    state
        .verify_current_owner(owner.certificate(), time(NOW + 4))
        .unwrap();
    assert_eq!(state.authority(), scope.host());

    // Signed governance that never admitted this device grants nothing.
    let unadmitted = owner_bootstrap(&scope, false, NOW + 3);
    assert!(owner.admit(&scope, &unadmitted, NOW + 4).is_err());
    let mut changed = bootstrap.clone();
    changed.governance_messages.clear();
    assert!(owner.admit(&scope, &changed, NOW + 4).is_err());
    let mut changed = bootstrap.clone();
    changed.governance_messages[0][0] ^= 128;
    assert!(owner.admit(&scope, &changed, NOW + 4).is_err());
    let mut changed = bootstrap.clone();
    changed.generation = 0;
    assert!(owner.admit(&scope, &changed, NOW + 4).is_err());
    let mut changed = bootstrap.clone();
    changed.workspace_id += 1;
    assert!(owner.admit(&scope, &changed, NOW + 4).is_err());
    let mut changed = bootstrap.clone();
    changed.authority = owner.certificate().clone();
    assert!(owner.admit(&scope, &changed, NOW + 4).is_err());
    let mut changed = bootstrap;
    changed.device_cert = scope.host().clone();
    assert!(owner.admit(&scope, &changed, NOW + 4).is_err());
}

#[test]
fn owner_onboarding_wrapper_is_signed_for_the_enrolled_scope_and_membership_only() {
    use tilo_relay::delivery_v3::onboarding_wire as wire;
    let budget = budget();
    let (owner, _) = enrolled_owner(&budget);
    let scope = owner.enrolled().unwrap();
    let origin = "https://cloud.example:8443";
    let prepare = |origin: &str, request: Request| {
        crate::peer_onboarding::prepare_owner(
            crate::peer_onboarding::OwnerOnboarding {
                origin,
                host: scope.host(),
                certificate: scope.certificate(),
                room: scope.room(),
                workspace: scope.workspace(),
                key: owner.device_key().unwrap(),
            },
            request,
            &budget,
            NOW + 3,
        )
    };
    let prepared = prepare(origin, owner.refresh_challenge([5; 32]).unwrap()).unwrap();
    let wrapper = wire::OnboardingRequest::decode(prepared.request_bytes()).unwrap();
    assert_eq!(
        wrapper.operation,
        wire::OnboardingOperation::MembershipChallenge
    );
    wrapper
        .verify_device_signature(
            wire::OnboardingScope {
                origin,
                host: scope.host().device_public_key,
                workspace: scope.workspace(),
                root: scope.host().user_public_key,
                room: ROOM.parse().unwrap(),
            },
            owner.certificate(),
            NOW + 3,
        )
        .unwrap();
    for bad in [
        "http://cloud.example",
        "https://cloud.example/path",
        "https://user@cloud.example",
        "https://",
    ] {
        assert!(
            prepare(bad, owner.refresh_challenge([5; 32]).unwrap()).is_err(),
            "{bad}"
        );
    }
    let Request::MembershipChallenge(mut other) = owner.refresh_challenge([5; 32]).unwrap() else {
        panic!("wrong request")
    };
    other.device_cert = scope.host().clone();
    assert!(prepare(origin, Request::MembershipChallenge(other)).is_err());
    assert!(
        prepare(
            origin,
            Request::Inspect {
                signed_invitation: vec![1]
            }
        )
        .is_err()
    );
}
