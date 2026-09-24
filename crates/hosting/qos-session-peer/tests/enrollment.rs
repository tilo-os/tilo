use ed25519_dalek::SigningKey;
use qos_peer_state::enrollment::{self, ChallengeRequest};
use qos_session_peer::{
    SessionOwner,
    enrollment::{EnrollmentAuthority, public_bootstrap},
};
use ui::automerge::AutoCommit;
use web_time::SystemTime;
fn request(key: &SigningKey, id: u8) -> ChallengeRequest {
    ChallengeRequest {
        device_public_key: hex::encode(key.verifying_key().to_bytes()),
        request_id: hex::encode([id; 32]),
    }
}
fn proof(
    authority: &mut EnrollmentAuthority,
    owner: &SessionOwner,
    key: &SigningKey,
    id: u8,
    now: u64,
) -> enrollment::CompleteRequest {
    let challenge = authority.challenge(owner, request(key, id), now).unwrap();
    enrollment::sign(&enrollment::decode(&challenge.challenge).unwrap(), key).unwrap()
}

fn owner_with_twenty_four_records(path: &std::path::Path) -> SessionOwner {
    let mut owner = SessionOwner::initialize(path).unwrap();
    let key = tilo_user_identity::persist::read_device_key(
        &tilo_user_identity::persist::IdentityPaths::under(
            owner.root().canonicalize().unwrap().join("identity"),
        )
        .device_privkey,
    )
    .unwrap();
    let mut candidate = owner.peer().clone();
    for index in candidate.rows().len()..24 {
        candidate
            .publish_document(
                "dev.qos.enrollment-fixture",
                &format!("record-{index:02}"),
                AutoCommit::new().save(),
                &key,
                SystemTime::now(),
            )
            .unwrap();
    }
    owner
        .commit_mutation_candidate(candidate, owner.directory().to_vec())
        .unwrap();
    owner.refresh_checkpoint().unwrap();
    assert_eq!(owner.status().records.len(), 24);
    assert_eq!(outgoing_bytes(&owner).len(), 25);
    assert_eq!(owner.publication_baseline().1, 25);
    owner
}

#[test]
fn verified_same_device_new_request_preserves_the_exact_publication_basis() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let mut owner = owner_with_twenty_four_records(&path);
    let key = SigningKey::from_bytes(&[31; 32]);
    let now = tilo_relay::clock::now_ns();
    let mut service = EnrollmentAuthority::default();
    let first = proof(&mut service, &owner, &key, 1, now);
    let certificate = service.complete(&mut owner, first, now).unwrap();
    let baseline = owner.publication_baseline();
    let outgoing = outgoing_bytes(&owner).to_vec();
    let kinds = outgoing_kinds(&owner).to_vec();
    assert_eq!(outgoing.len(), 25);

    let second = proof(&mut service, &owner, &key, 2, now);
    assert_eq!(
        service.complete(&mut owner, second, now).unwrap(),
        certificate
    );

    assert_eq!(owner.enrollment_count(), 1);
    assert_eq!(owner.peer().certificates().len(), 2);
    assert_eq!(owner.publication_baseline(), baseline);
    assert_eq!(outgoing_bytes(&owner), outgoing);
    assert_eq!(outgoing_kinds(&owner), kinds);

    let generation = owner.status().generation;
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    let mut service = EnrollmentAuthority::default();
    let retry = proof(&mut service, &owner, &key, 2, now);
    assert_eq!(
        service.complete(&mut owner, retry, now).unwrap(),
        certificate
    );
    assert_eq!(owner.status().generation, generation);
    assert_eq!(owner.publication_baseline(), baseline);
    assert_eq!(outgoing_bytes(&owner), outgoing);
}

