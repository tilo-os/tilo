//! Process-level acceptance coverage for ordinary nearby-device enrollment.
//!
//! The approver and recipient are separate `qos-enroll` processes.  This is
//! intentionally above the crypto unit tests: it proves the on-disk projection
//! that the shell reads is only published after the signed LAN exchange and
//! remains enough to reach a home peer once the approving device is gone.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable,
    reason = "test assertions and deterministic fixture keys"
)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use qos_relay::daemon::RelayIdentity;
use qos_relay::{Envelope, HomePeerRegistration, Relay, RelayClient, RelayConfig};
use qos_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, RevocationList, sign_device_cert,
};

use crate::common::client_endpoint;

const FILE_WORKSPACE: u64 = 42;

fn wait_for_file(path: &std::path::Path, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.is_file() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what} at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn signed_cert(
    root: &SigningKey,
    device: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

fn write_approver_fixture(
    root_dir: &std::path::Path,
    identities: &std::path::Path,
    root: &SigningKey,
    approver: &SigningKey,
    home: &SigningKey,
) -> HomePeerRegistration {
    let paths = IdentityPaths::under(root_dir);
    paths.ensure_directories().unwrap();
    qos_user_identity::persist::write_signing_key(&paths.user_privkey, root).unwrap();
    qos_user_identity::persist::write_public_key(
        &paths.user_pubkey,
        &root.verifying_key().to_bytes(),
    )
    .unwrap();
    qos_user_identity::persist::write_device_key(&paths.device_privkey, approver).unwrap();
    let approver_cert = signed_cert(
        root,
        approver,
        "Approving laptop",
        DeviceCapabilities::primary(),
    );
    qos_user_identity::persist::write_device_cert(&paths.device_cert, &approver_cert).unwrap();
    qos_user_identity::persist::write_device_cert(
        &paths.device_cert_for_user(&root.verifying_key().to_bytes()),
        &approver_cert,
    )
    .unwrap();

    let user = root.verifying_key().to_bytes();
    let fleet = serde_json::json!({
        "schema_version": 1,
        "identity_id": hex::encode(user),
        "certificates": [approver_cert],
        "revocations": [],
    });
    let fleet_path = identities
        .join("fleets")
        .join(format!("{}.json", hex::encode(user)));
    std::fs::create_dir_all(fleet_path.parent().unwrap()).unwrap();
    std::fs::write(fleet_path, serde_json::to_vec_pretty(&fleet).unwrap()).unwrap();

    let home_cert = signed_cert(root, home, "Home peer", DeviceCapabilities::host());
    let registration = HomePeerRegistration {
        schema_version: HomePeerRegistration::CURRENT_SCHEMA,
        device_node_id: hex::encode(home_cert.device_public_key),
        relay_service_node_id: hex::encode(qos_relay::daemon::relay_service_node_id(home)),
        label: "Home peer".into(),
        user_public_key: hex::encode(user),
        device_cert: Some(home_cert),
        relay_workspace_ids: vec![
            FILE_WORKSPACE,
            qos_relay::personal_continuity_workspace_id(&user),
        ],
    };
    std::fs::write(
        root_dir.join("home-peers.json"),
        serde_json::to_vec_pretty(&vec![registration.clone()]).unwrap(),
    )
    .unwrap();
    registration
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordinary_device_enrollment_survives_approver_offline_with_home_peer_route() {
    let workspace = tempfile::tempdir().unwrap();
    let approver_root = workspace.path().join("approver-qos");
    let approver_identities = workspace.path().join("approver-identities");
    let recipient_root = workspace.path().join("recipient-qos");
    let recipient_identities = workspace.path().join("recipient-identities");
    std::fs::create_dir_all(&approver_identities).unwrap();
    std::fs::create_dir_all(&recipient_identities).unwrap();

    let root = SigningKey::from_bytes(&[0x11; 32]);
    let approver = SigningKey::from_bytes(&[0x12; 32]);
    let home = SigningKey::from_bytes(&[0x13; 32]);
    let source_registration = write_approver_fixture(
        &approver_root,
        &approver_identities,
        &root,
        &approver,
        &home,
    );

    // Seed the recipient key before starting either process so approval pins
    // the exact device and domain-separated relay service identities.
    let recipient = SigningKey::from_bytes(&[0x14; 32]);
    let recipient_paths = IdentityPaths::under(&recipient_root);
    recipient_paths.ensure_directories().unwrap();
    qos_user_identity::persist::write_device_key(&recipient_paths.device_privkey, &recipient)
        .unwrap();
    let recipient_id = hex::encode(recipient.verifying_key().to_bytes());
    let recipient_service = hex::encode(qos_relay::daemon::relay_service_node_id(&recipient));

    let enroll = std::path::PathBuf::from(env!("CARGO_BIN_EXE_qos-enroll"));
    let status_file = recipient_identities.join("pairing-status.json");
    let mut recipient_process = Command::new(&enroll)
        .args(["pair-device", "--identity-root"])
        .arg(&recipient_root)
        .args(["--identity-dir"])
        .arg(&recipient_identities)
        .args(["--label", "New laptop", "--timeout", "10", "--status-file"])
        .arg(&status_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_file(&status_file, "recipient pairing status");

    // The recipient's persisted waiting state is the barrier. It can safely
    // retry its authenticated solicitation until this approver binds UDP.
    let mut approval = Command::new(&enroll)
        .args(["approve-device", "--identity-root"])
        .arg(&approver_root)
        .args(["--identity-dir"])
        .arg(&approver_identities)
        .args([
            "--expected-device",
            &recipient_id,
            "--expected-service",
            &recipient_service,
            "--timeout",
            "10",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let recipient_output = recipient_process.wait_with_output().unwrap();
    assert!(
        recipient_output.status.success(),
        "recipient enrollment failed: {}",
        String::from_utf8_lossy(&recipient_output.stderr)
    );
    let approval_output = approval.wait_with_output().unwrap();
    assert!(
        approval_output.status.success(),
        "approval failed: {}",
        String::from_utf8_lossy(&approval_output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&status_file).unwrap()).unwrap()
            ["state"],
        "complete",
        "a completed enrollment must not leave the first-run UI waiting"
    );

    let member_cert =
        qos_user_identity::persist::read_device_cert(&recipient_paths.device_cert).unwrap();
    assert_eq!(member_cert.capabilities, DeviceCapabilities::member());
    assert!(!recipient_paths.user_privkey.exists());
    let record = recipient_identities.join(format!(
        "identity-{}.json",
        &hex::encode(root.verifying_key().to_bytes())[..55]
    ));
    let projected = runtime_identity::load_from_path(record).unwrap();
    assert_eq!(
        projected.group_public_key(),
        root.verifying_key().to_bytes()
    );
    assert!(projected.sign(b"owner operation").is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let owner = std::fs::metadata(&recipient_identities).unwrap();
        let fleet = recipient_identities.join("fleets").join(format!(
            "{}.json",
            hex::encode(root.verifying_key().to_bytes())
        ));
        for projected_path in [recipient_identities.join("registry.json"), fleet] {
            let metadata = std::fs::metadata(&projected_path).unwrap();
            assert_eq!(
                metadata.uid(),
                owner.uid(),
                "{} uid",
                projected_path.display()
            );
            assert_eq!(
                metadata.gid(),
                owner.gid(),
                "{} gid",
                projected_path.display()
            );
        }
    }

    let transferred: Vec<HomePeerRegistration> =
        serde_json::from_slice(&std::fs::read(recipient_root.join("home-peers.json")).unwrap())
            .unwrap();
    assert_eq!(transferred, vec![source_registration.clone()]);
    assert!(transferred[0].relay_workspace_ids.contains(&FILE_WORKSPACE));
    assert!(transferred[0].relay_workspace_ids.contains(
        &qos_relay::personal_continuity_workspace_id(&root.verifying_key().to_bytes())
    ));

    // The approver process has exited. Start only the transferred home peer.
    // The shell's registry resolver dials an id-only EndpointAddr, so use that
    // exact representation rather than carrying the approver's live address.
    let state = tempfile::tempdir().unwrap();
    let relay = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            ..RelayConfig::default()
        },
        RelayIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: home.clone(),
            device_cert: source_registration.device_cert.clone().unwrap(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        hex::encode(relay.node_addr().id.as_bytes()),
        transferred[0].relay_service_node_id,
        "the transferred registration names the actual home-peer service"
    );
    relay
        .set_workspace_membership(
            FILE_WORKSPACE,
            true,
            vec![member_cert.device_public_key],
            state.path().to_path_buf(),
        )
        .await
        .unwrap();
    let service_id: [u8; 32] = hex::decode(&transferred[0].relay_service_node_id)
        .unwrap()
        .try_into()
        .unwrap();
    let transferred_addr =
        iroh::EndpointAddr::new(iroh::EndpointId::from_bytes(&service_id).unwrap());
    let client = RelayClient::connect(
        client_endpoint().await,
        transferred_addr,
        root.verifying_key().to_bytes(),
        member_cert.clone(),
        recipient,
        RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(Envelope::Subscribe {
            workspace_id: FILE_WORKSPACE,
        })
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), client.recv()).await,
        Ok(Some(Envelope::Ready {
            workspace_id: FILE_WORKSPACE,
            ..
        }))
    ));
    let continuity = qos_relay::personal_continuity_workspace_id(&root.verifying_key().to_bytes());
    relay
        .set_workspace_membership(
            continuity,
            true,
            vec![member_cert.device_public_key],
            state.path().to_path_buf(),
        )
        .await
        .unwrap();
    client
        .send(Envelope::Subscribe {
            workspace_id: continuity,
        })
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), client.recv()).await,
        Ok(Some(Envelope::Ready { workspace_id, .. })) if workspace_id == continuity
    ));
}

