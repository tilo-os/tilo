// This executable integration-test target uses panic/unwrap as assertion
// boundaries while driving child processes and mandatory system facilities.
#![allow(clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::ffi::CString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Barrier, Mutex,
    mpsc::{self, Receiver},
};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use fs::{Namespace, ROOT_INO};
use getrandom::{SysRng, rand_core::UnwrapErr};
use iroh_blobs::Hash;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, adopt_device_key,
    persist::{write_device_cert, write_public_key, write_signing_key},
    sign_device_cert,
};

struct MountProcess {
    child: Child,
    lines: Receiver<String>,
    logs: Arc<Mutex<Vec<String>>>,
    mount: PathBuf,
}

struct ReplicaProcess {
    child: Child,
    logs: Arc<Mutex<Vec<String>>>,
}

impl ReplicaProcess {
    fn kill_and_wait(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

impl Drop for ReplicaProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl MountProcess {
    fn wait_line(&mut self, expected: &str) -> String {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "process exited {status}; logs:\n{}",
                    self.logs.lock().unwrap().join("\n")
                );
            }
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| {
                    panic!(
                        "missing {expected:?}: {error}; logs:\n{}",
                        self.logs.lock().unwrap().join("\n")
                    )
                });
            if line.contains(expected) {
                return line;
            }
        }
    }

    fn wait_ticket(&mut self) -> String {
        loop {
            let line = self.wait_line("");
            if fs::Ticket::decode(line.trim()).is_ok() {
                return line.trim().to_string();
            }
        }
    }

    fn stop(&mut self) {
        let _ = Command::new("fusermount3")
            .args(["-u", "-z"])
            .arg(&self.mount)
            .status();
        tilo_test_env::wait_for("the unmounted daemon to exit", || {
            self.child.try_wait().unwrap().map(|_| ())
        });
    }
}

impl Drop for MountProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn install_identity(
    root: &Path,
    person_seed: [u8; 32],
    device_seed: [u8; 32],
    primary: bool,
) -> ([u8; 32], [u8; 32]) {
    let paths = IdentityPaths::under(root);
    paths.ensure_directories().unwrap();
    let person = SigningKey::from_bytes(&person_seed);
    let device = adopt_device_key(&paths, &device_seed).unwrap();
    write_public_key(&paths.user_pubkey, &person.verifying_key().to_bytes()).unwrap();
    if primary {
        write_signing_key(&paths.user_privkey, &person).unwrap();
    }
    let mut cert = DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "partition-matrix".into(),
        capabilities: if primary {
            DeviceCapabilities::primary()
        } else {
            DeviceCapabilities::member()
        },
        issued_at_ns: tilo_clock::unix_now_ns().saturating_sub(1_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &person).unwrap();
    write_device_cert(&paths.device_cert, &cert).unwrap();
    (
        person.verifying_key().to_bytes(),
        device.verifying_key().to_bytes(),
    )
}

fn capture(child: &mut Child) -> (Receiver<String>, Arc<Mutex<Vec<String>>>) {
    let (tx, rx) = mpsc::channel();
    let logs = Arc::new(Mutex::new(Vec::new()));
    for stream in [
        Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
        Box::new(child.stderr.take().unwrap()) as Box<dyn Read + Send>,
    ] {
        let tx = tx.clone();
        let logs = logs.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                let line = line.unwrap();
                eprintln!("child: {line}");
                if let Ok(mut logs) = logs.lock() {
                    logs.push(line.clone());
                }
                let _ = tx.send(line);
            }
        });
    }
    (rx, logs)
}