#[test]
fn new_device_and_missing_directory_membership_still_replace_publication() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let now = tilo_relay::clock::now_ns();
    let mut service = EnrollmentAuthority::default();
    let first_key = SigningKey::from_bytes(&[32; 32]);
    let first = proof(&mut service, &owner, &first_key, 1, now);
    service.complete(&mut owner, first, now).unwrap();

    let before_new_device = owner.publication_baseline();
    let second_key = SigningKey::from_bytes(&[33; 32]);
    let second = proof(&mut service, &owner, &second_key, 2, now);
    service.complete(&mut owner, second, now).unwrap();
    assert_ne!(owner.publication_baseline(), before_new_device);

    let mut directory = tilo_user_identity::WorkspaceDirectory::load(
        owner.peer().authority().user_public_key,
        owner.peer().authority().device_public_key,
        owner.directory(),
    )
    .unwrap();
    let workspace = tilo_relay::room_workspace_id(owner.peer().room()).unwrap();
    directory
        .set_member_devices(workspace, &[owner.peer().authority().device_public_key])
        .unwrap();
    owner
        .commit_admission(owner.peer().clone(), directory.save())
        .unwrap();
    let before_readmission = owner.publication_baseline();

    let readmit = proof(&mut service, &owner, &first_key, 3, now);
    service.complete(&mut owner, readmit, now).unwrap();
    assert_ne!(owner.publication_baseline(), before_readmission);
    let directory = tilo_user_identity::WorkspaceDirectory::load(
        owner.peer().authority().user_public_key,
        owner.peer().authority().device_public_key,
        owner.directory(),
    )
    .unwrap();
    assert!(
        directory.as_map()[&workspace]
            .member_devices
            .contains(&first_key.verifying_key().to_bytes())
    );
}
#[test]
fn eight_independent_keys_share_canonical_room_and_full_directory() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let records = owner.status().records;
    let mut service = EnrollmentAuthority::default();
    let now = tilo_relay::clock::now_ns();
    for n in 1..=8 {
        let key = SigningKey::from_bytes(&[n; 32]);
        let complete = proof(&mut service, &owner, &key, n, now);
        let certificate = service.complete(&mut owner, complete, now).unwrap();
        assert_eq!(
            certificate.device_public_key,
            key.verifying_key().to_bytes()
        );
        assert!(!certificate.capabilities.can_admit_devices);
        let response = public_bootstrap(
            &owner,
            &certificate,
            serde_json::json!({"id":"public-address"}),
            "http://127.0.0.1:12345",
            now,
        )
        .unwrap();
        assert!(response.get("device_secret").is_none());
        assert_eq!(response["version"], 2);
        let update: tilo_user_identity::DirectoryUpdate =
            serde_json::from_value(response["bootstrap"]["directory_update"].clone()).unwrap();
        let author: tilo_user_identity::DeviceCert =
            serde_json::from_value(response["bootstrap"]["directory_author"].clone()).unwrap();
        let mut fresh = tilo_user_identity::WorkspaceDirectory::new(
            certificate.user_public_key,
            certificate.device_public_key,
        );
        fresh
            .apply_incremental(
                &update,
                &author,
                &tilo_user_identity::RevocationList::new(),
                now,
            )
            .unwrap();
        let workspace = tilo_relay::room_workspace_id(owner.peer().room()).unwrap();
        assert_eq!(
            fresh.as_map()[&workspace].member_devices.len(),
            usize::from(n) + 1
        );
        assert_eq!(owner.status().records, records);
    }
    assert_eq!(owner.enrollment_count(), 8);
    assert_eq!(owner.peer().certificates().len(), 9);
}
#[test]
fn lost_response_retry_after_restart_keeps_certificate_but_refreshes_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let key = SigningKey::from_bytes(&[21; 32]);
    let now = tilo_relay::clock::now_ns();
    let mut service = EnrollmentAuthority::default();
    let complete = proof(&mut service, &owner, &key, 7, now);
    let replay = complete.clone();
    let first = service.complete(&mut owner, complete, now).unwrap();
    let generation = owner.status().generation;
    assert!(service.complete(&mut owner, replay.clone(), now).is_err());
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    let mut service = EnrollmentAuthority::default();
    assert!(service.complete(&mut owner, replay, now).is_err());
    let complete = proof(&mut service, &owner, &key, 7, now);
    let second = service.complete(&mut owner, complete, now).unwrap();
    assert_eq!(first, second);
    assert_eq!(owner.enrollment_count(), 1);
    assert_eq!(owner.status().generation, generation);
    let response = public_bootstrap(
        &owner,
        &second,
        serde_json::json!({"id":"new-address"}),
        "http://127.0.0.1:56789",
        now,
    )
    .unwrap();
    assert_eq!(response["packet_relay"], "http://127.0.0.1:56789");
}
#[test]
fn proof_scope_expiry_signature_and_replay_are_enforced_without_membership_changes() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let before = owner.status();
    let key = SigningKey::from_bytes(&[22; 32]);
    let now = tilo_relay::clock::now_ns();
    let mut service = EnrollmentAuthority::default();
    let response = service.challenge(&owner, request(&key, 4), now).unwrap();
    let original = enrollment::decode(&response.challenge).unwrap();
    for kind in 0..3 {
        let mut changed = original.clone();
        match kind {
            0 => changed.root_public_key = [55; 32],
            1 => changed.room = uuid::Uuid::new_v4().to_string(),
            _ => changed.workspace_id = changed.workspace_id.wrapping_add(1),
        };
        let complete = enrollment::sign(&changed, &key).unwrap();
        assert_eq!(
            service
                .complete(&mut owner, complete, now)
                .unwrap_err()
                .status,
            403
        );
    }
    let mut forged = enrollment::sign(&original, &key).unwrap();
    forged.signature = hex::encode([0; 64]);
    assert!(service.complete(&mut owner, forged, now).is_err());
    let expired = enrollment::sign(&original, &key).unwrap();
    assert!(
        service
            .complete(&mut owner, expired, original.expires_at_ns)
            .is_err()
    );
    assert_eq!(owner.status(), before);
    assert_eq!(owner.enrollment_count(), 0);
}
#[test]
fn failed_native_commit_cannot_publish_an_issuance_result() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let before = owner.status();
    let key = SigningKey::from_bytes(&[23; 32]);
    let now = tilo_relay::clock::now_ns();
    let mut service = EnrollmentAuthority::default();
    let complete = proof(&mut service, &owner, &key, 3, now);
    std::fs::rename(path.join("session.bin"), path.join("saved.bin")).unwrap();
    std::fs::create_dir(path.join("session.bin")).unwrap();
    assert_eq!(
        service
            .complete(&mut owner, complete, now)
            .unwrap_err()
            .status,
        503
    );
    assert_eq!(owner.enrollment_count(), 0);
    assert_eq!(owner.status(), before);
    assert!(!owner.permits_publication());
}

