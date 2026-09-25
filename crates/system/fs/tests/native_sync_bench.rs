//! Sync throughput between two Tier 1 daemons, measured end to end.
//!
//! Each scenario starts two `fs` daemons (one person, two devices) on two
//! dedicated Btrfs store volumes, writes on one and waits until the other
//! shows the same bytes. Like `native_tier1`, it needs
//! `TILO_FS_NATIVE_STORES=/store/a:/store/b` and `CAP_SYS_ADMIN`; it also
//! only runs when `TILO_FS_SYNC_BENCH=1`, because a scenario takes minutes.
//!
//! Tunables (environment):
//!
//! - `TILO_FS_BENCH_FILES` / `TILO_FS_BENCH_FILE_BYTES`: the many-small-files
//!   scenario (default 1000 × 4096);
//! - `TILO_FS_BENCH_BIG_MIB`: the one-large-file scenario (default 1024);
//! - `TILO_FS_BENCH_GIT_FILES`: files per commit in the git scenario
//!   (default 200);
//! - `TILO_FS_BENCH_DELAY_MS`: one-way delay between the devices. Non-zero
//!   runs each daemon in its own network namespace joined by a veth pair
//!   with `netem` on both ends (needs `ip` and `tc`);
//! - `TILO_FS_BENCH_NETNS=1`: use the veth pair even without a delay, so
//!   the bytes the peer received are counted (`peer_rx_bytes`);
//! - `TILO_FS_BENCH_EDIT_MIB` / `TILO_FS_BENCH_EDIT_BYTES`: the incremental
//!   edit scenario (default 1 MiB changed in the middle of a 1024 MiB file;
//!   it always runs on the veth pair to count bytes);
//! - `TILO_FS_BENCH_ONLY`: comma-separated scenario names to run;
//! - `TILO_FS_BENCH_DIR`: where caches and mount points go (default
//!   `/var/tmp`; the content store holds a copy of everything synced).
//!
//! Results are printed as `BENCH <scenario> <metric> <value> <unit>` lines.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "in a benchmark these are the assertion or fixture arithmetic, not a hazard"
)]

use std::io::{BufRead, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, adopt_device_key,
    persist::{write_device_cert, write_public_key, write_signing_key},
    sign_device_cert,
};

const TIMEOUT: Duration = Duration::from_secs(1800);

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn selected(scenario: &str) -> bool {
    std::env::var("TILO_FS_BENCH_ONLY")
        .map_or(true, |only| only.split(',').any(|s| s.trim() == scenario))
}

/// The two store volumes, or `None` (after saying why) when this machine
/// cannot or should not run the benchmark.
fn stores(scenario: &str) -> Option<(PathBuf, PathBuf)> {
    let skip = |why: &str| {
        eprintln!("skipping {scenario}: {why}");
        None
    };
    if std::env::var("TILO_FS_SYNC_BENCH").map_or(true, |v| v != "1") {
        return skip("set TILO_FS_SYNC_BENCH=1 to run the sync benchmarks");
    }
    if !selected(scenario) {
        return skip("not selected by TILO_FS_BENCH_ONLY");
    }
    let spec = std::env::var("TILO_FS_NATIVE_STORES")
        .expect("TILO_FS_SYNC_BENCH=1 needs TILO_FS_NATIVE_STORES=/store/a:/store/b");
    let (a, b) = spec
        .split_once(':')
        .expect("TILO_FS_NATIVE_STORES names two volumes");
    assert!(
        fs_native::sys::has_capability(fs_native::sys::CAP_SYS_ADMIN),
        "the sync benchmarks need CAP_SYS_ADMIN"
    );
    for store in [a, b] {
        fs_native::store::check_dedicated(Path::new(store))
            .unwrap_or_else(|e| panic!("{store} is not a dedicated store volume: {e}"));
    }
    Some((PathBuf::from(a), PathBuf::from(b)))
}

