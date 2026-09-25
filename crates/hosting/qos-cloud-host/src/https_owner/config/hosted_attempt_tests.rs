//! Exact browser protocol source against actual operator pin and Store barriers.
use super::OperatorConfig;
use qos_relay as relay;
use qos_user_identity as identity;
#[path = "../../../../../browser/qos-browser-runtime/src/peer_cloud/protocol.rs"]
mod protocol;
use automerge::resource_limits::NativeAllowance;
use ed25519_dalek::SigningKey;
use protocol::*;
use qos_peer_state::join::{JoinTransportRequest as Request, JoinTransportResponse as Response};

#[test]
fn hosted_attempt_allocator_refuses_oversized_phase_before_unfunded_content() {
    let fixture = Fixture::new();
    let mut material: serde_json::Value = serde_json::from_slice(&fixture.material).unwrap();
    // Bounded, valid JSON whose byte array must be refused without first
    // buffering an uncharged Vec<Serde Content>. This is a negative material
    // input, never a signed request or positive authority fixture.
    material["phase"] = serde_json::json!({
        "step": "apply", "enrollment": vec![0u8; 400_000], "signed_at_ns": 20
    });
    let bytes = serde_json::to_vec(&material).unwrap();
    assert!(bytes.len() < 1024 * 1024);
    let initial = fixture.budget.available_bytes();
    let result = crate::delivery::resources::precharged_test(&fixture.budget, || {
        // The observer spans these test-owned input strings too. Their
        // backing is funded independently of the component under test.
        let _selection = fixture
            .budget
            .reserve("account".len() + "hosted-machine".len())
            .unwrap();
        HostedAttempt::restore(
            &bytes,
            Selection {
                account_id: "account".into(),
                machine_id: "hosted-machine".into(),
            },
            &fixture.budget,
            20,
        )
    });
    assert!(result.is_err());
    assert_eq!(fixture.budget.available_bytes(), initial);
}

