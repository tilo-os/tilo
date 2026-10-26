//! The Tier 1 data plane end to end: real `fs` daemons serving native room
//! trees from dedicated Btrfs store volumes, syncing through the record
//! plane.
//!
//! These tests need two dedicated Btrfs store volumes and `CAP_SYS_ADMIN`
//! (fanotify filesystem marks, pre-content events, bind mounts). Name the
//! volumes in `TILO_FS_NATIVE_STORES=/store/a:/store/b` and run as root.
//! Without them the tests skip with a message; `TILO_REQUIRE_FANOTIFY=1`
//! turns every skip into a failure. `tools/native-tier1.sh` makes the two
//! volumes on loop devices, runs the suite as root and removes them again.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "in a test these are the assertion or fixture arithmetic, not a hazard"
)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_lock::MutexRecover as _;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, adopt_device_key,
    persist::{write_device_cert, write_public_key, write_signing_key},
    sign_device_cert,
};

const TIMEOUT: Duration = Duration::from_secs(180);

/// The local person who owns each room (as the installer would set it up).
/// The daemons run as root; everything they materialise must belong to
/// this account, never to root or to the author's uid.
const LOCAL_OWNER: (u32, u32) = (4242, 4242);

/// The two store volumes, or `None` (after saying why) when this machine
/// cannot run Tier 1.
fn stores(test: &str) -> Option<(PathBuf, PathBuf)> {
    let skip = |why: String| {
        tilo_test_env::capability_unavailable(
            tilo_test_env::Capability::Fanotify,
            format_args!(
                "{test}: {why} (set TILO_FS_NATIVE_STORES=/a:/b to two dedicated Btrfs volumes and run as root)"
            ),
        );
        None
    };
    let Some(spec) = std::env::var_os("TILO_FS_NATIVE_STORES") else {
        return skip("TILO_FS_NATIVE_STORES is not set".into());
    };
    let spec = spec.to_string_lossy().into_owned();
    let Some((a, b)) = spec.split_once(':') else {
        return skip(format!(
            "TILO_FS_NATIVE_STORES={spec} does not name two volumes"
        ));
    };
    if !fs_native::sys::has_capability(fs_native::sys::CAP_SYS_ADMIN) {
        return skip("CAP_SYS_ADMIN is required".into());
    }
    for store in [a, b] {
        if let Err(e) = fs_native::store::check_dedicated(Path::new(store)) {
            return skip(format!("{store} is not a dedicated store volume: {e}"));
        }
    }
    Some((PathBuf::from(a), PathBuf::from(b)))
}

fn install_identity(root: &Path, person_seed: [u8; 32], device_seed: [u8; 32], primary: bool) {
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
        device_label: "native-tier1".into(),
        capabilities: if primary {
            DeviceCapabilities::primary()
        } else {
            DeviceCapabilities::member()
        },
        issued_at_ns: tilo_clock::try_unix_now_ns()
            .unwrap()
            .saturating_sub(1_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &person).unwrap();
    write_device_cert(&paths.device_cert, &cert).unwrap();
}

fn full_access_acl(path: &Path, person_seed: [u8; 32]) {
    let person = SigningKey::from_bytes(&person_seed)
        .verifying_key()
        .to_bytes();
    let caps: Vec<collab_types::CapOverride> = [
        collab_types::caps::Access::Read,
        collab_types::caps::Access::Write,
    ]
    .into_iter()
    .map(|verb| {
        collab_types::CapOverride::new(
            collab_types::Subject::User(collab_types::Ed25519PublicKey(person)),
            collab_types::Scope::FsPath("/".into()),
            collab_types::Effect::Grant,
            verb,
        )
    })
    .collect();
    std::fs::write(
        path,
        serde_json::to_vec(&collab_types::FsAuthority::from(caps)).unwrap(),
    )
    .unwrap();
}

struct Daemon {
    name: &'static str,
    child: Child,
    lines: Receiver<String>,
    logs: Arc<Mutex<Vec<String>>>,
    mount: PathBuf,
    /// The room's live subvolume, from the startup line.
    live: Option<PathBuf>,
}

impl Daemon {
    /// Starts a daemon whose identity and cache are `profile`'s, so a
    /// restart reuses both.
    fn spawn(
        name: &'static str,
        profile: &str,
        dir: &Path,
        store: &Path,
        mode: &[&str],
        mount: &Path,
    ) -> Self {
        Self::spawn_env(name, profile, dir, store, mode, mount, &[])
    }

    /// [`Self::spawn`] with extra environment; a value may name
    /// `@profile` for a per-daemon path under `dir`.
    fn spawn_env(
        name: &'static str,
        profile: &str,
        dir: &Path,
        store: &Path,
        mode: &[&str],
        mount: &Path,
        env: &[(&str, &str)],
    ) -> Self {
        let cache = dir.join(format!("{profile}-cache"));
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(mount).unwrap();
        // Short: socket paths are bounded (SUN_LEN) wherever the temporary
        // directory is.
        let sockets = dir.join(format!("{profile}-s"));
        std::fs::create_dir_all(&sockets).unwrap();
        // `TILO_FS_TEST_BINARY` runs another build of the daemon (a release
        // build on a loaded machine) against the same assertions.
        let binary = std::env::var_os("TILO_FS_TEST_BINARY")
            .map_or_else(|| tilo_test_env::package_binary!("fs"), PathBuf::from);
        let mut command = Command::new(binary);
        command.env("TILO_FS_NATIVE_TRACE", "1");
        for (key, value) in env {
            command.env(key, dir.join(value.replace("@profile", profile)));
        }
        command
            .env(
                fs::identity::IDENTITY_ROOT_ENV,
                dir.join(format!("{profile}-identity")),
            )
            .env("TILO_AUTHORITY_SOCKET_DIR", &sockets)
            .args(mode)
            .arg("--mount-point")
            .arg(mount)
            .arg("--cache-dir")
            .arg(&cache)
            .arg("--acl")
            .arg(dir.join("acl.json"))
            .args(["--data-plane", "native", "--store"])
            .arg(store)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let (tx, lines) = mpsc::channel();
        let logs = Arc::new(Mutex::new(Vec::new()));
        for stream in [
            Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
            Box::new(child.stderr.take().unwrap()) as Box<dyn Read + Send>,
        ] {
            let tx = tx.clone();
            let logs = Arc::clone(&logs);
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    eprintln!("{name}: {line}");
                    logs.lock_recover().push(line.clone());
                    let _ = tx.send(line);
                }
            });
        }
        Self {
            name,
            child,
            lines,
            logs,
            mount: mount.to_path_buf(),
            live: None,
        }
    }

    fn alive(&mut self) {
        if let Some(status) = self.child.try_wait().unwrap() {
            panic!(
                "{} exited {status}; logs:\n{}",
                self.name,
                self.logs.lock_recover().join("\n")
            );
        }
    }

    fn wait_line(&mut self, expected: &str) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.alive();
            let left = deadline.saturating_duration_since(Instant::now());
            if let Ok(line) = self
                .lines
                .recv_timeout(left.min(Duration::from_millis(200)))
            {
                if line.contains(expected) {
                    return line;
                }
            } else if Instant::now() >= deadline {
                panic!(
                    "{}: no line containing {expected:?}; logs:\n{}",
                    self.name,
                    self.logs.lock_recover().join("\n")
                );
            }
        }
    }

    fn wait_serving(&mut self) {
        let line = self.wait_line("Serving workspace natively from ");
        let rest = line
            .split("Serving workspace natively from ")
            .nth(1)
            .unwrap();
        let live_dir = rest.split(" at ").next().unwrap();
        self.live = Some(PathBuf::from(live_dir));
    }

    fn wait_ticket(&mut self) -> String {
        loop {
            let line = self.wait_line("");
            if fs::Ticket::decode(line.trim()).is_ok() {
                return line.trim().to_string();
            }
        }
    }

    fn signal(&self, sig: libc::c_int) {
        let pid = libc::pid_t::try_from(self.child.id()).unwrap();
        // SAFETY: signalling our own child process.
        unsafe { libc::kill(pid, sig) };
    }

    /// A clean stop, as systemd does it.
    fn terminate(&mut self) {
        self.signal(libc::SIGTERM);
        let deadline = Instant::now() + TIMEOUT;
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "{} ignored SIGTERM", self.name);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A crash: no chance to unmount or persist anything.
    fn kill(&mut self) {
        self.signal(libc::SIGKILL);
        let _ = self.child.wait();
    }

    fn log_count(&self, needle: &str) -> usize {
        self.logs
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains(needle))
            .count()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.signal(libc::SIGTERM);
            let deadline = Instant::now() + Duration::from_secs(15);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = Command::new("umount").arg("-l").arg(&self.mount).status();
    }
}

