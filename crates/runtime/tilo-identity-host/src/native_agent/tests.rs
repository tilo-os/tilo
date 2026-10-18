use super::*;
use crate::join_proof::tests::Fixture;
use std::fs;
use tilo_user_identity::{RevocationReason, verify_device_cert};

mod selected_person;

fn request() -> NativeAgentRequest {
    NativeAgentRequest {
        agent_id: "native-helper".into(),
        label: "Helper".into(),
        color: [60, 120, 180],
        apps: vec!["notes".into(), "chat".into()],
        expires_at_ns: tilo_clock::unix_now_ns() + 3600 * 1_000_000_000,
    }
}

#[test]
fn real_selected_person_certifies_actor_and_canonical_delegation() {
    let fixture = Fixture::new();
    let request = request();
    let actor = provision(&fixture.room, &request).expect("real identity enrollment");
    let cert = actor.certificate();
    verify_device_cert(cert).expect("canonical certificate verifies");
    assert_eq!(cert.user_public_key, fixture.user_public_key);
    assert_eq!(cert.capabilities, minimal_capabilities());
    assert_eq!(cert.expires_at_ns, Some(request.expires_at_ns));
    let (_, issuer) = crate::broker::trusted_current_device(&fixture.room).expect("issuer");
    assert_ne!(cert.device_public_key, issuer.device_public_key);
    let delegation = Delegation::decode(&actor.delegation().encode()).expect("canonical wire");
    delegation
        .verify(&fixture.user_public_key, tilo_clock::unix_now_ns())
        .expect("real root signature");
    ed25519_dalek::VerifyingKey::from_bytes(&fixture.user_public_key)
        .expect("selected person's root")
        .verify_strict(
            &delegation
                .signing_bytes()
                .expect("bounded canonical message"),
            &ed25519_dalek::Signature::from_bytes(&delegation.signature),
        )
        .expect("host custody signed the owning crate's canonical message");
    assert_eq!(delegation.device_public_key, cert.device_public_key);
    assert_eq!(
        delegation.delegate.agent_id(),
        Some(request.agent_id.as_str())
    );
    assert_eq!(delegation.scope.rooms, vec![fixture.room.clone()]);
    assert_eq!(delegation.scope.apps, request.apps);
    assert!(!delegation.permits_app("settings"));
    assert!(!delegation.permits_room(&tilo_desktop_model::RoomId::new().to_string()));
    assert!(
        delegation
            .verify(&[7; 32], tilo_clock::unix_now_ns())
            .is_err()
    );
    ed25519_dalek::VerifyingKey::from_bytes(&cert.device_public_key)
        .expect("actor public key")
        .verify_strict(
            &runtime_identity::collaboration_join_signing_bytes(&fixture.room),
            &ed25519_dalek::Signature::from_bytes(&actor.join_proof().expect("bounded join")),
        )
        .expect("actor signs, not the person or host device");
    assert!(
        fixture
            .store()
            .load(&fixture.identity_id)
            .expect("fleet")
            .certificates
            .contains(cert)
    );
}

#[test]
fn actor_key_survives_reenrollment_and_is_private() {
    let fixture = Fixture::new();
    let request = request();
    let actor = provision(&fixture.room, &request).expect("first enrollment");
    let public_key = actor.certificate().device_public_key;
    let key_path = actor.key_path.clone();
    let metadata = fs::metadata(&key_path).expect("persisted key");
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert_eq!(
        fs::metadata(key_path.parent().expect("parent"))
            .expect("directory")
            .mode()
            & 0o777,
        0o700
    );
    drop(actor);
    let restored = provision(&fixture.room, &request).expect("restore enrollment");
    assert_eq!(restored.certificate().device_public_key, public_key);
    restored.join_proof().expect("restored actor really signs");
    fs::write(&key_path, [12; 32]).expect("replace custody");
    assert!(
        restored.join_proof().is_err(),
        "old handle cannot use stale custody"
    );
}

