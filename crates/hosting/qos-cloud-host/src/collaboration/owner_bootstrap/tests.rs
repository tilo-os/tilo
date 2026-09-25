use super::*;
use ed25519_dalek::SigningKey;
use qos_peer_state::join::sign_membership_proof;
use qos_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    root: SigningKey,
    first: SigningKey,
    device: SigningKey,
    first_cert: DeviceCert,
    cert: DeviceCert,
}
impl Fixture {
    fn new() -> Self {
        Self::with_expiry(None)
    }
    fn with_expiry(device_expiry: Option<u64>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[7; 32]);
        let first = SigningKey::from_bytes(&[8; 32]);
        let device = SigningKey::from_bytes(&[9; 32]);
        let mut store = Store::open(dir.path(), "owner-bootstrap").unwrap();
        let request = crate::tests::approved(store.inspect(10).unwrap(), &root, &first);
        let first_cert = request.browser_certificate.clone();
        store.apply(&request, 11).unwrap();
        let mut request = crate::tests::approved(store.inspect(20).unwrap(), &root, &device);
        request.browser_certificate.expires_at_ns = device_expiry;
        qos_user_identity::sign_device_cert(&mut request.browser_certificate, &root).unwrap();
        qos_user_identity::cloud_enrollment::sign_request(&mut request, &root, &device).unwrap();
        let cert = request.browser_certificate.clone();
        store.apply(&request, 21).unwrap();
        Self {
            dir,
            store,
            root,
            first,
            device,
            first_cert,
            cert,
        }
    }
    fn request(&mut self, id: u8) -> MembershipCompleteRequest {
        let challenge = self
            .store
            .membership_challenge(
                self.cert.device_public_key,
                MembershipChallengeRequest {
                    room: self.store.state.room.clone(),
                    workspace_id: qos_relay::room_workspace_id(&self.store.state.room).unwrap(),
                    device_cert: self.cert.clone(),
                    request_id: [id; 32],
                },
                30,
            )
            .unwrap();
        MembershipCompleteRequest {
            proof: sign_membership_proof(challenge.challenge, &self.device).unwrap(),
        }
    }
    fn bytes(&self) -> Vec<u8> {
        std::fs::read(self.dir.path().join("state.json")).unwrap()
    }
}

#[test]
fn cloud_owner_bootstrap_actual_signed_governance_and_restart_retry_are_exact() {
    let mut f = Fixture::new();
    let request = f.request(40);
    let before = f.store.state.generation;
    let result = f
        .store
        .owner_membership_complete(f.cert.device_public_key, request.clone(), 31)
        .unwrap();
    assert_eq!(result.generation, before + 1);
    assert_eq!(result.device_cert, f.cert);
    let mut peer = qos_peer_state::StatePeer::new(
        &result.room,
        result.authority.clone(),
        vec![result.authority.clone()],
        qos_user_identity::RevocationList::new(),
        now_time(32),
    )
    .unwrap();
    for message in &result.governance_messages {
        peer.receive_from(
            qos_peer_state::TransportOrigin::RelayDelta(result.authority.device_public_key),
            postcard::from_bytes(message).unwrap(),
            now_time(32),
        )
        .unwrap();
    }
    assert_eq!(
        peer.permissions(f.cert.device_public_key, now_time(32))
            .unwrap()
            .0,
        collab_types::Role::Owner
    );
    let state = f.bytes();
    let original = postcard::to_stdvec(&result).unwrap();
    assert_eq!(
        postcard::to_stdvec(
            &f.store
                .owner_membership_complete(f.cert.device_public_key, request.clone(), 32)
                .unwrap()
        )
        .unwrap(),
        original
    );
    drop(f.store);
    f.store = Store::open(f.dir.path(), "owner-bootstrap").unwrap();
    // Receipt retry survives the one-use challenge expiry; current membership is rechecked.
    let result = f
        .store
        .owner_membership_complete(f.cert.device_public_key, request, 400_000_000_000)
        .unwrap();
    assert_eq!(postcard::to_stdvec(&result).unwrap(), original);
    assert_eq!(f.bytes(), state);
}

