use qos_session_peer::SessionOwner;
use std::{fs, process::Command};

#[cfg(unix)]
fn device_key_paths(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    (
        root.join("device/keypair.bin"),
        root.join("device-private/keypair.bin"),
    )
}

#[cfg(unix)]
fn move_device_key_to_legacy(root: &std::path::Path) -> Vec<u8> {
    let (legacy, canonical) = device_key_paths(root);
    let bytes = fs::read(&canonical).unwrap();
    fs::rename(&canonical, &legacy).unwrap();
    fs::remove_dir(canonical.parent().unwrap()).unwrap();
    bytes
}

#[cfg(unix)]
fn copy_tree(source: &std::path::Path, destination: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::create_dir(destination).unwrap();
    fs::set_permissions(
        destination,
        fs::Permissions::from_mode(fs::metadata(source).unwrap().permissions().mode()),
    )
    .unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            assert!(entry.file_type().unwrap().is_file());
            fs::copy(entry.path(), &target).unwrap();
            fs::set_permissions(&target, entry.metadata().unwrap().permissions()).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn sigint_exits_after_flushing_with_an_open_idle_stdin_pipe() {
    use std::io::Read;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let ready = dir.path().join("ready.json");
    drop(SessionOwner::initialize(&path).unwrap());

    struct ChildGuard(Option<std::process::Child>);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    let child = Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
        .arg("run")
        .arg(&path)
        .arg(&ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = ChildGuard(Some(child));

    let startup_deadline = Instant::now() + Duration::from_secs(25);
    while !ready.exists() {
        assert!(
            Instant::now() < startup_deadline,
            "test host never wrote ready metadata"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let process = child.0.as_mut().unwrap();
    assert!(
        process.stdin.is_some(),
        "the idle input pipe must remain open"
    );
    assert_eq!(unsafe { libc::kill(process.id() as i32, libc::SIGINT) }, 0);

    let shutdown_deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = process.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < shutdown_deadline,
            "SIGINT left the peer waiting on its idle stdin pipe"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stderr = String::new();
    process
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    child.0 = None;

    assert!(
        status.success(),
        "graceful SIGINT shutdown must succeed (status={status}, signal={:?}, stderr={stderr})",
        status.signal()
    );
    assert!(!ready.exists(), "graceful shutdown must remove readiness");
    SessionOwner::open(&path).expect("graceful shutdown must flush and release durable custody");
}

#[test]
fn restart_preserves_canonical_state_and_journal() {
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    owner
        .queue_pending("action-42", b"canonical invocation", true)
        .unwrap();
    let before = owner.status();
    assert_eq!(before.records.len(), 3);
    assert!(before.outgoing > 0);
    assert!(SessionOwner::open(&path).is_err());
    drop(owner);
    let restored = SessionOwner::open(&path).unwrap();
    assert_eq!(restored.status(), before);
    assert_eq!(restored.pending()[0].outcome_unknown, true);
}

#[cfg(unix)]
#[test]
fn restart_adopts_both_legacy_device_keys_without_changing_identity_artifacts() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let owner = SessionOwner::initialize(&path).unwrap();
    let expected = owner.status();
    drop(owner);

    let identity = path.join("identity");
    let host = path.join("host-identity");
    let artifact_paths = [
        identity.join("user-identity/privkey.bin"),
        identity.join("user-identity/pubkey.bin"),
        identity.join("device/cert.bin"),
        host.join("user-identity/pubkey.bin"),
        host.join("device/cert.bin"),
    ];
    let artifacts_before = artifact_paths
        .iter()
        .map(fs::read)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let native_key = move_device_key_to_legacy(&identity);
    let host_key = move_device_key_to_legacy(&host);

    let restored = SessionOwner::open(&path).unwrap();
    assert_eq!(restored.status(), expected);
    assert_eq!(
        fs::read(identity.join("device/keypair.bin")).unwrap(),
        native_key
    );
    assert_eq!(
        fs::read(identity.join("device-private/keypair.bin")).unwrap(),
        native_key
    );
    assert_eq!(fs::read(host.join("device/keypair.bin")).unwrap(), host_key);
    assert_eq!(
        fs::read(host.join("device-private/keypair.bin")).unwrap(),
        host_key
    );
    for (path, before) in artifact_paths.iter().zip(artifacts_before) {
        assert_eq!(fs::read(path).unwrap(), before);
    }
    for root in [&identity, &host] {
        assert_eq!(
            fs::metadata(root.join("device-private"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    drop(restored);

    assert_eq!(SessionOwner::open(&path).unwrap().status(), expected);
}

#[cfg(unix)]
#[test]
fn restart_resumes_partial_matching_identity_adoption() {
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let owner = SessionOwner::initialize(&path).unwrap();
    let expected = owner.status();
    drop(owner);

    let identity = path.join("identity");
    let host = path.join("host-identity");
    let native_key = fs::read(identity.join("device-private/keypair.bin")).unwrap();
    fs::copy(
        identity.join("device-private/keypair.bin"),
        identity.join("device/keypair.bin"),
    )
    .unwrap();
    let host_key = move_device_key_to_legacy(&host);

    let restored = SessionOwner::open(&path).unwrap();
    assert_eq!(restored.status(), expected);
    assert_eq!(
        fs::read(identity.join("device-private/keypair.bin")).unwrap(),
        native_key
    );
    assert_eq!(
        fs::read(host.join("device-private/keypair.bin")).unwrap(),
        host_key
    );
}

#[cfg(unix)]
#[test]
fn restart_rejects_conflicting_legacy_and_canonical_device_keys() {
    use ed25519_dalek::SigningKey;

    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    drop(SessionOwner::initialize(&path).unwrap());
    let identity = path.join("identity");
    let (_, canonical) = device_key_paths(&identity);
    let canonical_before = fs::read(&canonical).unwrap();
    let legacy = identity.join("device/keypair.bin");
    tilo_user_identity::persist::write_device_key(&legacy, &SigningKey::from_bytes(&[0x5a; 32]))
        .unwrap();
    let legacy_before = fs::read(&legacy).unwrap();
    let aggregate_before = fs::read(path.join("session.bin")).unwrap();

    assert!(SessionOwner::open(&path).is_err());
    assert_eq!(fs::read(canonical).unwrap(), canonical_before);
    assert_eq!(fs::read(legacy).unwrap(), legacy_before);
    assert_eq!(
        fs::read(path.join("session.bin")).unwrap(),
        aggregate_before
    );
}

#[cfg(unix)]
#[test]
fn restart_rejects_a_linked_legacy_device_key() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    drop(SessionOwner::initialize(&path).unwrap());
    let identity = path.join("identity");
    let legacy = identity.join("device/keypair.bin");
    let canonical = identity.join("device-private/keypair.bin");
    let canonical_before = fs::read(&canonical).unwrap();
    let aggregate_before = fs::read(path.join("session.bin")).unwrap();
    symlink("../device-private/keypair.bin", &legacy).unwrap();

    assert!(SessionOwner::open(&path).is_err());
    assert_eq!(fs::read(canonical).unwrap(), canonical_before);
    assert_eq!(
        fs::read(path.join("session.bin")).unwrap(),
        aggregate_before
    );
    assert!(
        fs::symlink_metadata(legacy)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn current_identity_directory_modes_remain_compatible_but_writable_modes_fail() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    drop(SessionOwner::initialize(&path).unwrap());
    let canonical = path.join("identity/device-private");

    fs::set_permissions(&canonical, fs::Permissions::from_mode(0o755)).unwrap();
    let identity_directories = [
        path.join("identity/user-identity"),
        path.join("identity/device"),
        path.join("host-identity/user-identity"),
        path.join("host-identity/device"),
    ];
    let before_open = identity_directories
        .iter()
        .map(|directory| fs::metadata(directory).unwrap().modified().unwrap())
        .collect::<Vec<_>>();
    drop(SessionOwner::open(&path).unwrap());
    assert_eq!(
        fs::metadata(&canonical).unwrap().permissions().mode() & 0o777,
        0o755
    );
    for (directory, before) in identity_directories.iter().zip(before_open) {
        assert_eq!(
            fs::metadata(directory).unwrap().modified().unwrap(),
            before,
            "an already-canonical open must not publish temporary identity entries"
        );
    }

    fs::set_permissions(&canonical, fs::Permissions::from_mode(0o775)).unwrap();
    assert!(SessionOwner::open(&path).is_err());
}

#[cfg(unix)]
#[test]
fn fresh_identity_private_key_directories_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    drop(SessionOwner::initialize(&path).unwrap());
    for root in [path.join("identity"), path.join("host-identity")] {
        assert_eq!(
            fs::metadata(root.join("device-private"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[cfg(unix)]
#[test]
fn shared_host_identity_retains_exclusive_custody() {
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let export = dir.path().join("export.bin");
    owner.export(&export).unwrap();
    let copied_identity = dir.path().join("identity-copy");
    copy_tree(&path.join("identity"), &copied_identity);
    let destination = dir.path().join("shared-host");

    assert!(
        SessionOwner::import(
            &destination,
            &export,
            &copied_identity,
            &path.join("host-identity"),
        )
        .is_err()
    );
    assert!(!destination.exists());
}

/// The ordinary host must reopen persisted worker views on their original qOS
/// routes even when the installed application definition has been renamed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_host_restart_restores_bound_local_views() {
    use std::{
        io::Write,
        process::Stdio,
        time::{Duration, Instant},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let ready = dir.path().join("ready.json");
    let before = SessionOwner::initialize(&path).unwrap().status();
    let mut saved_notes: Option<ui::application_view::SavedInstanceView> = None;
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for cycle in 0..2 {
        let mut child = ChildGuard(
            Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
                .arg("run")
                .arg(&path)
                .arg(&ready)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "host {cycle} did not write readiness"
            );
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "host exited during startup"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(&ready).unwrap()).unwrap();
        let origin = metadata["fixture_origin"]
            .as_str()
            .unwrap()
            .strip_prefix("http://")
            .unwrap();
        let observed = loop {
            let mut socket = tokio::net::TcpStream::connect(origin).await.unwrap();
            socket
                .write_all(format!("GET /status HTTP/1.1\r\nHost: {origin}\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut bytes = vec![];
            socket.read_to_end(&mut bytes).await.unwrap();
            let text = String::from_utf8(bytes).unwrap();
            let status: serde_json::Value =
                serde_json::from_str(text.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert!(
                status["action_error"].is_null(),
                "host cycle {cycle} rejected its own view: {status}"
            );
            if status["native_peer"] == "subscribed" && status["actions"] == "ready" {
                break serde_json::from_value::<qos_session_peer::Status>(status).unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "host {cycle} actions never became ready: {status}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(observed.room, before.room);
        assert_eq!(observed.authority, before.authority);
        child
            .0
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"shutdown\n")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "host {cycle} did not close its components cleanly"
                );
                break;
            }
            assert!(Instant::now() < deadline, "host {cycle} did not shut down");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!ready.exists());
        let reopened = SessionOwner::open(&path).unwrap();
        assert_observed_state_survives(&reopened, &observed);
        let notes = fs::read_dir(path.join("actions/views"))
            .unwrap()
            .map(|entry| {
                postcard::from_bytes::<ui::application_view::SavedInstanceView>(
                    &fs::read(entry.unwrap().path()).unwrap(),
                )
                .unwrap()
            })
            .find(|saved| saved.app_id == "dev.qos.notes")
            .expect("ordinary Notes worker must persist its view");
        assert_eq!(notes.workspace_id, before.room);
        assert_eq!(notes.application.as_ref().unwrap().app_id, "dev.qos.notes");
        if let Some(previous) = &saved_notes {
            assert_eq!(notes.device_id, previous.device_id);
            assert_eq!(notes.instance_id, previous.instance_id);
            assert_eq!(notes.record_id, previous.record_id);
            assert_eq!(
                notes.application.as_ref().unwrap().payload,
                previous.application.as_ref().unwrap().payload
            );
        }
        saved_notes = Some(notes);
    }
}

#[test]
fn corrupt_or_missing_state_never_reseeds_identity() {
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    assert!(SessionOwner::open(&path).is_err());
    assert!(!path.exists());
    drop(SessionOwner::initialize(&path).unwrap());
    let root = fs::read(path.join("identity/user-identity/privkey.bin")).unwrap();
    fs::write(path.join("session.bin"), b"interrupted corrupt bytes").unwrap();
    assert!(SessionOwner::open(&path).is_err());
    assert!(SessionOwner::initialize(&path).is_err());
    assert_eq!(
        fs::read(path.join("identity/user-identity/privkey.bin")).unwrap(),
        root
    );
    assert_eq!(
        fs::read(path.join("session.bin")).unwrap(),
        b"interrupted corrupt bytes"
    );
}

#[test]
fn unfinished_temporary_write_keeps_last_committed_state() {
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let owner = SessionOwner::initialize(&path).unwrap();
    let before = owner.status();
    drop(owner);
    fs::write(path.join(".session.bin.999.tmp"), b"partial replacement").unwrap();
    assert_eq!(SessionOwner::open(&path).unwrap().status(), before);
}

#[test]
fn failed_commit_never_returns_a_receipt_and_requires_restore() {
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let before = owner.status();
    fs::rename(path.join("session.bin"), path.join("saved.bin")).unwrap();
    fs::create_dir(path.join("session.bin")).unwrap();
    assert!(
        owner
            .queue_pending("must-not-ack", b"request", false)
            .is_err()
    );
    assert_eq!(owner.status(), before);
    fs::remove_dir(path.join("session.bin")).unwrap();
    fs::rename(path.join("saved.bin"), path.join("session.bin")).unwrap();
    assert!(
        owner.flush().is_err(),
        "a failed durable barrier poisons the current owner"
    );
    drop(owner);
    assert_eq!(SessionOwner::open(&path).unwrap().status(), before);
}

#[test]
fn a_second_process_cannot_take_custody() {
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let owner = SessionOwner::initialize(&path).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
        .arg("status")
        .arg(&path)
        .output()
        .unwrap();
    assert!(!result.status.success());
    drop(owner);
    assert!(
        Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
            .arg("status")
            .arg(&path)
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn checked_export_needs_matching_custody_and_detects_tampering() {
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let export = dir.path().join("export.bin");
    owner.export(&export).unwrap();
    let expected = owner.status();
    assert!(
        SessionOwner::import(
            &dir.path().join("concurrent"),
            &export,
            &path.join("identity"),
            &path.join("host-identity")
        )
        .is_err()
    );
    assert!(!dir.path().join("concurrent").exists());
    drop(owner);
    let restored = SessionOwner::import(
        &dir.path().join("imported"),
        &export,
        &path.join("identity"),
        &path.join("host-identity"),
    )
    .unwrap();
    assert_eq!(restored.status(), expected);
    let mut bytes = fs::read(&export).unwrap();
    let last = bytes.last_mut().unwrap();
    *last ^= 1;
    fs::write(&export, bytes).unwrap();
    assert!(
        SessionOwner::import(
            &dir.path().join("bad"),
            &export,
            &path.join("identity"),
            &path.join("host-identity")
        )
        .is_err()
    );
    assert!(!dir.path().join("bad").exists());
}

#[test]
fn revoked_identity_and_replay_floors_survive_restart() {
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationEntry, RevocationList,
        persist::{self, IdentityPaths},
        sign_device_cert, sign_revocation_entry,
    };
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let root = persist::read_signing_key(&IdentityPaths::under(path.join("identity")).user_privkey)
        .unwrap();
    let now = tilo_relay::clock::now_ns();
    let (_, device) = DeviceIdentity::generate("revoked member", now);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.public_key,
        device_label: "revoked member".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let mut revoked = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: device.public_key,
        revoked_at_ns: now,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut revoked, &root).unwrap();
    let mut list = RevocationList::new();
    list.insert(revoked).unwrap();
    let peer = qos_peer_state::StatePeer::restore(
        &owner.peer().durable().unwrap(),
        owner.peer().room(),
        owner.peer().authority().clone(),
        vec![cert],
        list,
        web_time::SystemTime::now(),
    )
    .unwrap();
    owner
        .commit_admission(peer, owner.directory().to_vec())
        .unwrap();
    let before = owner.status();
    assert!(before.revoked.contains(&device.public_key));
    assert!(!before.replay_floors.is_empty());
    drop(owner);
    assert_eq!(SessionOwner::open(&path).unwrap().status(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_loopback_subscription_reconnect_keeps_same_owner() {
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let before = owner.status();
    let host = qos_session_peer::host::LocalHost::start(&owner)
        .await
        .unwrap();
    let mut first = host.connect(&owner).await.unwrap();
    admit_until_ready(&mut owner, &mut first).await;
    qos_session_peer::host::publish(&owner, &first).unwrap();
    first.into_client().close().await;
    owner.refresh_checkpoint().unwrap();
    let mut second = host.connect(&owner).await.unwrap();
    admit_until_ready(&mut owner, &mut second).await;
    qos_session_peer::host::publish(&owner, &second).unwrap();
    assert_eq!(owner.status().room, before.room);
    assert_eq!(owner.status().records, before.records);
    second.into_client().close().await;
    host.shutdown().await.unwrap();
    let final_status = owner.status();
    drop(owner);
    assert_eq!(SessionOwner::open(&path).unwrap().status(), final_status);
}

#[test]
fn authenticated_encryption_obligation_is_committed_even_without_plaintext_checkpoint() {
    use qos_peer_state::{NeighbourMsg, TransportOrigin};
    use tilo_user_identity::persist::{self, IdentityPaths};
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let key = persist::read_device_key(&IdentityPaths::under(path.join("identity")).device_privkey)
        .unwrap();
    let mut source = owner.peer().clone();
    let messages = source
        .bootstrap_messages(&key, web_time::SystemTime::now())
        .unwrap();
    let plain = messages
        .iter()
        .find(|message| matches!(message, NeighbourMsg::AppDocument { .. }))
        .unwrap()
        .clone();
    let mut encrypted = plain.clone();
    if let NeighbourMsg::AppDocument {
        envelope,
        app_id,
        cluster_id,
        record_id,
        ..
    } = &mut encrypted
    {
        envelope.encryption = Some(collab_types::EncryptionMetadata {
            epoch: 1,
            channel: collab_types::document_channel_id(app_id, cluster_id, record_id),
        });
        envelope.payload = vec![1, 2, 3];
        collab_types::sign_op(envelope, &key).unwrap();
    }
    assert!(
        owner
            .receive(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                encrypted
            )
            .unwrap()
            .is_some()
    );
    assert!(!owner.permits_publication());
    assert!(owner.outgoing().is_empty());
    drop(owner);
    let mut restored = SessionOwner::open(&path).unwrap();
    assert!(!restored.permits_publication());
    assert!(
        restored
            .receive(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                plain
            )
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_polling_does_not_cancel_initial_connection_or_reconnect() {
    use std::io::Write;
    use std::process::Stdio;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let ready = dir.path().join("ready.json");
    drop(SessionOwner::initialize(&path).unwrap());
    let mut child = Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
        .arg("run")
        .arg(&path)
        .arg(&ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A guard kills the isolated test child even on a failed assertion.
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut input = child.stdin.take().unwrap();
    let mut guard = ChildGuard(child);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    while !ready.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "test host never wrote ready metadata"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let metadata: serde_json::Value = serde_json::from_slice(&fs::read(&ready).unwrap()).unwrap();
    let origin = metadata["fixture_origin"]
        .as_str()
        .unwrap()
        .strip_prefix("http://")
        .unwrap();
    let mut last_generation = 1u64;
    let mut last_subscription = 0u64;
    let mut last_status = None;
    for cycle in 0..2 {
        if cycle == 1 {
            input.write_all(b"reconnect\n").unwrap();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut subscribed = false;
        while std::time::Instant::now() < deadline {
            let mut socket = tokio::net::TcpStream::connect(origin).await.unwrap();
            socket
                .write_all(format!("GET /status HTTP/1.1\r\nHost: {origin}\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut bytes = vec![];
            socket.read_to_end(&mut bytes).await.unwrap();
            let text = String::from_utf8(bytes).unwrap();
            let body = text.split_once("\r\n\r\n").unwrap().1;
            let status: serde_json::Value = serde_json::from_str(body).unwrap();
            let generation = status["generation"].as_u64().unwrap();
            let subscription = status["native_subscription_generation"].as_u64().unwrap();
            if status["native_peer"] == "subscribed"
                && generation > last_generation
                && subscription > last_subscription
            {
                last_subscription = subscription;
                last_generation = generation;
                last_status =
                    Some(serde_json::from_value::<qos_session_peer::Status>(status).unwrap());
                subscribed = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            subscribed,
            "frequent health polls must not starve connection cycle {cycle}"
        );
    }
    // Abrupt process death must release custody without losing acknowledged state.
    guard.0.kill().unwrap();
    guard.0.wait().unwrap();
    let restored = SessionOwner::open(&path).unwrap();
    assert_observed_state_survives(&restored, &last_status.unwrap());
}

// A status response acknowledges this cut, but the running owner can commit
// additional worker signatures before SIGKILL. Require causal preservation of
// every acknowledged route/head and author floor, allowing those later commits.
fn assert_observed_state_survives(owner: &SessionOwner, observed: &qos_session_peer::Status) {
    let current = owner.status();
    assert_eq!(current.room, observed.room);
    assert_eq!(current.authority, observed.authority);
    assert!(current.generation >= observed.generation);
    for row in &observed.records {
        let bytes = owner
            .peer()
            .document(&row.app_id, &row.record_id)
            .expect("every acknowledged route survives abrupt death");
        let mut doc = ui::automerge::AutoCommit::load(&bytes).unwrap();
        for head in &row.heads {
            let hash = ui::automerge::ChangeHash(hex::decode(head).unwrap().try_into().unwrap());
            assert!(
                doc.get_change_by_hash(&hash).is_some(),
                "acknowledged head missing after abrupt death"
            );
        }
    }
    for (app, record, author, floor) in &observed.replay_floors {
        let restored_floor = current
            .replay_floors
            .iter()
            .find(|(a, r, k, _)| a == app && r == record && k == author)
            .expect("acknowledged author floor survives abrupt death")
            .3;
        assert!(
            restored_floor >= *floor,
            "acknowledged author floor regressed"
        );
    }
}

#[test]
fn same_namespace_admission_cannot_roll_back_document_history_or_actor_floors() {
    use tilo_user_identity::persist::{self, IdentityPaths};
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let old = owner.peer().clone();
    let key = persist::read_device_key(&IdentityPaths::under(path.join("identity")).device_privkey)
        .unwrap();
    let mut edited = old.clone();
    qos_browser_runtime::fixture_writer_insert(
        &mut edited,
        &key,
        "retained user document",
        web_time::SystemTime::now(),
    )
    .unwrap();
    owner
        .commit_admission(edited, owner.directory().to_vec())
        .unwrap();
    let before = owner.status();
    assert!(
        owner
            .commit_admission(old, owner.directory().to_vec())
            .is_err()
    );
    assert_eq!(owner.status(), before);
}

async fn admit_until_ready(
    owner: &mut SessionOwner,
    session: &mut qos_session_peer::host::NativeSubscription,
) {
    use qos_peer_state::TransportOrigin;
    use tilo_relay::Inbound;
    while !session.is_ready() {
        match session
            .recv()
            .await
            .expect("native connection must produce Ready")
        {
            Inbound::Ready { .. } => {}
            Inbound::Delta { author, data } => {
                owner
                    .receive(
                        TransportOrigin::RelayDelta(author),
                        postcard::from_bytes(&data).unwrap(),
                    )
                    .unwrap();
            }
            Inbound::Presence { author, payload } => {
                owner
                    .receive(
                        TransportOrigin::UntrustedPresence(author),
                        postcard::from_bytes(&payload).unwrap(),
                    )
                    .unwrap();
            }
        }
    }
}

#[test]
fn deferred_snapshot_stays_reachable_until_governed_coverage_then_commits() {
    use qos_peer_state::{NeighbourMsg, TransportOrigin};
    use tilo_user_identity::persist::{self, IdentityPaths};
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let before = owner.status();
    let key = persist::read_device_key(&IdentityPaths::under(path.join("identity")).device_privkey)
        .unwrap();
    let mut source = owner.peer().clone();
    qos_browser_runtime::fixture_writer_insert(
        &mut source,
        &key,
        "queued snapshot",
        web_time::SystemTime::now(),
    )
    .unwrap();
    let messages = source
        .bootstrap_messages(&key, web_time::SystemTime::now())
        .unwrap();
    let snapshot=messages.iter().find(|message|matches!(message,NeighbourMsg::AppDocument{app_id,record_id,..} if app_id=="dev.qos.word-processor" && record_id=="tab:0000000000000001")).unwrap().clone();
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    assert!(
        owner.receive(origin, snapshot).unwrap().is_none(),
        "unadmitted data must not return a commit receipt"
    );
    assert_eq!(
        owner.status(),
        before,
        "no acknowledgement or committed record change yet"
    );
    assert_eq!(
        owner.peer().pending_count(),
        1,
        "deferred candidate remains reachable"
    );
    let governance = messages
        .into_iter()
        .find(|message| matches!(message, NeighbourMsg::SyncResp { .. }))
        .unwrap();
    assert!(owner.receive(origin, governance).unwrap().is_some());
    assert_eq!(owner.peer().pending_count(), 0);
    assert_eq!(owner.peer().rows(), source.rows());
    let committed = owner.status();
    drop(owner);
    assert_eq!(SessionOwner::open(&path).unwrap().status(), committed);
}

#[test]
fn published_durability_receipts_bind_committed_generation_and_survive_failed_barrier() {
    let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let receipt = owner.refresh_checkpoint().unwrap();
    for row in &receipt.records {
        let proof = owner
            .peer()
            .durable_coverage(&row.app_id, &row.record_id)
            .unwrap()
            .expect("native checkpoint must carry barrier-bound durable proof");
        assert_eq!(proof.generation, receipt.generation);
        assert_eq!(proof.coverage.heads, row.heads);
    }
    let outgoing = owner.outgoing().to_vec();
    let before = owner.status();
    fs::rename(path.join("session.bin"), path.join("saved.bin")).unwrap();
    fs::create_dir(path.join("session.bin")).unwrap();
    assert!(owner.refresh_checkpoint().is_err());
    assert!(!owner.permits_publication());
    assert_eq!(owner.outgoing(), outgoing);
    assert_eq!(owner.status(), before);
    fs::remove_dir(path.join("session.bin")).unwrap();
    fs::rename(path.join("saved.bin"), path.join("session.bin")).unwrap();
    drop(owner);
    let owner = SessionOwner::open(&path).unwrap();
    assert_eq!(owner.outgoing(), outgoing);
    for row in receipt.records {
        assert_eq!(
            owner
                .peer()
                .durable_coverage(&row.app_id, &row.record_id)
                .unwrap()
                .unwrap()
                .generation,
            receipt.generation
        );
    }
}

#[test]
fn desktop_bootstrap_is_durable_and_preserves_existing_record_routes() {
    use ui::automerge::{AutoCommit, ROOT, ReadDoc};
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let root = directory.path().join("desktop");
    let owner = qos_session_peer::SessionOwner::initialize(&root).unwrap();
    let before = owner.status();
    assert!(
        before.records.iter().any(|r| r.app_id == "dev.qos.notes"),
        "desktop must include a real initialized Notes record"
    );
    let writer = owner
        .peer()
        .document("dev.qos.word-processor", "tab:0000000000000001")
        .unwrap();
    let chat = owner.peer().document("dev.qos.chat", "").unwrap();
    let message = owner.peer().room_message();
    let qos_peer_state::NeighbourMsg::SyncResp { changes, .. } = message else {
        panic!("room state")
    };
    let mut doc = AutoCommit::new();
    doc.apply_changes(
        changes
            .iter()
            .map(|b| ui::automerge::Change::try_from(b.as_slice()).unwrap()),
    )
    .unwrap();
    assert!(doc.get(ROOT, "contents").unwrap().is_some());
    let heads = doc.get_heads();
    drop(owner);
    for _ in 0..2 {
        let owner = qos_session_peer::SessionOwner::open(&root).unwrap();
        assert_eq!(owner.status().room, before.room);
        assert_eq!(owner.status().authority, before.authority);
        assert_eq!(
            owner.status().generation,
            before.generation,
            "reopen must not reseed topology"
        );
        assert_eq!(
            owner
                .peer()
                .document("dev.qos.word-processor", "tab:0000000000000001")
                .unwrap(),
            writer
        );
        assert_eq!(owner.peer().document("dev.qos.chat", "").unwrap(), chat);
        let qos_peer_state::NeighbourMsg::SyncResp { changes, .. } = owner.peer().room_message()
        else {
            panic!("room state")
        };
        let mut doc = AutoCommit::new();
        doc.apply_changes(
            changes
                .iter()
                .map(|b| ui::automerge::Change::try_from(b.as_slice()).unwrap()),
        )
        .unwrap();
        assert_eq!(doc.get_heads(), heads);
    }
}
