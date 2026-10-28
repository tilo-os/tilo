// Real source admission and activation; the bounded private helper reply is synthetic here.
use crate::onboarding_view::OnboardingView;
use ed25519_dalek::Signer as _;
use sha2::{Digest as _, Sha256};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

fn private(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn current_owner_prepared_activation_keeps_original_record_device_and_selected_data() {
    // SAFETY: observes credentials only; this explicit qualifier cannot skip root custody.
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "requires native root-owned host qualification"
    );
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    private(&root);
    let home = root.join("home");
    let identity = home.join(".config/tilo/identity");
    let system = root.join("host");
    for directory in [&home, &identity, &identity.join("fleets"), &system] {
        private(directory);
    }
    let owner = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
    let key = owner.verifying_key().to_bytes();
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: key,
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Existing laptop".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    cert.signature = owner
        .sign(&tilo_user_identity::device_cert_signing_digest(&cert).unwrap())
        .to_bytes();
    let paths = tilo_user_identity::persist::IdentityPaths::under(&system);
    paths.ensure_directories().unwrap();
    tilo_user_identity::persist::write_device_key(&paths.device_privkey, &device).unwrap();
    tilo_user_identity::persist::write_public_key(&paths.user_pubkey, &key).unwrap();
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert).unwrap();
    let record = identity.join("member.json");
    tilo_files::native::atomic::write_with_mode(
        &record,
        &runtime_identity::public_member_record("Ada", [1, 2, 3], key).unwrap(),
        0o600,
    )
    .unwrap();
    let mut fleet = tilo_identity_host::device_fleet::DeviceFleet::new(hex::encode(key));
    fleet.certificates.push(cert);
    tilo_files::native::atomic::write_with_mode(
        &identity
            .join("fleets")
            .join(format!("{}.json", hex::encode(key))),
        &serde_json::to_vec(&fleet).unwrap(),
        0o600,
    )
    .unwrap();
    runtime_identity::set_active_identity(&identity, "member").unwrap();
    let registry_store = crate::identity_registry::IdentityRegistryStore::at(&identity);
    let mut registry = registry_store.load().unwrap();
    registry.register_record(&record).unwrap();
    registry_store.save(&registry).unwrap();
    let before = [
        std::fs::read(&record).unwrap(),
        std::fs::read(&paths.device_privkey).unwrap(),
        std::fs::read(&paths.device_cert).unwrap(),
        std::fs::read(identity.join("active")).unwrap(),
    ];
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("HOME", &home);
    environment.set("XDG_CONFIG_HOME", home.join(".config"));
    environment.set("TILO_IDENTITY_ROOT", &system);
    let selected =
        tilo_identity_host::enrolled_transport::CurrentDeviceReference::review(&system, &identity)
            .unwrap()
            .declaration()
            .unwrap();
    let pending_root = home.join(".local/state/tilo/prepared-setup");
    private(&pending_root);
    // Pending custody validates the encrypted-delivery receipt; this fixture does
    // not claim to exercise the trusted helper's separate password authentication.
    let ciphertext = b"synthetic private prepared ciphertext";
    tilo_files::native::atomic::write_with_mode(
        &pending_root.join("prepared-setup.sealed"),
        ciphertext,
        0o600,
    )
    .unwrap();
    let receipt = serde_json::json!({"schema":1,"sealed_bytes":ciphertext.len(),"ciphertext_sha256":hex::encode(Sha256::digest(ciphertext))});
    tilo_files::native::atomic::write_with_mode(
        &pending_root.join("receipt.json"),
        &serde_json::to_vec(&receipt).unwrap(),
        0o600,
    )
    .unwrap();
    let reply = serde_json::json!({"identity_public_key":key,"account_count":2,"current_identity":selected,"device_label":"Existing laptop","portable_home":{"schema":1,"setup_binding":"selected-original-files","selection":"full_home","applications":true},"service_sessions":{"schema":1,"sessions":[{"id":"approved-slack","service":"slack"}],"sealed_bytes":128,"ciphertext_sha256":vec![1;32]}});
    let bytes = serde_json::to_vec(&reply).unwrap();
    let imported = crate::onboarding_prepared::Imported::admit(&bytes, &identity).unwrap();
    let mut view = OnboardingView::new(identity.clone());
    view.prepared_review = Some(crate::onboarding_prepared::Pending::open(&home).unwrap());
    let (minted, root_key, home_declaration, applications, browser, sessions, held) =
        view.activate_prepared(imported).unwrap();
    assert_eq!(minted.path, record);
    assert_eq!(root_key, key);
    assert!(home_declaration.as_ref().unwrap().applications);
    assert_eq!(sessions.as_ref().unwrap().sessions[0].id, "approved-slack");
    assert!(applications.is_none());
    assert!(browser.is_none());
    let held = held.unwrap();
    view.prepared_review
        .as_ref()
        .unwrap()
        .acknowledge(
            key,
            home_declaration,
            applications,
            browser,
            sessions,
            Some(held.declaration().unwrap()),
        )
        .unwrap();
    held.validate().unwrap();
    assert!(
        crate::onboarding_prepared::Pending::open(&home)
            .unwrap()
            .completed(&identity)
            .unwrap(),
        "restart reattests actual current owner and exact bytes"
    );
    let after = [
        std::fs::read(&record).unwrap(),
        std::fs::read(&paths.device_privkey).unwrap(),
        std::fs::read(&paths.device_cert).unwrap(),
        std::fs::read(identity.join("active")).unwrap(),
    ];
    assert_eq!(before, after);
    tilo_files::native::atomic::write_with_mode(
        &record,
        &runtime_identity::public_member_record("Changed", [1, 2, 3], key).unwrap(),
        0o600,
    )
    .unwrap();
    assert!(crate::onboarding_prepared::Imported::admit(&bytes, &identity).is_err());
    assert!(
        crate::onboarding_prepared::Pending::open(&home)
            .unwrap()
            .completed(&identity)
            .is_err()
    );
}
