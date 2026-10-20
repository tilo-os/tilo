//! Contract tests for the public descriptor, the certified device proof and
//! the exact-package receipt. End-to-end transport tests live with the
//! prepared-media producer, where a real package and identity exist.

use super::*;
use ed25519_dalek::{Signer as _, SigningKey};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

fn signer(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn cert(root: &SigningKey, device: &SigningKey, now: u64, label: &str) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now.saturating_sub(1),
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

fn identity() -> HandoffIdentity {
    HandoffIdentity {
        operation_id: uuid::Uuid::new_v4().to_string(),
        ticket: "endpoint-ticket-value".into(),
        identity_public_key: signer(1).verifying_key().to_bytes(),
        expires_at_ns: 2_000,
    }
}

fn request(
    identity: &HandoffIdentity,
    package_sha256: &str,
    root: &SigningKey,
    device: &SigningKey,
    ephemeral: &SigningKey,
    now: u64,
) -> transport::Frame {
    let device_cert = Box::new(cert(root, device, now, "Destination"));
    let message = transport::proof_bytes(
        &identity.operation_id,
        "reply-ticket-value",
        identity.identity_public_key,
        package_sha256,
        identity.expires_at_ns,
        device_cert.device_public_key,
        ephemeral.verifying_key().to_bytes(),
    )
    .unwrap();
    let signature = device.sign(&message).to_bytes().to_vec();
    transport::Frame::Request {
        schema: SCHEMA,
        operation_id: identity.operation_id.clone(),
        reply_ticket: "reply-ticket-value".into(),
        identity_public_key: identity.identity_public_key,
        package_sha256: package_sha256.into(),
        expires_at_ns: identity.expires_at_ns,
        device_cert,
        ephemeral_public_key: ephemeral.verifying_key().to_bytes(),
        signature,
    }
}

fn package_digest() -> [u8; 32] {
    digest(b"package ciphertext")
}

fn package_digest_other() -> [u8; 32] {
    digest(b"other package ciphertext")
}

#[test]
fn public_handoff_carries_no_private_path_and_round_trips() {
    let identity = identity();
    let handoff = PublicHandoff::from_identity(&identity, "a".repeat(64), 4096);
    assert!(handoff.valid());
    let encoded = handoff.encode().unwrap();
    let text = std::str::from_utf8(&encoded).unwrap();
    assert!(!text.contains('/'));
    assert!(!text.contains("home"));
    assert!(!text.contains("token"));
    assert!(!text.contains("password"));
    assert_eq!(PublicHandoff::decode(&encoded).unwrap(), handoff);
}

#[test]
fn a_descriptor_with_a_bad_ticket_digest_or_expiry_is_refused() {
    let identity = identity();
    let good = PublicHandoff::from_identity(&identity, "b".repeat(64), 1024);
    for mutate in [
        |value: &mut PublicHandoff| value.ticket = "ticket\nwith control".into(),
        |value: &mut PublicHandoff| value.package_sha256 = "not-a-digest".into(),
        |value: &mut PublicHandoff| value.package_bytes = 0,
        |value: &mut PublicHandoff| value.operation_id = "operation".into(),
        |value: &mut PublicHandoff| value.identity_public_key = [0; 32],
    ] {
        let mut value = good.clone();
        mutate(&mut value);
        assert!(!value.valid());
        assert!(value.encode().is_err());
    }
    assert!(good.valid_at(1_999));
    assert!(!good.valid_at(2_000));
}

#[test]
fn a_certified_device_proof_binds_operation_package_person_and_ephemeral() {
    let identity = identity();
    let package = hex::encode(package_digest());
    let root = signer(1);
    let device = signer(2);
    let ephemeral = signer(3);
    let now = 1_000;
    let frame = request(&identity, &package, &root, &device, &ephemeral, now);
    let verified = transport::verify_device(&frame, now, &identity, &package);
    assert!(verified.is_some());
    let (label, device_key, ephemeral_key, cert) = verified.unwrap();
    assert_eq!(label, "Destination");
    assert_eq!(device_key, device.verifying_key().to_bytes());
    assert_eq!(ephemeral_key, ephemeral.verifying_key().to_bytes());
    assert_eq!(cert.user_public_key, identity.identity_public_key);

    // A captured request cannot be replayed into another operation or
    // package, nor for another person.
    let mut other = identity.clone();
    other.operation_id = uuid::Uuid::new_v4().to_string();
    assert!(transport::verify_device(&frame, now, &other, &package).is_none());
    assert!(transport::verify_device(&frame, now, &identity, &hex::encode([9; 32])).is_none());
    let mut other_person = identity.clone();
    other_person.identity_public_key = signer(4).verifying_key().to_bytes();
    assert!(transport::verify_device(&frame, now, &other_person, &package).is_none());

    // A device certificate for another root is not a certified proof.
    let foreign_root = signer(5);
    let foreign = request(&identity, &package, &foreign_root, &device, &ephemeral, now);
    assert!(transport::verify_device(&foreign, now, &identity, &package).is_none());

    // A changed signature, expired certificate or non-zero ephemeral key
    // alone is refused.
    let mut tampered = request(&identity, &package, &root, &device, &ephemeral, now);
    if let transport::Frame::Request { signature, .. } = &mut tampered {
        signature[0] ^= 1;
    }
    assert!(transport::verify_device(&tampered, now, &identity, &package).is_none());
    let mut expired = request(&identity, &package, &root, &device, &ephemeral, now);
    if let transport::Frame::Request { device_cert, .. } = &mut expired {
        device_cert.issued_at_ns = now.saturating_sub(10);
        device_cert.expires_at_ns = Some(now - 1);
        sign_device_cert(device_cert.as_mut(), &root).unwrap();
    }
    assert!(transport::verify_device(&expired, now, &identity, &package).is_none());
}

#[test]
fn only_the_approved_device_can_sign_the_package_receipt() {
    let device = signer(7);
    let operation = uuid::Uuid::new_v4().to_string();
    let package = hex::encode(package_digest());
    let message = transport::receipt_bytes(
        &operation,
        &package,
        transport::ReceiptStage::PreparedPackageImported,
    )
    .unwrap();
    let signature = device.sign(&message).to_bytes();
    assert!(transport::verify_receipt_signature(
        &operation,
        &package,
        transport::ReceiptStage::PreparedPackageImported,
        &device.verifying_key().to_bytes(),
        &signature,
    ));
    let mut tampered = signature;
    tampered[0] ^= 1;
    assert!(!transport::verify_receipt_signature(
        &operation,
        &package,
        transport::ReceiptStage::PreparedPackageImported,
        &device.verifying_key().to_bytes(),
        &tampered,
    ));
    assert!(!transport::verify_receipt_signature(
        &operation,
        &package,
        transport::ReceiptStage::PreparedPackageImported,
        &signer(8).verifying_key().to_bytes(),
        &signature,
    ));
    assert!(!transport::verify_receipt_signature(
        &operation,
        &hex::encode([9; 32]),
        transport::ReceiptStage::PreparedPackageImported,
        &device.verifying_key().to_bytes(),
        &signature,
    ));
}

#[test]
fn the_human_code_and_hpke_aad_bind_package_and_person() {
    let operation = uuid::Uuid::new_v4().to_string();
    let device = [1; 32];
    let ephemeral = [2; 32];
    let package = package_digest();
    let person = [3; 32];
    let code = verification_code(&operation, &device, &ephemeral, &package, &person);
    assert_eq!(code.len(), 6);
    assert!(code.bytes().all(|byte| byte.is_ascii_digit()));
    assert_eq!(
        code,
        verification_code(&operation, &device, &ephemeral, &package, &person)
    );
    assert_ne!(
        code,
        verification_code(
            &operation,
            &device,
            &ephemeral,
            &package_digest_other(),
            &person
        )
    );
    assert_ne!(
        code,
        verification_code(&operation, &device, &ephemeral, &package, &[4; 32])
    );
    assert_ne!(
        code,
        verification_code("another", &device, &ephemeral, &package, &person)
    );

    let aad = handoff_aad(&operation, &package, &person);
    assert_ne!(
        aad,
        handoff_aad(&operation, &package_digest_other(), &person)
    );
    assert_ne!(aad, handoff_aad(&operation, &package, &[4; 32]));
}

#[test]
fn only_the_exact_package_receipt_names_the_imported_stage() {
    let operation = uuid::Uuid::new_v4().to_string();
    let package = package_digest();
    assert!(transport::receipt_matches(
        &operation,
        &package,
        &operation,
        &hex::encode(package),
        transport::ReceiptStage::PreparedPackageImported,
    ));
    assert!(!transport::receipt_matches(
        &operation,
        &package,
        &uuid::Uuid::new_v4().to_string(),
        &hex::encode(package),
        transport::ReceiptStage::PreparedPackageImported,
    ));
    assert!(!transport::receipt_matches(
        &operation,
        &package,
        &operation,
        &hex::encode([9; 32]),
        transport::ReceiptStage::PreparedPackageImported,
    ));
}

#[test]
fn the_private_descriptor_reader_refuses_links_and_public_files() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let fixture = tempfile::tempdir().unwrap();
    let state = fixture.path().join(".local/state/tilo");
    std::fs::create_dir_all(&state).unwrap();
    for path in [
        fixture.path().join(".local"),
        fixture.path().join(".local/state"),
        state.clone(),
    ] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let handoff = PublicHandoff::from_identity(&identity(), "c".repeat(64), 1024);
    let bytes = handoff.encode().unwrap();
    let descriptor = state.join(DESCRIPTOR_FILE);
    std::fs::write(&descriptor, &bytes).unwrap();
    std::fs::set_permissions(&descriptor, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        read_descriptor(fixture.path()).unwrap().as_ref(),
        Some(&handoff)
    );

    std::fs::set_permissions(&descriptor, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_descriptor(fixture.path()).is_err());
    std::fs::remove_file(&descriptor).unwrap();
    let real = fixture.path().join("real.json");
    std::fs::write(&real, &bytes).unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&real, &descriptor).unwrap();
    assert!(read_descriptor(fixture.path()).is_err());
}