/// When the benchmark process started; verbose daemon logs are stamped
/// with the time since.
fn bench_clock() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// A stamped marker among the verbose daemon logs.
fn mark(what: &str) {
    if std::env::var_os("TILO_FS_BENCH_VERBOSE").is_some() {
        eprintln!(
            "{:9.3} bench: {what}",
            bench_clock().elapsed().as_secs_f64()
        );
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX)
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
        device_label: "native-sync-bench".into(),
        capabilities: if primary {
            DeviceCapabilities::primary()
        } else {
            DeviceCapabilities::member()
        },
        issued_at_ns: now_ns().saturating_sub(1_000_000_000),
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
    let caps: Vec<collab_types::CapOverride> =
        [collab_types::Verb::Read, collab_types::Verb::Write]
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
    std::fs::write(path, serde_json::to_vec(&caps).unwrap()).unwrap();
}

/// Two network namespaces joined by a veth pair with a fixed one-way delay
/// on both ends, removed on drop.
struct Link {
    names: [String; 2],
    devices: [String; 2],
}

impl Link {
    fn ip(args: &[&str]) {
        let status = Command::new("ip").args(args).status().unwrap();
        assert!(status.success(), "ip {args:?} failed");
    }

    fn new(delay_ms: u64) -> Self {
        let tag = std::process::id();
        let names = [format!("tbench-a-{tag}"), format!("tbench-b-{tag}")];
        let (va, vb) = (format!("tba{tag}"), format!("tbb{tag}"));
        for ns in &names {
            let _ = Command::new("ip").args(["netns", "del", ns]).status();
            Self::ip(&["netns", "add", ns]);
            Self::ip(&["netns", "exec", ns, "ip", "link", "set", "lo", "up"]);
        }
        Self::ip(&["link", "add", &va, "type", "veth", "peer", "name", &vb]);
        for ((ns, dev), addr) in names
            .iter()
            .zip([&va, &vb])
            .zip(["10.231.0.1/24", "10.231.0.2/24"])
        {
            Self::ip(&["link", "set", dev, "netns", ns]);
            Self::ip(&["netns", "exec", ns, "ip", "addr", "add", addr, "dev", dev]);
            Self::ip(&["netns", "exec", ns, "ip", "link", "set", dev, "up"]);
            let delay = format!("{delay_ms}ms");
            let status = Command::new("ip")
                .args(["netns", "exec", ns, "tc", "qdisc", "add", "dev", dev])
                .args(["root", "netem", "delay", &delay, "limit", "100000"])
                .status()
                .unwrap();
            assert!(status.success(), "tc netem failed");
        }
        Self {
            names,
            devices: [va, vb],
        }
    }

    fn netns(&self, i: usize) -> &str {
        &self.names[i]
    }

    /// Bytes received so far by the device in namespace `i`.
    fn rx_bytes(&self, i: usize) -> u64 {
        let out = Command::new("ip")
            .args(["netns", "exec", &self.names[i], "cat"])
            .arg(format!(
                "/sys/class/net/{}/statistics/rx_bytes",
                self.devices[i]
            ))
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        for ns in &self.names {
            let _ = Command::new("ip").args(["netns", "del", ns]).status();
        }
    }
}

struct Daemon {
    name: &'static str,
    child: Child,
    lines: Receiver<String>,
    logs: Arc<Mutex<Vec<String>>>,
    mount: PathBuf,
    live: Option<PathBuf>,
}