#[test]
fn reenrollment_reuses_the_exact_admitted_certificate() {
    let fixture = Fixture::new();
    let mut request = request();
    let first = provision(&fixture.room, &request).expect("first enrollment");
    let admitted = first.certificate().clone();
    let restored = provision(&fixture.room, &request).expect("repeat enrollment");
    assert_eq!(restored.certificate(), &admitted);
    // A new bounded delegation may narrow its lifetime or change its label,
    // but neither operation may replace the canonical admitted certificate.
    request.expires_at_ns -= 60_000_000_000;
    request.label = "Renamed helper".into();
    let narrowed = provision(&fixture.room, &request).expect("narrower delegation");
    assert_eq!(narrowed.certificate(), &admitted);
    assert_eq!(narrowed.delegation().expires_at_ns, request.expires_at_ns);
    assert_eq!(narrowed.delegation().label, request.label);
    verify_device_cert(narrowed.certificate()).expect("retained real certificate signature");
    narrowed
        .delegation()
        .verify(&fixture.user_public_key, tilo_clock::unix_now_ns())
        .expect("fresh delegation has a real selected-person signature");
    assert_eq!(first.join_proof().unwrap(), narrowed.join_proof().unwrap());
    let fleet = fixture.store().load(&fixture.identity_id).expect("fleet");
    let actor_certificates: Vec<_> = fleet
        .certificates
        .iter()
        .filter(|cert| cert.device_public_key == admitted.device_public_key)
        .collect();
    assert_eq!(actor_certificates, vec![&admitted]);
}

#[test]
fn retained_restart_preserves_exact_certificate_and_room_delegations() {
    let fixture = Fixture::new();
    let mut request = request();
    let first = provision_retained(&fixture.room, &request).expect("initial enrollment");
    let certificate = first.certificate().clone();
    let enrollment = first.delegation().clone();
    let task_room = tilo_desktop_model::RoomId::new().to_string();
    let admitted = first.delegate_room(&task_room).expect("initial room grant");
    assert_eq!(
        first.delegate_room(&task_room).expect("repeat prepare"),
        admitted
    );
    assert_eq!(admitted.issued_at_ns, certificate.issued_at_ns);
    assert_eq!(Some(admitted.expires_at_ns), certificate.expires_at_ns);
    drop(first);

    // A new process recomputes now + one day. Make that increase explicit,
    // without sleeping or relying on the wall clock advancing in the test.
    request.expires_at_ns = tilo_clock::unix_now_ns() + MAX_NATIVE_AGENT_LIFETIME_NS;
    assert!(Some(request.expires_at_ns) > certificate.expires_at_ns);
    let restored = provision_retained(&fixture.room, &request).expect("restart custody");
    assert_eq!(restored.certificate(), &certificate);
    assert_eq!(restored.delegation(), &enrollment);
    let restarted = restored
        .delegate_room(&task_room)
        .expect("restart room grant");
    assert_eq!(restarted, admitted);
    assert_eq!(restarted.encode(), admitted.encode());
    restored
        .join_proof_for(&admitted)
        .expect("retained grant still usable");
    restarted
        .verify(&fixture.user_public_key, tilo_clock::unix_now_ns())
        .expect("deterministic grant is signed by the selected person");
    let other_room = tilo_desktop_model::RoomId::new().to_string();
    let other = restored
        .delegate_room(&other_room)
        .expect("another bounded room");
    assert_eq!(other.scope.rooms, vec![other_room]);
    assert!(!other.permits_room(&task_room));
    assert_ne!(other.encode(), admitted.encode());
    assert_eq!(other.expires_at_ns, admitted.expires_at_ns);
}

#[test]
fn retained_provisioning_honors_a_shorter_requested_lifetime() {
    let fixture = Fixture::new();
    let mut request = request();
    let actor = provision_retained(&fixture.room, &request).expect("enrollment");
    request.expires_at_ns -= 60_000_000_000;
    let narrowed = provision_retained(&fixture.room, &request).expect("narrowed grant");
    assert_eq!(narrowed.certificate(), actor.certificate());
    assert_eq!(narrowed.delegation().expires_at_ns, request.expires_at_ns);
    let task_room = tilo_desktop_model::RoomId::new().to_string();
    assert_eq!(
        narrowed.delegate_room(&task_room).unwrap().expires_at_ns,
        request.expires_at_ns
    );
}

#[test]
fn longer_lifetime_requires_explicit_certificate_lifecycle() {
    let fixture = Fixture::new();
    let mut request = request();
    let actor = provision(&fixture.room, &request).expect("enrollment");
    request.expires_at_ns += 60_000_000_000;
    let error = provision(&fixture.room, &request)
        .err()
        .expect("no implicit renewal");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("explicit renewal or rotation"));
    assert!(
        fixture
            .store()
            .load(&fixture.identity_id)
            .unwrap()
            .certificates
            .contains(actor.certificate())
    );
    actor.join_proof().expect("original grant remains live");
}