#[test]
fn hosted_attempt_allocator_funds_full_pinned_store_checkpoint_lifecycle() {
    // Obtain all remote inputs from the real pinned Store before observation.
    // Store persistence and test fixture storage have their own ownership;
    // the measured operation below is the complete actual browser protocol.
    let mut fixture = Fixture::new();
    let challenge = serde_json::to_vec(&fixture.store.inspect(20).unwrap()).unwrap();
    let mut original = fixture.restore(&fixture.material, 20);
    let apply_checkpoint = original
        .prepare_enrollment(&challenge, 20)
        .unwrap()
        .material
        .to_vec();
    original.acknowledge(&apply_checkpoint, 20).unwrap();
    let apply = original.apply_request().unwrap();
    let apply_key = apply.operation_key;
    let apply_bytes = apply.payload.to_vec();
    let request = serde_json::from_slice(&apply_bytes).unwrap();
    let enrolled = serde_json::to_vec(&fixture.store.apply(&request, 21).unwrap()).unwrap();
    let challenge_checkpoint = original
        .accept_enrollment(&enrolled, 22)
        .unwrap()
        .material
        .to_vec();
    original.acknowledge(&challenge_checkpoint, 22).unwrap();
    let Request::MembershipChallenge(request) =
        postcard::from_bytes(original.membership_challenge_request().unwrap()).unwrap()
    else {
        panic!("wrong request")
    };
    let membership = fixture
        .store
        .membership_challenge(fixture.device.verifying_key().to_bytes(), request, 23)
        .unwrap();
    let expired = membership.challenge.expires_at_ns + 1;
    let membership = postcard::to_stdvec(&Response::MembershipChallenge(membership)).unwrap();
    let proof_checkpoint = original
        .prepare_membership(&membership, 24)
        .unwrap()
        .material
        .to_vec();
    original.acknowledge(&proof_checkpoint, 24).unwrap();
    let proof_bytes = original.membership_complete_request().unwrap().to_vec();
    let Request::MembershipComplete(request) = postcard::from_bytes(&proof_bytes).unwrap() else {
        panic!("wrong request")
    };
    let bootstrap = fixture
        .store
        .owner_membership_complete(fixture.device.verifying_key().to_bytes(), request, 25)
        .unwrap();
    let bootstrap = postcard::to_stdvec(&Response::OwnerMembershipComplete(bootstrap)).unwrap();
    drop(original);

    let initial = fixture.budget.available_bytes();
    crate::delivery::resources::precharged_test(&fixture.budget, || {
        // Both restore calls consume these independently paid fixture inputs.
        let _selection = fixture
            .budget
            .reserve(2 * ("account".len() + "hosted-machine".len()))
            .unwrap();
        let mut attempt = fixture.restore(&fixture.material, 20);
        let retained = fixture.budget.available_bytes();
        let pressure = fixture.budget.reserve(retained - 64).unwrap();
        assert!(matches!(
            attempt.prepare_enrollment(&challenge, 20),
            Err(HostedAttemptError::Capacity)
        ));
        drop(pressure);
        assert_eq!(fixture.budget.available_bytes(), retained);
        assert_eq!(
            attempt.prepare_enrollment(&challenge, 20).unwrap().material,
            apply_checkpoint
        );
        assert!(attempt.apply_request().is_err());
        attempt.acknowledge(&apply_checkpoint, 20).unwrap();
        let apply = attempt.apply_request().unwrap();
        assert_eq!(apply.operation_key, apply_key);
        assert_eq!(apply.payload, apply_bytes);
        assert_eq!(
            attempt.accept_enrollment(&enrolled, 22).unwrap().material,
            challenge_checkpoint
        );
        attempt.acknowledge(&challenge_checkpoint, 22).unwrap();
        assert_eq!(
            attempt
                .prepare_membership(&membership, 24)
                .unwrap()
                .material,
            proof_checkpoint
        );
        attempt.acknowledge(&proof_checkpoint, 24).unwrap();
        let mut retry = fixture.restore(&proof_checkpoint, expired);
        assert_eq!(retry.membership_complete_request().unwrap(), proof_bytes);
        retry.cancel();
        assert!(retry.membership_complete_request().is_err());
        drop(retry);
        let pending = attempt.accept_membership(&bootstrap, 25).unwrap();
        drop(attempt);
        assert!(fixture.budget.available_bytes() < initial);
        drop(pending);
    });
    assert_eq!(fixture.budget.available_bytes(), initial);
}