#[test]
fn cloud_owner_bootstrap_changed_proof_endpoint_and_unissued_request_do_not_commit() {
    let mut f = Fixture::new();
    let request = f.request(41);
    let before = f.bytes();
    assert!(
        f.store
            .owner_membership_complete([0; 32], request.clone(), 31)
            .is_err()
    );
    let mut changed = request.clone();
    changed.proof.signature[0] ^= 1;
    assert!(
        f.store
            .owner_membership_complete(f.cert.device_public_key, changed, 31)
            .is_err()
    );
    let mut unissued = request.clone();
    unissued.proof.challenge.request_id = [99; 32];
    unissued.proof = sign_membership_proof(unissued.proof.challenge, &f.device).unwrap();
    assert!(
        f.store
            .owner_membership_complete(f.cert.device_public_key, unissued, 31)
            .is_err()
    );
    assert_eq!(f.bytes(), before);
    f.store
        .owner_membership_complete(f.cert.device_public_key, request.clone(), 31)
        .unwrap();
    let committed = f.bytes();
    let mut changed = request.clone();
    changed.proof.challenge.nonce[0] ^= 1;
    changed.proof = sign_membership_proof(changed.proof.challenge, &f.device).unwrap();
    assert!(
        f.store
            .owner_membership_complete(f.cert.device_public_key, changed, 32)
            .is_err()
    );
    assert!(
        f.store
            .owner_membership_complete([0; 32], request, 32)
            .is_err()
    );
    assert_eq!(f.bytes(), committed);
}

#[test]
fn cloud_owner_bootstrap_cached_receipt_never_overrides_current_revocation() {
    let mut f = Fixture::new();
    let request = f.request(42);
    f.store
        .owner_membership_complete(f.cert.device_public_key, request.clone(), 31)
        .unwrap();
    let mut entry = RevocationEntry {
        user_public_key: f.cert.user_public_key,
        revoked_device_public_key: f.cert.device_public_key,
        revoked_at_ns: 32,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &f.root).unwrap();
    f.store
        .receive(
            qos_peer_state::TransportOrigin::RelayDelta(f.first.verifying_key().to_bytes()),
            qos_peer_state::NeighbourMsg::IdentityHello {
                cert: Box::new(f.first_cert.clone()),
                revocations: vec![entry],
            },
            33,
        )
        .unwrap();
    let before = f.bytes();
    assert!(
        f.store
            .owner_membership_complete(f.cert.device_public_key, request.clone(), 34)
            .is_err()
    );
    drop(f.store);
    f.store = Store::open(f.dir.path(), "owner-bootstrap").unwrap();
    assert!(
        f.store
            .owner_membership_complete(f.cert.device_public_key, request, 35)
            .is_err()
    );
    assert_eq!(f.bytes(), before);
}

#[test]
fn cloud_owner_bootstrap_original_store_failure_barrier_never_exposes_success() {
    for checkpoint in [
        atomic_file::test_support::Checkpoint::BeforePublish,
        atomic_file::test_support::Checkpoint::BeforeCommit,
    ] {
        let mut f = Fixture::new();
        let request = f.request(43);
        let generation = f.store.state.generation;
        let result =
            crate::storage::test_support::with_save_failure(f.dir.path(), checkpoint, || {
                f.store
                    .owner_membership_complete(f.cert.device_public_key, request.clone(), 31)
            });
        assert!(result.is_err());
        assert!(
            f.store
                .owner_membership_complete(f.cert.device_public_key, request.clone(), 32)
                .is_err()
        );
        drop(f.store);
        f.store = Store::open(f.dir.path(), "owner-bootstrap").unwrap();
        let result = f
            .store
            .owner_membership_complete(f.cert.device_public_key, request.clone(), 33)
            .unwrap();
        assert_eq!(result.generation, generation + 1);
        let before = f.bytes();
        assert_eq!(
            f.store
                .owner_membership_complete(f.cert.device_public_key, request, 34)
                .unwrap(),
            result
        );
        assert_eq!(before, f.bytes());
    }
}

