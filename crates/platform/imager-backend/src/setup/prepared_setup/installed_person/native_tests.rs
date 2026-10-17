//! Actual root/import-system qualification; no manufactured anchor record.
use super::*;
use ed25519_dalek::Signer as _;
use std::os::unix::process::CommandExt as _;
use std::os::{fd::AsRawFd, unix::fs::PermissionsExt};
use tilo_identity_host::{
    device_fleet::DeviceFleet,
    enrolled_transport::{AuthenticatedEnrolledDevice, EnrolledDevice},
};
use tilo_user_identity::{DeviceCapabilities, IdentityPaths, RevocationReason, persist};
const PASSWORD: &[u8] = b"synthetic retained anchor native fixture password";
const TEST: &str = "setup::prepared_setup::installed_person::native_tests::authenticated_native_seed_revocations_and_certificate_custody";
const CHILD: &str = "TILO_INSTALLED_PERSON_ANCHOR_NATIVE_CHILD";
struct Fixture {
    _temporary: tempfile::TempDir,
    system: PathBuf,
    signer: ed25519_dalek::SigningKey,
    person: [u8; 32],
    device: [u8; 32],
    baseline: RevocationEntry,
    certificate: PathBuf,
    anchor: Arc<InstalledPersonAnchor>,
}
fn private(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
fn revocation(signer: &ed25519_dalek::SigningKey, device: [u8; 32], at: u64) -> RevocationEntry {
    let mut entry = RevocationEntry {
        user_public_key: signer.verifying_key().to_bytes(),
        revoked_device_public_key: device,
        revoked_at_ns: at,
        reason: RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, signer).unwrap();
    tilo_user_identity::verify_revocation_entry(&entry).unwrap();
    entry
}
fn fixture() -> Fixture {
    root().expect("actual real/effective root qualifier required");
    let temporary = tempfile::Builder::new()
        .prefix("tilo-anchor-native-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/run")
        .unwrap();
    let source = temporary.path().join("enrolled-source");
    let identity = temporary.path().join("identity");
    let output = temporary.path().join("sealed");
    for path in [&source, &identity, &identity.join("fleets"), &output] {
        private(path);
    }
    let signer = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    let device_signer = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
    let person = signer.verifying_key().to_bytes();
    let device = device_signer.verifying_key().to_bytes();
    let mut certificate = DeviceCert {
        user_public_key: person,
        device_public_key: device,
        device_label: "Synthetic anchor qualifier".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    certificate.signature = signer
        .sign(&tilo_user_identity::device_cert_signing_digest(&certificate).unwrap())
        .to_bytes();
    let paths = IdentityPaths::under(&source);
    paths.ensure_directories().unwrap();
    persist::write_device_key(&paths.device_privkey, &device_signer).unwrap();
    persist::write_public_key(&paths.user_pubkey, &person).unwrap();
    persist::write_device_cert(&paths.device_cert, &certificate).unwrap();
    let record = identity.join("member.json");
    tilo_files::native::atomic::write_with_mode(
        &record,
        &runtime_identity::public_member_record("Synthetic qualifier", [88, 101, 242], person)
            .unwrap(),
        0o600,
    )
    .unwrap();
    let baseline = revocation(&signer, [55; 32], 100);
    let mut fleet = DeviceFleet::new(hex(&person));
    fleet.certificates.push(certificate.clone());
    fleet.revocations.push(baseline.clone());
    tilo_files::native::atomic::write_with_mode(
        &identity
            .join("fleets")
            .join(format!("{}.json", hex(&person))),
        &serde_json::to_vec(&fleet).unwrap(),
        0o600,
    )
    .unwrap();
    let enrolled = EnrolledDevice::review(
        &source,
        &record,
        person,
        device,
        super::super::now_ns().unwrap(),
    )
    .unwrap();
    let sealed = output.join("identity.sealed");
    enrolled
        .export_password_sealed(&sealed, PASSWORD, "native-anchor-qualification")
        .unwrap();
    assert!(
        AuthenticatedEnrolledDevice::open(
            &sealed,
            b"wrong synthetic password",
            "native-anchor-qualification",
            person,
            device,
            super::super::now_ns().unwrap()
        )
        .is_err()
    );
    let authenticated = AuthenticatedEnrolledDevice::open(
        &sealed,
        PASSWORD,
        "native-anchor-qualification",
        person,
        device,
        super::super::now_ns().unwrap(),
    )
    .unwrap();
    let system = temporary.path().join("installed");
    let installed_root = system.join("var/lib/tilo");
    private(&installed_root);
    authenticated.import_system(&installed_root).unwrap();
    let installed_paths = IdentityPaths::under(&installed_root);
    let certificate_path = installed_paths.device_cert_for_user(&person);
    assert_eq!(
        std::fs::metadata(certificate_path.parent().unwrap())
            .unwrap()
            .mode()
            & 0o7777,
        0o755,
        "test must retain actual import_system public directory permissions"
    );
    assert_eq!(
        std::fs::metadata(&certificate_path).unwrap().mode() & 0o7777,
        0o644
    );
    assert!(
        !installed_paths.user_privkey.exists(),
        "no human root key may be imported"
    );
    publish_namespace(&authenticated, &installed_root).unwrap();
    assert!(
        installed_root
            .join("retained-persons")
            .join(format!("{}.json", hex(&person)))
            .is_file()
    );
    assert!(!installed_root.join("var/lib/tilo").exists());
    // The anchor record is minted only above by password-authenticated transport.
    // Never chmod the installed public certificate to rescue a private-chain bug.
    let anchor = InstalledPersonAnchor::open_at(&system, person, device).unwrap();
    let (actual, revoked) = anchor.current().unwrap();
    assert_eq!(
        serde_json::to_vec(&actual).unwrap(),
        serde_json::to_vec(&certificate).unwrap()
    );
    assert!(revoked.is_revoked(&baseline.revoked_device_public_key));
    assert!(InstalledPersonAnchor::open_at(&system, [7; 32], device).is_err());
    assert!(InstalledPersonAnchor::open_at(&system, person, [7; 32]).is_err());
    Fixture {
        _temporary: temporary,
        system,
        signer,
        person,
        device,
        baseline,
        certificate: certificate_path,
        anchor,
    }
}
fn refresh_path(f: &Fixture) -> PathBuf {
    f.system
        .join(LOCATION)
        .join(format!("{}.revocations.json", hex(&f.person)))
}
fn monotonic_and_foreign_issuer() {
    let f = fixture();
    let learned = revocation(&f.signer, [56; 32], 200);
    f.anchor
        .refresh_verified(std::slice::from_ref(&learned))
        .unwrap();
    // A valid omission/retry cannot discard a revocation learned from a fresh proof.
    f.anchor.refresh_verified(&[]).unwrap();
    let reopened = InstalledPersonAnchor::open_at(&f.system, f.person, f.device).unwrap();
    for anchor in [&f.anchor, &reopened] {
        let (_, list) = anchor.current().unwrap();
        assert!(list.is_revoked(&f.baseline.revoked_device_public_key));
        assert!(list.is_revoked(&learned.revoked_device_public_key));
    }
    // Earliest root-authored cutoff survives a later valid duplicate.
    let earlier = revocation(&f.signer, [56; 32], 150);
    let later = revocation(&f.signer, [56; 32], 300);
    f.anchor.refresh_verified(&[earlier, later]).unwrap();
    let (_, list) = reopened.current().unwrap();
    assert!(list.is_revoked_at(&[56; 32], 150));
    assert!(!list.is_revoked_at(&[56; 32], 149));
    let before = std::fs::read(refresh_path(&f)).unwrap();
    let foreign = revocation(
        &ed25519_dalek::SigningKey::from_bytes(&[71; 32]),
        [57; 32],
        250,
    );
    assert!(
        f.anchor.refresh_verified(&[foreign]).is_err(),
        "valid signature from another root is not this person's authority"
    );
    let mut forged = revocation(&f.signer, [58; 32], 250);
    forged.signature[0] ^= 1;
    assert!(f.anchor.refresh_verified(&[forged]).is_err());
    assert_eq!(
        std::fs::read(refresh_path(&f)).unwrap(),
        before,
        "refused entries must not replace protected custody"
    );
    let self_revocation = revocation(&f.signer, f.device, 400);
    f.anchor
        .refresh_verified(std::slice::from_ref(&self_revocation))
        .unwrap();
    let retained: Record =
        serde_json::from_slice(&std::fs::read(refresh_path(&f)).unwrap()).unwrap();
    assert!(retained.revocations.iter().any(|e| e == &self_revocation));
    for entry in &retained.revocations {
        tilo_user_identity::verify_revocation_entry(entry).unwrap();
    }
    assert!(f.anchor.current().is_err());
    assert!(reopened.current().is_err());
    assert!(
        f.anchor.refresh_verified(&[]).is_err(),
        "omission cannot revive an already revoked installed signer"
    );
    assert!(InstalledPersonAnchor::open_at(&f.system, f.person, f.device).is_err());
}
fn certificate_inode_replacement() {
    let f = fixture();
    let original = std::fs::metadata(&f.certificate).unwrap();
    let bytes = std::fs::read(&f.certificate).unwrap();
    let replacement = f.certificate.with_extension("replacement");
    tilo_files::native::atomic::write_with_mode(&replacement, &bytes, 0o644).unwrap();
    std::fs::rename(&replacement, &f.certificate).unwrap();
    let actual = std::fs::metadata(&f.certificate).unwrap();
    assert_ne!(
        (original.dev(), original.ino()),
        (actual.dev(), actual.ino())
    );
    assert_eq!(std::fs::read(&f.certificate).unwrap(), bytes);
    assert!(
        f.anchor.current().is_err(),
        "equal certificate bytes on a replaced inode cannot substitute live custody"
    );
    assert!(f.anchor.refresh_verified(&[]).is_err());
}
fn certificate_same_inode_mutation() {
    let f = fixture();
    let original = std::fs::metadata(&f.certificate).unwrap();
    let mut bytes = std::fs::read(&f.certificate).unwrap();
    bytes[0] ^= 1;
    let file = File::options().write(true).open(&f.certificate).unwrap();
    file.write_all_at(&bytes, 0).unwrap();
    file.sync_all().unwrap();
    let actual = std::fs::metadata(&f.certificate).unwrap();
    assert_eq!(
        (original.dev(), original.ino()),
        (actual.dev(), actual.ino())
    );
    assert!(
        f.anchor.current().is_err(),
        "held inode alone cannot authorize changed installed certificate bytes"
    );
    assert!(f.anchor.refresh_verified(&[]).is_err());
}
#[test]
fn authenticated_native_seed_revocations_and_certificate_custody() {
    root().expect("root qualifier required; cannot skip unavailable authority");
    if std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new("1")) {
        monotonic_and_foreign_issuer();
        certificate_inode_replacement();
        certificate_same_inode_mutation();
        return;
    }
    // Reexecute the exact already-selected test binary, preserving actual root;
    // the marker chooses a test phase and grants no credentials or anchor.
    let executable = File::open("/proc/self/exe").unwrap();
    let fd = executable.as_raw_fd();
    let mut command = std::process::Command::new(format!("/proc/self/fd/{fd}"));
    command
        .args(["--exact", TEST, "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env(CHILD, "1");
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let status = command.status().unwrap();
    assert!(
        status.success(),
        "actual root enrolled-import anchor qualification failed: {status}"
    );
}