impl Daemon {
    fn spawn(
        name: &'static str,
        dir: &Path,
        store: &Path,
        mode: &[&str],
        mount: &Path,
        netns: Option<&str>,
    ) -> Self {
        let cache = dir.join(format!("{name}-cache"));
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(mount).unwrap();
        let sockets = dir.join(format!("{name}-s"));
        std::fs::create_dir_all(&sockets).unwrap();
        let binary = std::env::var_os("TILO_FS_TEST_BINARY")
            .map_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_fs")), PathBuf::from);
        // Only the network namespace changes: the daemon's bind mount must
        // stay visible to this process.
        let mut command = match netns {
            Some(ns) => {
                let mut c = Command::new("nsenter");
                c.arg(format!("--net=/run/netns/{ns}"))
                    .arg("--")
                    .arg(binary);
                c
            }
            None => Command::new(binary),
        };
        command
            .env(
                fs::identity::IDENTITY_ROOT_ENV,
                dir.join(format!("{name}-identity")),
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
        let verbose = std::env::var_os("TILO_FS_BENCH_VERBOSE").is_some();
        for stream in [
            Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
            Box::new(child.stderr.take().unwrap()) as Box<dyn Read + Send>,
        ] {
            let tx = tx.clone();
            let logs = Arc::clone(&logs);
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    if verbose {
                        let at = bench_clock().elapsed().as_secs_f64();
                        eprintln!("{at:9.3} {name}: {line}");
                    }
                    let mut logs = logs
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if logs.len() < 200_000 {
                        logs.push(line.clone());
                    }
                    drop(logs);
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

    fn dump(&self) -> String {
        let logs = self
            .logs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let skip = logs.len().saturating_sub(200);
        logs[skip..].join("\n")
    }

    fn alive(&mut self) {
        if let Some(status) = self.child.try_wait().unwrap() {
            panic!("{} exited {status}; logs:\n{}", self.name, self.dump());
        }
    }

    fn wait_line(&mut self, expected: &str) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.alive();
            match self.lines.recv_timeout(Duration::from_millis(200)) {
                Ok(line) if line.contains(expected) => return line,
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("{}: its output closed; logs:\n{}", self.name, self.dump())
                }
            }
            assert!(
                Instant::now() < deadline,
                "{}: no line containing {expected:?}; logs:\n{}",
                self.name,
                self.dump()
            );
        }
    }

    fn wait_serving(&mut self) {
        let line = self.wait_line("Serving workspace natively from ");
        let rest = line
            .split("Serving workspace natively from ")
            .nth(1)
            .unwrap();
        self.live = Some(PathBuf::from(rest.split(" at ").next().unwrap()));
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
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.signal(libc::SIGTERM);
            let deadline = Instant::now() + Duration::from_secs(30);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = Command::new("umount").arg("-l").arg(&self.mount).status();
    }
}

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

/// Two devices of one person. Dropped in field order: daemons first, then
/// their rooms, then the link and the directory.
struct Pair {
    host: Daemon,
    peer: Daemon,
    _rooms: Rooms,
    link: Option<Link>,
    _dir: tempfile::TempDir,
}

fn start_pair(store_a: &Path, store_b: &Path, seed: u8) -> Pair {
    start_pair_with(store_a, store_b, seed, false)
}

/// Bytes the peer has received over the link, when there is one.
fn peer_rx(pair: &Pair) -> Option<u64> {
    pair.link.as_ref().map(|link| link.rx_bytes(1))
}

fn start_pair_with(store_a: &Path, store_b: &Path, seed: u8, netns: bool) -> Pair {
    let base =
        std::env::var_os("TILO_FS_BENCH_DIR").map_or_else(|| "/var/tmp".into(), PathBuf::from);
    let temp = tempfile::tempdir_in(base).unwrap();
    let dir = temp.path().to_path_buf();
    // Identities (and so room ids) differ per run: a run never reuses a
    // room another run left or still uses.
    let salt = u8::try_from(std::process::id() % 83).unwrap_or(0);
    let seed = seed.wrapping_add(salt.wrapping_mul(3));
    let person = [seed; 32];
    install_identity(
        &dir.join("host-identity"),
        person,
        [seed.wrapping_add(1); 32],
        true,
    );
    install_identity(
        &dir.join("peer-identity"),
        person,
        [seed.wrapping_add(2); 32],
        false,
    );
    full_access_acl(&dir.join("acl.json"), person);
    let delay = env_u64("TILO_FS_BENCH_DELAY_MS", 0);
    let netns = netns || env_u64("TILO_FS_BENCH_NETNS", 0) == 1;
    let link = (delay > 0 || netns).then(|| Link::new(delay));
    let mut host = Daemon::spawn(
        "host",
        &dir,
        store_a,
        &["serve"],
        &dir.join("host-mount"),
        link.as_ref().map(|l| l.netns(0)),
    );
    let ticket = host.wait_ticket();
    host.wait_serving();
    let mut peer = Daemon::spawn(
        "peer",
        &dir,
        store_b,
        &["mount", "--ticket", &ticket],
        &dir.join("peer-mount"),
        link.as_ref().map(|l| l.netns(1)),
    );
    peer.wait_line("Joined workspace");
    peer.wait_serving();
    let rooms = Rooms(vec![host.live.clone().unwrap(), peer.live.clone().unwrap()]);
    Pair {
        host,
        peer,
        _rooms: rooms,
        link,
        _dir: temp,
    }
}