#[test]
fn cloud_owner_bootstrap_scope_expiry_and_current_device_expiry_refuse_without_commit() {
    let mut f = Fixture::new();
    let request = f.request(44);
    let before = f.bytes();
    for field in 0..3 {
        let mut changed = request.clone();
        match field {
            0 => changed.proof.challenge.workspace_id ^= 1,
            1 => changed.proof.challenge.authority_device_public_key = [1; 32],
            _ => changed.proof.challenge.room = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
        }
        changed.proof = sign_membership_proof(changed.proof.challenge, &f.device).unwrap();
        assert!(
            f.store
                .owner_membership_complete(f.cert.device_public_key, changed, 31)
                .is_err()
        );
    }
    assert!(
        f.store
            .owner_membership_complete(
                f.cert.device_public_key,
                request.clone(),
                request.proof.challenge.expires_at_ns
            )
            .is_err()
    );
    assert_eq!(f.bytes(), before);
    {
        let mut f = Fixture::with_expiry(Some(100));
        let request = f.request(45);
        f.store
            .owner_membership_complete(f.cert.device_public_key, request.clone(), 31)
            .unwrap();
        let before = f.bytes();
        assert!(
            f.store
                .owner_membership_complete(f.cert.device_public_key, request, 100)
                .is_err()
        );
        assert_eq!(f.bytes(), before);
    }
}

#[test]
fn cloud_owner_bootstrap_collaborator_refresh_roles_and_cached_results_stay_separate() {
    use qos_peer_state::join::MemberRole;
    let mut f = Fixture::new();
    let root = SigningKey::from_bytes(&[71; 32]);
    let device = SigningKey::from_bytes(&[72; 32]);
    let (cert, _) = crate::tests::invite_and_admit(
        &mut f.store,
        &f.first,
        &root,
        &device,
        "owner-test-guest",
        [51; 32],
        40,
    );
    for (i, role) in [
        MemberRole::Viewer,
        MemberRole::Commenter,
        MemberRole::Editor,
    ]
    .into_iter()
    .enumerate()
    {
        f.store
            .action(
                f.first.verifying_key().to_bytes(),
                &crate::ActionRequest {
                    version: 1,
                    room: f.store.state.room.clone(),
                    invocation_id: format!("owner-test-role-{i}"),
                    operation: crate::NativeOperation::CollaborationSetRole {
                        user_public_key: cert.user_public_key,
                        role,
                    },
                },
                50 + i as u64,
            )
            .unwrap();
        let challenge = f
            .store
            .membership_challenge(
                cert.device_public_key,
                MembershipChallengeRequest {
                    room: f.store.state.room.clone(),
                    workspace_id: qos_relay::room_workspace_id(&f.store.state.room).unwrap(),
                    device_cert: cert.clone(),
                    request_id: [60 + i as u8; 32],
                },
                60 + i as u64,
            )
            .unwrap();
        let request = MembershipCompleteRequest {
            proof: sign_membership_proof(challenge.challenge, &device).unwrap(),
        };
        let before = f.bytes();
        assert!(
            f.store
                .owner_membership_complete(cert.device_public_key, request.clone(), 70 + i as u64)
                .is_err()
        );
        assert_eq!(f.bytes(), before);
        let bootstrap = f
            .store
            .membership_complete(cert.device_public_key, request.clone(), 70 + i as u64)
            .unwrap();
        assert_eq!(bootstrap.role, role);
        let before = f.bytes();
        assert_eq!(
            f.store
                .membership_complete(cert.device_public_key, request.clone(), 80 + i as u64)
                .unwrap(),
            bootstrap
        );
        assert!(
            f.store
                .owner_membership_complete(cert.device_public_key, request, 80 + i as u64)
                .is_err()
        );
        assert_eq!(f.bytes(), before);
    }
}

