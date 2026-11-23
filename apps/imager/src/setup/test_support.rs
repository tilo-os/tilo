//! Shared helpers for the setup tests.

pub(crate) use imager_backend::test_support::{isolated, qualified_tempdir};

/// A real certified device identity in a private tree, for tests that need one
/// without waiting on the other device that would approve it.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn enrolled_identity() -> (
    tempfile::TempDir,
    std::sync::Arc<imager_backend::prepared_identity::PreparedIdentity>,
) {
    use imager_backend::prepared_identity::PreparedIdentity;
    use std::{os::unix::fs::PermissionsExt as _, sync::Arc};
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(tilo_identity::store::FILE_SECRETS_ENV, "1");
    let temporary = qualified_tempdir();
    let state = temporary.path().canonicalize().unwrap();
    let directory = state.join("identity");
    let system = directory.join("future-device");
    let identities = directory.join("identities");
    for path in [&state, &directory, &system, &identities] {
        std::fs::create_dir_all(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::create_dir(identities.join("fleets")).unwrap();
    let approver = PreparedIdentity::create(
        &directory.join("approver"),
        "Approving person",
        [10, 20, 30],
        tilo_identity::provisioning::IdentityStorage::for_session(),
    )
    .unwrap();
    let signer =
        tilo_identity::store::load_from_path(directory.join("approver/primary.json")).unwrap();
    let paths = tilo_device_identity::persist::IdentityPaths::under(&system);
    let (device, _) = tilo_device_identity::persist::load_or_create_device_key(&paths).unwrap();
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: approver.public_key(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Approved future computer".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    certificate.signature = signer
        .sign(&tilo_user_identity::device_cert_signing_digest(&certificate).unwrap())
        .unwrap();
    tilo_device_identity::persist::write_public_key(
        &paths.user_pubkey,
        &certificate.user_public_key,
    )
    .unwrap();
    tilo_device_identity::persist::write_device_cert(&paths.device_cert, &certificate).unwrap();
    let record = identities.join("member.json");
    let bytes = tilo_identity::store::public_member_record(
        "Approving person",
        [10, 20, 30],
        certificate.user_public_key,
    )
    .unwrap();
    tilo_files::native::atomic::write_with_mode(&record, &bytes, 0o600).unwrap();
    let mut fleet =
        tilo_identity::device_fleet::DeviceFleet::new(hex::encode(certificate.user_public_key));
    fleet.certificates.push(certificate.clone());
    tilo_files::native::atomic::write_with_mode(
        &identities
            .join("fleets")
            .join(format!("{}.json", hex::encode(certificate.user_public_key))),
        &serde_json::to_vec(&fleet).unwrap(),
        0o600,
    )
    .unwrap();
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
    let enrolled = tilo_identity::enrolled_transport::EnrolledDevice::review(
        &system,
        &record,
        certificate.user_public_key,
        certificate.device_public_key,
        now,
    )
    .unwrap();
    let identity = Arc::new(PreparedIdentity::from_enrolled(Arc::new(enrolled)).unwrap());
    (temporary, identity)
}