fn wait_for<T>(pair: &mut Pair, what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(v) = probe() {
            return v;
        }
        pair.host.alive();
        pair.peer.alive();
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Deterministic pseudo-random bytes: distinct per `(seed, offset)` so no
/// two chunks deduplicate.
fn fill(seed: u64, offset: u64, out: &mut [u8]) {
    for (i, word) in out.chunks_mut(8).enumerate() {
        let mut x = (offset / 8 + i as u64)
            .wrapping_add(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15))
            .wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 31;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 29;
        let bytes = x.to_le_bytes();
        word.copy_from_slice(&bytes[..word.len()]);
    }
}

fn report(scenario: &str, metric: &str, value: f64, unit: &str) {
    println!("BENCH {scenario} {metric} {value:.3} {unit}");
}

fn small_path(root: &Path, i: u64) -> PathBuf {
    root.join(format!("small/d{:03}/f{i:06}", i / 100))
}

#[test]
fn many_small_files() {
    let scenario = "small";
    let Some((a, b)) = stores(scenario) else {
        return;
    };
    let files = env_u64("TILO_FS_BENCH_FILES", 1000);
    let bytes = env_u64("TILO_FS_BENCH_FILE_BYTES", 4096) as usize;
    let mut pair = start_pair(&a, &b, 0x71);
    let (m1, m2) = (pair.host.mount.clone(), pair.peer.mount.clone());
    let t0 = Instant::now();
    let mut buf = vec![0u8; bytes];
    for i in 0..files {
        let p = small_path(&m1, i);
        if i % 100 == 0 {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        }
        fill(i, 0, &mut buf);
        std::fs::write(&p, &buf).unwrap();
    }
    let written = t0.elapsed();
    mark("every small file is written on the host");
    let last = small_path(&m2, files - 1);
    wait_for(&mut pair, "the last small file on the peer", || {
        (std::fs::metadata(&last).ok()?.len() == bytes as u64).then_some(())
    });
    let visible = t0.elapsed();
    mark("the last small file is on the peer");
    // Every file, with the author's bytes.
    wait_for(&mut pair, "every small file on the peer", || {
        (0..files)
            .all(|i| {
                std::fs::read(small_path(&m2, i)).is_ok_and(|got| {
                    fill(i, 0, &mut buf);
                    got == buf
                })
            })
            .then_some(())
    });
    let complete = t0.elapsed();
    let total = (files as f64) * bytes as f64;
    report(scenario, "files", files as f64, "files");
    report(scenario, "write", written.as_secs_f64(), "s");
    report(scenario, "last_visible", visible.as_secs_f64(), "s");
    report(scenario, "complete", complete.as_secs_f64(), "s");
    report(
        scenario,
        "files_per_s",
        files as f64 / complete.as_secs_f64(),
        "files/s",
    );
    report(
        scenario,
        "throughput",
        total / complete.as_secs_f64() / 1e6,
        "MB/s",
    );
}

/// Bytes of `(seed, offset)` a large-file scenario writes, with each
/// `(edit seed, start, len)` range overlaid by the edit seed's bytes.
fn expected(seed: u64, edits: &[(u64, u64, u64)], offset: u64, out: &mut [u8]) {
    fill(seed, offset, out);
    let end = offset + out.len() as u64;
    for &(edit_seed, start, len) in edits {
        let (from, to) = (start.max(offset), (start + len).min(end));
        if from < to {
            let slice = &mut out[(from - offset) as usize..(to - offset) as usize];
            fill(edit_seed, from, slice);
        }
    }
}

const BLOCK: usize = 8 * 1024 * 1024;

/// Writes `size` bytes of `seed` at `path` and syncs them.
fn write_big(path: &Path, seed: u64, size: u64) {
    let mut buf = vec![0u8; BLOCK];
    let mut f = std::fs::File::create(path).unwrap();
    let mut off = 0u64;
    while off < size {
        let n = BLOCK.min((size - off) as usize);
        fill(seed, off, &mut buf[..n]);
        f.write_all(&buf[..n]).unwrap();
        off += n as u64;
    }
    f.sync_all().unwrap();
}