#[test]
fn cloud_owner_bootstrap_typed_receipt_rejects_unknown_versions_fields_bounds_and_scope() {
    let mut f = Fixture::new();
    let request = f.request(46);
    let result = f
        .store
        .owner_membership_complete(f.cert.device_public_key, request.clone(), 31)
        .unwrap();
    let key = receipt::key(f.cert.device_public_key);
    let original = f.store.state.consumed.get(&key).unwrap().1.clone();
    let decoded = receipt::OwnerReceipt::decode(&original).unwrap();
    assert_eq!(decoded.request_id, [46; 32]);
    assert_eq!(decoded.device, f.cert.device_public_key);
    assert_eq!(decoded.generation, result.generation);
    // Minimal: a receipt never retains the reply.
    assert!(serde_json::to_vec(&original).unwrap().len() < 1024);
    let mut cases = Vec::new();
    for (field, value) in [
        ("version", serde_json::json!(1)),
        ("purpose", serde_json::json!("enrollment")),
        ("unknown", serde_json::json!(true)),
        ("generation", serde_json::json!(0)),
        ("request_id", serde_json::json!(vec![0u8; 32])),
        ("device", serde_json::json!(vec![1u8; 32])),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        cases.push(changed);
    }
    let mut nested = serde_json::json!(null);
    for _ in 0..20 {
        nested = serde_json::json!([nested]);
    }
    cases.push(nested);
    let mut large = original.clone();
    large["padding"] = serde_json::json!("x".repeat(2048));
    cases.push(large);
    for value in cases {
        let rejected =
            receipt::OwnerReceipt::decode(&value).is_err() || value["device"] != original["device"];
        assert!(rejected);
        f.store.state.consumed.get_mut(&key).unwrap().1 = value;
        let before = f.bytes();
        assert!(
            f.store
                .owner_membership_complete(f.cert.device_public_key, request.clone(), 32)
                .is_err()
        );
        assert!(
            f.store
                .membership_refresh(f.cert.device_public_key, request.clone(), 32)
                .is_err()
        );
        assert_eq!(f.bytes(), before);
    }
    // A stored digest that disagrees with the typed receipt is refused too.
    f.store.state.consumed.get_mut(&key).unwrap().1 = original.clone();
    f.store.state.consumed.get_mut(&key).unwrap().0 = hex::encode([5; 32]);
    assert!(
        f.store
            .owner_membership_complete(f.cert.device_public_key, request.clone(), 32)
            .is_err()
    );
    f.store.state.consumed.get_mut(&key).unwrap().0 = hex::encode(decoded.proof_digest);
    assert_eq!(
        f.store
            .owner_membership_complete(f.cert.device_public_key, request, 32)
            .unwrap(),
        result
    );
}

fn owner_keys(store: &Store) -> Vec<String> {
    store
        .state
        .consumed
        .keys()
        .filter(|key| key.starts_with("owner-membership"))
        .cloned()
        .collect()
}

#[test]
fn cloud_owner_bootstrap_state_size_is_constant_over_fifty_refreshes() {
    let mut f = Fixture::new();
    let mut sizes = Vec::new();
    for i in 0..50u8 {
        let request = f.request(100 + i);
        let MembershipRefresh::Owner(result) = f
            .store
            .membership_refresh(f.cert.device_public_key, request, 31 + u64::from(i))
            .unwrap()
        else {
            panic!("structural owner must receive the owner bootstrap")
        };
        assert_eq!(
            admitted_owner_role(&result, f.cert.device_public_key),
            collab_types::Role::Owner
        );
        sizes.push(f.bytes().len());
        assert_eq!(
            owner_keys(&f.store),
            vec![receipt::key(f.cert.device_public_key)]
        );
        assert!(
            super::super::issued_challenges(f.dir.path()).len() <= MEMBERSHIP_CHALLENGES_PER_DEVICE
        );
    }
    // Once the bounded outstanding challenges are reached, nothing grows: an
    // unchanged governed state is answered without a commit or new messages.
    let steady = &sizes[10..];
    let (min, max) = (steady.iter().min().unwrap(), steady.iter().max().unwrap());
    assert!(max - min < 1024, "state size drifted: {sizes:?}");
    // The only growth is the bounded set of outstanding challenges.
    assert!(
        sizes[49] < sizes[0] + MEMBERSHIP_CHALLENGES_PER_DEVICE * 2048,
        "state grew: {sizes:?}"
    );
}

#[test]
fn cloud_owner_bootstrap_committed_refreshes_keep_one_minimal_receipt() {
    // Without process reuse (a restarted host) every refresh commits; the
    // Store still keeps one minimal receipt and no retained bootstraps.
    let mut f = Fixture::new();
    let mut growth = Vec::new();
    for i in 0..10u8 {
        reuse::forget(f.dir.path());
        let before = f.bytes().len();
        let generation = f.store.state.generation;
        let request = f.request(160 + i);
        let result = f
            .store
            .owner_membership_complete(f.cert.device_public_key, request, 31 + u64::from(i))
            .unwrap();
        assert!(result.generation > generation);
        let encoded = postcard::to_stdvec(&result).unwrap().len();
        growth.push((f.bytes().len() - before, encoded));
        assert_eq!(
            owner_keys(&f.store),
            vec![receipt::key(f.cert.device_public_key)]
        );
    }
    for (grew, reply) in &growth[1..] {
        assert!(grew < reply, "a refresh retained its reply: {growth:?}");
    }
}