#[test]
fn expired_certificate_is_not_silently_reissued() {
    let fixture = Fixture::new();
    let request = request();
    let actor = provision(&fixture.room, &request).expect("enrollment");
    let mut expired = actor.certificate().clone();
    let now = tilo_clock::unix_now_ns();
    expired.issued_at_ns = now - 120_000_000_000;
    expired.expires_at_ns = Some(now - 60_000_000_000);
    let store = fixture.store();
    let (root, _) = store
        .identity(&fixture.identity_id)
        .expect("real root custody");
    expired.signature = root
        .sign(&tilo_user_identity::device_cert_signing_digest(&expired).unwrap())
        .expect("sign expired historical certificate");
    store
        .remember_certificate(&fixture.identity_id, expired.clone())
        .expect("remember valid historical signature");
    let error = provision(&fixture.room, &request)
        .err()
        .expect("expired actor denied");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("explicit renewal or rotation"));
    let retained_error = provision_retained(&fixture.room, &request)
        .err()
        .expect("retaining custody cannot revive an expired actor");
    assert_eq!(retained_error.kind(), io::ErrorKind::PermissionDenied);
    assert!(
        retained_error
            .to_string()
            .contains("explicit renewal or rotation")
    );
    assert!(
        store
            .load(&fixture.identity_id)
            .unwrap()
            .certificates
            .contains(&expired)
    );
}

#[test]
fn unsafe_requests_never_create_actor_custody() {
    let fixture = Fixture::new();
    let mut request = request();
    request.apps.clear();
    assert!(
        provision(&fixture.room, &request).is_err(),
        "empty means all apps"
    );
    request.apps = vec!["notes".into()];
    request.expires_at_ns =
        tilo_clock::unix_now_ns() + MAX_NATIVE_AGENT_LIFETIME_NS + 60_000_000_000;
    assert!(provision(&fixture.room, &request).is_err());
    request.expires_at_ns = 1;
    assert!(provision(&fixture.room, &request).is_err());
    request.expires_at_ns = tilo_clock::unix_now_ns() + 60_000_000_000;
    request.agent_id = "x".repeat(MAX_DELEGATE_TEXT_BYTES + 1);
    assert!(provision(&fixture.room, &request).is_err());
    assert!(provision("not-a-room", &request).is_err());
    let registry = IdentityRegistryStore::from_env().expect("registry");
    assert!(!registry.identity_dir().join("native-agents").exists());
}

#[test]
fn revoked_actor_cannot_sign_or_reenroll() {
    let fixture = Fixture::new();
    let request = request();
    let actor = provision(&fixture.room, &request).expect("enrollment");
    fixture
        .store()
        .revoke(
            &fixture.identity_id,
            actor.certificate().device_public_key,
            RevocationReason::Lost,
            tilo_clock::unix_now_ns(),
        )
        .expect("revoke enrolled actor with real root");
    assert!(actor.join_proof().is_err());
    assert!(
        actor
            .delegate_room(&tilo_desktop_model::RoomId::new().to_string())
            .is_err()
    );
    assert!(
        provision(&fixture.room, &request).is_err(),
        "cannot renew a revoked key"
    );
    assert!(
        provision_retained(&fixture.room, &request).is_err(),
        "retaining custody cannot revive a revoked actor"
    );
}

#[test]
fn issuer_revocation_and_selection_removal_stop_retained_custody() {
    let fixture = Fixture::new();
    let actor = provision(&fixture.room, &request()).expect("enrollment");
    let store = IdentityRegistryStore::from_env().expect("registry");
    let mut registry = store.load().expect("read registry");
    let original = registry.clone();
    // Reconciliation deliberately restores a missing default while any record
    // remains. Remove the actual record as well, rather than testing a state
    // production can never observe.
    let record = &original.identities[&fixture.identity_id].record_path;
    let held_dir = store.identity_dir().join("held-record");
    fs::create_dir(&held_dir).expect("holding directory");
    let held = held_dir.join("identity.json");
    fs::rename(record, &held).expect("remove identity record");
    registry.default_identity = None;
    registry.workspace_identities.clear();
    registry.identities.clear();
    store.save(&registry).expect("remove selection");
    assert!(actor.join_proof().is_err());
    fs::rename(&held, record).expect("restore identity record");
    store.save(&original).expect("restore selection");
    let (_, issuer) = crate::broker::trusted_current_device(&fixture.room).expect("issuer");
    fixture
        .store()
        .revoke(
            &fixture.identity_id,
            issuer.device_public_key,
            RevocationReason::Lost,
            tilo_clock::unix_now_ns(),
        )
        .expect("revoke issuer");
    assert!(actor.join_proof().is_err());
    assert!(provision(&fixture.room, &request()).is_err());
}