#[test]
fn hosted_attempt_allocator_funds_parser_and_signature_refusals() {
    let fixture = Fixture::new();
    let mut broken_signature: serde_json::Value =
        serde_json::from_slice(&fixture.material).unwrap();
    broken_signature["certificate"]["signature"][0] = serde_json::json!(
        broken_signature["certificate"]["signature"][0]
            .as_u64()
            .unwrap()
            ^ 1
    );
    let mut wrong_signature_length = broken_signature.clone();
    wrong_signature_length["certificate"]["signature"] = serde_json::json!(vec![0u8; 8192]);
    let mut too_deep: serde_json::Value = serde_json::from_slice(&fixture.material).unwrap();
    too_deep["phase"] = serde_json::json!({"step":"initial","extension":0});
    let valid = String::from_utf8(fixture.material.clone()).unwrap();
    let inputs = [
        format!("{{\"{}\\q\":0}}", "\\u0001".repeat(8192)).into_bytes(),
        format!("\"{}\"", "\\u0001".repeat(8192)).into_bytes(),
        valid
            .replace(
                "\"created_at_ns\":12",
                "\"created_at_ns\":0.1000000000000000001",
            )
            .into_bytes(),
        valid
            .replace(
                "\"created_at_ns\":12",
                "\"created_at_ns\":9.999999999999999999e-64",
            )
            .into_bytes(),
        valid
            .replace(
                "\"created_at_ns\":12",
                "\"created_at_ns\":99999999999999999999",
            )
            .into_bytes(),
        serde_json::to_vec(&broken_signature).unwrap(),
        serde_json::to_vec(&wrong_signature_length).unwrap(),
        format!("{}0{}", "[".repeat(65), "]".repeat(65)).into_bytes(),
        serde_json::to_vec(&too_deep).unwrap(),
    ];
    let initial = fixture.budget.available_bytes();
    for input in inputs {
        crate::delivery::resources::precharged_test(&fixture.budget, || {
            let _selection = fixture
                .budget
                .reserve("account".len() + "hosted-machine".len())
                .unwrap();
            let result = HostedAttempt::restore(
                &input,
                Selection {
                    account_id: "account".into(),
                    machine_id: "hosted-machine".into(),
                },
                &fixture.budget,
                20,
            );
            assert!(result.is_err());
        });
        assert_eq!(fixture.budget.available_bytes(), initial);
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: crate::Store,
    root: SigningKey,
    device: SigningKey,
    budget: NativeAllowance,
    material: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[47; 32]);
        let first = SigningKey::from_bytes(&[48; 32]);
        let device = SigningKey::from_bytes(&[49; 32]);
        let mut store = crate::Store::open(dir.path(), "hosted-machine").unwrap();
        let initial = crate::tests::approved(store.inspect(10).unwrap(), &root, &first);
        store.apply(&initial, 11).unwrap();
        let config = OperatorConfig::decode(
            &serde_json::to_vec(&serde_json::json!({
                "version": 1, "state_root": dir.path(), "machine_id": "hosted-machine",
                "transport_config": dir.path().join("transport.json"),
                "initial_owner_root": hex::encode(root.verifying_key().to_bytes())
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            config.owner_root(&store).unwrap(),
            root.verifying_key().to_bytes()
        );
        let mut certificate = identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Hosted browser".into(),
            capabilities: identity::DeviceCapabilities::member(),
            issued_at_ns: 12,
            expires_at_ns: None,
            signature: [0; 64],
        };
        identity::sign_device_cert(&mut certificate, &root).unwrap();
        let material = serde_json::to_vec(&serde_json::json!({
            "version":4,"kind":"cloud-owner","account_id":"account","machine_id":"hosted-machine",
            "root_secret":root.to_bytes(),"device_secret":device.to_bytes(),"attempt_id":([50u8;32]),
            "created_at_ns":12,"certificate":certificate,"phase":{"step":"initial"}
        })).unwrap();
        Self {
            _dir: dir,
            store,
            root,
            device,
            budget: NativeAllowance::new(64 * 1024 * 1024).unwrap(),
            material,
        }
    }
    fn restore(&self, material: &[u8], now: u64) -> HostedAttempt {
        HostedAttempt::restore(
            material,
            Selection {
                account_id: "account".into(),
                machine_id: "hosted-machine".into(),
            },
            &self.budget,
            now,
        )
        .unwrap()
    }
    fn enrollment(&mut self) -> (HostedAttempt, Vec<u8>, serde_json::Value) {
        let mut attempt = self.restore(&self.material, 20);
        let challenge = serde_json::to_vec(&self.store.inspect(20).unwrap()).unwrap();
        let checkpoint = attempt
            .prepare_enrollment(&challenge, 20)
            .unwrap()
            .material
            .to_vec();
        assert!(attempt.apply_request().is_err());
        attempt.acknowledge(&checkpoint, 20).unwrap();
        let request = serde_json::from_slice(attempt.apply_request().unwrap().payload).unwrap();
        let result = self.store.apply(&request, 21).unwrap();
        (attempt, checkpoint, result)
    }

    fn pending_membership(
        &mut self,
    ) -> (
        HostedAttempt,
        Vec<u8>,
        qos_peer_state::join::MembershipCompleteRequest,
    ) {
        let (mut attempt, _, result) = self.enrollment();
        let checkpoint = attempt
            .accept_enrollment(&serde_json::to_vec(&result).unwrap(), 22)
            .unwrap()
            .material
            .to_vec();
        attempt.acknowledge(&checkpoint, 22).unwrap();
        let Request::MembershipChallenge(request) =
            postcard::from_bytes(attempt.membership_challenge_request().unwrap()).unwrap()
        else {
            panic!("wrong request")
        };
        let challenge = self
            .store
            .membership_challenge(self.device.verifying_key().to_bytes(), request, 23)
            .unwrap();
        let checkpoint = attempt
            .prepare_membership(
                &postcard::to_stdvec(&Response::MembershipChallenge(challenge)).unwrap(),
                24,
            )
            .unwrap()
            .material
            .to_vec();
        attempt.acknowledge(&checkpoint, 24).unwrap();
        let Request::MembershipComplete(request) =
            postcard::from_bytes(attempt.membership_complete_request().unwrap()).unwrap()
        else {
            panic!("wrong request")
        };
        (attempt, checkpoint, request)
    }
}

#[test]
fn hosted_attempt_refuses_collaborator_dto_even_when_owner_fields_match() {
    let mut f = Fixture::new();
    let (mut attempt, _, _) = f.pending_membership();
    let peer = f.store.peer_at(25).unwrap();
    let certificate = peer
        .certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == f.device.verifying_key().to_bytes())
        .unwrap();
    // Deliberately forged negative input, never an authority fixture: even
    // matching owner bindings cannot turn this collaborator DTO into Owner.
    let old = qos_peer_state::join::JoinBootstrap {
        version: 1,
        invite_id: [0; 32],
        device_cert: certificate,
        role: qos_peer_state::join::MemberRole::Editor,
        room: f.store.state.room.clone(),
        workspace_id: relay::room_workspace_id(&f.store.state.room).unwrap(),
        authority: peer.authority().clone(),
        certified_roster: peer.certificates(),
        revocations: peer.revocations(),
        records: Vec::new(),
        governance_messages: Vec::new(),
        generation: f.store.state.generation,
    };
    assert!(
        attempt
            .accept_membership(
                &postcard::to_stdvec(&Response::MembershipComplete(old)).unwrap(),
                25
            )
            .is_err(),
        "collaborator result must never satisfy the hosted owner exchange"
    );
}