#[test]
fn cloud_owner_bootstrap_replay_is_exact_equivalent_or_refused() {
    let mut f = Fixture::new();
    let request = f.request(120);
    let first = f
        .store
        .owner_membership_complete(f.cert.device_public_key, request.clone(), 31)
        .unwrap();
    // Lost reply, same process: byte-identical and no commit.
    let committed = f.bytes();
    assert_eq!(
        f.store
            .owner_membership_complete(f.cert.device_public_key, request.clone(), 40)
            .unwrap(),
        first
    );
    assert_eq!(f.bytes(), committed);
    // Lost reply after a host restart: an equivalent current bootstrap.
    reuse::forget(f.dir.path());
    drop(f.store);
    f.store = Store::open(f.dir.path(), "owner-bootstrap").unwrap();
    let rebuilt = f
        .store
        .owner_membership_complete(f.cert.device_public_key, request.clone(), 50)
        .unwrap();
    assert!(rebuilt.generation > first.generation);
    assert_eq!(rebuilt.device_cert, first.device_cert);
    assert_eq!(
        admitted_owner_role(&rebuilt, f.cert.device_public_key),
        collab_types::Role::Owner
    );
    assert_eq!(
        owner_keys(&f.store),
        vec![receipt::key(f.cert.device_public_key)]
    );
    // ...which then replays exactly again.
    let committed = f.bytes();
    assert_eq!(
        f.store
            .membership_refresh(f.cert.device_public_key, request.clone(), 60)
            .unwrap(),
        MembershipRefresh::Owner(rebuilt.clone())
    );
    assert_eq!(f.bytes(), committed);
    // Past the replay window: refused, nothing committed.
    assert!(
        f.store
            .owner_membership_complete(
                f.cert.device_public_key,
                request.clone(),
                50 + receipt::REPLAY_WINDOW_NS
            )
            .is_err()
    );
    assert_eq!(f.bytes(), committed);
    // A newer refresh supersedes the receipt: the older request ID is refused.
    reuse::forget(f.dir.path());
    let newer = f.request(121);
    f.store
        .owner_membership_complete(f.cert.device_public_key, newer, 70)
        .unwrap();
    let committed = f.bytes();
    assert!(
        f.store
            .membership_refresh(f.cert.device_public_key, request, 71)
            .is_err()
    );
    assert_eq!(f.bytes(), committed);
}

#[test]
fn cloud_owner_bootstrap_legacy_receipts_are_pruned_on_next_write() {
    let mut f = Fixture::new();
    f.store.state.consumed.insert(
        "owner-membership-v1:0101".into(),
        (
            hex::encode([1; 32]),
            serde_json::json!({"bootstrap": "x".repeat(4096)}),
        ),
    );
    // Challenge issuance no longer commits (lane M); the next commit prunes.
    let _ = f.request(130);
    let candidate = f.store.state.clone();
    f.store.commit(candidate).unwrap();
    assert!(owner_keys(&f.store).is_empty());
    assert!(
        !String::from_utf8(f.bytes())
            .unwrap()
            .contains("owner-membership-v1")
    );
}

#[test]
fn cloud_owner_bootstrap_membership_challenges_are_swept_and_bounded() {
    let mut f = Fixture::new();
    for i in 0..10u8 {
        let _ = f.request(140 + i);
    }
    assert_eq!(
        super::super::issued_challenges(f.dir.path()).len(),
        MEMBERSHIP_CHALLENGES_PER_DEVICE
    );
    // The newest survive; evicted request IDs are no longer answerable.
    assert!(super::super::issued_challenges(f.dir.path()).contains_key(&hex::encode([149; 32])));
    assert!(!super::super::issued_challenges(f.dir.path()).contains_key(&hex::encode([140; 32])));
    // Expired challenges are swept on the next issuance.
    let later = 30 + MEMBERSHIP_CHALLENGE_LIFETIME_NS;
    f.store
        .membership_challenge(
            f.cert.device_public_key,
            MembershipChallengeRequest {
                room: f.store.state.room.clone(),
                workspace_id: qos_relay::room_workspace_id(&f.store.state.room).unwrap(),
                device_cert: f.cert.clone(),
                request_id: [150; 32],
            },
            later,
        )
        .unwrap();
    assert_eq!(super::super::issued_challenges(f.dir.path()).len(), 1);
    // An expired challenge under a reused request ID is reissued, not replayed.
    let reissued = f
        .store
        .membership_challenge(
            f.cert.device_public_key,
            MembershipChallengeRequest {
                room: f.store.state.room.clone(),
                workspace_id: qos_relay::room_workspace_id(&f.store.state.room).unwrap(),
                device_cert: f.cert.clone(),
                request_id: [150; 32],
            },
            later + MEMBERSHIP_CHALLENGE_LIFETIME_NS,
        )
        .unwrap();
    assert!(reissued.challenge.expires_at_ns > later + MEMBERSHIP_CHALLENGE_LIFETIME_NS);
}