/// Removes a room's subvolume and its capture snapshots.
fn remove_room(live: &Path) {
    let Some(room) = live.file_name() else { return };
    let Some(store) = live.parent().and_then(Path::parent) else {
        return;
    };
    let epochs = store.join(".epochs").join(room);
    if let Ok(entries) = std::fs::read_dir(&epochs) {
        for e in entries.flatten() {
            let _ = fs_native::btrfs::delete_subvolume(&e.path());
        }
    }
    let _ = std::fs::remove_dir(&epochs);
    let _ = fs_native::btrfs::delete_subvolume(live);
}

struct Rooms(Vec<PathBuf>);

impl Drop for Rooms {
    fn drop(&mut self) {
        for live in &self.0 {
            remove_room(live);
        }
    }
}

fn wait_for<T>(daemons: &mut [&mut Daemon], what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(v) = probe() {
            return v;
        }
        for d in daemons.iter_mut() {
            d.alive();
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn pattern(seed: u8, len: usize) -> Vec<u8> {
    (0..len as u64)
        .map(|i| {
            let x = i.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ u64::from(seed).wrapping_mul(0xD6E8_FEB8_6659_FD93);
            (x >> 56) as u8
        })
        .collect()
}

fn residency_xattr(path: &Path) -> Option<Vec<u8>> {
    let p = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    let mut buf = vec![0u8; 512];
    // SAFETY: valid path and buffer.
    let r = unsafe {
        libc::getxattr(
            p.as_ptr(),
            c"trusted.tilo.residency".as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    (r >= 0).then(|| {
        buf.truncate(r.cast_unsigned());
        buf
    })
}

fn mtime_ms(path: &Path) -> i64 {
    let m = std::fs::metadata(path).unwrap();
    m.mtime() * 1000 + m.mtime_nsec() / 1_000_000
}

/// Two devices of one person, each on its own store volume.
struct Pair {
    // Dropped in this order: the daemons stop (and unmount) before their
    // rooms are removed, and only then does the temporary directory holding
    // the mount points go.
    host: Daemon,
    peer: Daemon,
    _rooms: Rooms,
    ticket: String,
    dir: PathBuf,
    _dir: Option<tempfile::TempDir>,
}

fn start_pair(store_a: &Path, store_b: &Path) -> Pair {
    start_pair_with(store_a, store_b, &[], &[])
}

/// [`start_pair`] with extra arguments for the host (the always-on device)
/// and extra environment for both.
fn start_pair_with(store_a: &Path, store_b: &Path, extra: &[&str], env: &[(&str, &str)]) -> Pair {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_path_buf();
    // Owner-only, as the daemons require of their control sockets' parent.
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    // The daemons advertise their Iroh endpoints beyond this machine and
    // dial each other by device key, so every pair gets identities no other
    // run shares. With fixed seeds, a concurrent copy of a scenario, or
    // another suite's fixture built from the same bytes, answered as the
    // restarted host's peer and handed it a foreign workspace.
    let person: [u8; 32] = rand::random();
    install_identity(&dir.join("host-identity"), person, rand::random(), true);
    install_identity(&dir.join("peer-identity"), person, rand::random(), false);
    full_access_acl(&dir.join("acl.json"), person);
    for mount in ["host-mount", "peer-mount"] {
        let m = dir.join(mount);
        std::fs::create_dir_all(&m).unwrap();
        std::os::unix::fs::chown(&m, Some(LOCAL_OWNER.0), Some(LOCAL_OWNER.1)).unwrap();
    }
    let host_args: Vec<&str> = ["serve"].into_iter().chain(extra.iter().copied()).collect();
    let mut host = Daemon::spawn_env(
        "host",
        "host",
        &dir,
        store_a,
        &host_args,
        &dir.join("host-mount"),
        env,
    );
    let ticket = host.wait_ticket();
    host.wait_serving();
    let peer_args = ["mount", "--ticket", ticket.as_str()];
    let mut peer = Daemon::spawn_env(
        "peer",
        "peer",
        &dir,
        store_b,
        &peer_args,
        &dir.join("peer-mount"),
        env,
    );
    peer.wait_line("Joined workspace");
    peer.wait_serving();
    let rooms = Rooms(vec![host.live.clone().unwrap(), peer.live.clone().unwrap()]);
    Pair {
        _dir: if std::env::var_os("TILO_KEEP_TEST_DIR").is_some() {
            eprintln!("keeping {}", temp.keep().display());
            None
        } else {
            Some(temp)
        },
        dir,
        host,
        peer,
        _rooms: rooms,
        ticket,
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end scenario whose steps build on each other"
)]
fn local_changes_publish_in_journal_order_and_arrive_as_placeholders_and_files() {
    let Some((a, b)) = stores("native_sync") else {
        return;
    };
    let mut pair = start_pair(&a, &b);
    let m1 = pair.host.mount.clone();
    let m2 = pair.peer.mount.clone();
    assert_eq!(
        std::fs::canonicalize(&m1).unwrap(),
        m1.canonicalize().unwrap(),
        "the workspace path is a real directory"
    );

    // Writer order: objects first, then the file that names them.
    std::fs::create_dir_all(m1.join("objects")).unwrap();
    std::fs::write(m1.join("objects/1"), pattern(1, 4096)).unwrap();
    let big = pattern(2, 3 * 1024 * 1024 + 17);
    std::fs::write(m1.join("objects/2"), &big).unwrap();
    std::fs::write(m1.join("ref"), b"objects/2\n").unwrap();

    // The peer never shows the ref before what it names.
    let t = Instant::now();
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "ref on the peer",
        || {
            let r = std::fs::read(m2.join("ref")).ok()?;
            assert!(
                m2.join("objects/1").exists() && m2.join("objects/2").exists(),
                "the ref became visible before the objects it names"
            );
            (r == b"objects/2\n").then_some(())
        },
    );
    eprintln!("host write to peer visibility: {:?}", t.elapsed());
    assert_same_bytes(
        &std::fs::read(m2.join("objects/1")).unwrap(),
        &pattern(1, 4096),
        "small object on the peer",
    );

    let placeholder = m2.join("objects/2");
    assert_placeholder_hydrates(&placeholder, &m1.join("objects/2"), &big);

    // Rename and delete. The rename is a move of the record: on the peer
    // the file is renamed, so its inode and its resident bytes stay.
    let peer_ino = std::fs::metadata(&placeholder).unwrap().ino();
    std::fs::rename(m1.join("objects/2"), m1.join("objects/3")).unwrap();
    std::fs::remove_file(m1.join("objects/1")).unwrap();
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "rename and delete on the peer",
        || {
            (m2.join("objects/3").exists()
                && !m2.join("objects/2").exists()
                && !m2.join("objects/1").exists())
            .then_some(())
        },
    );
    assert_eq!(
        std::fs::metadata(m2.join("objects/3")).unwrap().ino(),
        peer_ino,
        "the peer renamed the file rather than writing it again"
    );
    assert!(
        residency_xattr(&m2.join("objects/3")).is_none(),
        "still resident"
    );
    assert_same_bytes(
        &std::fs::read(m2.join("objects/3")).unwrap(),
        &big,
        "renamed object on the peer",
    );

    // A directory rename moves the subtree in one operation, both ways.
    std::fs::create_dir_all(m1.join("tree/sub")).unwrap();
    std::fs::write(m1.join("tree/sub/leaf"), b"leaf").unwrap();
    std::fs::write(m1.join("tree/top"), b"top").unwrap();
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "the tree on the peer",
        || (std::fs::read(m2.join("tree/sub/leaf")).ok()? == b"leaf").then_some(()),
    );
    let tree_ino = std::fs::metadata(m2.join("tree")).unwrap().ino();
    let leaf_ino = std::fs::metadata(m2.join("tree/sub/leaf")).unwrap().ino();
    std::fs::rename(m1.join("tree"), m1.join("renamed")).unwrap();
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "the renamed tree on the peer",
        || (m2.join("renamed/sub/leaf").exists() && !m2.join("tree").exists()).then_some(()),
    );
    assert_eq!(
        std::fs::metadata(m2.join("renamed")).unwrap().ino(),
        tree_ino
    );
    assert_eq!(
        std::fs::metadata(m2.join("renamed/sub/leaf"))
            .unwrap()
            .ino(),
        leaf_ino
    );
    assert_eq!(std::fs::read(m2.join("renamed/top")).unwrap(), b"top");
    std::fs::rename(m2.join("renamed/sub"), m2.join("moved-back")).unwrap();
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "the peer's move on the host",
        || {
            (std::fs::read(m1.join("moved-back/leaf")).ok()? == b"leaf"
                && !m1.join("renamed/sub").exists())
            .then_some(())
        },
    );
    assert!(
        pair.host.log_count("1 move(s)") >= 2,
        "renames were published as moves"
    );

    // The other direction.
    std::fs::create_dir_all(m2.join("back")).unwrap();
    std::fs::write(m2.join("back/from-peer.txt"), b"hello from the peer").unwrap();
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "peer file on the host",
        || {
            (std::fs::read(m1.join("back/from-peer.txt")).ok()? == b"hello from the peer")
                .then_some(())
        },
    );

    assert_records_follow_writer_order(&mut pair, &m1, &m2);
}

