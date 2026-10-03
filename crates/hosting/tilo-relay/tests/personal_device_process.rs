//! Process-level acceptance coverage for ordinary nearby-device enrollment.
//!
//! The approver and recipient are separate `tilo-enroll` processes.  This is
//! intentionally above the crypto unit tests: it proves the on-disk projection
//! that the shell reads is only published after the signed LAN exchange and
//! remains enough to reach a home peer once the approving device is gone.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable,
    reason = "test assertions and isolated fixture keys"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use getrandom::{SysRng, rand_core::UnwrapErr};
use std::process::{Command, Stdio};

use ed25519_dalek::SigningKey;
use tilo_relay::daemon::RelayIdentity;
use tilo_relay::{Envelope, HomePeerRegistration, Relay, RelayClient, RelayConfig};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, RevocationList, sign_device_cert,
};

use crate::common::client_endpoint;

const FILE_WORKSPACE: u64 = 42;
fn wait_for_file(path: &std::path::Path, what: &str) {
    tilo_test_env::wait_until(format_args!("{what} at {}", path.display()), || {
        path.is_file()
    });
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
    relay_endpoint: Option<iroh::EndpointAddr>,
) -> HomePeerRegistration {
    let paths = IdentityPaths::under(root_dir);
    paths.ensure_directories().unwrap();
    tilo_user_identity::persist::write_signing_key(&paths.user_privkey, root).unwrap();
    tilo_user_identity::persist::write_public_key(
        &paths.user_pubkey,
        &root.verifying_key().to_bytes(),
    )
    .unwrap();
    tilo_user_identity::persist::write_device_key(&paths.device_privkey, approver).unwrap();
    let approver_cert = signed_cert(
        root,
        approver,
        "Approving laptop",
        DeviceCapabilities::primary(),
    );
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &approver_cert).unwrap();
    tilo_user_identity::persist::write_device_cert(
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
        relay_service_node_id: hex::encode(tilo_relay::daemon::relay_service_node_id(home)),
        relay_endpoint,
        label: "Home peer".into(),
        user_public_key: hex::encode(user),
        device_cert: Some(home_cert),
        relay_workspace_ids: vec![
            FILE_WORKSPACE,
            tilo_relay::personal_continuity_workspace_id(&user),
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
#[allow(
    clippy::too_many_lines,
    reason = "keeps enrollment and offline-owner continuity in one process-level trace"
)]
async fn ordinary_device_enrollment_survives_approver_offline_with_home_peer_route() {
    let workspace = tempfile::tempdir().unwrap();
    let approver_root = workspace.path().join("approver-tilo");
    let approver_identities = workspace.path().join("approver-identities");
    let recipient_root = workspace.path().join("recipient-tilo");
    let recipient_identities = workspace.path().join("recipient-identities");
    std::fs::create_dir_all(&approver_identities).unwrap();
    std::fs::create_dir_all(&recipient_identities).unwrap();

    // Production Iroh endpoints publish these identities outside the guest.
    // Concurrent CI jobs must not register the same transport identity.
    let root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let approver = SigningKey::generate(&mut UnwrapErr(SysRng));
    let home = SigningKey::generate(&mut UnwrapErr(SysRng));
    let state = tempfile::tempdir().unwrap();
    let home_cert = signed_cert(&root, &home, "Home peer", DeviceCapabilities::host());
    let relay = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            ..RelayConfig::default()
        },
        RelayIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: home.clone(),
            device_cert: home_cert,
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let source_registration = write_approver_fixture(
        &approver_root,
        &approver_identities,
        &root,
        &approver,
        &home,
        Some(relay.node_addr()),
    );

    // Seed the recipient key before starting either process so approval pins
    // the exact device and domain-separated relay service identities.
    let recipient = SigningKey::generate(&mut UnwrapErr(SysRng));
    let recipient_paths = IdentityPaths::under(&recipient_root);
    recipient_paths.ensure_directories().unwrap();
    tilo_user_identity::persist::write_device_key(&recipient_paths.device_privkey, &recipient)
        .unwrap();
    let recipient_id = hex::encode(recipient.verifying_key().to_bytes());
    let recipient_service = hex::encode(tilo_relay::daemon::relay_service_node_id(&recipient));

    let enroll = std::path::PathBuf::from(env!("CARGO_BIN_EXE_tilo-enroll"));
    let status_file = recipient_identities.join("pairing-status.json");
    let requester = tilo_relay::ProcessLease::capture(std::process::id()).unwrap();
    let recipient_process = Command::new(&enroll)
        .args(["pair-device", "--identity-root"])
        .arg(&recipient_root)
        .args(["--identity-dir"])
        .arg(&recipient_identities)
        .args(["--label", "New laptop", "--timeout", "60", "--status-file"])
        .arg(&status_file)
        .arg("--requester-pid")
        .arg(requester.pid().to_string())
        .arg("--requester-start-time")
        .arg(requester.start_time().to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_file(&status_file, "recipient pairing status");

    // The recipient's persisted waiting state is the barrier. It can safely
    // retry its authenticated solicitation until this approver binds UDP.
    let approval = Command::new(&enroll)
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
            "60",
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
        tilo_user_identity::persist::read_device_cert(&recipient_paths.device_cert).unwrap();
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
        &tilo_relay::personal_continuity_workspace_id(&root.verifying_key().to_bytes())
    ));

    // The approver process has exited; the transferred, authenticated relay
    // address remains enough to reach the home peer without DNS/lookup.
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
    let transferred_addr = transferred[0]
        .relay_endpoint
        .clone()
        .expect("enrollment must transfer the relay address hint");
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
        tilo_test_env::within("the file workspace's Ready", client.recv()).await,
        Some(Envelope::Ready {
            workspace_id: FILE_WORKSPACE,
            ..
        })
    ));
    let continuity = tilo_relay::personal_continuity_workspace_id(&root.verifying_key().to_bytes());
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
        tilo_test_env::within("the continuity workspace's Ready", client.recv()).await,
        Some(Envelope::Ready { workspace_id, .. }) if workspace_id == continuity
    ));
}