#[test]
fn cloud_owner_bootstrap_cached_reply_cannot_survive_root_signed_host_revocation() {
    let mut f = Fixture::new();
    let request = f.request(47);
    f.store
        .owner_membership_complete(f.cert.device_public_key, request.clone(), 31)
        .unwrap();
    let mut entry = RevocationEntry {
        user_public_key: f.cert.user_public_key,
        revoked_device_public_key: f
            .store
            .state
            .bound
            .as_ref()
            .unwrap()
            .authority
            .device_public_key,
        revoked_at_ns: 32,
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &f.root).unwrap();
    f.store
        .receive(
            qos_peer_state::TransportOrigin::RelayDelta(f.first.verifying_key().to_bytes()),
            qos_peer_state::NeighbourMsg::IdentityHello {
                cert: Box::new(f.first_cert.clone()),
                revocations: vec![entry],
            },
            33,
        )
        .unwrap();
    let before = f.bytes();
    assert!(
        f.store
            .owner_membership_complete(f.cert.device_public_key, request, 34)
            .is_err()
    );
    drop(f.store);
    assert!(Store::open(f.dir.path(), "owner-bootstrap").is_err());
    assert_eq!(
        std::fs::read(f.dir.path().join("state.json")).unwrap(),
        before
    );
}

fn admitted_owner_role(result: &OwnerBootstrapV1, device: [u8; 32]) -> collab_types::Role {
    let mut peer = qos_peer_state::StatePeer::new(
        &result.room,
        result.authority.clone(),
        vec![result.authority.clone()],
        qos_user_identity::RevocationList::new(),
        now_time(32),
    )
    .unwrap();
    for message in &result.governance_messages {
        peer.receive_from(
            qos_peer_state::TransportOrigin::RelayDelta(result.authority.device_public_key),
            postcard::from_bytes(message).unwrap(),
            now_time(32),
        )
        .unwrap();
    }
    peer.verify_current_owner(&result.device_cert, now_time(32))
        .unwrap();
    peer.permissions(device, now_time(32)).unwrap().0
}

#[test]
fn cloud_owner_bootstrap_refresh_routes_owner_device_to_owner_reply_and_replays_exactly() {
    let mut f = Fixture::new();
    let request = f.request(90);
    let before = f.store.state.generation;
    let MembershipRefresh::Owner(result) = f
        .store
        .membership_refresh(f.cert.device_public_key, request.clone(), 31)
        .unwrap()
    else {
        panic!("structural owner must receive the owner bootstrap")
    };
    assert_eq!(result.version, 1);
    assert_eq!(result.device_cert, f.cert);
    assert_eq!(result.generation, before + 1);
    assert_eq!(
        result.authority,
        f.store.state.bound.as_ref().unwrap().authority
    );
    assert_eq!(
        admitted_owner_role(&result, f.cert.device_public_key),
        collab_types::Role::Owner
    );
    let response = MembershipRefresh::Owner(result.clone()).into_response();
    assert!(matches!(
        &response,
        JoinTransportResponse::OwnerMembershipComplete(reply) if reply == &result
    ));
    f.store
        .verify_membership_refresh(
            f.cert.device_public_key,
            &MembershipRefresh::Owner(result.clone()),
            32,
        )
        .unwrap();
    assert!(
        f.store
            .verify_membership_refresh([0; 32], &MembershipRefresh::Owner(result.clone()), 32)
            .is_err()
    );
    let committed = f.bytes();
    let original = postcard::to_stdvec(&response).unwrap();
    for at in [32, 400_000_000_000] {
        let replay = f
            .store
            .membership_refresh(f.cert.device_public_key, request.clone(), at)
            .unwrap()
            .into_response();
        assert_eq!(postcard::to_stdvec(&replay).unwrap(), original);
    }
    assert_eq!(f.bytes(), committed);
    // The collaborator path never answers an owner, even for a fresh request.
    let request = f.request(91);
    let before = f.bytes();
    assert!(
        f.store
            .membership_complete(f.cert.device_public_key, request, 33)
            .is_err()
    );
    assert_eq!(f.bytes(), before);
}