#[test]
fn hosted_attempt_pinned_store_ack_signature_and_lost_apply_reply() {
    let mut f = Fixture::new();
    let (attempt, checkpoint, result) = f.enrollment();
    let original = attempt.apply_request().unwrap();
    let bytes = original.payload.to_vec();
    let operation = original.operation_key;
    let request: identity::cloud_enrollment::EnrollmentRequest =
        serde_json::from_slice(&bytes).unwrap();
    identity::cloud_enrollment::verify_request(&request, &request.challenge, 21).unwrap();
    assert_eq!(
        request.browser_certificate.device_public_key,
        f.device.verifying_key().to_bytes()
    );
    let generation = f.store.state.generation;
    drop(attempt);
    let restored = f.restore(&checkpoint, request.challenge.expires_at_ns + 1);
    assert_eq!(restored.apply_request().unwrap().payload, bytes);
    assert_eq!(restored.apply_request().unwrap().operation_key, operation);
    assert_eq!(
        f.store
            .apply(&request, request.challenge.expires_at_ns + 1)
            .unwrap(),
        result
    );
    assert_eq!(f.store.state.generation, generation);
    let mut changed = request.clone();
    changed.device_signature[0] ^= 1;
    assert!(f.store.apply(&changed, 22).is_err());
}

#[test]
fn hosted_attempt_membership_checkpoints_replay_actual_store_once() {
    let mut f = Fixture::new();
    let (mut attempt, _, result) = f.enrollment();
    let checkpoint = attempt
        .accept_enrollment(&serde_json::to_vec(&result).unwrap(), 22)
        .unwrap()
        .material
        .to_vec();
    assert!(attempt.membership_challenge_request().is_err());
    attempt.acknowledge(&checkpoint, 22).unwrap();
    let bytes = attempt.membership_challenge_request().unwrap().to_vec();
    let Request::MembershipChallenge(request) = postcard::from_bytes(&bytes).unwrap() else {
        panic!("wrong request")
    };
    let endpoint = f.device.verifying_key().to_bytes();
    let challenge = f
        .store
        .membership_challenge(endpoint, request.clone(), 23)
        .unwrap();
    let generation = f.store.state.generation;
    drop(attempt);
    let mut attempt = f.restore(&checkpoint, 24);
    assert_eq!(attempt.membership_challenge_request().unwrap(), bytes);
    assert_eq!(
        f.store
            .membership_challenge(endpoint, request, 24)
            .unwrap()
            .challenge,
        challenge.challenge
    );
    assert_eq!(f.store.state.generation, generation);
    let reply = postcard::to_stdvec(&Response::MembershipChallenge(challenge)).unwrap();
    let complete = attempt
        .prepare_membership(&reply, 24)
        .unwrap()
        .material
        .to_vec();
    assert!(attempt.membership_complete_request().is_err());
    attempt.acknowledge(&complete, 24).unwrap();
    let bytes = attempt.membership_complete_request().unwrap().to_vec();
    let Request::MembershipComplete(request) = postcard::from_bytes(&bytes).unwrap() else {
        panic!("wrong request")
    };
    let bootstrap = f
        .store
        .owner_membership_complete(endpoint, request.clone(), 25)
        .unwrap();
    let generation = f.store.state.generation;
    drop(attempt);
    drop(f.store);
    f.store = crate::Store::open(f._dir.path(), "hosted-machine").unwrap();
    let replay_at = request.proof.challenge.expires_at_ns + 1;
    let mut attempt = f.restore(&complete, replay_at);
    assert_eq!(attempt.membership_complete_request().unwrap(), bytes);
    let replay = f
        .store
        .owner_membership_complete(endpoint, request, replay_at)
        .unwrap();
    assert_eq!(
        postcard::to_stdvec(&replay).unwrap(),
        postcard::to_stdvec(&bootstrap).unwrap()
    );
    assert_eq!(f.store.state.generation, generation);
    attempt
        .accept_membership(
            &postcard::to_stdvec(&Response::OwnerMembershipComplete(replay)).unwrap(),
            replay_at + 1,
        )
        .unwrap();
}