// Each path is a distinct externally observable daemon boundary in this
// end-to-end fixture; bundling them would make call-site mixups less visible.
fn spawn_fs(
    binary: &Path,
    identity: &Path,
    mount: &Path,
    cache: &Path,
    gate: &Path,
    acl: &Path,
    mode: &[&str],
    initial_partition: bool,
) -> MountProcess {
    std::fs::create_dir_all(cache).unwrap();
    std::fs::set_permissions(cache, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let authority_sockets = authority_sockets(cache);
    std::fs::create_dir_all(&authority_sockets).unwrap();
    let mut command = Command::new(binary);
    command
        .env(fs::identity::IDENTITY_ROOT_ENV, identity)
        .env("TILO_AUTHORITY_SOCKET_DIR", authority_sockets)
        .env("TILO_FS_FUSE_OWNER_ONLY", "1")
        .env(fs::transport_gate::TRANSPORT_GATE_SOCKET_ENV, gate)
        .args(mode)
        .arg("--mount-point")
        .arg(mount)
        .arg("--cache-dir")
        .arg(cache)
        .arg("--acl")
        .arg(acl)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if initial_partition {
        command.env(
            fs::transport_gate::TRANSPORT_GATE_INITIAL_ENV,
            "partitioned",
        );
    }
    let mut child = command.spawn().unwrap();
    let (lines, logs) = capture(&mut child);
    MountProcess {
        child,
        lines,
        logs,
        mount: mount.into(),
    }
}

/// The authority socket directory of the daemon caching in `cache`: a short
/// sibling, because the socket path (this directory plus
/// `workspace-authority-<uuid>.sock`) must stay under `SUN_LEN` wherever the
/// temporary directory is.
fn authority_sockets(cache: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    cache.hash(&mut hasher);
    cache.with_file_name(format!("s{:04x}", hasher.finish() & 0xffff))
}

fn export_home_peer_bundle(
    binary: &Path,
    identity: &Path,
    workspace_cache: &Path,
    acl: &Path,
    bundle: &Path,
    replica_cache: &Path,
    status: &Path,
) {
    let status = Command::new(binary)
        .env(fs::identity::IDENTITY_ROOT_ENV, identity)
        .args([
            "export-home-peer",
            "--cache-dir",
            workspace_cache.to_str().unwrap(),
            "--acl",
            acl.to_str().unwrap(),
            "--output",
            bundle.to_str().unwrap(),
            "--replica-cache-dir",
            replica_cache.to_str().unwrap(),
            "--status-file",
            status.to_str().unwrap(),
            "--relay-workspace-id",
            "42",
        ])
        .status()
        .unwrap();
    assert!(status.success(), "home-peer bundle export failed: {status}");
}

fn publish_authority(binary: &Path, identity: &Path, workspace_cache: &Path, acl: &Path) {
    let status = Command::new(binary)
        .env(fs::identity::IDENTITY_ROOT_ENV, identity)
        .env(
            "TILO_AUTHORITY_SOCKET_DIR",
            authority_sockets(workspace_cache),
        )
        .args(["publish-authority", "--acl", acl.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "authority publication failed: {status}");
}

fn spawn_replica(binary: &Path, identity: &Path, config: &Path) -> ReplicaProcess {
    let mut child = Command::new(binary)
        .env(fs::identity::IDENTITY_ROOT_ENV, identity)
        .args(["replica", "--config", config.to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (_lines, logs) = capture(&mut child);
    ReplicaProcess { child, logs }
}

fn ready_replica_endpoint(status: &Path, process: &mut ReplicaProcess) -> iroh_base::EndpointAddr {
    let deadline = tilo_test_env::hang_deadline();
    let mut last_status = None;
    loop {
        if let Some(exit) = process.child.try_wait().unwrap() {
            panic!(
                "replica exited {exit}; logs:\n{}",
                process.logs.lock().unwrap().join("\n")
            );
        }
        let current_process_joined = process
            .logs
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("Home peer joined workspace"));
        if let Some(current) = fs::full_replica::load_status(status).ok().flatten() {
            // A status file deliberately survives daemon restarts for
            // diagnostics. Do not mistake the previous process's last ready
            // snapshot for this process becoming reachable.
            if current_process_joined
                && current.full_content_replication
                && let Some(endpoint) = current.filesystem_endpoints.first()
            {
                return endpoint.clone();
            }
            last_status = Some(current);
        }
        assert!(
            Instant::now() < deadline,
            "replica readiness timed out; last status: {last_status:#?}; logs:\n{}",
            process.logs.lock().unwrap().join("\n")
        );
        std::thread::park_timeout(Duration::from_millis(10));
    }
}

fn gate(socket: &Path, command: &str) -> String {
    eprintln!("barrier: gate {} {command}", socket.display());
    tilo_test_env::wait_for("the barrier socket", || UnixStream::connect(socket).ok());
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.write_all(format!("{command}\n").as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn real_fuse_available(scenario: &str) -> bool {
    let available = std::fs::metadata("/dev/fuse")
        .is_ok_and(|fuse| std::os::unix::fs::FileTypeExt::is_char_device(&fuse.file_type()))
        && Command::new("fusermount3")
            .arg("--version")
            .status()
            .is_ok_and(|status| status.success());
    if available {
        return true;
    }
    assert_ne!(
        std::env::var("TILO_FS_FUSE_REQUIRED").as_deref(),
        Ok("1"),
        "mandatory real-FUSE scenario {scenario} has no usable /dev/fuse"
    );
    eprintln!("skipping {scenario}: real FUSE is covered by the dedicated mandatory CI job");
    false
}

fn wait_for_process<T>(process: &mut MountProcess, mut inspect: impl FnMut() -> Option<T>) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(value) = inspect() {
            return value;
        }
        if let Some(status) = process.child.try_wait().unwrap() {
            panic!(
                "process exited {status}; logs:\n{}",
                process.logs.lock().unwrap().join("\n")
            );
        }
        assert!(
            Instant::now() < deadline,
            "process state barrier timed out; logs:\n{}",
            process.logs.lock().unwrap().join("\n")
        );
        std::thread::park_timeout(Duration::from_millis(10));
    }
}

fn get_xattr(path: &Path, name: &str) -> std::io::Result<Vec<u8>> {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = CString::new(name).unwrap();
    let size = unsafe { libc::getxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut bytes = vec![0; size.cast_unsigned()];
    let read = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if read < 0 {
        return Err(std::io::Error::last_os_error());
    }
    bytes.truncate(read.cast_unsigned());
    Ok(bytes)
}

fn hash(byte: u8) -> Hash {
    Hash::from([byte; 32])
}

type StructuralProjection = Vec<(
    fs_protocol::FileRecordId,
    Option<(fs_protocol::FileRecordId, Vec<u8>)>,
)>;

fn structural_projection(
    namespace: &mut Namespace,
    records: &[fs_protocol::FileRecordId],
) -> StructuralProjection {
    let actor = fs_protocol::FileActor {
        person: collab_types::Ed25519PublicKey([0x91; 32]),
        device: collab_types::Ed25519PublicKey([0x92; 32]),
    };
    let trashed = namespace
        .directly_trashed()
        .into_iter()
        .filter_map(|id| namespace.record_id_for_id(&id))
        .collect::<BTreeSet<_>>();
    // Reopened snapshots assign process-local inodes lazily while walking the
    // live tree. Warm that map before resolving stable record IDs.
    let mut pending = vec![ROOT_INO];
    let mut visited = BTreeSet::new();
    while let Some(parent) = pending.pop() {
        if !visited.insert(parent) {
            continue;
        }
        if let Some(entries) = namespace.readdir(parent) {
            pending.extend(entries.into_iter().map(|(ino, _, _)| ino));
        }
        assert!(
            visited.len() <= 1_024,
            "bounded structural fixture traversal"
        );
    }
    records
        .iter()
        .copied()
        .map(|record| {
            let live = namespace.ino_for_record_id(record).and_then(|ino| {
                namespace
                    .signed_binding_for(ino, actor, 1)
                    .and_then(|binding| Some((binding.parent?, binding.name.as_bytes().to_vec())))
            });
            assert_ne!(live.is_some(), trashed.contains(&record));
            (record, live)
        })
        .collect()
}

fn converge_structural_partition(
    left: &[u8],
    right: &[u8],
    records: &[fs_protocol::FileRecordId],
) -> (Namespace, Namespace) {
    let mut left_peer = Namespace::from_bytes("structural-left-restart", left).unwrap();
    let mut right_peer = Namespace::from_bytes("structural-right-restart", right).unwrap();
    let mut right_wire = Namespace::from_bytes("right-wire", right).unwrap();
    let mut left_wire = Namespace::from_bytes("left-wire", left).unwrap();
    left_peer.merge_doc(&mut right_wire).unwrap();
    right_peer.merge_doc(&mut left_wire).unwrap();
    left_peer.resolve();
    right_peer.resolve();

    // Exchange deterministic repair changes as an explicit second barrier.
    let left_repair = left_peer.save();
    let right_repair = right_peer.save();
    let mut right_repair_wire = Namespace::from_bytes("right-repair-wire", &right_repair).unwrap();
    let mut left_repair_wire = Namespace::from_bytes("left-repair-wire", &left_repair).unwrap();
    left_peer.merge_doc(&mut right_repair_wire).unwrap();
    right_peer.merge_doc(&mut left_repair_wire).unwrap();
    left_peer.resolve();
    right_peer.resolve();
    assert_eq!(
        structural_projection(&mut left_peer, records),
        structural_projection(&mut right_peer, records)
    );

    // A durable reopen and exact replay cannot change the chosen structure.
    let expected = structural_projection(&mut left_peer, records);
    let mut left_restarted =
        Namespace::from_bytes("left-final-restart", &left_peer.save()).unwrap();
    let mut right_restarted =
        Namespace::from_bytes("right-final-restart", &right_peer.save()).unwrap();
    let mut replay = Namespace::from_bytes("replay", &right_repair).unwrap();
    left_restarted.merge_doc(&mut replay).unwrap();
    left_restarted.resolve();
    assert_eq!(
        structural_projection(&mut left_restarted, records),
        expected
    );
    assert_eq!(
        structural_projection(&mut right_restarted, records),
        expected
    );
    (left_restarted, right_restarted)
}

/// Mandatory deterministic coverage for structural conflicts representable by
/// the namespace CRDT: divergent rename/move, delete versus rename, and a
/// POSIX replacement racing a victim move. Stable record identity is never
/// synthesized from a path, every source is either live exactly once or
/// directly trashed, and both merge orders survive restart/replay identically.
#[test]
fn concurrent_structural_conflicts_preserve_sources_and_converge_after_restart() {
    // One source concurrently moves to another directory and renames in place.
    let mut seed = Namespace::new("structural-seed-move").unwrap();
    let left_dir = seed.create_dir(ROOT_INO, "left", 1).unwrap();
    let _right_dir = seed.create_dir(ROOT_INO, "right", 1).unwrap();
    let source = seed
        .create_file(left_dir, "source.txt", Some(&hash(1)), 1, 1)
        .unwrap();
    let source_record = seed.record_id_for(source).unwrap();
    let baseline = seed.save();
    let mut left = Namespace::from_bytes("move-left", &baseline).unwrap();
    let mut right = Namespace::from_bytes("move-right", &baseline).unwrap();
    let left_parent = left.lookup(ROOT_INO, "left").unwrap().ino.0;
    let left_destination = left.lookup(ROOT_INO, "right").unwrap().ino.0;
    let right_parent = right.lookup(ROOT_INO, "left").unwrap().ino.0;
    assert!(left.rename(left_parent, "source.txt", left_destination, "moved.txt", 2));
    assert!(right.rename(right_parent, "source.txt", right_parent, "renamed.txt", 2));
    let (mut moved_left, mut moved_right) =
        converge_structural_partition(&left.save(), &right.save(), &[source_record]);
    assert!(moved_left.ino_for_record_id(source_record).is_some());
    assert_eq!(
        structural_projection(&mut moved_left, &[source_record]),
        structural_projection(&mut moved_right, &[source_record])
    );

    // Delete wins over a concurrent path edit without erasing the stable node
    // or its recorded deletion source hash.
    let mut seed = Namespace::new("structural-seed-delete").unwrap();
    let source = seed
        .create_file(ROOT_INO, "delete-me.txt", Some(&hash(2)), 1, 1)
        .unwrap();
    let source_record = seed.record_id_for(source).unwrap();
    let baseline = seed.save();
    let mut left = Namespace::from_bytes("delete-left", &baseline).unwrap();
    let mut right = Namespace::from_bytes("delete-right", &baseline).unwrap();
    let left_source = left.lookup(ROOT_INO, "delete-me.txt").unwrap().ino.0;
    assert!(left.remove(left_source));
    assert!(right.rename(ROOT_INO, "delete-me.txt", ROOT_INO, "renamed.txt", 2));
    let (mut deleted_left, mut deleted_right) =
        converge_structural_partition(&left.save(), &right.save(), &[source_record]);
    assert!(deleted_left.ino_for_record_id(source_record).is_none());
    let deleted_id = deleted_left
        .directly_trashed()
        .into_iter()
        .find(|id| deleted_left.record_id_for_id(id) == Some(source_record))
        .unwrap();
    assert_eq!(deleted_left.deleted_hash(&deleted_id), Some(hash(2)));
    assert_eq!(
        structural_projection(&mut deleted_left, &[source_record]),
        structural_projection(&mut deleted_right, &[source_record])
    );

    // A replacing rename racing a move of the victim preserves both record
    // identities. The conflict may choose either live placement, but neither
    // source can disappear or be duplicated and both peers choose identically.
    let mut seed = Namespace::new("structural-seed-replace").unwrap();
    let _archive = seed.create_dir(ROOT_INO, "archive", 1).unwrap();
    let source = seed
        .create_file(ROOT_INO, "source.txt", Some(&hash(3)), 1, 1)
        .unwrap();
    let victim = seed
        .create_file(ROOT_INO, "target.txt", Some(&hash(4)), 1, 1)
        .unwrap();
    let source_record = seed.record_id_for(source).unwrap();
    let victim_record = seed.record_id_for(victim).unwrap();
    let baseline = seed.save();
    let mut left = Namespace::from_bytes("replace-left", &baseline).unwrap();
    let mut right = Namespace::from_bytes("replace-right", &baseline).unwrap();
    let right_archive = right.lookup(ROOT_INO, "archive").unwrap().ino.0;
    assert!(left.rename(ROOT_INO, "source.txt", ROOT_INO, "target.txt", 2));
    assert!(right.rename(ROOT_INO, "target.txt", right_archive, "saved.txt", 2));
    let records = [source_record, victim_record];
    let (mut replaced_left, mut replaced_right) =
        converge_structural_partition(&left.save(), &right.save(), &records);
    let projection = structural_projection(&mut replaced_left, &records);
    assert_eq!(
        projection,
        structural_projection(&mut replaced_right, &records)
    );
    assert!(projection.iter().all(|(_, state)| {
        state.is_some()
            || replaced_left.directly_trashed().iter().any(|id| {
                replaced_left.record_id_for_id(id) == Some(source_record)
                    || replaced_left.record_id_for_id(id) == Some(victim_record)
            })
    }));
}

/// Deterministic release evidence for the partition boundary. The two peers
/// cannot exchange state until the explicit merge barrier below; each offline
/// edit is atomically persisted and reopened first. No timeout or sleep can
/// turn a missing event into a pass.
#[test]
fn forced_partition_offline_edits_restart_and_converge_as_a_conflict() {
    let temp = tempfile::tempdir().unwrap();
    let mut seed = Namespace::new("seed").unwrap();
    seed.create_file(ROOT_INO, "offline.txt", Some(&hash(1)), 1, 1)
        .unwrap();
    let baseline = seed.save();
    let mut left = Namespace::from_bytes("left", &baseline).unwrap();
    let mut right = Namespace::from_bytes("right", &baseline).unwrap();
    let left_ino = left.lookup(ROOT_INO, "offline.txt").unwrap().ino.0;
    let right_ino = right.lookup(ROOT_INO, "offline.txt").unwrap().ino.0;
    assert!(left.set_content(left_ino, Some(&hash(10)), 10, 10));
    assert!(right.set_content(right_ino, Some(&hash(20)), 20, 20));

    let left_path = temp.path().join("left.automerge");
    let right_path = temp.path().join("right.automerge");
    tilo_files::native::atomic::write(&left_path, &left.save()).unwrap();
    tilo_files::native::atomic::write(&right_path, &right.save()).unwrap();
    drop(left);
    drop(right);

    let mut left =
        Namespace::from_bytes("left-restart", &std::fs::read(left_path).unwrap()).unwrap();
    let mut right =
        Namespace::from_bytes("right-restart", &std::fs::read(right_path).unwrap()).unwrap();
    assert_eq!(left.entry_at("offline.txt").unwrap().hash, Some(hash(10)));
    assert_eq!(right.entry_at("offline.txt").unwrap().hash, Some(hash(20)));

    // Explicitly heal the partition in both orders.
    let mut right_for_left = right.clone();
    let mut left_for_right = left.clone();
    left.merge_doc(&mut right_for_left).unwrap();
    right.merge_doc(&mut left_for_right).unwrap();
    for namespace in [&mut left, &mut right] {
        let conflicts = fs::conflict::detect(namespace);
        assert_eq!(conflicts.len(), 1);
        assert!(!fs::conflict::materialize(namespace, &conflicts[0], 20, 21).is_empty());
        assert!(fs::conflict::detect(namespace).is_empty());
    }

    let names = |namespace: &mut Namespace| {
        namespace
            .readdir(ROOT_INO)
            .unwrap()
            .into_iter()
            .map(|(_, _, name)| name.display_lossy())
            .collect::<BTreeSet<_>>()
    };
    let left_names = names(&mut left);
    let right_names = names(&mut right);
    assert_eq!(left_names, right_names);
    assert!(left_names.contains("offline.txt"));
    assert!(
        left_names
            .iter()
            .any(|name| name.contains(".sync-conflict-"))
    );
}

// This mandatory release scenario intentionally keeps every process barrier,
// restart, ACL check, and convergence assertion visible in one linear trace.
#[allow(
    clippy::too_many_lines,
    reason = "the release scenario keeps every barrier, restart and assertion in one linear trace"
)]
#[test]
fn shipped_daemons_partition_restart_and_converge_through_real_fuse() {
    if !real_fuse_available("partition/restart convergence") {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let host_identity = temp.path().join("host-identity");
    let peer_identity = temp.path().join("peer-identity");
    let reader_identity = temp.path().join("reader-identity");
    // The daemons dial each other by endpoint id through the public relay.
    // Fixed keys would let a concurrent run of this scenario answer as one of
    // these peers; fresh keys keep every run's peers its own.
    let person_seed = rand::random();
    let (person, _host_device) =
        install_identity(&host_identity, person_seed, rand::random(), true);
    let (_, _peer_device) = install_identity(&peer_identity, person_seed, rand::random(), false);
    let (reader, _reader_device) =
        install_identity(&reader_identity, rand::random(), rand::random(), false);
    let acl = temp.path().join("acl.json");
    let caps = vec![
        collab_types::CapOverride::new(
            collab_types::Subject::User(collab_types::Ed25519PublicKey(person)),
            collab_types::Scope::FsPath("/".into()),
            collab_types::Effect::Grant,
            collab_types::caps::Access::Read,
        ),
        collab_types::CapOverride::new(
            collab_types::Subject::User(collab_types::Ed25519PublicKey(person)),
            collab_types::Scope::FsPath("/".into()),
            collab_types::Effect::Grant,
            collab_types::caps::Access::Write,
        ),
        collab_types::CapOverride::new(
            collab_types::Subject::User(collab_types::Ed25519PublicKey(reader)),
            collab_types::Scope::FsPath("/".into()),
            collab_types::Effect::Grant,
            collab_types::caps::Access::Read,
        ),
    ];
    std::fs::write(
        &acl,
        serde_json::to_vec(&collab_types::FsAuthority::from(caps)).unwrap(),
    )
    .unwrap();

    let host_mount = temp.path().join("host-mount");
    let peer_mount = temp.path().join("peer-mount");
    let reader_mount = temp.path().join("reader-mount");
    for path in [&host_mount, &peer_mount, &reader_mount] {
        std::fs::create_dir(path).unwrap();
    }
    let host_cache = temp.path().join("host-cache");
    let peer_cache = temp.path().join("peer-cache");
    let reader_cache = temp.path().join("reader-cache");
    let host_gate = host_cache.join("transport-gate.sock");
    let peer_gate = peer_cache.join("transport-gate.sock");
    let reader_gate = reader_cache.join("transport-gate.sock");
    let binary = Path::new(env!("CARGO_BIN_EXE_fs"));
    let seed = temp.path().join("seed");
    std::fs::create_dir(&seed).unwrap();
    std::fs::write(seed.join("seeded.txt"), b"signed-before-ticket").unwrap();
    let host_mode = ["serve", "--seed", seed.to_str().unwrap()];

    let mut host = spawn_fs(
        binary,
        &host_identity,
        &host_mount,
        &host_cache,
        &host_gate,
        &acl,
        &host_mode,
        false,
    );
    host.wait_line("Hosting workspace");
    let ticket = host.wait_ticket();
    host.wait_line("Mounting QuickFS");
    eprintln!("barrier: host empty mount ready");

    let mut peer = spawn_fs(
        binary,
        &peer_identity,
        &peer_mount,
        &peer_cache,
        &peer_gate,
        &acl,
        &["mount", "--ticket", &ticket],
        false,
    );
    peer.wait_line("Joined workspace");
    peer.wait_line("Mounting QuickFS");
    wait_for_process(&mut peer, || {
        (std::fs::read(peer_mount.join("seeded.txt"))
            .ok()?
            .as_slice()
            == b"signed-before-ticket")
            .then_some(())
    });
    std::fs::write(host_mount.join("contended.txt"), b"baseline").unwrap();
    eprintln!("barrier: peer baseline visible");
    tilo_test_env::wait_for("the baseline on the peer", || {
        peer_mount.join("contended.txt").exists().then_some(())
    });
    wait_for_process(&mut peer, || {
        (std::fs::read(peer_mount.join("contended.txt"))
            .ok()?
            .as_slice()
            == b"baseline")
            .then_some(())
    });

    assert_eq!(gate(&host_gate, "partition"), "partitioned\n");
    assert_eq!(gate(&peer_gate, "partition"), "partitioned\n");
    assert_eq!(gate(&host_gate, "status"), "partitioned\n");
    assert_eq!(gate(&peer_gate, "status"), "partitioned\n");

    let barrier = Arc::new(Barrier::new(3));
    let left_path = host_mount.join("contended.txt");
    let right_path = peer_mount.join("contended.txt");
    let left = {
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            std::fs::write(left_path, b"left-offline").unwrap();
        })
    };
    let right = {
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            std::fs::write(right_path, b"right-offline").unwrap();
        })
    };
    barrier.wait();
    left.join().unwrap();
    right.join().unwrap();
    assert_eq!(
        std::fs::read(host_mount.join("contended.txt")).unwrap(),
        b"left-offline"
    );
    wait_for_process(&mut peer, || {
        (std::fs::read(peer_mount.join("contended.txt"))
            .ok()?
            .as_slice()
            == b"right-offline")
            .then_some(())
    });
    assert_eq!(
        std::fs::read_dir(&host_mount).unwrap().count(),
        2,
        "no half conflict publication on host"
    );
    assert_eq!(
        std::fs::read_dir(&peer_mount).unwrap().count(),
        2,
        "no half conflict publication on peer"
    );

    peer.stop();
    let mut peer = spawn_fs(
        binary,
        &peer_identity,
        &peer_mount,
        &peer_cache,
        &peer_gate,
        &acl,
        &["mount", "--ticket", &ticket],
        true,
    );
    peer.wait_line("starting from persisted workspace");
    peer.wait_line("Mounting QuickFS");
    assert_eq!(gate(&peer_gate, "status"), "partitioned\n");
    wait_for_process(&mut peer, || {
        (std::fs::read(peer_mount.join("contended.txt"))
            .ok()?
            .as_slice()
            == b"right-offline")
            .then_some(())
    });

    assert_eq!(gate(&host_gate, "reconnect"), "connected\n");
    assert_eq!(gate(&peer_gate, "reconnect"), "connected\n");
    let converged = |mount: &Path| -> Option<(Vec<u8>, fs_protocol::HistoryProjectionV1)> {
        let entries = std::fs::read_dir(mount)
            .ok()?
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        if entries.len() != 2 {
            return None;
        }
        let path = entries
            .iter()
            .find(|entry| entry.file_name() == "contended.txt")?
            .path();
        let content = std::fs::read(&path).ok()?;
        let history = fs_protocol::HistoryProjectionV1::parse_xattr(
            &get_xattr(&path, fs_protocol::HISTORY_XATTR).ok()?,
        )
        .ok()?;
        let conflict = history.conflicts.iter().find(|conflict| {
            conflict.status == fs_protocol::ConflictStatus::Open && conflict.versions.len() == 2
        })?;
        let mut heads = history.heads.clone();
        heads.sort();
        let mut conflict_versions = conflict.versions.clone();
        conflict_versions.sort();
        (heads == conflict_versions).then_some((content, history))
    };
    // A device shows the conflict as soon as it holds both heads, and the
    // winner once it has projected them; both converge on the same one.
    let ((host_content, host_history), (peer_content, peer_history)) =
        tilo_test_env::wait_for("both mounts to converge", || {
            let host = converged(&host_mount)?;
            let peer = converged(&peer_mount)?;
            (host == peer).then_some((host, peer))
        });
    eprintln!("barrier: host and peer converged");
    assert_eq!(
        host_content, peer_content,
        "the same signed head must win live"
    );
    assert!(
        [b"left-offline".as_slice(), b"right-offline".as_slice()]
            .contains(&host_content.as_slice())
    );
    assert_eq!(host_history, peer_history);
    let head_versions = host_history
        .versions
        .iter()
        .filter(|version| host_history.heads.contains(&version.id))
        .collect::<Vec<_>>();
    assert_eq!(head_versions.len(), 2);
    assert!(
        head_versions
            .iter()
            .all(|version| version.actor.person.as_bytes() == &person)
    );
    assert_eq!(
        head_versions
            .iter()
            .map(|version| *version.actor.device.as_bytes())
            .collect::<BTreeSet<_>>()
            .len(),
        2,
        "both exact certified authoring devices remain attributed"
    );

    let mut reader_process = spawn_fs(
        binary,
        &reader_identity,
        &reader_mount,
        &reader_cache,
        &reader_gate,
        &acl,
        &["mount", "--ticket", &ticket],
        false,
    );
    reader_process.wait_line("Joined workspace");
    reader_process.wait_line("Mounting QuickFS");
    eprintln!("barrier: reader entry visible");
    let reader_file = tilo_test_env::wait_for("the reader entry", || {
        std::fs::read_dir(&reader_mount)
            .ok()?
            .next()?
            .ok()
            .map(|entry| entry.path())
    });
    assert_eq!(
        std::fs::write(reader_file, b"forbidden")
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EROFS)
    );

    reader_process.stop();
    peer.stop();
    host.stop();
}