#[test]
fn failed_same_device_audit_commit_cannot_return_enrollment_success() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let key = SigningKey::from_bytes(&[34; 32]);
    let now = tilo_relay::clock::now_ns();
    let mut service = EnrollmentAuthority::default();
    let first = proof(&mut service, &owner, &key, 1, now);
    service.complete(&mut owner, first, now).unwrap();
    let before = owner.status();
    let baseline = owner.publication_baseline();
    let outgoing = outgoing_bytes(&owner).to_vec();
    let second = proof(&mut service, &owner, &key, 2, now);

    std::fs::rename(path.join("session.bin"), path.join("saved.bin")).unwrap();
    std::fs::create_dir(path.join("session.bin")).unwrap();
    assert_eq!(
        service
            .complete(&mut owner, second, now)
            .unwrap_err()
            .status,
        503
    );
    assert_eq!(owner.status(), before);
    assert_eq!(owner.enrollment_count(), 1);
    assert_eq!(owner.publication_baseline(), baseline);
    assert_eq!(outgoing_bytes(&owner), outgoing);
    assert!(!owner.permits_publication());
}

#[test]
fn revoked_viewer_is_rejected_without_blocking_other_viewers() {
    use tilo_user_identity::{
        RevocationEntry, RevocationList, RevocationReason, WorkspaceDirectory,
        persist::{self, IdentityPaths},
        sign_revocation_entry,
    };
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let key = SigningKey::from_bytes(&[24; 32]);
    let now = tilo_relay::clock::now_ns();
    let mut service = EnrollmentAuthority::default();
    let complete = proof(&mut service, &owner, &key, 24, now);
    service.complete(&mut owner, complete, now).unwrap();
    let pending = proof(&mut service, &owner, &key, 24, now);
    let root = persist::read_signing_key(
        &IdentityPaths::under(owner.root().canonicalize().unwrap().join("identity")).user_privkey,
    )
    .unwrap();
    let mut revoked = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: key.verifying_key().to_bytes(),
        revoked_at_ns: now,
        reason: RevocationReason::Retired,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut revoked, &root).unwrap();
    let mut list = RevocationList::new();
    list.insert(revoked).unwrap();
    let candidate = qos_peer_state::StatePeer::restore(
        &owner.peer().durable().unwrap(),
        owner.peer().room(),
        owner.peer().authority().clone(),
        vec![],
        list,
        web_time::SystemTime::now(),
    )
    .unwrap();
    let mut directory = WorkspaceDirectory::load(
        root.verifying_key().to_bytes(),
        owner.peer().authority().device_public_key,
        owner.directory(),
    )
    .unwrap();
    directory
        .set_member_devices(
            tilo_relay::room_workspace_id(owner.peer().room()).unwrap(),
            &[owner.peer().authority().device_public_key],
        )
        .unwrap();
    owner.commit_admission(candidate, directory.save()).unwrap();
    let before = owner.status();
    assert_eq!(
        service
            .complete(&mut owner, pending, now)
            .unwrap_err()
            .status,
        403
    );
    assert_eq!(
        service
            .challenge(&owner, request(&key, 24), now)
            .unwrap_err()
            .status,
        403
    );
    assert_eq!(owner.status(), before);
    assert_eq!(owner.enrollment_count(), 1);

    // A retained revoked certificate is history, not an active bootstrap peer.
    let other = SigningKey::from_bytes(&[25; 32]);
    let request = proof(&mut service, &owner, &other, 25, now);
    let certificate = service.complete(&mut owner, request, now).unwrap();
    let bootstrap = public_bootstrap(
        &owner,
        &certificate,
        serde_json::json!({"id":"public-address"}),
        "http://127.0.0.1:12345",
        now,
    )
    .unwrap();
    let roster: Vec<tilo_user_identity::DeviceCert> =
        serde_json::from_value(bootstrap["bootstrap"]["certified_roster"].clone()).unwrap();
    let mut revoked = RevocationList::new();
    for entry in
        serde_json::from_value::<Vec<RevocationEntry>>(bootstrap["revocations"].clone()).unwrap()
    {
        revoked.insert(entry).unwrap();
    }
    let fresh = qos_peer_state::StatePeer::new(
        owner.peer().room(),
        owner.peer().authority().clone(),
        roster.clone(),
        revoked.clone(),
        web_time::SystemTime::now(),
    );
    assert!(
        fresh.is_ok(),
        "an unrelated revoked viewer must not block browser startup: {:?}",
        fresh.err()
    );
    let restored = qos_peer_state::StatePeer::restore(
        &owner.peer().durable().unwrap(),
        owner.peer().room(),
        owner.peer().authority().clone(),
        roster,
        revoked,
        web_time::SystemTime::now(),
    )
    .unwrap();
    assert!(
        restored
            .permissions(
                other.verifying_key().to_bytes(),
                web_time::SystemTime::now()
            )
            .is_ok()
    );
    assert!(
        restored
            .permissions(key.verifying_key().to_bytes(), web_time::SystemTime::now())
            .is_err()
    );
    assert_eq!(restored.rows(), owner.peer().rows());
}