/// `left` has exactly the bytes of `right`. Reports the length or first
/// differing offset instead of printing megabytes of content on failure.
fn assert_same_bytes(left: &[u8], right: &[u8], what: &str) {
    assert_eq!(left.len(), right.len(), "{what}: length differs");
    if let Some(at) = left.iter().zip(right).position(|(l, r)| l != r) {
        panic!("{what}: first differing byte at offset {at}");
    }
}

/// A large remote file starts as a placeholder with the author's exact size
/// and mtime, and a full read hydrates the author's bytes without touching
/// the mtime.
fn assert_placeholder_hydrates(placeholder: &Path, original: &Path, expected: &[u8]) {
    assert!(
        residency_xattr(placeholder).is_some(),
        "a 3 MiB remote file starts as a placeholder"
    );
    assert_eq!(
        std::fs::metadata(placeholder).unwrap().len(),
        expected.len() as u64
    );
    assert_eq!(mtime_ms(placeholder), mtime_ms(original));
    let t = Instant::now();
    let read = std::fs::read(placeholder).unwrap();
    let took = t.elapsed();
    assert_same_bytes(&read, expected, "hydrated bytes match the author's");
    let megabytes = f64::from(u32::try_from(read.len()).unwrap()) / 1e6;
    eprintln!(
        "cold placeholder read: {} bytes in {took:?} ({:.1} MB/s)",
        read.len(),
        megabytes / took.as_secs_f64()
    );
    assert!(
        residency_xattr(placeholder).is_none(),
        "resident after a full read"
    );
    assert_eq!(mtime_ms(placeholder), mtime_ms(original));
}

/// Records written in sequence on the host arrive, and are versioned, in the
/// writer's order; after the host stops its tree is no longer reachable.
fn assert_records_follow_writer_order(pair: &mut Pair, m1: &Path, m2: &Path) {
    std::fs::create_dir_all(m1.join("seq")).unwrap();
    for name in ["a", "b", "c"] {
        std::fs::write(m1.join("seq").join(name), name).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "seq on the peer",
        || {
            let c = m2.join("seq/c").exists();
            if c {
                assert!(m2.join("seq/a").exists() && m2.join("seq/b").exists());
            }
            c.then_some(())
        },
    );
    pair.host.terminate();
    let versions = first_versions(&pair.dir.join("host-cache"), &["seq/a", "seq/b", "seq/c"]);
    assert!(
        versions.windows(2).all(|w| w[0] <= w[1]),
        "record versions follow the writer's order: {versions:?}"
    );
    // The renamed file is the record created as objects/2: its history
    // runs from that creation through the rename.
    let history = provenance(&pair.dir.join("host-cache"), "objects/3");
    assert!(
        history.first() == Some(&fs_protocol::FileProvenance::Create)
            && history.contains(&fs_protocol::FileProvenance::Rename),
        "history follows the renamed record: {history:?}"
    );
    assert!(pair.host.log_count("epoch ") >= 1);
    assert!(
        !m1.join("ref").exists(),
        "the tree is not reachable at the workspace path after the daemon stopped"
    );
}