#[test]
fn hosted_attempt_matches_actual_signed_apply_response_without_parsing_directory() {
    let mut f = Fixture::new();
    let (mut attempt, _, result) = f.enrollment();
    let mut changed = result.clone();
    changed["bootstrap"]["room"] = serde_json::json!("different");
    assert!(
        attempt
            .accept_enrollment(&serde_json::to_vec(&changed).unwrap(), 22)
            .is_err()
    );
    let mut changed = result.clone();
    changed["bootstrap"]["directory_update"]["signature"][0] = serde_json::json!(
        result["bootstrap"]["directory_update"]["signature"][0]
            .as_u64()
            .unwrap()
            ^ 1
    );
    assert!(
        attempt
            .accept_enrollment(&serde_json::to_vec(&changed).unwrap(), 22)
            .is_err()
    );
    assert!(attempt.membership_challenge_request().is_err());
    let saved = attempt
        .accept_enrollment(&serde_json::to_vec(&result).unwrap(), 22)
        .unwrap()
        .material
        .to_vec();
    assert_eq!(
        attempt
            .accept_enrollment(&serde_json::to_vec(&result).unwrap(), 23)
            .unwrap()
            .material,
        saved
    );
    attempt.cancel();
    assert!(matches!(
        attempt.accept_enrollment(&serde_json::to_vec(&result).unwrap(), 24),
        Err(HostedAttemptError::Cancelled)
    ));
    assert_eq!(
        attempt.acknowledge(&saved, 24),
        Err(HostedAttemptError::Cancelled)
    );
}