#[test]
fn task_room_delegation_keeps_actor_apps_and_expiry() {
    let fixture = Fixture::new();
    let actor = provision(&fixture.room, &request()).expect("enrollment");
    let task_room = tilo_desktop_model::RoomId::new().to_string();
    let delegation = actor
        .delegate_room(&task_room)
        .expect("same person's task room");
    delegation
        .verify(&fixture.user_public_key, tilo_clock::unix_now_ns())
        .expect("canonical signature");
    assert_eq!(delegation.scope.rooms, vec![task_room.clone()]);
    ed25519_dalek::VerifyingKey::from_bytes(&actor.certificate().device_public_key)
        .expect("actor key")
        .verify_strict(
            &runtime_identity::collaboration_join_signing_bytes(&task_room),
            &ed25519_dalek::Signature::from_bytes(
                &actor.join_proof_for(&delegation).expect("task join"),
            ),
        )
        .expect("bounded task-room proof");
    let mut widened = delegation.clone();
    widened.scope.apps.clear();
    assert!(actor.join_proof_for(&widened).is_err());
    assert_eq!(delegation.scope.apps, actor.delegation().scope.apps);
    assert_eq!(delegation.expires_at_ns, actor.delegation().expires_at_ns);
    assert_eq!(
        delegation.device_public_key,
        actor.certificate().device_public_key
    );
    assert_eq!(delegation.delegate, actor.delegation().delegate);
    assert!(actor.delegate_room("not-a-room").is_err());
}

#[test]
fn malformed_or_symlinked_private_key_is_not_replaced() {
    let fixture = Fixture::new();
    let request = request();
    let actor = provision(&fixture.room, &request).expect("enrollment");
    let key_path = actor.key_path.clone();
    fs::write(&key_path, b"truncated").expect("corrupt key");
    assert!(provision(&fixture.room, &request).is_err());
    assert_eq!(fs::read(&key_path).expect("unchanged"), b"truncated");
    fs::remove_file(&key_path).expect("remove key");
    let target = key_path.with_extension("target");
    tilo_files::native::atomic::write_private(&target, &[8; 32]).expect("target");
    std::os::unix::fs::symlink(&target, &key_path).expect("symlink");
    assert!(provision(&fixture.room, &request).is_err());
    assert_eq!(fs::read(target).expect("untouched target"), [8; 32]);
}

#[test]
fn retained_custody_refuses_shared_or_linked_key_files() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new();
    let request = request();
    let actor = provision(&fixture.room, &request).expect("enrollment");
    fs::set_permissions(&actor.key_path, fs::Permissions::from_mode(0o640)).unwrap();
    assert!(actor.join_proof().is_err());
    assert!(provision_retained(&fixture.room, &request).is_err());
    fs::set_permissions(&actor.key_path, fs::Permissions::from_mode(0o600)).unwrap();
    let alias = actor.key_path.with_extension("alias");
    fs::hard_link(&actor.key_path, &alias).unwrap();
    assert!(actor.join_proof().is_err());
    assert!(provision_retained(&fixture.room, &request).is_err());
    fs::remove_file(alias).unwrap();
    actor.join_proof().expect("private custody restored");
}

#[test]
fn certified_device_without_person_root_cannot_substitute_its_device_key() {
    let fixture = Fixture::new();
    let registry = IdentityRegistryStore::from_env().unwrap();
    let profile = selected(&fixture.room).unwrap();
    let public =
        runtime_identity::public_member_record("Person", [10, 20, 30], profile.user_public_key)
            .unwrap();
    tilo_files::native::atomic::write_private(&profile.record_path, &public).unwrap();
    // The actual enrolled device still verifies. Its signing key is not the
    // person's root, and cannot certify or delegate another actor in its place.
    crate::broker::trusted_current_device(&fixture.room).expect("certified device");
    let error = provision(&fixture.room, &request())
        .err()
        .expect("root unavailable");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("actual person root custody"));
    let directory = registry
        .identity_dir()
        .join("native-agents")
        .join(hex::encode(profile.user_public_key));
    assert!(
        !directory
            .join(format!("{}.key", hex::encode(request().agent_id)))
            .exists()
    );
}