/// Real owner/NAS/collaborator process boundary. The NAS starts from the
/// exported production bundle, receives an update and deletion, then remains
/// the only reachable source across both owner loss and its own restart.
macro_rules! run_nas_replica_lifecycle_scenario {
    () => {{
        if !real_fuse_available("owner/NAS/collaborator lifecycle") {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let owner_identity = temp.path().join("owner");
        let nas_identity = temp.path().join("nas");
        let reader_identity = temp.path().join("reader");
        let offline_reader_identity = temp.path().join("offline-reader");
        // Every peer is reachable by endpoint id through the public relay, so
        // fixed keys would let a concurrent run's owner answer the restarted
        // NAS's bootstrap dial and hand it that run's namespace. Fresh keys
        // keep the owner provably offline once it is stopped.
        let owner_person_seed = rand::random();
        let (owner, _) = install_identity(&owner_identity, owner_person_seed, rand::random(), true);
        let (_, _) = install_identity(&nas_identity, owner_person_seed, rand::random(), false);
        let (reader, _) = install_identity(&reader_identity, rand::random(), rand::random(), false);
        let (offline_reader, _) = install_identity(
            &offline_reader_identity,
            rand::random(),
            rand::random(),
            false,
        );
        let acl = temp.path().join("acl.json");
        let caps = vec![
            collab_types::CapOverride::new(
                collab_types::Subject::User(collab_types::Ed25519PublicKey(owner)),
                collab_types::Scope::FsPath("/".into()),
                collab_types::Effect::Grant,
                collab_types::caps::Access::Read,
            ),
            collab_types::CapOverride::new(
                collab_types::Subject::User(collab_types::Ed25519PublicKey(owner)),
                collab_types::Scope::FsPath("/".into()),
                collab_types::Effect::Grant,
                collab_types::caps::Access::Write,
            ),
            collab_types::CapOverride::new(
                collab_types::Subject::User(collab_types::Ed25519PublicKey(reader)),
                collab_types::Scope::FsPath("/".into()),
                collab_types::Effect::Grant,
                collab_types::caps::Access::Read,
            ),
            collab_types::CapOverride::new(
                collab_types::Subject::User(collab_types::Ed25519PublicKey(offline_reader)),
                collab_types::Scope::FsPath("/".into()),
                collab_types::Effect::Grant,
                collab_types::caps::Access::Read,
            ),
        ];
        std::fs::write(
            &acl,
            serde_json::to_vec(&collab_types::FsAuthority::from(caps.clone())).unwrap(),
        )
        .unwrap();
        let binary = Path::new(env!("CARGO_BIN_EXE_fs"));
        let owner_mount = temp.path().join("owner-mount");
        std::fs::create_dir(&owner_mount).unwrap();
        let owner_cache = temp.path().join("owner-cache");
        let owner_gate = owner_cache.join("gate.sock");
        let seed = temp.path().join("seed");
        std::fs::create_dir(&seed).unwrap();
        std::fs::write(seed.join("survivor.txt"), b"before-nas").unwrap();
        std::fs::write(seed.join("deleted.txt"), b"delete-me").unwrap();
        let mut owner_process = spawn_fs(
            binary,
            &owner_identity,
            &owner_mount,
            &owner_cache,
            &owner_gate,
            &acl,
            &["serve", "--seed", seed.to_str().unwrap()],
            false,
        );
        owner_process.wait_line("Hosting workspace");
        let owner_ticket = owner_process.wait_ticket();
        owner_process.wait_line("Mounting QuickFS");
        let deleted_manifest_bytes: [u8; 32] = wait_for_process(&mut owner_process, || {
            get_xattr(
                &owner_mount.join("deleted.txt"),
                fs_protocol::CONTENT_HASH_XATTR,
            )
            .ok()
        })
        .try_into()
        .unwrap();
        let deleted_manifest = Hash::from(deleted_manifest_bytes);
        let original_survivor_manifest: [u8; 32] = get_xattr(
            &owner_mount.join("survivor.txt"),
            fs_protocol::CONTENT_HASH_XATTR,
        )
        .unwrap()
        .try_into()
        .unwrap();

        let bundle = temp.path().join("home-peer.toml");
        let replica_cache = temp.path().join("nas-cache");
        let status = temp.path().join("nas-status.json");
        export_home_peer_bundle(
            binary,
            &owner_identity,
            &owner_cache,
            &acl,
            &bundle,
            &replica_cache,
            &status,
        );
        let mut nas = spawn_replica(binary, &nas_identity, &bundle);
        let nas_endpoint = ready_replica_endpoint(&status, &mut nas);

        // Update/delete after the NAS is ready. Its next coalesced generation
        // must be complete before the owner is removed.
        std::fs::write(owner_mount.join("survivor.txt"), b"updated-through-nas").unwrap();
        std::fs::remove_file(owner_mount.join("deleted.txt")).unwrap();

        let original = fs::Ticket::decode(&owner_ticket).unwrap();
        let updated_manifest: [u8; 32] = wait_for_process(&mut owner_process, || {
            let manifest: [u8; 32] = get_xattr(
                &owner_mount.join("survivor.txt"),
                fs_protocol::CONTENT_HASH_XATTR,
            )
            .ok()?
            .try_into()
            .ok()?;
            (manifest != original_survivor_manifest).then_some(manifest)
        });
        let updated_manifest = Hash::from(updated_manifest);
        let replica_workspace_cache = replica_cache.join(original.workspace_id().to_string());
        // The replica advertises a complete generation that is exactly the
        // namespace it holds durably, and that namespace has the change. The
        // namespace may move on after the change first lands (the records
        // that go with it are projected after it); the proof is for the
        // generation the replica advertises.
        tilo_test_env::wait_for("the replica to hold the change durably", || {
            if let Some(exit) = nas.child.try_wait().unwrap() {
                panic!(
                    "replica exited {exit}; logs:\n{}",
                    nas.logs.lock().unwrap().join("\n")
                );
            }
            let current = fs::full_replica::load_status(&status).ok().flatten()?;
            let bytes = std::fs::read(replica_workspace_cache.join("namespace.bin")).ok()?;
            let namespace = Namespace::from_bytes("nas-generation-barrier", &bytes).ok()?;
            (current.full_content_replication
                && current.namespace_generation.as_deref()
                    == Some(hex::encode(blake3::hash(&bytes).as_bytes()).as_str())
                && namespace.entry_at("survivor.txt")?.hash == Some(updated_manifest)
                && namespace.entry_at("deleted.txt").is_none())
            .then_some(())
        });
        let nas_ticket = fs::Ticket {
            node: nas_endpoint,
            workspace: original.workspace,
        }
        .encode()
        .unwrap();
        let reader_mount = temp.path().join("reader-mount");
        std::fs::create_dir(&reader_mount).unwrap();
        let reader_cache = temp.path().join("reader-cache");
        let reader_gate = reader_cache.join("gate.sock");
        let mut reader_process = spawn_fs(
            binary,
            &reader_identity,
            &reader_mount,
            &reader_cache,
            &reader_gate,
            &acl,
            &["mount", "--ticket", &nas_ticket],
            false,
        );
        reader_process.wait_line("Joined workspace");
        reader_process.wait_line("Mounting QuickFS");
        let reader_file = reader_mount.join("survivor.txt");
        let reader_manifest: [u8; 32] = wait_for_process(&mut reader_process, || {
            get_xattr(&reader_file, fs_protocol::CONTENT_HASH_XATTR)
                .ok()?
                .try_into()
                .ok()
        });
        assert_eq!(Hash::from(reader_manifest), updated_manifest);
        assert_eq!(std::fs::metadata(&reader_file).unwrap().len(), 19);
        let reader_bytes = std::fs::read(&reader_file).unwrap_or_else(|error| {
            panic!(
                "collaborator could not hydrate NAS content: {error}; NAS logs:\n{}",
                nas.logs.lock().unwrap().join("\n")
            )
        });
        assert_eq!(
            reader_bytes,
            b"updated-through-nas",
            "collaborator observed wrong NAS bytes; NAS logs:\n{}",
            nas.logs.lock().unwrap().join("\n")
        );
        assert!(!reader_mount.join("deleted.txt").exists());
        reader_process.stop();

        let revoked_acl = temp.path().join("revoked-acl.json");
        let revoked_caps = vec![
            collab_types::CapOverride::new(
                collab_types::Subject::User(collab_types::Ed25519PublicKey(owner)),
                collab_types::Scope::FsPath("/".into()),
                collab_types::Effect::Grant,
                collab_types::caps::Access::Read,
            ),
            collab_types::CapOverride::new(
                collab_types::Subject::User(collab_types::Ed25519PublicKey(owner)),
                collab_types::Scope::FsPath("/".into()),
                collab_types::Effect::Grant,
                collab_types::caps::Access::Write,
            ),
            collab_types::CapOverride::new(
                collab_types::Subject::User(collab_types::Ed25519PublicKey(offline_reader)),
                collab_types::Scope::FsPath("/".into()),
                collab_types::Effect::Grant,
                collab_types::caps::Access::Read,
            ),
        ];
        std::fs::write(
            &revoked_acl,
            serde_json::to_vec(&collab_types::FsAuthority::from(revoked_caps.clone())).unwrap(),
        )
        .unwrap();
        publish_authority(binary, &owner_identity, &owner_cache, &revoked_acl);
        let replica_authority = replica_cache
            .join(original.workspace_id().to_string())
            .join("workspace-authority.bin");
        tilo_test_env::wait_for("the replica workspace authority", || {
            let authority = fs::workspace_authority::WorkspaceAuthority::open(
                &replica_authority,
                original.workspace_id(),
                owner,
                owner,
                Vec::new(),
            )
            .ok()?;
            (!authority
                .handle()
                .permits_peer(&reader, "/survivor.txt", fs::Access::Read))
            .then_some(())
        });
        let revoked_mount = temp.path().join("revoked-reader-mount");
        std::fs::create_dir(&revoked_mount).unwrap();
        let revoked_cache = temp.path().join("revoked-reader-cache");
        let revoked_gate = revoked_cache.join("gate.sock");
        let mut revoked = spawn_fs(
            binary,
            &reader_identity,
            &revoked_mount,
            &revoked_cache,
            &revoked_gate,
            &acl,
            &["mount", "--ticket", &nas_ticket],
            false,
        );
        let revoked_status = tilo_test_env::wait_for("the revoked replica to exit", || {
            revoked.child.try_wait().unwrap()
        });
        assert!(
            !revoked_status.success(),
            "a newer signed authority revision revokes the previously granted collaborator"
        );
        owner_process.stop();

        // Crash/restart the NAS while the owner remains offline. The second fresh
        // collaborator reads the same retained namespace and bytes from its
        // durable cache, proving startup does not require the owner endpoint.
        nas.kill_and_wait();
        let mut nas = spawn_replica(binary, &nas_identity, &bundle);
        let nas_endpoint = ready_replica_endpoint(&status, &mut nas);
        let restart_ticket = fs::Ticket {
            node: nas_endpoint,
            workspace: original.workspace,
        }
        .encode()
        .unwrap();
        let restart_mount = temp.path().join("restart-reader-mount");
        std::fs::create_dir(&restart_mount).unwrap();
        let restart_cache = temp.path().join("restart-reader-cache");
        let restart_gate = restart_cache.join("gate.sock");
        let mut restarted = spawn_fs(
            binary,
            &offline_reader_identity,
            &restart_mount,
            &restart_cache,
            &restart_gate,
            &acl,
            &["mount", "--ticket", &restart_ticket],
            false,
        );
        restarted.wait_line("Joined workspace");
        restarted.wait_line("Mounting QuickFS");
        wait_for_process(&mut restarted, || {
            (std::fs::read(restart_mount.join("survivor.txt"))
                .ok()?
                .as_slice()
                == b"updated-through-nas")
                .then_some(())
        });
        restarted.stop();

        nas.kill_and_wait();
        // Inspect the stopped daemon's real durable store: removal from the
        // namespace is insufficient if obsolete manifests/chunks still consume a
        // home peer's bounded disk indefinitely.
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let store = fs::ContentStore::open(replica_workspace_cache)
                .await
                .unwrap();
            let deleted_chunk = Hash::new(b"delete-me");
            tilo_test_env::within(
                "replica GC to reclaim the deleted manifest and chunk",
                async {
                    loop {
                        if !store.has(deleted_manifest).await && !store.has(deleted_chunk).await {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                },
            )
            .await;
            store.store().shutdown().await.unwrap();
        });
    }};
}

#[test]
fn nas_replica_survives_owner_loss_restart_delete_and_denies_ungranted_reader() {
    run_nas_replica_lifecycle_scenario!();
}

// The skew matrix spells out receipt and grant behavior at every clock edge.
#[allow(
    clippy::too_many_lines,
    reason = "the skew matrix spells out receipt and grant behaviour at every clock edge"
)]
#[test]
fn skewed_clocks_fail_closed_for_receipts_and_grant_expiry() {
    use collab_types::Ed25519PublicKey;
    use fs::availability::{
        EvictionBlocker, EvictionDecision, EvictionRequirements, RecordAvailability, RecordKind,
    };
    use fs::{
        AuthenticatedFsPeer, AvailabilityRegistry, ReplicaAckError, ReplicaAckRequestV1,
        ReplicaAckV1, ShareAuthority,
    };
    use fs_protocol::ShareGrantStatus;
    use fs_protocol::{
        ContentHash, ContentRevisionV1, FileActor, FileInviteSpec, FileInviteSubject,
        FilePermission, FileRecordId, FileVersionId,
    };
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let device = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let capabilities = DeviceCapabilities::member();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "replica".into(),
        capabilities: capabilities.clone(),
        issued_at_ns: 0,
        expires_at_ns: Some(2_000_000_000_000),
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let peer = AuthenticatedFsPeer {
        user_public_key: cert.user_public_key,
        device_public_key: cert.device_public_key,
        device_label: cert.device_label.clone(),
        capabilities,
        cert,
    };
    let revision = ContentRevisionV1 {
        schema_version: fs_protocol::CONTENT_REVISION_VERSION,
        record_id: FileRecordId::new(),
        version_id: FileVersionId::new(),
        content_hash: Some(ContentHash([7; 32])),
        byte_len: 9,
        metadata_hash: ContentHash([8; 32]),
    };
    let request = ReplicaAckRequestV1::new(uuid::Uuid::new_v4(), revision).unwrap();
    let receipt = ReplicaAckV1::signed(
        &request,
        FileActor {
            person: Ed25519PublicKey(peer.user_public_key),
            device: Ed25519PublicKey(peer.device_public_key),
        },
        100,
        &device,
    )
    .unwrap();
    let observation = receipt
        .verify_for(&request, &peer, Ed25519PublicKey([3; 32]), 100)
        .unwrap();
    assert!(matches!(
        receipt.verify_for(&request, &peer, Ed25519PublicKey([3; 32]), 99),
        Err(ReplicaAckError::Stale)
    ));

    let availability_dir = tempfile::tempdir().unwrap();
    let mut availability =
        AvailabilityRegistry::open(availability_dir.path().join("availability.json")).unwrap();
    let mut record = RecordAvailability::new(revision.record_id, RecordKind::File);
    record.current_version = Some(revision.version_id);
    availability.put(record).unwrap();
    availability
        .observe_replica(
            revision.record_id,
            Ed25519PublicKey([3; 32]),
            observation.clone(),
            100,
        )
        .unwrap();
    assert_eq!(
        availability.eviction_decision(
            revision.record_id,
            99,
            EvictionRequirements::default(),
            false,
        ),
        EvictionDecision::Denied(EvictionBlocker::ReplicaAcknowledgementMissing),
        "a clock behind the signed receipt cannot turn future evidence into eviction authority"
    );
    assert_eq!(
        availability.eviction_decision(
            revision.record_id,
            101,
            EvictionRequirements {
                maximum_replica_age_ms: 0,
                ..EvictionRequirements::default()
            },
            false,
        ),
        EvictionDecision::Denied(EvictionBlocker::ReplicaAcknowledgementStale),
        "forward skew beyond the configured freshness budget fails closed"
    );
    assert_eq!(
        availability.eviction_decision(
            revision.record_id,
            observation.expires_at_ms + 1,
            EvictionRequirements::default(),
            false,
        ),
        EvictionDecision::Denied(EvictionBlocker::ReplicaAcknowledgementMissing),
        "an expired exact-version receipt cannot authorize eviction"
    );
    assert!(matches!(
        receipt.verify_for(
            &request,
            &peer,
            Ed25519PublicKey([3; 32]),
            receipt.expires_at_ms + 1,
        ),
        Err(ReplicaAckError::Stale)
    ));

    let temp = tempfile::tempdir().unwrap();
    let (issuer_key, issuer) = collab_types::generate_keypair();
    let (_, person) = collab_types::generate_keypair();
    let invite = fs_protocol::mint_file_invite(
        FileInviteSpec {
            workspace_id: uuid::Uuid::new_v4(),
            record_id: FileRecordId::new(),
            subject: FileInviteSubject::Person(person),
            permission: FilePermission::View,
            expires_at_ms: Some(10_000),
            max_redemptions: None,
        },
        &issuer_key,
    )
    .unwrap();
    let authority = ShareAuthority::open(temp.path().join("shares.json")).unwrap();
    authority.register(&invite, &[issuer], 9_999).unwrap();
    authority
        .authorize_named(&invite, person, FilePermission::View, 9_998)
        .expect("grants have an absolute expiry and no invented registration-time not-before");
    assert!(
        authority
            .authorize_named(&invite, person, FilePermission::View, 10_000)
            .is_err()
    );
    assert_eq!(
        authority.projection(invite.record_id, 10_000).grants[0].status,
        ShareGrantStatus::Expired
    );
}