/// The polkit helper deliberately has a fixed `/usr/bin/tilo-enroll` route.
/// Exercise that route only where CI provides the mount capability needed to
/// put the freshly-built binary at the production path without touching the
/// runner's mount namespace.
#[test]
#[ignore = "requires root plus CAP_SYS_ADMIN; ci-filesystem.yml runs this explicitly"]
#[allow(
    clippy::too_many_lines,
    reason = "keeps privileged helper setup and ownership assertions in one auditable trace"
)]
fn privileged_helper_preserves_user_owned_personal_device_projection() {
    #[cfg(not(unix))]
    panic!("the privileged helper is Linux-only");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        const REQUEST_UID: u32 = 42_424;

        assert_eq!(
            std::fs::metadata("/proc/self").unwrap().uid(),
            0,
            "privileged runner must be root"
        );
        let scope = tempfile::tempdir().unwrap();
        let approver_root = scope.path().join("approver/tilo");
        let approver_identities = scope.path().join("approver/identities");
        let recipient_root = scope.path().join("recipient/tilo");
        let recipient_identities = scope.path().join("recipient/identities");
        for path in [&approver_identities, &recipient_identities] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }

        let root = SigningKey::generate(&mut UnwrapErr(SysRng));
        let approver = SigningKey::generate(&mut UnwrapErr(SysRng));
        let home = SigningKey::generate(&mut UnwrapErr(SysRng));
        let registration = write_approver_fixture(
            &approver_root,
            &approver_identities,
            &root,
            &approver,
            &home,
            None,
        );
        std::fs::set_permissions(&approver_root, std::fs::Permissions::from_mode(0o700)).unwrap();

        let recipient = SigningKey::generate(&mut UnwrapErr(SysRng));
        let recipient_paths = IdentityPaths::under(&recipient_root);
        recipient_paths.ensure_directories().unwrap();
        std::fs::set_permissions(&recipient_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        tilo_user_identity::persist::write_device_key(&recipient_paths.device_privkey, &recipient)
            .unwrap();
        let recipient_id = hex::encode(recipient.verifying_key().to_bytes());
        let recipient_service = hex::encode(tilo_relay::daemon::relay_service_node_id(&recipient));
        let enroll = env!("CARGO_BIN_EXE_tilo-enroll");
        let helper = env!("CARGO_BIN_EXE_tilo-home-peer-control");
        let ownership = Command::new("chown")
            .args(["-R", &format!("{REQUEST_UID}:{REQUEST_UID}")])
            .arg(scope.path())
            .status()
            .unwrap();
        assert!(ownership.success(), "prepare non-root identity ownership");

        // Keep the bind mount private. The helper itself retains its hardened,
        // non-configurable `/usr/bin/tilo-enroll` path.
        let script = r#"
set -eu
exec 9</usr/bin/mount
exec 8</usr/bin/setpriv
exec 7</usr/bin/sleep
exec 6</usr/bin/awk
mount --make-rprivate /
/proc/self/fd/9 -t tmpfs tmpfs /etc
printf '%s\n' 'tiloaccept:x:42424:42424::/home/tiloaccept:/sbin/nologin' >/etc/passwd
/proc/self/fd/9 -t tmpfs tmpfs /home
mkdir -p /home/tiloaccept
/proc/self/fd/9 --bind "$TILO_SCOPE" /home/tiloaccept
/proc/self/fd/9 -t tmpfs tmpfs /usr/bin
: > /usr/bin/tilo-enroll
/proc/self/fd/9 --bind "$TILO_ENROLL_BIN" /usr/bin/tilo-enroll
export PKEXEC_UID=42424
/proc/self/fd/8 --reuid=42424 --regid=42424 --clear-groups /proc/self/fd/7 300 &
requester=$!
trap 'kill "$requester" 2>/dev/null || true' EXIT
# The background child initially retains the shell's root credentials until
# setpriv executes. Do not race the helper's ownership validation against that
# transition.
requester_uid=
while [ "$requester_uid" != 42424 ]; do
    requester_uid=$(/proc/self/fd/6 '$1 == "Uid:" { print $2; exit }' "/proc/$requester/status" 2>/dev/null) || requester_uid=
    if [ "$SECONDS" -ge "$TILO_HANG_BOUND_SECS" ]; then
        state=$(/proc/self/fd/6 '$1 == "State:" { print $2 $3; exit }' "/proc/$requester/status" 2>/dev/null) || state=
        echo "requester did not assume uid 42424 within ${TILO_HANG_BOUND_SECS}s (observed ${requester_uid:-missing}, state ${state:-missing})" >&2
        exit 1
    fi
    /proc/self/fd/7 0.01
done
requester_start=$(/proc/self/fd/6 '{print $22}' "/proc/$requester/stat")
"$TILO_HELPER" approve-device --identity-root "$TILO_APPROVER_ROOT" --identity-dir "$TILO_APPROVER_IDENTITIES" --expected-device "$TILO_RECIPIENT_ID" --expected-service "$TILO_RECIPIENT_SERVICE" &
approval=$!
"$TILO_HELPER" pair-device --identity-root "$TILO_RECIPIENT_ROOT" --identity-dir "$TILO_RECIPIENT_IDENTITIES" --label "New laptop" --status-file "$TILO_RECIPIENT_IDENTITIES/pairing-status.json" --requester-pid "$requester" --requester-start-time "$requester_start"
wait "$approval"
"#;
        let output = Command::new("unshare")
            .args(["--mount", "--fork", "--propagation", "private", "--"])
            .arg("bash")
            .arg("-ceu")
            .arg(script)
            .env(
                "TILO_HANG_BOUND_SECS",
                tilo_test_env::hang_bound().as_secs().to_string(),
            )
            .env("TILO_ENROLL_BIN", enroll)
            .env("TILO_HELPER", helper)
            .env("TILO_SCOPE", scope.path())
            .env("TILO_APPROVER_ROOT", "/home/tiloaccept/approver/tilo")
            .env(
                "TILO_APPROVER_IDENTITIES",
                "/home/tiloaccept/approver/identities",
            )
            .env("TILO_RECIPIENT_ROOT", "/home/tiloaccept/recipient/tilo")
            .env(
                "TILO_RECIPIENT_IDENTITIES",
                "/home/tiloaccept/recipient/identities",
            )
            .env("TILO_RECIPIENT_ID", &recipient_id)
            .env("TILO_RECIPIENT_SERVICE", &recipient_service)
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
            tilo_user_identity::persist::read_device_cert(&recipient_paths.device_cert).unwrap();
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