fn persisted_namespace(cache: &Path) -> fs::Namespace {
    let outcome = fs::durable_backup::read_or_recover(&cache.join("namespace.bin"), |bytes| {
        fs::Namespace::from_bytes("reader", bytes).is_ok()
    })
    .unwrap()
    .unwrap();
    fs::Namespace::from_bytes("reader", &outcome.bytes).unwrap()
}

/// The provenance of every version of the record at `path`, oldest first.
fn provenance(cache: &Path, path: &str) -> Vec<fs_protocol::FileProvenance> {
    let ns = persisted_namespace(cache);
    let history = fs::FileHistoryStore::open(cache.join("file-history.json")).unwrap();
    let entry = ns
        .entry_at(path)
        .unwrap_or_else(|| panic!("{path} is not a record"));
    let mut versions = history.snapshot(entry.record_id).unwrap().versions;
    versions.sort_by_key(|v| v.timestamp_ms);
    versions.into_iter().map(|v| v.provenance).collect()
}

/// The time of the first content version of each path, from the host's
/// persisted namespace and history.
fn first_versions(cache: &Path, paths: &[&str]) -> Vec<u64> {
    let ns = persisted_namespace(cache);
    let history = fs::FileHistoryStore::open(cache.join("file-history.json")).unwrap();
    paths
        .iter()
        .map(|p| {
            let entry = ns
                .entry_at(p)
                .unwrap_or_else(|| panic!("{p} is not a record"));
            let snapshot = history.snapshot(entry.record_id).unwrap();
            snapshot
                .versions
                .iter()
                .filter(|v| v.content_hash.is_some())
                .map(|v| v.timestamp_ms)
                .min()
                .unwrap_or_else(|| panic!("{p} has no content version"))
        })
        .collect()
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        // The compatibility defaults Tilo OS ships in /etc/gitconfig.
        .args([
            "-c",
            "core.trustctime=false",
            "-c",
            "core.checkStat=minimal",
        ])
        .args(["-c", "gc.auto=0", "-c", "safe.directory=*"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

#[test]
fn a_git_repository_made_on_one_device_is_clean_and_complete_on_the_other() {
    let Some((a, b)) = stores("native_git") else {
        return;
    };
    if Command::new("git").arg("--version").output().is_err() {
        assert!(!required(), "git is not installed");
        return;
    }
    let mut pair = start_pair(&a, &b);
    let repo1 = pair.host.mount.join("repo");
    let repo2 = pair.peer.mount.join("repo");
    std::fs::create_dir_all(&repo1).unwrap();
    git(&repo1, &["init", "-q", "-b", "main"]).unwrap();
    std::fs::create_dir_all(repo1.join("src")).unwrap();
    let mut files: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    files.insert("README.md", b"# demo\n".to_vec());
    files.insert("src/lib.rs", b"pub fn f() {}\n".to_vec());
    files.insert("assets/blob.bin", pattern(7, 2 * 1024 * 1024));
    // An executable and a symbolic link: git compares both.
    files.insert("tool.sh", b"#!/bin/sh\necho tool\n".to_vec());
    std::os::unix::fs::symlink("README.md", repo1.join("latest")).unwrap();
    for round in 0..3u8 {
        for (path, data) in &mut files {
            data.extend_from_slice(format!("round {round}\n").as_bytes());
            let p = repo1.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, &data).unwrap();
            if *path == "tool.sh" {
                set_mode(&p, 0o755);
            }
        }
        // Edits and the commit land in different seconds, as when a person
        // works. An index written in the second its files changed holds
        // "racily clean" entries that the next `git status` anywhere
        // rewrites (git's own rule), which is not a sync artifact.
        std::thread::sleep(Duration::from_millis(1100));
        git(&repo1, &["add", "-A"]).unwrap();
        git(&repo1, &["commit", "-q", "-m", &format!("round {round}")]).unwrap();
    }
    let head = git(&repo1, &["rev-parse", "HEAD"]).unwrap();
    assert_eq!(git(&repo1, &["status", "--porcelain"]).unwrap(), "");

    // A tool keeps running `git status` on the peer while it syncs, as a
    // shell prompt does. It may write the index whenever what it sees is
    // not a state the author's disk had; nothing it sees may make it.
    let t = Instant::now();
    let mut last_status = String::new();
    let mut polls = 0u32;
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "the host's HEAD on the peer",
        || {
            std::thread::sleep(Duration::from_millis(100));
            polls += 1;
            let at_head = git(&repo2, &["rev-parse", "HEAD"]).ok()? == head;
            let status = git(&repo2, &["status", "--porcelain"]);
            let shown = format!("{status:?}");
            if shown != last_status {
                eprintln!("peer git status: {shown}");
                last_status.clone_from(&shown);
            }
            if at_head {
                assert_eq!(
                    status.as_deref(),
                    Ok(""),
                    "the status is clean as soon as the new HEAD is visible"
                );
            }
            at_head.then_some(())
        },
    );
    eprintln!(
        "three commits visible on the peer after {:?}, clean at once ({polls} status runs while syncing)",
        t.elapsed()
    );
    let index = repo2.join(".git/index");
    let index_before = std::fs::metadata(&index).unwrap().mtime_nsec();
    git(&repo2, &["fsck", "--strict", "--no-dangling"]).unwrap();
    assert_eq!(git(&repo2, &["status", "--porcelain"]).unwrap(), "");
    assert_eq!(
        git(&repo2, &["log", "--oneline"]).unwrap().lines().count(),
        3
    );
    assert_eq!(
        std::fs::metadata(&index).unwrap().mtime_nsec(),
        index_before,
        "status on the peer did not rewrite the index (no stat ping-pong)"
    );
    // Give a stray capture on the peer time to show up.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        pair.peer.log_count(" change(s), "),
        0,
        "the peer published nothing: no spurious index or other transient state"
    );
    for (path, data) in &files {
        assert!(std::fs::read(repo2.join(path)).unwrap() == *data, "{path}");
    }
    assert_eq!(mode_of(&repo2.join("tool.sh")), 0o755);
    assert_eq!(
        std::fs::read_link(repo2.join("latest")).unwrap(),
        Path::new("README.md")
    );
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn mode_of(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

/// Every path below `root`, links not followed.
/// The daemon's own staging entries come and go; they are not part of the
/// tree.
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".tilo-staging-")
            {
                continue;
            }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(entry.path());
            }
            if std::fs::symlink_metadata(entry.path()).is_ok() {
                out.push(entry.path());
            }
        }
    }
    out
}