/// Reads `path` from start to end and checks every byte.
fn read_big(path: &Path, seed: u64, edits: &[(u64, u64, u64)], size: u64) {
    let mut f = std::fs::File::open(path).unwrap();
    let mut got = vec![0u8; BLOCK];
    let mut want = vec![0u8; BLOCK];
    let mut off = 0u64;
    while off < size {
        let n = BLOCK.min((size - off) as usize);
        f.read_exact(&mut got[..n]).unwrap();
        expected(seed, edits, off, &mut want[..n]);
        if got[..n] != want[..n] {
            let first = got[..n]
                .iter()
                .zip(&want[..n])
                .position(|(a, b)| a != b)
                .unwrap();
            let last = n - got[..n]
                .iter()
                .zip(&want[..n])
                .rev()
                .position(|(a, b)| a != b)
                .unwrap();
            let zeros = got[first..last].iter().all(|b| *b == 0);
            panic!(
                "bytes differ in {}..{} (read block at {off}; the wrong bytes are {})",
                off + first as u64,
                off + last as u64,
                if zeros { "all zero" } else { "not zero" }
            );
        }
        off += n as u64;
    }
}

#[test]
fn one_large_file() {
    let scenario = "large";
    let Some((a, b)) = stores(scenario) else {
        return;
    };
    let mib = env_u64("TILO_FS_BENCH_BIG_MIB", 1024);
    let size = mib * 1024 * 1024;
    let mut pair = start_pair(&a, &b, 0x72);
    let (m1, m2) = (pair.host.mount.clone(), pair.peer.mount.clone());
    let rx0 = peer_rx(&pair);
    let t0 = Instant::now();
    write_big(&m1.join("big.bin"), 7, size);
    let written = t0.elapsed();
    let target = m2.join("big.bin");
    wait_for(&mut pair, "the large file on the peer", || {
        (std::fs::metadata(&target).ok()?.len() == size).then_some(())
    });
    let visible = t0.elapsed();
    eprintln!("reading the large file cold on the peer");
    let t1 = Instant::now();
    read_big(&target, 7, &[], size);
    let read = t1.elapsed();
    report(scenario, "bytes", size as f64, "B");
    report(scenario, "write", written.as_secs_f64(), "s");
    report(scenario, "visible", visible.as_secs_f64(), "s");
    report(scenario, "cold_read", read.as_secs_f64(), "s");
    report(
        scenario,
        "cold_read_throughput",
        size as f64 / read.as_secs_f64() / 1e6,
        "MB/s",
    );
    report(
        scenario,
        "end_to_end_throughput",
        size as f64 / (visible + read).as_secs_f64() / 1e6,
        "MB/s",
    );
    if let (Some(before), Some(after)) = (rx0, peer_rx(&pair)) {
        report(scenario, "peer_rx_bytes", (after - before) as f64, "B");
    }
}

