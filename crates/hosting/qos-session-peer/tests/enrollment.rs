use ed25519_dalek::SigningKey;
use qos_peer_state::enrollment::{self, ChallengeRequest};
use qos_session_peer::{
    SessionOwner,
    enrollment::{EnrollmentAuthority, public_bootstrap},
};
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
#[test]
fn eight_independent_keys_share_canonical_room_and_full_directory() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let records = owner.status().records;
    let mut service = EnrollmentAuthority::default();
    let now = qos_relay::clock::now_ns();
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
        let update: qos_user_identity::DirectoryUpdate =
            serde_json::from_value(response["bootstrap"]["directory_update"].clone()).unwrap();
        let author: qos_user_identity::DeviceCert =
            serde_json::from_value(response["bootstrap"]["directory_author"].clone()).unwrap();
        let mut fresh = qos_user_identity::WorkspaceDirectory::new(
            certificate.user_public_key,
            certificate.device_public_key,
        );
        fresh
            .apply_incremental(
                &update,
                &author,
                &qos_user_identity::RevocationList::new(),
                now,
            )
            .unwrap();
        let workspace = qos_relay::room_workspace_id(owner.peer().room()).unwrap();
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
    let now = qos_relay::clock::now_ns();
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
    let now = qos_relay::clock::now_ns();
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
    let now = qos_relay::clock::now_ns();
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
fn revoked_viewer_cannot_complete_or_refresh_membership() {
    use qos_user_identity::{
        RevocationEntry, RevocationList, RevocationReason, WorkspaceDirectory,
        persist::{self, IdentityPaths},
        sign_revocation_entry,
    };
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let key = SigningKey::from_bytes(&[24; 32]);
    let now = qos_relay::clock::now_ns();
    let mut service = EnrollmentAuthority::default();
    let complete = proof(&mut service, &owner, &key, 24, now);
    service.complete(&mut owner, complete, now).unwrap();
    let pending = proof(&mut service, &owner, &key, 24, now);
    let root = persist::read_signing_key(&IdentityPaths::under(path.join("identity")).user_privkey)
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
            qos_relay::room_workspace_id(owner.peer().room()).unwrap(),
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
}