#[test]
fn modes_links_and_ownership_replicate_and_hard_links_stay_consistent() {
    let Some((a, b)) = stores("native_posix") else {
        return;
    };
    let mut pair = start_pair(&a, &b);
    let m1 = pair.host.mount.clone();
    let m2 = pair.peer.mount.clone();

    std::fs::create_dir_all(m1.join("bin")).unwrap();
    std::fs::write(m1.join("bin/run.sh"), b"#!/bin/sh\necho hi\n").unwrap();
    set_mode(&m1.join("bin/run.sh"), 0o755);
    std::fs::write(m1.join("secret"), b"s").unwrap();
    set_mode(&m1.join("secret"), 0o600);
    std::fs::create_dir(m1.join("shared")).unwrap();
    set_mode(&m1.join("shared"), 0o2775);
    std::fs::create_dir(m1.join("private")).unwrap();
    std::fs::write(m1.join("private/note"), b"n").unwrap();
    set_mode(&m1.join("private"), 0o700);
    std::os::unix::fs::symlink("bin/run.sh", m1.join("link-rel")).unwrap();
    std::os::unix::fs::symlink("/etc/hostname", m1.join("link-abs")).unwrap();
    std::os::unix::fs::symlink("missing/target", m1.join("link-dangling")).unwrap();
    std::fs::write(m1.join("a"), b"linked").unwrap();
    std::fs::hard_link(m1.join("a"), m1.join("b")).unwrap();

    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "the tree on the peer",
        || {
            (std::fs::symlink_metadata(m2.join("link-dangling")).is_ok()
                && std::fs::read(m2.join("b")).ok()? == b"linked"
                && std::fs::read(m2.join("private/note")).ok()? == b"n"
                && mode_of(&m2.join("private")) == 0o700)
                .then_some(())
        },
    );
    assert_eq!(mode_of(&m2.join("bin/run.sh")), 0o755);
    assert_eq!(mode_of(&m2.join("secret")), 0o600);
    assert_eq!(mode_of(&m2.join("shared")), 0o2775);
    for (link, target) in [
        ("link-rel", "bin/run.sh"),
        ("link-abs", "/etc/hostname"),
        ("link-dangling", "missing/target"),
    ] {
        assert_eq!(
            std::fs::read_link(m2.join(link)).unwrap(),
            Path::new(target),
            "{link}"
        );
    }
    let out = Command::new(m2.join("link-rel")).output().unwrap();
    assert_eq!(out.stdout, b"hi\n", "the replicated script runs");
    for path in walk(&m2) {
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            (meta.uid(), meta.gid()),
            LOCAL_OWNER,
            "{} belongs to the local person",
            path.display()
        );
    }
    // Hard links arrive as separate files with the same bytes; a write
    // through one link reaches both.
    assert_eq!(std::fs::read(m2.join("a")).unwrap(), b"linked");
    std::fs::write(m1.join("b"), b"written through b").unwrap();
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "both links updated on the peer",
        || {
            (std::fs::read(m2.join("a")).ok()? == b"written through b"
                && std::fs::read(m2.join("b")).ok()? == b"written through b")
                .then_some(())
        },
    );

    // Changes of mode and target only, both ways.
    set_mode(&m1.join("bin/run.sh"), 0o644);
    std::fs::remove_file(m1.join("link-rel")).unwrap();
    std::os::unix::fs::symlink("secret", m1.join("link-rel")).unwrap();
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "mode and target changes on the peer",
        || {
            (mode_of(&m2.join("bin/run.sh")) == 0o644
                && std::fs::read_link(m2.join("link-rel")).ok()? == Path::new("secret"))
            .then_some(())
        },
    );
    set_mode(&m2.join("secret"), 0o640);
    set_mode(&m2.join("shared"), 0o755);
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "the peer's mode changes on the host",
        || {
            (mode_of(&m1.join("secret")) == 0o640 && mode_of(&m1.join("shared")) == 0o755)
                .then_some(())
        },
    );
    assert_eq!(std::fs::read(m1.join("secret")).unwrap(), b"s");
}

#[test]
fn a_crashed_daemon_resumes_from_its_last_epoch_and_publishes_only_what_changed() {
    let Some((a, b)) = stores("native_restart") else {
        return;
    };
    let mut pair = start_pair(&a, &b);
    let m1 = pair.host.mount.clone();
    let m2 = pair.peer.mount.clone();
    for i in 0..50u8 {
        std::fs::write(m1.join(format!("f{i}")), pattern(i, 1000)).unwrap();
    }
    let big = pattern(99, 4 * 1024 * 1024);
    std::fs::write(m1.join("big"), &big).unwrap();
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "files on the peer",
        || {
            let big_len = std::fs::metadata(m2.join("big")).ok()?.len();
            (big_len == big.len() as u64 && m2.join("f49").exists()).then_some(())
        },
    );
    assert!(residency_xattr(&m2.join("big")).is_some());

    // The host crashes. While nothing journals, the tree changes.
    let live = pair.host.live.clone().unwrap();
    pair.host.kill();
    std::fs::write(live.join("f0"), b"edited while the daemon was down").unwrap();
    std::fs::remove_file(live.join("f1")).unwrap();
    std::fs::write(live.join("new"), b"created while down").unwrap();

    let dir = pair.dir.clone();
    let mut host = Daemon::spawn("host2", "host", &dir, &a, &["serve"], &m1);
    // The first epoch compares the room against the last persisted one
    // before the tree is exposed again.
    let comparison = host.wait_line("full comparison");
    assert!(
        comparison.contains(": 3 change(s)"),
        "only the three changes made while down are published: {comparison}"
    );
    host.wait_serving();
    assert_eq!(host.live.as_ref(), Some(&live), "the same room is resumed");
    wait_for(
        &mut [&mut host, &mut pair.peer],
        "the offline changes on the peer",
        || {
            (std::fs::read(m2.join("f0")).ok()? == b"edited while the daemon was down"
                && !m2.join("f1").exists()
                && std::fs::read(m2.join("new")).ok()? == b"created while down")
                .then_some(())
        },
    );

    // The peer restarts with a placeholder never read: it is re-adopted
    // before the tree is exposed, and reads the right bytes.
    pair.peer.terminate();
    assert!(!m2.join("big").exists(), "nothing is exposed while stopped");
    let ticket = pair.ticket.clone();
    let mut peer = Daemon::spawn(
        "peer2",
        "peer",
        &dir,
        &b,
        &["mount", "--ticket", &ticket],
        &m2,
    );
    peer.wait_line("Re-adopted 1 placeholder(s)");
    peer.wait_serving();
    assert_same_bytes(
        &std::fs::read(m2.join("big")).unwrap(),
        &big,
        "re-adopted placeholder",
    );
    host.terminate();
    peer.terminate();
}

/// The index of the last log line containing `needle`.
fn last_line(daemon: &Daemon, needle: &str) -> Option<usize> {
    daemon
        .logs
        .lock()
        .unwrap()
        .iter()
        .rposition(|l| l.contains(needle))
}