/// A large file that is already on both devices changes in a small range:
/// the author saves it the usual way (a new file renamed over the old
/// one). Reports how long the new version takes to show and to read, and
/// how many bytes the peer received for it.
#[test]
fn incremental_edit() {
    let scenario = "edit";
    let Some((a, b)) = stores(scenario) else {
        return;
    };
    let size = env_u64("TILO_FS_BENCH_EDIT_MIB", 1024) * 1024 * 1024;
    let change = env_u64("TILO_FS_BENCH_EDIT_BYTES", 1024 * 1024).min(size);
    let mut pair = start_pair_with(&a, &b, 0x74, true);
    let (m1, m2) = (pair.host.mount.clone(), pair.peer.mount.clone());
    let (host_file, target) = (m1.join("big.bin"), m2.join("big.bin"));
    write_big(&host_file, 9, size);
    wait_for(&mut pair, "the first version on the peer", || {
        (std::fs::metadata(&target).ok()?.len() == size).then_some(())
    });
    read_big(&target, 9, &[], size);
    let first_rx = peer_rx(&pair).unwrap();

    // Save the edited file: a copy with the change applied, synced and
    // renamed over the original.
    let at = size / 2 + 12_345;
    let edits = [(10, at, change)];
    let staged = m1.join(".big.bin.save");
    let t0 = Instant::now();
    {
        let mut buf = vec![0u8; BLOCK];
        let mut f = std::fs::File::create(&staged).unwrap();
        let mut off = 0u64;
        while off < size {
            let n = BLOCK.min((size - off) as usize);
            expected(9, &edits, off, &mut buf[..n]);
            f.write_all(&buf[..n]).unwrap();
            off += n as u64;
        }
        f.sync_all().unwrap();
    }
    std::fs::rename(&staged, &host_file).unwrap();
    let saved = t0.elapsed();
    let mut probe = vec![0u8; change as usize];
    let mut want = vec![0u8; change as usize];
    expected(9, &edits, at, &mut want);
    wait_for(&mut pair, "the edited version on the peer", || {
        use std::os::unix::fs::FileExt as _;
        let f = std::fs::File::open(&target).ok()?;
        f.read_exact_at(&mut probe, at).ok()?;
        (probe == want).then_some(())
    });
    let visible = t0.elapsed();
    let t1 = Instant::now();
    read_big(&target, 9, &edits, size);
    let read = t1.elapsed();
    let rx = peer_rx(&pair).unwrap() - first_rx;
    report(scenario, "bytes", size as f64, "B");
    report(scenario, "changed", change as f64, "B");
    report(scenario, "save", saved.as_secs_f64(), "s");
    report(scenario, "edit_visible", visible.as_secs_f64(), "s");
    report(scenario, "edit_read", read.as_secs_f64(), "s");
    report(scenario, "peer_rx_bytes", rx as f64, "B");
    report(
        scenario,
        "rx_per_changed_byte",
        rx as f64 / change as f64,
        "x",
    );
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "core.trustctime=false",
            "-c",
            "core.checkStat=minimal",
        ])
        .args(["-c", "gc.auto=0", "-c", "safe.directory=*"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "Bench")
        .env("GIT_AUTHOR_EMAIL", "bench@example.invalid")
        .env("GIT_COMMITTER_NAME", "Bench")
        .env("GIT_COMMITTER_EMAIL", "bench@example.invalid")
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
fn git_repository() {
    let scenario = "git";
    let Some((a, b)) = stores(scenario) else {
        return;
    };
    let per_commit = env_u64("TILO_FS_BENCH_GIT_FILES", 200);
    let mut pair = start_pair(&a, &b, 0x73);
    let repo1 = pair.host.mount.join("repo");
    let repo2 = pair.peer.mount.join("repo");
    std::fs::create_dir_all(&repo1).unwrap();
    let t0 = Instant::now();
    git(&repo1, &["init", "-q", "-b", "main"]).unwrap();
    let mut buf = vec![0u8; 2048];
    for round in 0..3u64 {
        for i in 0..per_commit {
            let p = repo1.join(format!("src/m{:02}/f{i:04}.rs", i / 50));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            fill(round * 100_000 + i, 0, &mut buf);
            let text: String = buf.iter().map(|b| char::from(b'a' + b % 26)).collect();
            std::fs::write(p, format!("// round {round}\n{text}\n")).unwrap();
        }
        git(&repo1, &["add", "-A"]).unwrap();
        git(&repo1, &["commit", "-q", "-m", &format!("round {round}")]).unwrap();
    }
    let head = git(&repo1, &["rev-parse", "HEAD"]).unwrap();
    let committed = t0.elapsed();
    wait_for(&mut pair, "the host's HEAD on the peer", || {
        (git(&repo2, &["rev-parse", "HEAD"]).ok()? == head).then_some(())
    });
    let head_visible = t0.elapsed();
    wait_for(&mut pair, "a clean status on the peer", || {
        std::thread::sleep(Duration::from_millis(100));
        git(&repo2, &["status", "--porcelain"])
            .ok()
            .filter(String::is_empty)
    });
    let clean = t0.elapsed();
    git(&repo2, &["fsck", "--strict", "--no-dangling"]).unwrap();
    report(scenario, "files_per_commit", per_commit as f64, "files");
    report(scenario, "committed", committed.as_secs_f64(), "s");
    report(scenario, "head_visible", head_visible.as_secs_f64(), "s");
    report(scenario, "clean", clean.as_secs_f64(), "s");
    report(
        scenario,
        "sync_after_commit",
        clean.saturating_sub(committed).as_secs_f64(),
        "s",
    );
}
