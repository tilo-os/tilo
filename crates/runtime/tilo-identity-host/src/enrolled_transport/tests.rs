use super::*;
use ed25519_dalek::Signer as _;
use std::os::unix::fs::PermissionsExt as _;
use tilo_user_identity::{DeviceCapabilities, IdentityPaths, persist};
const PASSWORD: &[u8] = b"a private device transport password";
struct Fixture {
    source: tempfile::TempDir,
    system: PathBuf,
    record: PathBuf,
    output: PathBuf,
    root: [u8; 32],
    device: [u8; 32],
}
fn private(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let system = temp.path().join("future-system");
    private(&system);
    let identity = temp.path().join("future-identity");
    private(&identity);
    private(&identity.join("fleets"));
    let output = temp.path().join("sealed-output");
    private(&output);
    let root_signer = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    let device_signer = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
    let root = root_signer.verifying_key().to_bytes();
    let device = device_signer.verifying_key().to_bytes();
    let mut cert = DeviceCert {
        user_public_key: root,
        device_public_key: device,
        device_label: "Future laptop".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    cert.signature = root_signer
        .sign(&tilo_user_identity::device_cert_signing_digest(&cert).unwrap())
        .to_bytes();
    let paths = IdentityPaths::under(&system);
    paths.ensure_directories().unwrap();
    persist::write_device_key(&paths.device_privkey, &device_signer).unwrap();
    persist::write_public_key(&paths.user_pubkey, &root).unwrap();
    persist::write_device_cert(&paths.device_cert, &cert).unwrap();
    let record = identity.join("member.json");
    let bytes = runtime_identity::public_member_record("ada", [88, 101, 242], root).unwrap();
    tilo_files::native::atomic::write_with_mode(&record, &bytes, 0o600).unwrap();
    let mut fleet = DeviceFleet::new(hex::encode(root));
    fleet.certificates.push(cert);
    tilo_files::native::atomic::write_with_mode(
        &identity
            .join("fleets")
            .join(format!("{}.json", hex::encode(root))),
        &serde_json::to_vec(&fleet).unwrap(),
        0o600,
    )
    .unwrap();
    Fixture {
        source: temp,
        system,
        record,
        output,
        root,
        device,
    }
}
fn snapshot(path: &Path) -> Vec<(PathBuf, Vec<u8>, u32)> {
    fn walk(path: &Path, root: &Path, out: &mut Vec<(PathBuf, Vec<u8>, u32)>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                walk(&entry.path(), root, out);
            } else {
                out.push((
                    entry.path().strip_prefix(root).unwrap().to_owned(),
                    std::fs::read(entry.path()).unwrap(),
                    metadata.permissions().mode(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(path, path, &mut out);
    out.sort_by(|left, right| left.0.cmp(&right.0));
    out
}
#[test]
fn canonical_enrolled_device_roundtrip_preserves_root_and_exact_device_without_source_changes() {
    let fixture = fixture();
    let source_before = snapshot(fixture.source.path());
    let enrolled = EnrolledDevice::review(
        &fixture.system,
        &fixture.record,
        fixture.root,
        fixture.device,
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    assert_eq!(enrolled.device_label(), "Future laptop");
    let sealed = fixture.output.join("identity.sealed");
    enrolled
        .export_password_sealed(&sealed, PASSWORD, "prepared-operation")
        .unwrap();
    let authenticated = AuthenticatedEnrolledDevice::open(
        &sealed,
        PASSWORD,
        "prepared-operation",
        fixture.root,
        fixture.device,
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let target = tempfile::tempdir().unwrap();
    let system = target.path().join("system");
    let identity = target.path().join("identity");
    private(&system);
    private(&identity);
    let imported = authenticated.import_into(&system, &identity).unwrap();
    assert_eq!(imported.user_public_key, fixture.root);
    assert_eq!(imported.device_public_key, fixture.device);
    let paths = IdentityPaths::under(&system);
    assert!(persist::joined_device_is_ready(&paths, tilo_clock::unix_now_ns()).unwrap());
    assert!(!paths.user_privkey.exists());
    assert_eq!(
        persist::read_device_key(&paths.device_privkey)
            .unwrap()
            .verifying_key()
            .to_bytes(),
        fixture.device
    );
    assert_eq!(
        runtime_identity::load_metadata_from_path(&imported.identity_record)
            .unwrap()
            .group_public_key,
        fixture.root
    );
    let before_retry = snapshot(target.path());
    assert_eq!(
        authenticated.import_into(&system, &identity).unwrap(),
        imported
    );
    assert_eq!(snapshot(target.path()), before_retry);
    let source_after = snapshot(fixture.source.path());
    assert_eq!(
        source_after
            .into_iter()
            .filter(|(path, _, _)| !path.starts_with("sealed-output"))
            .collect::<Vec<_>>(),
        source_before
    );
}
#[test]
fn enrolled_device_wrong_password_binding_authority_and_target_conflicts_refuse_before_import() {
    let fixture = fixture();
    let enrolled = EnrolledDevice::review(
        &fixture.system,
        &fixture.record,
        fixture.root,
        fixture.device,
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let sealed = fixture.output.join("identity.sealed");
    enrolled
        .export_password_sealed(&sealed, PASSWORD, "operation")
        .unwrap();
    for (password, binding, root, device) in [
        (
            b"wrong private device transport password".as_slice(),
            "operation",
            fixture.root,
            fixture.device,
        ),
        (PASSWORD, "other-operation", fixture.root, fixture.device),
        (PASSWORD, "operation", [3; 32], fixture.device),
        (PASSWORD, "operation", fixture.root, [4; 32]),
    ] {
        assert!(
            AuthenticatedEnrolledDevice::open(
                &sealed,
                password,
                binding,
                root,
                device,
                tilo_clock::unix_now_ns()
            )
            .is_err()
        );
    }
    let authenticated = AuthenticatedEnrolledDevice::open(
        &sealed,
        PASSWORD,
        "operation",
        fixture.root,
        fixture.device,
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let target = tempfile::tempdir().unwrap();
    let system = target.path().join("system");
    let identity = target.path().join("identity");
    private(&system);
    private(&identity);
    private(&identity.join("fleets"));
    tilo_files::native::atomic::write_with_mode(
        &identity.join("primary.json"),
        b"unrelated persona",
        0o600,
    )
    .unwrap();
    let before = snapshot(target.path());
    assert!(authenticated.import_into(&system, &identity).is_err());
    assert_eq!(snapshot(target.path()), before);
    let source = IdentityPaths::under(&fixture.system);
    let human_root = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    persist::write_signing_key(&source.user_privkey, &human_root).unwrap();
    assert!(
        EnrolledDevice::review(
            &fixture.system,
            &fixture.record,
            fixture.root,
            fixture.device,
            tilo_clock::unix_now_ns()
        )
        .is_err()
    );
}

#[test]
#[cfg(feature = "native-current-device-qualification")]
fn current_owner_reference_roundtrip_uses_existing_member_without_exporting_private_keys() {
    let fixture = fixture();
    let identity = fixture.record.parent().unwrap();
    runtime_identity::set_active_identity(identity, "member").unwrap();
    let before = snapshot(fixture.source.path());
    let reviewed = CurrentDeviceReference::review(&fixture.system, identity).unwrap();
    let sealed = fixture.output.join("current-device.sealed");
    reviewed
        .export_password_sealed(&sealed, PASSWORD, "current-owner-operation")
        .unwrap();
    let authenticated = AuthenticatedCurrentDeviceReference::open(
        &sealed,
        PASSWORD,
        "current-owner-operation",
        fixture.root,
        fixture.device,
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let resolved = authenticated
        .resolve_current(&fixture.system, identity)
        .unwrap();
    assert_eq!(resolved.load().unwrap().group_public_key(), fixture.root);
    assert_eq!(resolved.metadata().unwrap().display_name, "ada");
    let after = snapshot(fixture.source.path());
    // Only the separately owned output artifact was published.
    assert_eq!(
        before
            .into_iter()
            .filter(|(p, _, _)| !p.starts_with("sealed-output"))
            .collect::<Vec<_>>(),
        after
            .into_iter()
            .filter(|(p, _, _)| !p.starts_with("sealed-output"))
            .collect::<Vec<_>>()
    );
    let mut context = b"tilo-prepared-current-device/v1\0".to_vec();
    context.extend_from_slice(b"current-owner-operation");
    let plain = envelope::open_with_password(
        PASSWORD,
        Context::new(Label::MigrationPackage, &context),
        &std::fs::read(&sealed).unwrap(),
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&plain).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 5);
    assert!(value.get("device_seed").is_none());
    assert!(value.get("public_record").is_none());
    assert!(
        AuthenticatedEnrolledDevice::open(
            &sealed,
            PASSWORD,
            "current-owner-operation",
            fixture.root,
            fixture.device,
            tilo_clock::unix_now_ns()
        )
        .is_err()
    );
    assert!(
        AuthenticatedCurrentDeviceReference::open(
            &sealed,
            PASSWORD,
            "other-operation",
            fixture.root,
            fixture.device,
            tilo_clock::unix_now_ns()
        )
        .is_err()
    );
    assert!(
        AuthenticatedCurrentDeviceReference::open(
            &sealed,
            PASSWORD,
            "current-owner-operation",
            fixture.root,
            [0; 32],
            tilo_clock::unix_now_ns()
        )
        .is_err()
    );
}

#[test]
#[cfg(feature = "native-current-device-qualification")]
fn current_owner_reference_refuses_other_owner_and_changed_current_key_before_activation() {
    let fixture = fixture();
    let identity = fixture.record.parent().unwrap();
    runtime_identity::set_active_identity(identity, "member").unwrap();
    let reviewed = CurrentDeviceReference::review(&fixture.system, identity).unwrap();
    let sealed = fixture.output.join("current-device.sealed");
    reviewed
        .export_password_sealed(&sealed, PASSWORD, "current-owner-operation")
        .unwrap();
    let authenticated = AuthenticatedCurrentDeviceReference::open(
        &sealed,
        PASSWORD,
        "current-owner-operation",
        fixture.root,
        fixture.device,
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let paths = IdentityPaths::under(&fixture.system);
    persist::write_public_key(&paths.user_pubkey, &[17; 32]).unwrap();
    let refused_owner = snapshot(fixture.source.path());
    assert!(CurrentDeviceReference::review(&fixture.system, identity).is_err());
    assert!(
        authenticated
            .resolve_current(&fixture.system, identity)
            .is_err()
    );
    assert_eq!(snapshot(fixture.source.path()), refused_owner);
    persist::write_public_key(&paths.user_pubkey, &fixture.root).unwrap();
    persist::write_device_key(
        &paths.device_privkey,
        &ed25519_dalek::SigningKey::from_bytes(&[91; 32]),
    )
    .unwrap();
    let refused_key = snapshot(fixture.source.path());
    assert!(reviewed.validate().is_err());
    assert!(
        authenticated
            .resolve_current(&fixture.system, identity)
            .is_err()
    );
    assert_eq!(snapshot(fixture.source.path()), refused_key);
}

#[test]
#[cfg(feature = "native-current-device-qualification")]
fn current_owner_reference_refuses_changed_original_record_and_never_replaces_it() {
    let fixture = fixture();
    let identity = fixture.record.parent().unwrap();
    runtime_identity::set_active_identity(identity, "member").unwrap();
    let reviewed = CurrentDeviceReference::review(&fixture.system, identity).unwrap();
    let sealed = fixture.output.join("current-device.sealed");
    reviewed
        .export_password_sealed(&sealed, PASSWORD, "current-owner-operation")
        .unwrap();
    let authenticated = AuthenticatedCurrentDeviceReference::open(
        &sealed,
        PASSWORD,
        "current-owner-operation",
        fixture.root,
        fixture.device,
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    tilo_files::native::atomic::write_with_mode(
        &fixture.record,
        &runtime_identity::public_member_record(
            "Changed owner presentation",
            [88, 101, 242],
            fixture.root,
        )
        .unwrap(),
        0o600,
    )
    .unwrap();
    let changed = snapshot(fixture.source.path());
    assert!(reviewed.validate().is_err());
    assert!(
        authenticated
            .resolve_current(&fixture.system, identity)
            .is_err()
    );
    assert_eq!(snapshot(fixture.source.path()), changed);
}

#[test]
#[cfg(feature = "native-current-device-qualification")]
fn current_owner_reference_actual_user_reads_root_owned_host_and_retains_activation_custody() {
    use std::os::unix::process::CommandExt as _;
    const ROLE: &str = "TILO_CURRENT_REFERENCE_QUALIFICATION";
    if let Some(base) = std::env::var_os(ROLE) {
        // SAFETY: only observes the qualification child's native credentials.
        assert_eq!(unsafe { libc::geteuid() }, 1000);
        // SAFETY: these calls only observe this child's real/effective IDs.
        assert_eq!(unsafe { libc::getuid() }, 1000);
        assert_eq!(unsafe { libc::getgid() }, 1000);
        assert_eq!(unsafe { libc::getegid() }, 1000);
        use std::io::Read as _;
        let mut status = Vec::new();
        std::fs::File::open("/proc/self/status")
            .unwrap()
            .take(65_537)
            .read_to_end(&mut status)
            .unwrap();
        assert!(
            status.len() <= 65_536,
            "native credential observation overflow"
        );
        let status = std::str::from_utf8(&status).unwrap();
        for name in ["CapEff:", "CapPrm:", "CapInh:", "CapAmb:"] {
            let values: Vec<_> = status
                .lines()
                .filter_map(|line| line.strip_prefix(name))
                .collect();
            assert_eq!(
                values.len(),
                1,
                "missing or duplicate native capability field"
            );
            assert_eq!(
                u64::from_str_radix(values[0].trim(), 16).unwrap(),
                0,
                "original-user reads must not retain root capabilities"
            );
        }
        let base = PathBuf::from(base);
        let system = base.join("future-system");
        let identity = base.join("future-identity");
        let source = CurrentDeviceReference::review(&system, &identity).unwrap();
        let sealed = base.join("sealed-output/current-device.sealed");
        source
            .export_password_sealed(&sealed, PASSWORD, "current-owner-user-proof")
            .unwrap();
        let authenticated = AuthenticatedCurrentDeviceReference::open(
            &sealed,
            PASSWORD,
            "current-owner-user-proof",
            source.public_key(),
            source.device_public_key(),
            tilo_clock::unix_now_ns(),
        )
        .unwrap();
        let resolved = authenticated.resolve_current(&system, &identity).unwrap();
        assert_eq!(
            resolved.load().unwrap().group_public_key(),
            source.public_key()
        );
        assert_eq!(resolved.metadata().unwrap().display_name, "ada");
        // Returning a path would lose custody here. The retained admission
        // must reject replacement while the same source object is still alive.
        tilo_files::native::atomic::write_with_mode(
            &identity.join("member.json"),
            &runtime_identity::public_member_record(
                "Replaced",
                [88, 101, 242],
                source.public_key(),
            )
            .unwrap(),
            0o600,
        )
        .unwrap();
        assert!(resolved.load().is_err());
        assert!(resolved.metadata().is_err());
        assert!(resolved.validate().is_err());
        return;
    }
    // This opt-in native qualifier requires genuine root-owned custody and a
    // real original-user child; it never treats missing privilege as a pass.
    // SAFETY: observes current credentials only.
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "requires actual native root/original-user qualification"
    );
    let fixture = fixture();
    let identity = fixture.record.parent().unwrap();
    runtime_identity::set_active_identity(identity, "member").unwrap();
    fn ownership(path: &Path, uid: u32, gid: u32) {
        use std::os::unix::ffi::OsStrExt as _;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: terminated exact synthetic fixture path; no recursive syscall.
        assert_eq!(unsafe { libc::chown(path.as_ptr(), uid, gid) }, 0);
    }
    fn user_tree(path: &Path) {
        ownership(path, 1000, 1000);
        if path.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                user_tree(&entry.unwrap().path());
            }
        }
    }
    std::fs::set_permissions(
        fixture.source.path(),
        // The held-directory walker reads each public ancestor. Private
        // identity and key endpoints keep their separate access restrictions.
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    for path in [
        &fixture.system,
        &fixture.system.join("user-identity"),
        &fixture.system.join("device"),
    ] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let private_key = fixture.system.join("device-private");
    ownership(&private_key, 0, 1000);
    ownership(&private_key.join("keypair.bin"), 0, 1000);
    std::fs::set_permissions(&private_key, std::fs::Permissions::from_mode(0o2750)).unwrap();
    user_tree(identity);
    user_tree(&fixture.output);
    let mut command = std::process::Command::new("/proc/self/exe");
    command.args(["--exact", "enrolled_transport::tests::current_owner_reference_actual_user_reads_root_owned_host_and_retains_activation_custody", "--nocapture"])
        .env(ROLE, fixture.source.path())
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::inherit());
    // SAFETY: pre-exec clears inherited root groups using one async-safe syscall.
    unsafe {
        command.pre_exec(|| {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(1000) != 0
                || libc::setuid(1000) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let deadline = tilo_test_env::hang_deadline();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("current-owner original-user qualifier exceeded deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "actual original-user reference producer/consumer failed"
    );
}

#[test]
fn current_owner_reference_refuses_corrupt_selector_and_non_root_namespace() {
    let fixture = fixture();
    let identity = fixture.record.parent().unwrap();
    tilo_files::native::atomic::write_with_mode(&identity.join("active"), b"../../member", 0o600)
        .unwrap();
    let before = snapshot(fixture.source.path());
    assert!(CurrentDeviceReference::review(&fixture.system, identity).is_err());
    assert_eq!(snapshot(fixture.source.path()), before);
    // Strict canonical parsing independently proves a present bad selector
    // cannot become the legacy primary selection on an unprivileged reader.
    assert!(runtime_identity::selected_identity_id_from_bytes(Some(b"../../member")).is_err());
    assert!(runtime_identity::selected_identity_id_from_bytes(Some(b"\xff")).is_err());
    assert_eq!(
        runtime_identity::selected_identity_id_from_bytes(None).unwrap(),
        "primary"
    );
}