/// Whether a log line after index `after` contains `needle`.
fn logged_after(daemon: &Daemon, after: usize, needle: &str) -> bool {
    daemon
        .logs
        .lock()
        .unwrap()
        .iter()
        .skip(after + 1)
        .any(|l| l.contains(needle))
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one crash scenario: publish, kill, restart and check the peer at each step"
)]
fn a_daemon_killed_while_publishing_a_changeset_leaves_peers_all_of_it_or_none() {
    const BURST: usize = 1500;
    let Some((a, b)) = stores("native_changeset_crash") else {
        return;
    };
    if Command::new("git").arg("--version").output().is_err() {
        assert!(!required(), "git is not installed");
        return;
    }
    let mut pair = start_pair(&a, &b);
    let m1 = pair.host.mount.clone();
    let m2 = pair.peer.mount.clone();
    let live = pair.host.live.clone().unwrap();
    let dir = pair.dir.clone();
    std::fs::write(m1.join("marker"), b"synced").unwrap();
    wait_for(&mut [&mut pair.host, &mut pair.peer], "the marker", || {
        m2.join("marker").exists().then_some(())
    });

    // One burst of files is one epoch; the host dies while that epoch's
    // changeset is being written.
    std::fs::create_dir_all(m1.join("burst")).unwrap();
    for i in 0..BURST {
        std::fs::write(
            m1.join(format!("burst/f{i:04}")),
            pattern((i % 251) as u8, 4096),
        )
        .unwrap();
    }
    let cut = pair
        .host
        .wait_line(&format!("upsert File burst/f{:04}", BURST - 1));
    pair.host.kill();
    let cut_at = last_line(&pair.host, &cut).unwrap();
    assert!(
        !logged_after(&pair.host, cut_at, "is durable"),
        "the host was killed after its changeset was written, not during it"
    );
    // Nothing of an unwritten changeset reaches the peer.
    let down = Instant::now();
    while down.elapsed() < Duration::from_secs(2) {
        pair.peer.alive();
        let seen = std::fs::read_dir(m2.join("burst")).map_or(0, Iterator::count);
        assert_eq!(
            seen, 0,
            "the peer shows part of a changeset that never became durable"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // The restarted host publishes the epoch again, as one changeset: the
    // peer takes all of it in one projection.
    let mut host = Daemon::spawn("host2", "host", &dir, &a, &["serve"], &m1);
    host.wait_serving();
    assert_eq!(host.live.as_ref(), Some(&live), "the same room is resumed");
    let before = pair.peer.logs.lock().unwrap().len();
    wait_for(
        &mut [&mut host, &mut pair.peer],
        "the burst on the peer",
        || {
            (0..BURST)
                .all(|i| {
                    std::fs::read(m2.join(format!("burst/f{i:04}")))
                        .is_ok_and(|got| got == pattern((i % 251) as u8, 4096))
                })
                .then_some(())
        },
    );
    // The files are in place before the projection reports them.
    let peer_logs = Arc::clone(&pair.peer.logs);
    let projections: Vec<String> = wait_for(
        &mut [&mut host, &mut pair.peer],
        "the projection's report",
        || {
            let applied: Vec<String> = peer_logs.lock().unwrap()[before..]
                .iter()
                .filter(|l| l.contains("applied remote changes:") && !l.contains(": 0 file(s)"))
                .cloned()
                .collect();
            (!applied.is_empty()).then_some(applied)
        },
    );
    assert!(
        projections
            .first()
            .is_some_and(|l| l.contains(&format!(": {BURST} file(s)"))),
        "the whole changeset arrives in one projection: {projections:?}"
    );

    // A git commit: the host dies once its ref update is captured. Whatever
    // the peer shows, now or later, is a consistent repository.
    let repo1 = m1.join("repo");
    let repo2 = m2.join("repo");
    std::fs::create_dir_all(repo1.join("src")).unwrap();
    git(&repo1, &["init", "-q", "-b", "main"]).unwrap();
    for i in 0..200 {
        std::fs::write(
            repo1.join(format!("src/m{i:03}.rs")),
            format!("pub fn f{i}() {{}}\n"),
        )
        .unwrap();
    }
    git(&repo1, &["add", "-A"]).unwrap();
    git(&repo1, &["commit", "-q", "-m", "one"]).unwrap();
    let head = git(&repo1, &["rev-parse", "HEAD"]).unwrap();
    host.wait_line("upsert File repo/.git/refs/heads/main");
    host.kill();
    let consistent = |repo: &Path| {
        // No HEAD commit yet, or one whose every object is present.
        git(repo, &["rev-parse", "--verify", "-q", "HEAD"]).map_or(true, |_| {
            git(repo, &["fsck", "--connectivity-only", "--no-dangling"]).is_ok()
        })
    };
    let down = Instant::now();
    while down.elapsed() < Duration::from_secs(2) {
        pair.peer.alive();
        if repo2.join(".git").exists() {
            assert!(
                consistent(&repo2),
                "the peer shows a ref before its objects"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut host = Daemon::spawn("host3", "host", &dir, &a, &["serve"], &m1);
    host.wait_serving();
    wait_for(
        &mut [&mut host, &mut pair.peer],
        "the commit on the peer",
        || {
            std::thread::sleep(Duration::from_millis(100));
            if repo2.join(".git").exists() {
                assert!(
                    consistent(&repo2),
                    "the peer shows a ref before its objects"
                );
            }
            (git(&repo2, &["rev-parse", "HEAD"]).ok()? == head).then_some(())
        },
    );
    wait_for(&mut [&mut host, &mut pair.peer], "a clean peer", || {
        std::thread::sleep(Duration::from_millis(250));
        git(&repo2, &["status", "--porcelain"])
            .ok()
            .filter(String::is_empty)
    });
    git(&repo2, &["fsck", "--strict", "--no-dangling"]).unwrap();
    assert_eq!(git(&repo1, &["status", "--porcelain"]).unwrap(), "");
    git(&repo1, &["fsck", "--strict", "--no-dangling"]).unwrap();
    host.terminate();
}

/// Writes into `path` every 300 ms until stopped: a writing session.
struct Session {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    fn start(path: &Path) -> Self {
        Self::continue_with(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap(),
        )
    }

    /// Keeps writing through a descriptor that already wrote once.
    fn continue_with(file: std::fs::File) -> Self {
        use std::os::unix::fs::FileExt as _;
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut n = 0u64;
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                file.write_all_at(&n.to_le_bytes(), 16384).unwrap();
                n += 1;
                std::thread::sleep(Duration::from_millis(300));
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }

    fn end(mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

#[test]
fn a_file_modified_in_place_takes_a_lease_and_a_second_writer_gets_ebusy() {
    use std::os::unix::fs::FileExt as _;
    let Some((a, b)) = stores("native_lease") else {
        return;
    };
    let mut pair = start_pair(&a, &b);
    let db1 = pair.host.mount.join("db.bin");
    let db2 = pair.peer.mount.join("db.bin");
    let original = pattern(3, 64 * 1024);
    std::fs::write(&db1, &original).unwrap();
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "db.bin on the peer",
        || (std::fs::read(&db2).ok()? == original).then_some(()),
    );

    // A write inside the file, keeping its inode and most of its extents:
    // the file becomes lease-class. Nothing names a file type.
    {
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&db1)
            .unwrap();
        f.write_all_at(b"page", 8192).unwrap();
    }
    pair.host.wait_line("db.bin is modified in place");
    std::thread::sleep(Duration::from_millis(1500));

    // The host's next writing session takes the lease before its first write.
    let session = Session::start(&db1);
    pair.host
        .wait_line("took the write lease of db.bin (epoch 1)");
    pair.peer.wait_line("db.bin is read-only here");

    // On the peer, a reader reads and opening it for writing gets EBUSY.
    let mut head = vec![0u8; 16];
    std::fs::File::open(&db2)
        .unwrap()
        .read_exact_at(&mut head, 0)
        .unwrap();
    assert!(head == original[..16], "readers keep reading a leased file");
    let err = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&db2)
        .unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::EBUSY), "{err}");

    // The host closes the file: the lease is released, and the peer's next
    // writing session takes it over with a new epoch.
    session.end();
    let t = Instant::now();
    // Each attempt opens the file anew: the host's last versions replace it
    // on the peer, and a descriptor of a replaced version stays refused.
    let taken = wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "the peer's write after the handover",
        || {
            std::thread::sleep(Duration::from_millis(100));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&db2)
                .ok()?;
            file.write_all_at(b"peer", 24576).ok()?;
            Some(file)
        },
    );
    // The peer keeps its session open while the host tries to write.
    let peer_session = Session::continue_with(taken);
    eprintln!(
        "lease handed over {:?} after the holder closed the file",
        t.elapsed()
    );
    pair.peer
        .wait_line("took the write lease of db.bin (epoch 2)");
    pair.host.wait_line("db.bin is read-only here");
    let err = std::fs::OpenOptions::new()
        .write(true)
        .open(&db1)
        .unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::EBUSY), "{err}");
    peer_session.end();
}

