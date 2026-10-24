//! A device holds one certificate for its key: the device's own file and the
//! identity's file are the same bytes, from provisioning on, through
//! renewal, and once a device that holds two different ones starts again.
use super::*;

const FILES: IdentityStorage = IdentityStorage::Files;

/// The device's own certificate file and its owner's, as bytes.
fn both_files(paths: &IdentityPaths) -> (Vec<u8>, Vec<u8>) {
    let own = fs::read(&paths.device_cert).expect("the device's certificate");
    let user = persist::read_device_cert(&paths.device_cert)
        .expect("the device's certificate reads")
        .user_public_key;
    let owners = fs::read(paths.device_cert_for_user(&user)).expect("the owner's certificate");
    (own, owners)
}

#[test]
fn a_freshly_provisioned_device_holds_one_certificate_in_both_places() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let identity = temp.path().join("identity");
    let paths = IdentityPaths::under(temp.path().join("system"));

    ensure_collaboration_identity(&identity, &paths, FILES).expect("provision");

    let (own, owners) = both_files(&paths);
    assert_eq!(own, owners);
    tilo_user_identity::verify_device_cert(&persist::read_device_cert(&paths.device_cert).unwrap())
        .expect("the certificate verifies");
}

#[test]
#[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
fn a_renewed_certificate_is_the_same_bytes_in_both_places() {
    let tmp = tempfile::tempdir().unwrap();
    let (personal, paths, expired) =
        super::renewal_tests::aged_throwaway(tmp.path(), 48 * 60 * 60 * 1_000_000_000);

    ensure_collaboration_identity(&personal, &paths, FILES).expect("start");

    let (own, owners) = both_files(&paths);
    assert_eq!(own, owners);
    let renewed = persist::read_device_cert(&paths.device_cert).unwrap();
    assert!(
        renewed.issued_at_ns > expired.issued_at_ns,
        "it was renewed"
    );
}

#[test]
fn a_device_holding_two_different_certificates_keeps_its_own_in_both_places() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let identity = temp.path().join("identity");
    let paths = IdentityPaths::under(temp.path().join("system"));
    ensure_collaboration_identity(&identity, &paths, FILES).expect("provision");
    let kept = fs::read(&paths.device_cert).unwrap();
    let first = persist::read_device_cert(&paths.device_cert).unwrap();

    // A second certificate for the same key, valid and issued separately,
    // sits in the owner's place, as devices provisioned before were left.
    let signer =
        runtime_identity::load_from_path(identity.join(PRIMARY_IDENTITY_FILE)).expect("identity");
    let other = issue_device_certificate_at(
        &signer,
        first.device_public_key,
        &first.device_label,
        DeviceClass::Native,
        first.issued_at_ns + 1,
    )
    .expect("a second certificate");
    tilo_user_identity::verify_device_cert(&other).expect("the second one is valid too");
    let owners_place = paths.device_cert_for_user(&first.user_public_key);
    persist::write_device_cert(&owners_place, &other).unwrap();
    assert_ne!(fs::read(&owners_place).unwrap(), kept);

    assert_eq!(
        ensure_collaboration_identity(&identity, &paths, FILES).expect("start"),
        ProvisionOutcome::Existing
    );

    let (own, owners) = both_files(&paths);
    assert_eq!(own, kept, "the device's own certificate is kept");
    assert_eq!(owners, kept, "and the owner's place holds the same one");
}