#[test]
fn cloud_owner_bootstrap_refresh_refuses_wrong_device_changed_proof_and_unissued_request() {
    let mut f = Fixture::new();
    let request = f.request(92);
    let before = f.bytes();
    assert!(
        f.store
            .membership_refresh([0; 32], request.clone(), 31)
            .is_err()
    );
    assert!(
        f.store
            .membership_refresh(f.first_cert.device_public_key, request.clone(), 31)
            .is_err()
    );
    let mut changed = request.clone();
    changed.proof.signature[0] ^= 1;
    assert!(
        f.store
            .membership_refresh(f.cert.device_public_key, changed, 31)
            .is_err()
    );
    // Another owner-root device key cannot sign for this challenge.
    assert!(sign_membership_proof(request.proof.challenge.clone(), &f.first).is_err());
    let mut unissued = request.clone();
    unissued.proof.challenge.request_id = [93; 32];
    unissued.proof = sign_membership_proof(unissued.proof.challenge, &f.device).unwrap();
    assert!(
        f.store
            .membership_refresh(f.cert.device_public_key, unissued, 31)
            .is_err()
    );
    assert_eq!(f.bytes(), before);
    assert!(matches!(
        f.store
            .membership_refresh(f.cert.device_public_key, request.clone(), 31)
            .unwrap(),
        MembershipRefresh::Owner(_)
    ));
    let committed = f.bytes();
    let mut substituted = request;
    substituted.proof.challenge.nonce[0] ^= 1;
    substituted.proof = sign_membership_proof(substituted.proof.challenge, &f.device).unwrap();
    assert!(
        f.store
            .membership_refresh(f.cert.device_public_key, substituted, 32)
            .is_err()
    );
    assert_eq!(f.bytes(), committed);
}

#[test]
fn cloud_owner_bootstrap_refresh_keeps_collaborators_on_member_reply() {
    use qos_peer_state::join::MemberRole;
    let mut f = Fixture::new();
    let root = SigningKey::from_bytes(&[81; 32]);
    let device = SigningKey::from_bytes(&[82; 32]);
    let (cert, _) = crate::tests::invite_and_admit(
        &mut f.store,
        &f.first,
        &root,
        &device,
        "owner-refresh-guest",
        [83; 32],
        40,
    );
    let challenge = f
        .store
        .membership_challenge(
            cert.device_public_key,
            MembershipChallengeRequest {
                room: f.store.state.room.clone(),
                workspace_id: qos_relay::room_workspace_id(&f.store.state.room).unwrap(),
                device_cert: cert.clone(),
                request_id: [84; 32],
            },
            50,
        )
        .unwrap();
    let request = MembershipCompleteRequest {
        proof: sign_membership_proof(challenge.challenge, &device).unwrap(),
    };
    let before = f.bytes();
    assert!(
        f.store
            .owner_membership_complete(cert.device_public_key, request.clone(), 51)
            .is_err()
    );
    assert_eq!(f.bytes(), before);
    let MembershipRefresh::Member(bootstrap) = f
        .store
        .membership_refresh(cert.device_public_key, request.clone(), 51)
        .unwrap()
    else {
        panic!("collaborator must receive the member bootstrap")
    };
    assert_eq!(bootstrap.role, MemberRole::Editor);
    assert_eq!(bootstrap.device_cert, cert);
    assert!(matches!(
        MembershipRefresh::Member(bootstrap.clone()).into_response(),
        JoinTransportResponse::MembershipComplete(_)
    ));
    let committed = f.bytes();
    assert_eq!(
        f.store
            .membership_refresh(cert.device_public_key, request.clone(), 52)
            .unwrap(),
        MembershipRefresh::Member(bootstrap)
    );
    // A cached member response never becomes an owner reply.
    assert!(
        f.store
            .owner_membership_complete(cert.device_public_key, request, 52)
            .is_err()
    );
    assert_eq!(f.bytes(), committed);
}