fn sqlite(db: &Path, sql: &str) -> Result<String, String> {
    let out = Command::new("sqlite3")
        .arg(db)
        .arg(sql)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Rewrites the first 16 bytes of `path` with what they already hold every
/// 300 ms: a writing session that holds the lease without changing a byte.
struct SameBytes {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SameBytes {
    fn start(path: &Path) -> Self {
        use std::os::unix::fs::FileExt as _;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let mut head = [0u8; 16];
        file.read_exact_at(&mut head, 0).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                file.write_all_at(&head, 0).unwrap();
                std::thread::sleep(Duration::from_millis(300));
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }

    fn end(mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

#[test]
fn sqlite_opened_read_write_on_the_device_without_the_lease_reads_and_refuses_writes() {
    let Some((a, b)) = stores("native_sqlite") else {
        return;
    };
    if Command::new("sqlite3").arg("--version").output().is_err() {
        assert!(!required(), "sqlite3 is not installed");
        return;
    }
    let mut pair = start_pair(&a, &b);
    let db1 = pair.host.mount.join("app.db");
    let db2 = pair.peer.mount.join("app.db");
    sqlite(
        &db1,
        "CREATE TABLE t(v INTEGER); INSERT INTO t VALUES (1), (2), (3);",
    )
    .unwrap();
    // sqlite3 creates a database it cannot find, and an empty file made on
    // the peer would be a concurrent creation of the path: ask only once
    // the host's file is there.
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "the database on the peer",
        || {
            (std::fs::metadata(&db2).ok()?.len() > 0
                && sqlite(&db2, "SELECT count(*) FROM t").ok()? == "3")
                .then_some(())
        },
    );
    // An update writes pages in place: the database becomes lease-class.
    sqlite(&db1, "UPDATE t SET v = v + 10;").unwrap();
    pair.host.wait_line("app.db is modified in place");
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "the update on the peer",
        || (sqlite(&db2, "SELECT sum(v) FROM t").ok()? == "36").then_some(()),
    );

    // The host holds the lease.
    let session = SameBytes::start(&db1);
    pair.host.wait_line("took the write lease of app.db");
    pair.peer.wait_line("app.db is read-only here");

    // sqlite3 opens read-write by default. On the peer it reads...
    for _ in 0..3 {
        assert_eq!(sqlite(&db2, "SELECT sum(v) FROM t").unwrap(), "36");
    }
    // ...and a write fails cleanly, leaving no journal behind.
    let err = sqlite(&db2, "INSERT INTO t VALUES (99);").unwrap_err();
    assert!(err.contains("readonly"), "{err}");
    assert!(
        !pair.peer.mount.join("app.db-journal").exists(),
        "a refused write leaves no rollback journal to publish"
    );
    assert_eq!(sqlite(&db2, "PRAGMA integrity_check;").unwrap(), "ok");

    // Once the host lets go, the peer writes, and the host sees it.
    session.end();
    let t = Instant::now();
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "the peer's write after the handover",
        || {
            std::thread::sleep(Duration::from_millis(200));
            sqlite(&db2, "INSERT INTO t VALUES (100);").ok()
        },
    );
    eprintln!("the peer wrote {:?} after the host let go", t.elapsed());
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "the peer's row on the host",
        || {
            std::thread::sleep(Duration::from_millis(200));
            (sqlite(&db1, "SELECT sum(v) FROM t").ok()? == "136").then_some(())
        },
    );
    assert_eq!(sqlite(&db1, "PRAGMA integrity_check;").unwrap(), "ok");
    assert!(
        !pair.host.mount.join("app.db-journal").exists(),
        "no journal reached the host"
    );
}

/// Asks a daemon's transport partition gate to `partition` or `reconnect`.
fn gate(socket: &Path, command: &str) {
    use std::io::Write as _;
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    stream.write_all(format!("{command}\n").as_bytes()).unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    assert!(
        !answer.is_empty(),
        "{command}: no answer from {}",
        socket.display()
    );
}

/// Makes `name` lease-class on both devices of `pair`: written once,
/// modified in place on the host, then held and released by the host.
fn lease_class_file(pair: &mut Pair, name: &str, content: &[u8]) {
    use std::os::unix::fs::FileExt as _;
    let f1 = pair.host.mount.join(name);
    let f2 = pair.peer.mount.join(name);
    std::fs::write(&f1, content).unwrap();
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "the file on the peer",
        || (std::fs::read(&f2).ok()? == content).then_some(()),
    );
    std::fs::OpenOptions::new()
        .write(true)
        .open(&f1)
        .unwrap()
        .write_all_at(&content[..8], 0)
        .unwrap();
    pair.host.wait_line(&format!("{name} is modified in place"));
    // The write gate is placed at the next lease tick; a descriptor opened
    // before it is not watched by the kernel.
    std::thread::sleep(Duration::from_millis(1500));
    let session = SameBytes::start(&f1);
    pair.host
        .wait_line(&format!("took the write lease of {name}"));
    pair.peer.wait_line(&format!("{name} is read-only here"));
    session.end();
    // Released: the peer opens it for writing again.
    wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "the lease released",
        || {
            std::thread::sleep(Duration::from_millis(100));
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&f2)
                .ok()
                .map(|_| ())
        },
    );
}