fn outgoing_bytes(owner: &qos_session_peer::SessionOwner) -> Vec<Vec<u8>> {
    owner.outgoing().iter().map(|p| p.bytes.to_vec()).collect()
}

#[test]
fn local_bootstrap_admits_real_governance_without_transport_sync() {
    use qos_peer_state::{StatePeer, TransportOrigin, transport::Retention};
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let now = tilo_relay::clock::now_ns();
    let key = SigningKey::from_bytes(&[73; 32]);
    let mut enrollment = EnrollmentAuthority::default();
    let complete = proof(&mut enrollment, &owner, &key, 73, now);
    let certificate = enrollment.complete(&mut owner, complete, now).unwrap();
    let reply = public_bootstrap(
        &owner,
        &certificate,
        serde_json::json!({"id":"unused-transport"}),
        "http://127.0.0.1:1",
        now,
    )
    .unwrap();
    let bootstrap = &reply["bootstrap"];
    let messages: Vec<Vec<u8>> = serde_json::from_value(bootstrap["governance_messages"].clone())
        .expect("fresh local enrollment includes actual signed governance");
    let kinds: Vec<Retention> =
        serde_json::from_value(bootstrap["governance_retention"].clone()).unwrap();
    assert_eq!(messages, outgoing_bytes(&owner));
    assert_eq!(kinds, outgoing_kinds(&owner));
    assert_eq!(
        bootstrap["governance_generation"],
        owner.status().generation.to_string()
    );
    let roster = serde_json::from_value(bootstrap["certified_roster"].clone()).unwrap();
    let mut fresh = StatePeer::new(
        owner.peer().room(),
        owner.peer().authority().clone(),
        roster,
        tilo_user_identity::RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    assert!(fresh.admitted_room().is_none());
    for bytes in &messages {
        let origin = TransportOrigin::RelayDelta(owner.peer().authority().device_public_key);
        let outcome = fresh
            .receive_from(
                origin,
                postcard::from_bytes(bytes).unwrap(),
                SystemTime::now(),
            )
            .unwrap();
        assert_eq!(outcome.rejected, 0);
    }
    assert!(fresh.admitted_room().is_some());
    assert_eq!(fresh.rows(), owner.peer().rows());
    assert!(
        fresh
            .permissions(certificate.device_public_key, SystemTime::now())
            .is_ok()
    );
}
fn outgoing_kinds(
    owner: &qos_session_peer::SessionOwner,
) -> Vec<qos_peer_state::transport::Retention> {
    owner.outgoing().iter().map(|p| p.kind).collect()
}