#[test]
fn hosted_attempt_membership_scope_and_saved_proof_cannot_be_substituted() {
    let mut f = Fixture::new();
    let (mut attempt, _, result) = f.enrollment();
    let saved = attempt
        .accept_enrollment(&serde_json::to_vec(&result).unwrap(), 22)
        .unwrap()
        .material
        .to_vec();
    attempt.acknowledge(&saved, 22).unwrap();
    let Request::MembershipChallenge(request) =
        postcard::from_bytes(attempt.membership_challenge_request().unwrap()).unwrap()
    else {
        panic!("wrong request")
    };
    let endpoint = f.device.verifying_key().to_bytes();
    let challenge = f.store.membership_challenge(endpoint, request, 23).unwrap();
    let mut wrong = challenge.clone();
    wrong.challenge.request_id[0] ^= 1;
    assert!(
        attempt
            .prepare_membership(
                &postcard::to_stdvec(&Response::MembershipChallenge(wrong)).unwrap(),
                24
            )
            .is_err()
    );
    let reply = postcard::to_stdvec(&Response::MembershipChallenge(challenge)).unwrap();
    let saved = attempt
        .prepare_membership(&reply, 24)
        .unwrap()
        .material
        .to_vec();
    assert!(attempt.membership_complete_request().is_err());
    let mut changed: serde_json::Value = serde_json::from_slice(&saved).unwrap();
    let proof = changed["phase"]["proof"].as_array_mut().unwrap();
    let last = proof.last_mut().unwrap();
    *last = serde_json::json!(last.as_u64().unwrap() ^ 1);
    assert!(
        HostedAttempt::restore(
            &serde_json::to_vec(&changed).unwrap(),
            Selection {
                account_id: "account".into(),
                machine_id: "hosted-machine".into()
            },
            &f.budget,
            24
        )
        .is_err()
    );
    attempt.acknowledge(&saved, 24).unwrap();
    let Request::MembershipComplete(mut request) =
        postcard::from_bytes(attempt.membership_complete_request().unwrap()).unwrap()
    else {
        panic!("wrong request")
    };
    request.proof.signature[0] ^= 1;
    assert!(
        f.store
            .owner_membership_complete(endpoint, request, 25)
            .is_err()
    );
}

#[test]
fn hosted_attempt_owner_response_scope_substitution_is_refused() {
    let mut f = Fixture::new();
    let (_, saved, request) = f.pending_membership();
    let endpoint = f.device.verifying_key().to_bytes();
    let result = f
        .store
        .owner_membership_complete(endpoint, request.clone(), 25)
        .unwrap();
    let generation = f.store.state.generation;
    let mut changed = request;
    changed.proof.signature[0] ^= 1;
    assert!(
        f.store
            .owner_membership_complete(endpoint, changed, 26)
            .is_err()
    );
    assert_eq!(f.store.state.generation, generation);
    let mut alternatives = Vec::new();
    let mut bad = result.clone();
    bad.version = 0;
    alternatives.push(bad);
    let mut bad = result.clone();
    bad.generation = 0;
    alternatives.push(bad);
    let mut bad = result.clone();
    bad.room = "another-room".into();
    alternatives.push(bad);
    let mut bad = result.clone();
    bad.workspace_id ^= 1;
    alternatives.push(bad);
    let mut bad = result.clone();
    bad.device_cert.device_public_key[0] ^= 1;
    alternatives.push(bad);
    let mut bad = result;
    bad.authority.user_public_key[0] ^= 1;
    alternatives.push(bad);
    for bad in alternatives {
        let mut attempt = f.restore(&saved, 26);
        assert!(
            attempt
                .accept_membership(
                    &postcard::to_stdvec(&Response::OwnerMembershipComplete(bad)).unwrap(),
                    26
                )
                .is_err()
        );
        assert!(
            attempt.membership_complete_request().is_ok(),
            "refusal cannot discard retained retry custody"
        );
    }
}