#[test]
fn a_fresh_destination_joins_its_person_before_the_package_is_released() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::Arc;

    let fixture = tempfile::tempdir().unwrap();
    std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = fixture.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    struct Env(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Env {
        fn set(&mut self, key: &'static str, value: impl AsRef<std::ffi::OsStr>) {
            self.0.push((key, std::env::var_os(key)));
            // SAFETY: this test owns the process environment for the keys it
            // sets and restores them when it ends.
            unsafe { std::env::set_var(key, value) };
        }
    }
    impl Drop for Env {
        fn drop(&mut self) {
            for (key, previous) in self.0.drain(..) {
                // SAFETY: as above.
                unsafe {
                    match previous {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }
    let mut env = Env(Vec::new());
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    env.set("HOME", &home);
    env.set("XDG_CONFIG_HOME", home.join(".config"));
    env.set("XDG_STATE_HOME", home.join(".local/state"));

    let source_dir = fixture.path().join("source-identity");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::set_permissions(&source_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    tilo_identity_host::provisioning::create_primary_identity(
        &source_dir,
        "Source person",
        [10, 20, 30],
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let source_signer = runtime_identity::load_from_path(source_dir.join("primary.json")).unwrap();
    let person = source_signer.group_public_key();
    let metadata =
        runtime_identity::load_metadata_from_path(&source_dir.join("primary.json")).unwrap();
    let record = public_identity_record(
        &metadata.display_name,
        metadata.avatar_color,
        person,
        metadata.profile_photo,
    )
    .unwrap();
    let signer_path = source_dir.join("primary.json");
    let authority = SourceIdentity {
        identity_public_key: person,
        identity_record: record,
        certificates: Vec::new(),
        home_peers: Vec::new(),
        signer: Arc::new(move |digest: &[u8]| {
            let identity = runtime_identity::load_from_path(signer_path.clone())
                .map_err(|error| io::Error::other(error.to_string()))?;
            identity
                .sign(digest)
                .map_err(|error| io::Error::other(error.to_string()))
        }),
    };
    let source = Source::bind_with_identity(uuid::Uuid::new_v4().to_string(), authority).unwrap();
    let package = b"prepared ciphertext for the joining destination, with enough bytes".to_vec();
    let package_sha256 = digest(&package);
    source
        .serve(
            package.clone(),
            zeroize::Zeroizing::new("unused test password".into()),
            package_sha256,
        )
        .unwrap();
    let handoff = PublicHandoff::from_identity(
        source.identity(),
        hex::encode(package_sha256),
        package.len() as u64,
    );

    let target_system = fixture.path().join("target-system");
    let target_identity = fixture.path().join("target-identity");
    env.set("TILO_IDENTITY_ROOT", &target_system);
    let receiver = Receiver::join(
        handoff,
        ReceiverIdentity {
            identity_root: target_system.clone(),
            identity_dir: target_identity.clone(),
        },
        home.join(".local/state/tilo/prepared-setup"),
        "Fresh destination".into(),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut approval = None;
    while approval.is_none() {
        assert!(std::time::Instant::now() < deadline, "join was not offered");
        while let Some(state) = source.poll() {
            if let SourceState::JoinRequested {
                approval: token, ..
            } = state
            {
                approval = Some(token);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    source.approve(&uuid::Uuid::new_v4().to_string()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert!(runtime_identity::resolve_active_identity(&target_identity).is_none());
    source.approve(&approval.unwrap()).unwrap();
    let mut received = None;
    let mut states = Vec::new();
    while received.is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "package did not arrive after the join; source states {states:?}; active {:?}",
            runtime_identity::resolve_active_identity(&target_identity)
        );
        while let Some(state) = source.poll() {
            states.push(format!("{state:?}"));
            if matches!(state, SourceState::Failed | SourceState::Rejected) {
                panic!("source ended after the join: {state:?}");
            }
        }
        if let Some(result) = receiver.poll() {
            received = Some(result.unwrap());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let received = received.unwrap();
    assert_eq!(received.package_sha256, package_sha256);
    assert_eq!(std::fs::read(&received.package_path).unwrap(), package);
    let joined = runtime_identity::resolve_active_identity(&target_identity).unwrap();
    assert_eq!(
        runtime_identity::load_metadata_from_path(&joined)
            .unwrap()
            .group_public_key,
        person
    );
    assert!(
        !tilo_user_identity::persist::IdentityPaths::under(&target_system)
            .user_privkey
            .exists(),
        "the destination joins as a member, never with the source root"
    );
    receiver.acknowledge(&received).unwrap();
    drop(receiver);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut complete = false;
    while !complete {
        assert!(
            std::time::Instant::now() < deadline,
            "source saw no receipt"
        );
        while let Some(state) = source.poll() {
            if let SourceState::PackageImported { .. } = state {
                complete = true;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn two_bound_endpoints_can_send_each_way() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut a = runtime
        .block_on(tilo_net::p2p::Endpoint::<Vec<u8>>::bind(
            b"tilo-handoff-test/1",
            1024,
            8,
            |bytes| Some(bytes.to_vec()),
        ))
        .unwrap();
    let mut b = runtime
        .block_on(tilo_net::p2p::Endpoint::<Vec<u8>>::bind(
            b"tilo-handoff-test/1",
            1024,
            8,
            |bytes| Some(bytes.to_vec()),
        ))
        .unwrap();
    let from_a = a.connect(b.ticket(), 8).unwrap();
    let from_b = b.connect(a.ticket(), 8).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let (mut got_a, mut got_b) = (false, false);
    while (!got_a || !got_b) && std::time::Instant::now() < deadline {
        let _ = from_a.try_send(vec![1]);
        let _ = from_b.try_send(vec![2]);
        got_a |= a.try_recv().is_some();
        got_b |= b.try_recv().is_some();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(got_a, "B did not reach A");
    assert!(got_b, "A did not reach B");
}