/// The polkit helper deliberately has a fixed `/usr/bin/qos-enroll` route.
/// Exercise that route only where CI provides the mount capability needed to
/// put the freshly-built binary at the production path without touching the
/// runner's mount namespace.
#[test]
#[ignore = "requires root plus CAP_SYS_ADMIN; fs-fuse.yml runs this explicitly"]
fn privileged_helper_preserves_user_owned_personal_device_projection() {
    #[cfg(not(unix))]
    panic!("the privileged helper is Linux-only");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        assert_eq!(
            std::fs::metadata("/proc/self").unwrap().uid(),
            0,
            "privileged runner must be root"
        );
        const REQUEST_UID: u32 = 42_424;
        let scope = tempfile::tempdir().unwrap();
        let approver_root = scope.path().join("approver/qos");
        let approver_identities = scope.path().join("approver/identities");
        let recipient_root = scope.path().join("recipient/qos");
        let recipient_identities = scope.path().join("recipient/identities");
        for path in [&approver_identities, &recipient_identities] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }

        let root = SigningKey::from_bytes(&[0x21; 32]);
        let approver = SigningKey::from_bytes(&[0x22; 32]);
        let home = SigningKey::from_bytes(&[0x23; 32]);
        let registration = write_approver_fixture(
            &approver_root,
            &approver_identities,
            &root,
            &approver,
            &home,
        );
        std::fs::set_permissions(&approver_root, std::fs::Permissions::from_mode(0o700)).unwrap();

        let recipient = SigningKey::from_bytes(&[0x24; 32]);
        let recipient_paths = IdentityPaths::under(&recipient_root);
        recipient_paths.ensure_directories().unwrap();
        std::fs::set_permissions(&recipient_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        qos_user_identity::persist::write_device_key(&recipient_paths.device_privkey, &recipient)
            .unwrap();
        let recipient_id = hex::encode(recipient.verifying_key().to_bytes());
        let recipient_service = hex::encode(qos_relay::daemon::relay_service_node_id(&recipient));
        let enroll = env!("CARGO_BIN_EXE_qos-enroll");
        let helper = env!("CARGO_BIN_EXE_qos-home-peer-control");
        let ownership = Command::new("chown")
            .args(["-R", &format!("{REQUEST_UID}:{REQUEST_UID}")])
            .arg(scope.path())
            .status()
            .unwrap();
        assert!(ownership.success(), "prepare non-root identity ownership");

        // Keep the bind mount private. The helper itself retains its hardened,
        // non-configurable `/usr/bin/qos-enroll` path.
        let script = r#"
set -eu
exec 9</usr/bin/mount
mount --make-rprivate /
/proc/self/fd/9 -t tmpfs tmpfs /etc
printf '%s\n' 'qosaccept:x:42424:42424::/home/qosaccept:/sbin/nologin' >/etc/passwd
/proc/self/fd/9 -t tmpfs tmpfs /home
mkdir -p /home/qosaccept
/proc/self/fd/9 --bind "$QOS_SCOPE" /home/qosaccept
/proc/self/fd/9 -t tmpfs tmpfs /usr/bin
: > /usr/bin/qos-enroll
/proc/self/fd/9 --bind "$QOS_ENROLL_BIN" /usr/bin/qos-enroll
export PKEXEC_UID=42424
"$QOS_HELPER" approve-device --identity-root "$QOS_APPROVER_ROOT" --identity-dir "$QOS_APPROVER_IDENTITIES" --expected-device "$QOS_RECIPIENT_ID" --expected-service "$QOS_RECIPIENT_SERVICE" &
approval=$!
"$QOS_HELPER" pair-device --identity-root "$QOS_RECIPIENT_ROOT" --identity-dir "$QOS_RECIPIENT_IDENTITIES" --label "New laptop"
wait "$approval"
"#;
        let output = Command::new("unshare")
            .args(["--mount", "--fork", "--propagation", "private", "--"])
            .arg("bash")
            .arg("-ceu")
            .arg(script)
            .env("QOS_ENROLL_BIN", enroll)
            .env("QOS_HELPER", helper)
            .env("QOS_SCOPE", scope.path())
            .env("QOS_APPROVER_ROOT", "/home/qosaccept/approver/qos")
            .env(
                "QOS_APPROVER_IDENTITIES",
                "/home/qosaccept/approver/identities",
            )
            .env("QOS_RECIPIENT_ROOT", "/home/qosaccept/recipient/qos")
            .env(
                "QOS_RECIPIENT_IDENTITIES",
                "/home/qosaccept/recipient/identities",
            )
            .env("QOS_RECIPIENT_ID", &recipient_id)
            .env("QOS_RECIPIENT_SERVICE", &recipient_service)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "privileged helper acceptance failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let status: serde_json::Value = serde_json::from_slice(
            &std::fs::read(recipient_identities.join("pairing-status.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(status["state"], "complete");
        let cert =
            qos_user_identity::persist::read_device_cert(&recipient_paths.device_cert).unwrap();
        assert_eq!(cert.capabilities, DeviceCapabilities::member());
        assert!(!recipient_paths.user_privkey.exists());
        let transferred: Vec<HomePeerRegistration> =
            serde_json::from_slice(&std::fs::read(recipient_root.join("home-peers.json")).unwrap())
                .unwrap();
        assert_eq!(transferred, vec![registration]);
        for path in [
            recipient_paths.user_pubkey.parent().unwrap().to_path_buf(),
            recipient_paths
                .device_privkey
                .parent()
                .unwrap()
                .to_path_buf(),
            recipient_paths
                .device_cert_for_user(&root.verifying_key().to_bytes())
                .parent()
                .unwrap()
                .to_path_buf(),
            recipient_paths.user_pubkey.clone(),
            recipient_paths.device_privkey.clone(),
            recipient_paths.device_cert.clone(),
            recipient_paths.device_cert_for_user(&root.verifying_key().to_bytes()),
            recipient_root.join("home-peers.json"),
            recipient_identities.join("pairing-status.json"),
            recipient_identities.join("registry.json"),
            recipient_identities.join("fleets").join(format!(
                "{}.json",
                hex::encode(root.verifying_key().to_bytes())
            )),
        ] {
            let metadata = std::fs::metadata(&path).unwrap();
            assert_eq!(metadata.uid(), REQUEST_UID, "{} uid", path.display());
            assert_eq!(metadata.gid(), REQUEST_UID, "{} gid", path.display());
        }
    }
}