#[test]
fn hosted_attempt_keeps_saved_proof_through_both_store_barriers_and_restart() {
    use atomic_file::test_support::Checkpoint;
    for point in [Checkpoint::BeforePublish, Checkpoint::BeforeCommit] {
        let mut f = Fixture::new();
        let available = f.budget.available_bytes();
        let (attempt, saved, request) = f.pending_membership();
        let exact_request = attempt.membership_complete_request().unwrap().to_vec();
        let endpoint = f.device.verifying_key().to_bytes();
        let generation = f.store.state.generation;
        let failure = crate::storage::test_support::with_save_failure(f._dir.path(), point, || {
            f.store
                .owner_membership_complete(endpoint, request.clone(), 25)
        })
        .err()
        .expect("actual barrier must return failure without a response");
        assert!(failure.is::<crate::StoreCommitFailure>());
        assert!(f.store.peer_at(25).is_err());
        assert_eq!(
            attempt.membership_complete_request().unwrap(),
            exact_request
        );
        drop(attempt);
        drop(f.store);
        f.store = crate::Store::open(f._dir.path(), "hosted-machine").unwrap();
        let mut attempt = f.restore(&saved, 26);
        assert_eq!(
            attempt.membership_complete_request().unwrap(),
            exact_request
        );
        let response = f
            .store
            .owner_membership_complete(endpoint, request.clone(), 26)
            .unwrap();
        assert_eq!(response.generation, generation + 1);
        assert_eq!(f.store.state.generation, generation + 1);
        let response_bytes =
            postcard::to_stdvec(&Response::OwnerMembershipComplete(response)).unwrap();
        let disk = std::fs::read(f._dir.path().join("state.json")).unwrap();
        drop(f.store);
        f.store = crate::Store::open(f._dir.path(), "hosted-machine").unwrap();
        let replay = f
            .store
            .owner_membership_complete(endpoint, request, 27)
            .unwrap();
        assert_eq!(
            postcard::to_stdvec(&Response::OwnerMembershipComplete(replay)).unwrap(),
            response_bytes
        );
        assert_eq!(
            std::fs::read(f._dir.path().join("state.json")).unwrap(),
            disk
        );
        let pending = attempt.accept_membership(&response_bytes, 27).unwrap();
        drop(attempt);
        assert!(
            f.budget.available_bytes() < available,
            "pending DTO retains its original charged backing"
        );
        drop(pending);
        assert_eq!(f.budget.available_bytes(), available);
    }
}

#[test]
fn hosted_attempt_mismatched_scope_ack_expiry_and_cancel_are_closed() {
    let mut f = Fixture::new();
    let mut attempt = f.restore(&f.material, 20);
    let challenge = f.store.inspect(20).unwrap();
    let mut wrong = challenge.clone();
    wrong.machine_id = "different".into();
    assert!(
        attempt
            .prepare_enrollment(&serde_json::to_vec(&wrong).unwrap(), 20)
            .is_err()
    );
    assert!(
        attempt
            .prepare_enrollment(
                &serde_json::to_vec(&challenge).unwrap(),
                challenge.expires_at_ns
            )
            .is_err()
    );
    let saved = attempt
        .prepare_enrollment(&serde_json::to_vec(&challenge).unwrap(), 20)
        .unwrap()
        .material
        .to_vec();
    let mut altered = saved.clone();
    altered.push(b' ');
    assert!(attempt.acknowledge(&altered, 20).is_err());
    assert!(attempt.apply_request().is_err());
    attempt.cancel();
    assert_eq!(
        attempt.acknowledge(&saved, 20),
        Err(HostedAttemptError::Cancelled)
    );
    assert!(attempt.apply_request().is_err());
    assert!(
        HostedAttempt::restore(
            &saved,
            Selection {
                account_id: "other".into(),
                machine_id: "hosted-machine".into()
            },
            &f.budget,
            20
        )
        .is_err()
    );
    let mut config = OperatorConfig::decode(&serde_json::to_vec(&serde_json::json!({"version":1,"state_root":f._dir.path(),"machine_id":"hosted-machine","transport_config":f._dir.path().join("tls"),"initial_owner_root":hex::encode(f.root.verifying_key().to_bytes())})).unwrap()).unwrap();
    config.initial_owner_root = Some(hex::encode([1; 32]));
    assert!(config.owner_root(&f.store).is_err());
}