/// From a thread that opens `path` read-write and waits at `start`, writes
/// `bytes` at offset 0 and keeps rewriting them for `hold`.
fn contend(
    path: PathBuf,
    bytes: Vec<u8>,
    start: Arc<std::sync::Barrier>,
    hold: Duration,
) -> std::thread::JoinHandle<Result<(), i32>> {
    std::thread::spawn(move || {
        use std::os::unix::fs::FileExt as _;
        let errno = |e: std::io::Error| e.raw_os_error().unwrap_or(0);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path);
        start.wait();
        let file = file.map_err(errno)?;
        file.write_all_at(&bytes, 0).map_err(errno)?;
        let until = Instant::now() + hold;
        while Instant::now() < until {
            std::thread::sleep(Duration::from_millis(200));
            file.write_all_at(&bytes, 0).map_err(errno)?;
        }
        Ok(())
    })
}

#[test]
fn the_lease_authority_gives_a_free_lease_to_one_of_two_devices_asking_at_once() {
    let Some((a, b)) = stores("native_authority") else {
        return;
    };
    let mut pair = start_pair_with(&a, &b, &["--lease-authority"], &[]);
    assert_eq!(pair.host.log_count("arbitrating write leases"), 1);
    assert_eq!(pair.peer.log_count("arbitrating write leases"), 0);
    let content = pattern(9, 64 * 1024);
    lease_class_file(&mut pair, "shared.db", &content);
    let f1 = pair.host.mount.join("shared.db");
    let f2 = pair.peer.mount.join("shared.db");
    let mut winners = Vec::new();
    for round in 0..3u8 {
        let start = Arc::new(std::sync::Barrier::new(2));
        let host = contend(
            f1.clone(),
            vec![b'h', round],
            Arc::clone(&start),
            Duration::from_secs(2),
        );
        let peer = contend(
            f2.clone(),
            vec![b'p', round],
            Arc::clone(&start),
            Duration::from_secs(2),
        );
        let (host, peer) = (host.join().unwrap(), peer.join().unwrap());
        eprintln!("round {round}: host {host:?}, peer {peer:?}");
        assert!(
            host.is_ok() != peer.is_ok(),
            "exactly one device writes: host {host:?}, peer {peer:?}"
        );
        let loser = if host.is_ok() { peer } else { host };
        assert_eq!(loser, Err(libc::EBUSY), "the other is refused at once");
        let winner = if host.is_ok() { b'h' } else { b'p' };
        winners.push(winner);
        wait_for(
            &mut [&mut pair.host, &mut pair.peer],
            "both devices on the winner's version",
            || {
                std::thread::sleep(Duration::from_millis(100));
                let want = [winner, round];
                (std::fs::read(&f1).ok()?[..2] == want && std::fs::read(&f2).ok()?[..2] == want)
                    .then_some(())
            },
        );
        // The winner lets go; the next round starts from a free lease.
        wait_for(
            &mut [&mut pair.host, &mut pair.peer],
            "the lease released on both",
            || {
                std::thread::sleep(Duration::from_millis(100));
                (std::fs::OpenOptions::new().write(true).open(&f1).is_ok()
                    && std::fs::OpenOptions::new().write(true).open(&f2).is_ok())
                .then_some(())
            },
        );
    }
    eprintln!("winners: {:?}", String::from_utf8_lossy(&winners));
    // Alone, the peer asks the authority and is granted the lease.
    let start = Arc::new(std::sync::Barrier::new(1));
    let t = Instant::now();
    let peer = contend(f2, b"pz".to_vec(), start, Duration::ZERO);
    assert_eq!(peer.join().unwrap(), Ok(()));
    eprintln!(
        "the peer's first write waited {:?} for the grant",
        t.elapsed()
    );
    pair.peer.wait_line("granted by the lease authority");
    assert!(
        pair.host
            .log_count("granted the write lease of shared.db to device")
            >= 1
    );
    for daemon in [&pair.host, &pair.peer] {
        assert_eq!(daemon.log_count("fenced version"), 0, "no conflict");
        assert_eq!(daemon.log_count("tenancy its author did not hold"), 0);
    }
}

#[test]
fn a_version_from_a_tenancy_its_author_did_not_hold_is_kept_as_a_conflict_and_never_applied() {
    let Some((a, b)) = stores("native_fencing") else {
        return;
    };
    let mut pair = start_pair_with(
        &a,
        &b,
        &["--lease-authority"],
        &[("TILO_FS_TRANSPORT_GATE_SOCKET", "@profile-gate.sock")],
    );
    let host_gate = pair.dir.join("host-gate.sock");
    let peer_gate = pair.dir.join("peer-gate.sock");
    let content = pattern(10, 64 * 1024);
    lease_class_file(&mut pair, "data.db", &content);
    let f1 = pair.host.mount.join("data.db");
    let f2 = pair.peer.mount.join("data.db");

    // Cut off from each other, both devices write the free file: the host
    // is the lease authority and grants itself the lease; the peer cannot
    // reach it and takes the same epoch optimistically.
    gate(&host_gate, "partition");
    gate(&peer_gate, "partition");
    let start = Arc::new(std::sync::Barrier::new(2));
    let host_bytes = b"HOST-TENANCY".to_vec();
    let peer_bytes = b"PEER-OPTIMIS".to_vec();
    let host = contend(
        f1.clone(),
        host_bytes.clone(),
        Arc::clone(&start),
        Duration::from_millis(500),
    );
    let peer = contend(
        f2.clone(),
        peer_bytes.clone(),
        Arc::clone(&start),
        Duration::from_millis(500),
    );
    assert_eq!(host.join().unwrap(), Ok(()));
    assert_eq!(peer.join().unwrap(), Ok(()));
    pair.peer
        .wait_line("optimistically: the lease authority did not answer");
    // Each publishes its version while still cut off.
    std::thread::sleep(Duration::from_secs(8));
    gate(&host_gate, "reconnect");
    gate(&peer_gate, "reconnect");

    // The granted tenancy wins. The host never shows the peer's bytes; both
    // end on the host's version, and the peer's version is kept beside it
    // as a conflict copy on both devices.
    let (m1, m2) = (pair.host.mount.clone(), pair.peer.mount.clone());
    let t = Instant::now();
    let copy = wait_for(
        &mut [&mut pair.host, &mut pair.peer],
        "convergence on the host's version with the peer's kept",
        || {
            std::thread::sleep(Duration::from_millis(100));
            let on_host = std::fs::read(&f1).ok()?;
            assert!(
                on_host[..12] != peer_bytes[..],
                "the host never applies a version from a tenancy its author did not hold"
            );
            let on_peer = std::fs::read(&f2).ok()?;
            if on_host[..12] != host_bytes[..] || on_peer[..12] != host_bytes[..] {
                return None;
            }
            let copy = std::fs::read_dir(&m1)
                .ok()?
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .find(|n| n.starts_with("data.db.sync-conflict-"))?;
            let on_peer_copy = std::fs::read(m2.join(&copy)).ok()?;
            let on_host_copy = std::fs::read(m1.join(&copy)).ok()?;
            (on_host_copy[..12] == peer_bytes[..] && on_peer_copy[..12] == peer_bytes[..])
                .then_some(copy)
        },
    );
    eprintln!(
        "converged {:?} after reconnecting; conflict copy {copy}",
        t.elapsed()
    );
}
