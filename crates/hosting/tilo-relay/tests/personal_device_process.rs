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

fn require_root_and_person_account() {
    // SAFETY: geteuid has no preconditions.
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "requires the root test runner"
    );
    assert!(
        std::fs::read_to_string("/etc/passwd")
            .expect("the runner account database must be observable")
            .lines()
            .any(|line| line.split(':').nth(2) == Some("65534")),
        "requires an existing test-person account with uid 65534"
    );
}

// These fixtures cross uid boundaries. A runner's TMPDIR can be below a
// root-only container directory, so use the world-traversable system scratch
// directory and expose only this freshly allocated fixture to the test person.
fn person_scope() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let scope = tempfile::Builder::new()
        .prefix("tilo-person-test-")
        .tempdir_in("/var/tmp")
        .unwrap();
    std::fs::set_permissions(scope.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    scope
}

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
        environments: tilo_relay_protocol::RelayEnvironments::default(),
    };
    tilo_files::native::atomic::write_creating_parent(
        &tilo_relay::home_peer_registry_path(root_dir),
        &serde_json::to_vec_pretty(&vec![registration.clone()]).unwrap(),
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
    assert_eq!(member_cert.capabilities, DeviceCapabilities::joined());
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

    let transferred: Vec<HomePeerRegistration> = serde_json::from_slice(
        &std::fs::read(tilo_relay::home_peer_registry_path(&recipient_root)).unwrap(),
    )
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

fn fleet_lists(identities: &std::path::Path, user: [u8; 32], label: &str) -> bool {
    let path = identities
        .join("fleets")
        .join(format!("{}.json", hex::encode(user)));
    let fleet: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    fleet["certificates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|cert| cert["device_label"] == label)
}

/// A device onboarded today keeps the person's root only in their identity
/// record, never as a seed under the identity root. Approving from it signs
/// the new device through that record.
#[test]
fn ordinary_device_enrollment_is_approved_with_the_identity_record() {
    let workspace = tempfile::tempdir().unwrap();
    let approver_root = workspace.path().join("approver-tilo");
    let approver_identities = workspace.path().join("approver-identities");
    let recipient_root = workspace.path().join("recipient-tilo");
    let recipient_identities = workspace.path().join("recipient-identities");
    std::fs::create_dir_all(&recipient_identities).unwrap();

    let approver_paths = IdentityPaths::under(&approver_root);
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &approver_identities,
        &approver_paths,
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    assert!(!approver_paths.user_privkey.exists());
    let user = tilo_user_identity::persist::read_device_cert(&approver_paths.device_cert)
        .unwrap()
        .user_public_key;

    let recipient = SigningKey::generate(&mut UnwrapErr(SysRng));
    let recipient_paths = IdentityPaths::under(&recipient_root);
    recipient_paths.ensure_directories().unwrap();
    tilo_user_identity::persist::write_device_key(&recipient_paths.device_privkey, &recipient)
        .unwrap();
    let recipient_id = hex::encode(recipient.verifying_key().to_bytes());
    let recipient_service = hex::encode(tilo_relay::daemon::relay_service_node_id(&recipient));

    let enroll = std::path::PathBuf::from(env!("CARGO_BIN_EXE_tilo-enroll"));
    let status_file = recipient_identities.join("pairing-status.json");
    let recipient_process = Command::new(&enroll)
        .args(["pair-device", "--headless", "--identity-root"])
        .arg(&recipient_root)
        .args(["--identity-dir"])
        .arg(&recipient_identities)
        .args([
            "--label",
            "Headless board",
            "--timeout",
            "60",
            "--status-file",
        ])
        .arg(&status_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_file(&status_file, "recipient pairing status");
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

    let member_cert =
        tilo_user_identity::persist::read_device_cert(&recipient_paths.device_cert).unwrap();
    assert_eq!(member_cert.user_public_key, user);
    assert_eq!(
        member_cert.device_public_key,
        recipient.verifying_key().to_bytes()
    );
    assert_eq!(member_cert.capabilities, DeviceCapabilities::joined());
    tilo_user_identity::verify_device_cert(&member_cert).unwrap();
    // The session's first-boot check accepts it as an enrolled device, as it
    // does a device the imager enrolled, instead of asking for recovery.
    assert!(
        tilo_user_identity::persist::joined_device_is_ready(
            &recipient_paths,
            tilo_clock::try_unix_now_ns().unwrap()
        )
        .unwrap()
    );
    assert!(!recipient_paths.user_privkey.exists());
    assert!(!approver_paths.user_privkey.exists());

    assert!(
        fleet_lists(&approver_identities, user, "Headless board"),
        "the approver's fleet must remember the device it admitted"
    );
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
# The joining device runs as the person, never through the broker.
/proc/self/fd/8 --reuid=42424 --regid=42424 --clear-groups /usr/bin/tilo-enroll pair-device --identity-root "$TILO_RECIPIENT_ROOT" --identity-dir "$TILO_RECIPIENT_IDENTITIES" --label "New laptop" --status-file "$TILO_RECIPIENT_IDENTITIES/pairing-status.json" --requester-pid "$requester" --requester-start-time "$requester_start"
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
        assert_eq!(cert.capabilities, DeviceCapabilities::joined());
        assert!(!recipient_paths.user_privkey.exists());
        let transferred: Vec<HomePeerRegistration> = serde_json::from_slice(
            &std::fs::read(tilo_relay::home_peer_registry_path(&recipient_root)).unwrap(),
        )
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
            tilo_relay::home_peer_registry_path(&recipient_root),
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

/// The approver reads its fleet without following a link planted in its
/// place, even one to a valid fleet: a root-run approval would otherwise
/// read and then replace whatever the link names.
#[test]
fn approval_refuses_a_fleet_link_planted_in_the_identity_directory() {
    let scope = tempfile::tempdir().unwrap();
    let approver_root = scope.path().join("approver/tilo");
    let approver_identities = scope.path().join("approver/identities");
    let root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let approver = SigningKey::generate(&mut UnwrapErr(SysRng));
    let home = SigningKey::generate(&mut UnwrapErr(SysRng));
    write_approver_fixture(
        &approver_root,
        &approver_identities,
        &root,
        &approver,
        &home,
        None,
    );
    let fleet = approver_identities.join("fleets").join(format!(
        "{}.json",
        hex::encode(root.verifying_key().to_bytes())
    ));
    let elsewhere = scope.path().join("system-file.json");
    std::fs::rename(&fleet, &elsewhere).unwrap();
    let original = std::fs::read(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &fleet).unwrap();

    let device = "11".repeat(32);
    let service = "22".repeat(32);
    let approve = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tilo-enroll"))
            .args(["approve-device", "--identity-root"])
            .arg(&approver_root)
            .arg("--identity-dir")
            .arg(&approver_identities)
            .args(["--expected-device", &device, "--expected-service", &service])
            .args(["--timeout", "1"])
            .args(extra)
            .output()
            .unwrap()
    };
    let output = approve(&[]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("verify approving device fleet"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(&elsewhere).unwrap(), original);
    assert!(
        std::fs::symlink_metadata(&fleet)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    // Not root, the approval acts only for its own account; as root it
    // reaches another account's files only as that account, which cannot
    // open this one's private directory.
    // SAFETY: geteuid has no preconditions.
    let euid = unsafe { libc::geteuid() };
    let other = euid.wrapping_add(1).to_string();
    let output = approve(&["--person-uid", &other]);
    assert!(!output.status.success());
    if euid != 0 {
        assert!(String::from_utf8_lossy(&output.stderr).contains("--person-uid"));
    }
    assert_eq!(std::fs::read(&elsewhere).unwrap(), original);
}

/// The child a root-run approval delegates to: it answers for the account it
/// runs as and never follows a link planted in that account's home.
#[test]
fn the_person_home_child_refuses_planted_links() {
    let enroll = env!("CARGO_BIN_EXE_tilo-enroll");
    let user = hex::encode([5_u8; 32]);
    let child = |args: &[std::ffi::OsString], input: Option<&[u8]>| {
        let mut process = Command::new(enroll)
            .arg("person-home")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = process.stdin.take().unwrap();
        if let Some(input) = input {
            // A refusing child may exit before consuming stdin. Its status
            // and the unchanged target below still prove the refusal.
            if let Err(error) = std::io::Write::write_all(&mut stdin, input) {
                assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
            }
        }
        drop(stdin);
        process.wait_with_output().unwrap()
    };
    let fleet_args = |op: &'static str, dir: &std::path::Path| -> Vec<std::ffi::OsString> {
        vec![
            op.into(),
            "--path".into(),
            dir.into(),
            "--subdir".into(),
            "fleets".into(),
            "--name".into(),
            format!("{user}.json").into(),
            "--mode".into(),
            "644".into(),
        ]
    };
    let scope = tempfile::tempdir().unwrap();

    // The person's own directory round-trips.
    let identities = scope.path().join("identities");
    std::fs::create_dir(&identities).unwrap();
    let read = fleet_args("read", &identities);
    let output = child(&read, None);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"0");
    let write = fleet_args("write", &identities);
    assert!(child(&write, Some(b"{\"fleet\":1}")).status.success());
    let output = child(&read, None);
    assert_eq!(output.stdout, b"1{\"fleet\":1}");

    // A linked identity directory and a linked fleet directory.
    let elsewhere = scope.path().join("etc");
    std::fs::create_dir(&elsewhere).unwrap();
    let linked = scope.path().join("linked-identities");
    std::os::unix::fs::symlink(&elsewhere, &linked).unwrap();
    for op in ["read", "write"] {
        assert!(!child(&fleet_args(op, &linked), Some(b"x")).status.success());
    }
    let swapped = scope.path().join("swapped");
    std::fs::create_dir(&swapped).unwrap();
    std::os::unix::fs::symlink(&elsewhere, swapped.join("fleets")).unwrap();
    for op in ["read", "write"] {
        assert!(
            !child(&fleet_args(op, &swapped), Some(b"x"))
                .status
                .success()
        );
    }
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);

    // A linked room export.
    let state = scope.path().join("state/tilo");
    std::fs::create_dir_all(&state).unwrap();
    let secret = scope.path().join("secret.json");
    std::fs::write(&secret, b"[]").unwrap();
    let rooms = state.join("personal-rooms.json");
    std::os::unix::fs::symlink(&secret, &rooms).unwrap();
    let rooms_args: Vec<std::ffi::OsString> = vec![
        "read".into(),
        "--path".into(),
        state.into(),
        "--name".into(),
        "personal-rooms.json".into(),
    ];
    let output = child(&rooms_args, None);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

/// Run as root (as the build containers do), an approval for another account
/// reads that account's fleet only through a child running as them: their
/// valid fleet is read, and a link they plant to a root-only file is not.
#[test]
fn a_root_run_approval_reads_the_persons_fleet_only_as_the_person() {
    use std::os::unix::fs::PermissionsExt as _;
    const PERSON: u32 = 65_534;
    require_root_and_person_account();
    let scope = person_scope();
    let approver_root = scope.path().join("approver/tilo");
    let approver_identities = scope.path().join("approver/identities");
    let root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let approver = SigningKey::generate(&mut UnwrapErr(SysRng));
    let home = SigningKey::generate(&mut UnwrapErr(SysRng));
    write_approver_fixture(
        &approver_root,
        &approver_identities,
        &root,
        &approver,
        &home,
        None,
    );
    std::fs::set_permissions(
        scope.path().join("approver"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let chown = Command::new("chown")
        .args(["-R", &format!("{PERSON}:{PERSON}")])
        .arg(&approver_identities)
        .status()
        .unwrap();
    assert!(chown.success());

    let device = "11".repeat(32);
    let service = "22".repeat(32);
    let approve = || {
        Command::new(env!("CARGO_BIN_EXE_tilo-enroll"))
            .args(["approve-device", "--identity-root"])
            .arg(&approver_root)
            .arg("--identity-dir")
            .arg(&approver_identities)
            .args(["--expected-device", &device, "--expected-service", &service])
            .args(["--timeout", "1", "--person-uid", &PERSON.to_string()])
            .output()
            .unwrap()
    };
    // The fleet is read as the person; the approval then waits for a device
    // that never comes.
    let output = approve();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("no valid matching nearby device request"),
        "{stderr}"
    );

    // Root acting for itself refuses the person's directory outright.
    let output = Command::new(env!("CARGO_BIN_EXE_tilo-enroll"))
        .args(["approve-device", "--identity-root"])
        .arg(&approver_root)
        .arg("--identity-dir")
        .arg(&approver_identities)
        .args(["--expected-device", &device, "--expected-service", &service])
        .args(["--timeout", "1"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("verify approving device fleet"), "{stderr}");

    // A link to a file only root can read is not followed.
    let fleet = approver_identities.join("fleets").join(format!(
        "{}.json",
        hex::encode(root.verifying_key().to_bytes())
    ));
    let secret = scope.path().join("root-only.json");
    std::fs::copy(&fleet, &secret).unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::remove_file(&fleet).unwrap();
    std::os::unix::fs::symlink(&secret, &fleet).unwrap();
    let output = approve();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("verify approving device fleet"), "{stderr}");
}

/// Run as root, a headless join for another account (`tilo-identity pair`
/// runs `sudo tilo-enroll pair-device`) publishes under the root-owned
/// identity root itself and writes the person's identity directory and
/// status file only as the person, so everything there is theirs.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "keeps both processes of the root-run join and its ownership assertions in one trace"
)]
fn a_root_run_headless_join_writes_the_persons_files_only_as_the_person() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    const PERSON: u32 = 65_534;
    require_root_and_person_account();
    let workspace = person_scope();
    let approver_root = workspace.path().join("approver-tilo");
    let approver_identities = workspace.path().join("approver-identities");
    let recipient_root = workspace.path().join("recipient-tilo");
    let recipient_identities = workspace.path().join("recipient-identities");
    std::fs::create_dir_all(&recipient_identities).unwrap();
    let chown = Command::new("chown")
        .arg(format!("{PERSON}:{PERSON}"))
        .arg(&recipient_identities)
        .status()
        .unwrap();
    assert!(chown.success());

    let approver_paths = IdentityPaths::under(&approver_root);
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &approver_identities,
        &approver_paths,
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let user = tilo_user_identity::persist::read_device_cert(&approver_paths.device_cert)
        .unwrap()
        .user_public_key;
    let recipient = SigningKey::generate(&mut UnwrapErr(SysRng));
    let recipient_paths = IdentityPaths::under(&recipient_root);
    recipient_paths.ensure_directories().unwrap();
    tilo_user_identity::persist::write_device_key(&recipient_paths.device_privkey, &recipient)
        .unwrap();
    let recipient_id = hex::encode(recipient.verifying_key().to_bytes());
    let recipient_service = hex::encode(tilo_relay::daemon::relay_service_node_id(&recipient));
    // The image's shared peers directory: root's, sticky, in a group the
    // person belongs to (their own here, the device-trust group on a device).
    let peers = tilo_relay_protocol::personal_devices::peers_dir(&recipient_root);
    std::fs::create_dir_all(&peers).unwrap();
    assert!(
        Command::new("chown")
            .arg(format!("0:{PERSON}"))
            .arg(&peers)
            .status()
            .unwrap()
            .success()
    );
    std::fs::set_permissions(&peers, std::fs::Permissions::from_mode(0o3770)).unwrap();

    let enroll = std::path::PathBuf::from(env!("CARGO_BIN_EXE_tilo-enroll"));
    let status_file = recipient_identities.join("pairing-status.json");
    let recipient_process = Command::new(&enroll)
        .args(["pair-device", "--headless", "--identity-root"])
        .arg(&recipient_root)
        .arg("--identity-dir")
        .arg(&recipient_identities)
        .args([
            "--label",
            "Headless board",
            "--timeout",
            "60",
            "--status-file",
        ])
        .arg(&status_file)
        .args(["--person-uid", &PERSON.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_file(&status_file, "recipient pairing status");
    let approval = Command::new(&enroll)
        .args(["approve-device", "--identity-root"])
        .arg(&approver_root)
        .arg("--identity-dir")
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

    let member_cert =
        tilo_user_identity::persist::read_device_cert(&recipient_paths.device_cert).unwrap();
    assert_eq!(member_cert.user_public_key, user);
    assert_eq!(member_cert.capabilities, DeviceCapabilities::joined());
    for path in [
        status_file,
        recipient_identities.join("registry.json"),
        recipient_identities.join("fleets"),
        recipient_identities
            .join("fleets")
            .join(format!("{}.json", hex::encode(user))),
    ] {
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(metadata.uid(), PERSON, "{} uid", path.display());
    }
    // The person's peers directory is theirs, written as them.
    let person = tilo_relay_protocol::personal_devices::PersonPeers::find(&peers, &user, PERSON)
        .unwrap()
        .expect("the person's peers directory");
    let metadata = std::fs::metadata(person.dir()).unwrap();
    assert_eq!(metadata.uid(), PERSON);
    assert_eq!(
        metadata.mode() & 0o7777,
        tilo_relay_protocol::personal_devices::PERSON_DIR_MODE
    );
    assert_eq!(
        std::fs::metadata(person.direct_peers_path()).unwrap().uid(),
        PERSON
    );
    // What lands under the identity root stays root's.
    assert_eq!(
        std::fs::metadata(&recipient_paths.device_cert)
            .unwrap()
            .uid(),
        0
    );
}

#[test]
fn another_account_cannot_remove_a_consumed_nonce_and_redeem_again() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::process::CommandExt as _;
    use tilo_identity_host::enrollment::{EnrollmentRole, NonceStore, Redemption, redeem};

    require_root_and_person_account();
    let scope = person_scope();
    let directory = scope.path().join("enrollment-nonces");
    std::fs::create_dir(&directory).unwrap();
    let policy = include_str!("../../../../os/usr/lib/tmpfiles.d/tilo.conf");
    let mode = policy
        .lines()
        .find(|line| line.split_whitespace().nth(1) == Some("/var/lib/tilo/enrollment-nonces"))
        .and_then(|line| line.split_whitespace().nth(2))
        .map(|mode| u32::from_str_radix(mode, 8).unwrap())
        .expect("the shipped nonce directory must be provisioned");
    let path = std::ffi::CString::new(directory.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: path is NUL-terminated; the root runner owns this isolated fixture.
    assert_eq!(unsafe { libc::chown(path.as_ptr(), 0, 65534) }, 0);
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(mode)).unwrap();

    let root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let issuer = SigningKey::generate(&mut UnwrapErr(SysRng));
    let admitted = SigningKey::generate(&mut UnwrapErr(SysRng));
    let issuer_cert = signed_cert(&root, &issuer, "Issuer", DeviceCapabilities::primary());
    let token = tilo_user_identity::mint_enrollment_token(
        root.verifying_key().to_bytes(),
        DeviceCapabilities::joined(),
        10,
        100,
        [17; 16],
        &issuer,
    )
    .unwrap();
    let nonces = NonceStore::under(scope.path());
    let sign = |digest: &[u8]| {
        use ed25519_dalek::Signer as _;
        Ok(root.sign(digest).to_bytes())
    };
    let request = Redemption {
        token: &token,
        role: EnrollmentRole::PersonalDevice,
        device_public_key: admitted.verifying_key().to_bytes(),
        label: "Laptop",
        now_ns: 20,
        valid_from_ns: 20,
        local_issuer: Some(&issuer_cert),
    };
    redeem(None, &nonces, &request, &sign).unwrap();
    let marker = directory.join(hex::encode(token.nonce));
    for (permission, path) in [("-r", &marker), ("-w", &directory)] {
        assert!(
            Command::new("test")
                .arg(permission)
                .arg(path)
                .uid(65534)
                .gid(65534)
                .status()
                .unwrap()
                .success(),
            "the other account must reach the shared nonce store"
        );
    }
    let removal = Command::new("rm")
        .arg("--")
        .arg(&marker)
        .uid(65534)
        .gid(65534)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!removal.success(), "a different account removed the nonce");
    assert!(nonces.is_consumed(&token.nonce).unwrap());
    let replay = Redemption {
        device_public_key: SigningKey::generate(&mut UnwrapErr(SysRng))
            .verifying_key()
            .to_bytes(),
        ..request
    };
    let error = redeem(None, &nonces, &replay, &sign).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("already been consumed"));
}
