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
    clippy::similar_names,
    clippy::unnested_or_patterns,
    clippy::doc_markdown,
    clippy::map_unwrap_or,
    clippy::redundant_closure,
    clippy::too_many_lines,
    clippy::needless_late_init,
    clippy::redundant_closure_for_method_calls,
    reason = "in a benchmark these are the assertion or fixture arithmetic, not a hazard"
)]

use std::io::{BufRead, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tilo_test_env::SpawnUnderEnvLock as _;

use ed25519_dalek::SigningKey;
use tilo_device_identity::{
    IdentityPaths, adopt_device_key,
    persist::{write_device_cert, write_public_key},
};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

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

/// Which local backend the daemons serve: the native Btrfs tree (needs root,
/// dedicated store volumes and the kernel's fanotify support) or the portable
/// FUSE backend, which runs anywhere a user can write and need not mount.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Native,
    Fuse,
}

fn backend() -> Backend {
    match std::env::var("TILO_FS_BENCH_BACKEND").as_deref() {
        Ok("fuse") | Ok("portable") => Backend::Fuse,
        _ => Backend::Native,
    }
}

/// The two store volumes, native only: the portable backend keeps its content
/// in each daemon's cache directory.
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
    if backend() == Backend::Fuse {
        return skip("the portable backend needs no store volumes");
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

/// [`stores`] for the ladder, which the portable backend runs too: its two
/// "volumes" are the daemons' cache roots, made by the pair.
fn ladder_selected(scenario: &str) -> bool {
    if std::env::var("TILO_FS_SYNC_BENCH").map_or(true, |v| v != "1") {
        eprintln!("skipping {scenario}: set TILO_FS_SYNC_BENCH=1 to run the sync benchmarks");
        return false;
    }
    if !selected(scenario) {
        eprintln!("skipping {scenario}: not selected by TILO_FS_BENCH_ONLY");
        return false;
    }
    true
}

fn bench_clock() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn mark(what: &str) {
    if std::env::var_os("TILO_FS_BENCH_VERBOSE").is_some() {
        eprintln!(
            "{:9.3} bench: {what}",
            bench_clock().elapsed().as_secs_f64()
        );
    }
}

fn install_identity(root: &Path, person_seed: [u8; 32], device_seed: [u8; 32], primary: bool) {
    let paths = IdentityPaths::under(root);
    paths.ensure_directories().unwrap();
    let person = SigningKey::from_bytes(&person_seed);
    let device = adopt_device_key(&paths, &device_seed).unwrap();
    write_public_key(&paths.user_pubkey, &person.verifying_key().to_bytes()).unwrap();
    let mut cert = DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "native-sync-bench".into(),
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
        issuer: None,
    };
    sign_device_cert(&mut cert, &person).unwrap();
    write_device_cert(&paths.device_cert, &cert).unwrap();
}

/// The serving daemon's Files authority, signed by the person's identity as `tilo-server
/// files-workspace-initialize` signs it: the daemon holds no root to sign one.
fn publish_authority(cache: &Path, person_seed: [u8; 32], device_seed: [u8; 32], acl: &Path) {
    use fs::workspace_authority::{WorkspaceAuthority, WorkspaceAuthorityEnvelope};
    let person = SigningKey::from_bytes(&person_seed);
    let device = SigningKey::from_bytes(&device_seed)
        .verifying_key()
        .to_bytes();
    let workspace = fs::Ticket::workspace_id_for_topic(device);
    let capabilities = fs::caps_file::authority(&std::fs::read(acl).unwrap(), acl).unwrap();
    std::fs::create_dir_all(cache).unwrap();
    let governor = person.verifying_key().to_bytes();
    WorkspaceAuthority::open(
        cache.join("workspace-authority.bin"),
        workspace,
        governor,
        governor,
        Vec::new(),
    )
    .unwrap()
    .apply(WorkspaceAuthorityEnvelope::sign(workspace, 1, capabilities, false, &person).unwrap())
    .unwrap();
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
            tilo_action_policy::Grantee::person(hex::encode(person)),
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

struct Link {
    names: [String; 2],
    devices: [String; 2],
}

impl Link {
    fn ip(args: &[&str]) {
        let status = Command::new("ip").args(args).status_locked().unwrap();
        assert!(status.success(), "ip {args:?} failed");
    }

    fn new(delay_ms: u64) -> Self {
        let tag = std::process::id();
        let names = [format!("tbench-a-{tag}"), format!("tbench-b-{tag}")];
        let (va, vb) = (format!("tba{tag}"), format!("tbb{tag}"));
        for ns in &names {
            let _ = Command::new("ip")
                .args(["netns", "del", ns])
                .status_locked();
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
                .status_locked()
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

    fn rx_bytes(&self, i: usize) -> u64 {
        let out = Command::new("ip")
            .args(["netns", "exec", &self.names[i], "cat"])
            .arg(format!(
                "/sys/class/net/{}/statistics/rx_bytes",
                self.devices[i]
            ))
            .output_locked()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        for ns in &self.names {
            let _ = Command::new("ip")
                .args(["netns", "del", ns])
                .status_locked();
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
    pid: u32,
}

impl Daemon {
    fn spawn(
        name: &'static str,
        dir: &Path,
        store: Option<&Path>,
        mode: &[&str],
        mount: &Path,
        netns: Option<&str>,
    ) -> Self {
        let cache = dir.join(format!("{name}-cache"));
        std::fs::create_dir_all(&cache).unwrap();
        // A mount the previous instance left answering nothing is cleared, as the daemon does.
        let _ = fs::fuse_session::clear_stale_mount(mount);
        std::fs::create_dir_all(mount).unwrap();
        let sockets = dir.join(format!("{name}-s"));
        std::fs::create_dir_all(&sockets).unwrap();
        let binary = std::env::var_os("TILO_FS_TEST_BINARY")
            .map_or_else(|| tilo_test_env::package_binary!("fs"), PathBuf::from);
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
            .arg(dir.join("acl.json"));
        match store {
            Some(store) => command.args(["--backend", "native", "--store"]).arg(store),
            None => command.args(["--backend", "fuse"]),
        };
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn_locked().unwrap();
        let pid = child.id();
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
                    let mut logs = logs.lock_recover();
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
            pid,
        }
    }

    fn dump(&self) -> String {
        let logs = self.logs.lock_recover();
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
        if backend() == Backend::Fuse {
            // The portable backend's daemon has served its workspace once FUSE answers.
            self.wait_line("Mounting QuickFS at ");
            return;
        }
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
        unsafe { libc::kill(pid, sig) };
    }

    /// The daemon's resident bytes, as its kernel accounting sees them.
    fn rss_bytes(&self) -> u64 {
        let status =
            std::fs::read_to_string(format!("/proc/{}/status", self.pid)).unwrap_or_default();
        status
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kb| kb.parse::<u64>().ok())
            .map_or(0, |kb| kb.saturating_mul(1024))
    }

    /// The daemon's plain (clean+anon) resident pages, which leave out its
    /// page-cache-shared file mappings better than VmRSS does.
    fn pss_bytes(&self) -> u64 {
        let rollup =
            std::fs::read_to_string(format!("/proc/{}/smaps_rollup", self.pid)).unwrap_or_default();
        rollup
            .lines()
            .find_map(|line| line.strip_prefix("Pss:"))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kb| kb.parse::<u64>().ok())
            .map_or(0, |kb| kb.saturating_mul(1024))
    }

    /// Stop the daemon as a service manager would and wait for it to exit.
    fn stop(&mut self, timeout: Duration) -> Duration {
        let timeout = std::env::var("TILO_FS_BENCH_STOP_SECS")
            .ok()
            .and_then(|secs| secs.parse::<u64>().ok())
            .map_or(timeout, Duration::from_secs);
        let started = Instant::now();
        self.signal(libc::SIGTERM);
        let deadline = Instant::now() + timeout;
        while self.child.try_wait().ok().flatten().is_none() {
            assert!(
                Instant::now() < deadline,
                "{} did not stop within {timeout:?}; logs:\n{}",
                self.name,
                self.dump()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        started.elapsed()
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
        let _ = Command::new("umount")
            .arg("-l")
            .arg(&self.mount)
            .status_locked();
    }
}

fn remove_workspace(live: &Path) {
    let Some(workspace) = live.file_name() else {
        return;
    };
    let Some(store) = live.parent().and_then(Path::parent) else {
        return;
    };
    let epochs = store.join(".epochs").join(workspace);
    if let Ok(entries) = std::fs::read_dir(&epochs) {
        for e in entries.flatten() {
            let _ = fs_native::btrfs::delete_subvolume(&e.path());
        }
    }
    let _ = std::fs::remove_dir(&epochs);
    let _ = fs_native::btrfs::delete_subvolume(live);
}

struct LiveWorkspaces(Vec<PathBuf>);

impl Drop for LiveWorkspaces {
    fn drop(&mut self) {
        for live in &self.0 {
            remove_workspace(live);
        }
    }
}

struct Pair {
    host: Daemon,
    peer: Option<Daemon>,
    _workspaces: LiveWorkspaces,
    link: Option<Link>,
    dir: PathBuf,
    store_a: Option<PathBuf>,
    store_b: Option<PathBuf>,
    ticket: String,
    _dir: tempfile::TempDir,
}

impl Pair {
    /// Stop the host as a service manager would; returns how long it took to exit.
    fn stop_host(&mut self) -> Duration {
        self.host.stop(Duration::from_secs(120))
    }

    /// Start the host again over the state it left, as a restart after an
    /// update or a machine reboot does; returns how long until it serves.
    fn restart_host(&mut self) -> Duration {
        let started = Instant::now();
        self.host = Daemon::spawn(
            "host",
            &self.dir,
            self.store_a.as_deref(),
            &["serve"],
            &self.dir.join("host-mount"),
            None,
        );
        // A serving host prints its ticket and then mounts; only a joining device says it joined.
        self.host.wait_ticket();
        self.host.wait_serving();
        started.elapsed()
    }

    fn peer_mount(&self) -> PathBuf {
        self.peer
            .as_ref()
            .expect("the pair's peer is serving")
            .mount
            .clone()
    }

    fn stop_peer(&mut self) -> Duration {
        self.peer
            .as_mut()
            .expect("the pair's peer is serving")
            .stop(Duration::from_secs(120))
    }

    fn restart_peer(&mut self) -> Duration {
        let started = Instant::now();
        let peer = Daemon::spawn(
            "peer",
            &self.dir,
            self.store_b.as_deref(),
            &["mount", "--ticket", &self.ticket],
            &self.dir.join("peer-mount"),
            None,
        );
        self.peer = Some(peer);
        let peer = self.peer.as_mut().expect("the peer was just started");
        peer.wait_line("Joined workspace");
        peer.wait_serving();
        started.elapsed()
    }

    /// Start the second device now, over everything the host has published;
    /// answers how long until it serves and how long until the whole tree is
    /// listed through its mount.
    fn join_peer(&mut self) -> (Duration, Duration) {
        let started = Instant::now();
        // A device joins by being given the tree's records, which the workspace's session
        // carries between devices. The build container runs no session, so the portable backend
        // hands the joiner the same documents the host keeps, exactly as the records would.
        if backend() == Backend::Fuse {
            let from = self.dir.join("host-cache").join("namespace");
            let to = self.dir.join("peer-cache").join("namespace");
            let _ = std::fs::remove_dir_all(&to);
            copy_tree(&from, &to).expect("the joiner is given the host's kept tree");
        }
        let mut peer = Daemon::spawn(
            "peer",
            &self.dir,
            self.store_b.as_deref(),
            &["mount", "--ticket", &self.ticket],
            &self.dir.join("peer-mount"),
            None,
        );
        peer.wait_line("Joined workspace");
        peer.wait_serving();
        let serving = started.elapsed();
        self.peer = Some(peer);
        let mount = self.peer_mount();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.host.alive();
            self.peer.as_mut().expect("the peer is serving").alive();
            if std::fs::read_dir(&mount).is_ok_and(|entries| entries.count() >= 3) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the joining peer never listed the workspace's tree"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        (serving, started.elapsed())
    }
}

fn start_pair(store_a: Option<&Path>, store_b: Option<&Path>, seed: u8) -> Pair {
    start_pair_with(store_a, store_b, seed, false)
}

fn peer_rx(pair: &Pair) -> Option<u64> {
    pair.link.as_ref().map(|link| link.rx_bytes(1))
}

fn start_pair_with(store_a: Option<&Path>, store_b: Option<&Path>, seed: u8, netns: bool) -> Pair {
    start_pair_joining(store_a, store_b, seed, netns, true)
}

fn start_pair_joining(
    store_a: Option<&Path>,
    store_b: Option<&Path>,
    seed: u8,
    netns: bool,
    join_at_start: bool,
) -> Pair {
    let base =
        std::env::var_os("TILO_FS_BENCH_DIR").map_or_else(|| "/var/tmp".into(), PathBuf::from);
    std::fs::create_dir_all(&base).unwrap();
    let temp = tempfile::tempdir_in(base).unwrap();
    let dir = temp.path().to_path_buf();
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
    publish_authority(
        &dir.join("host-cache"),
        person,
        [seed.wrapping_add(1); 32],
        &dir.join("acl.json"),
    );
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
    let peer = join_at_start.then(|| {
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
        peer
    });
    let workspaces = host
        .live
        .clone()
        .map(|live| LiveWorkspaces(vec![live]))
        .unwrap_or_else(|| LiveWorkspaces(Vec::new()));
    Pair {
        host,
        peer,
        _workspaces: workspaces,
        link,
        dir,
        store_a: store_a.map(Path::to_path_buf),
        store_b: store_b.map(Path::to_path_buf),
        ticket,
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
        if let Some(peer) = &mut pair.peer {
            peer.alive();
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

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
    let mut pair = start_pair(Some(&a), Some(&b), 0x71);
    let (m1, m2) = (pair.host.mount.clone(), pair.peer_mount());
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
    let mut pair = start_pair(Some(&a), Some(&b), 0x72);
    let (m1, m2) = (pair.host.mount.clone(), pair.peer_mount());
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

#[test]
fn incremental_edit() {
    let scenario = "edit";
    let Some((a, b)) = stores(scenario) else {
        return;
    };
    let size = env_u64("TILO_FS_BENCH_EDIT_MIB", 1024) * 1024 * 1024;
    let change = env_u64("TILO_FS_BENCH_EDIT_BYTES", 1024 * 1024).min(size);
    let mut pair = start_pair_with(Some(&a), Some(&b), 0x74, true);
    let (m1, m2) = (pair.host.mount.clone(), pair.peer_mount());
    let (host_file, target) = (m1.join("big.bin"), m2.join("big.bin"));
    write_big(&host_file, 9, size);
    wait_for(&mut pair, "the first version on the peer", || {
        (std::fs::metadata(&target).ok()?.len() == size).then_some(())
    });
    read_big(&target, 9, &[], size);
    let first_rx = peer_rx(&pair).unwrap();

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
        .output_locked()
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
    let mut pair = start_pair(Some(&a), Some(&b), 0x73);
    let repo1 = pair.host.mount.join("repo");
    let repo2 = pair.peer_mount().join("repo");
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

/// Copy every file under `from` to the same relative path under `to`.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Bytes of the files under `root`, without following links and without
/// reading file content.
fn disk_bytes(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut total = 0_u64;
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            total = total.saturating_add(disk_bytes(&entry.path()));
        } else if kind.is_file() {
            total = total.saturating_add(entry.metadata().map_or(0, |m| m.len()));
        }
    }
    total
}

fn list_directory(path: &Path) -> usize {
    std::fs::read_dir(path).map_or(0, |entries| entries.count())
}

fn read_range(path: &Path, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt as _;
    let file = std::fs::File::open(path)?;
    let mut bytes = vec![0_u8; len];
    file.read_exact_at(&mut bytes, offset)?;
    Ok(bytes)
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// A person copies a folder of many small files into the mounted workspace with
/// `cp -r`, reads back what the copy wrote, then restarts the daemon: every file
/// is there, with its bytes, before and after the restart.
///
/// Tunables: `TILO_FS_COPY_FILES` (default 10000), `TILO_FS_COPY_BYTES`
/// (default 2048), and the same selection and backend as the ladder
/// (`TILO_FS_SYNC_BENCH=1`, `TILO_FS_BENCH_BACKEND`).
#[test]
fn copying_a_folder_in_and_restarting() {
    let scenario = "copy";
    if !ladder_selected(scenario) {
        return;
    }
    let files = env_u64("TILO_FS_COPY_FILES", 10_000);
    let bytes = env_u64("TILO_FS_COPY_BYTES", 2_048);
    let folder = |i: u64| format!("d{:03}", i / 1_000);
    let name = |i: u64| format!("f{i:07}");
    let (store_a, store_b) = match backend() {
        Backend::Native => {
            let (a, b) = stores(scenario).expect("the native copy needs two store volumes");
            (Some(a), Some(b))
        }
        Backend::Fuse => (None, None),
    };
    let mut pair = start_pair_joining(store_a.as_deref(), store_b.as_deref(), 0x77, false, false);
    let source = pair.dir.join("source");
    let mut buf = vec![0_u8; usize::try_from(bytes).unwrap_or(2048)];
    for i in 0..files {
        let dir = source.join(folder(i));
        std::fs::create_dir_all(&dir).unwrap();
        fill(i, 0, &mut buf);
        std::fs::write(dir.join(name(i)), &buf).unwrap();
    }
    mark("the source folder is written");

    let into = pair.host.mount.join("in");
    let started = Instant::now();
    let copied = Command::new("cp")
        .arg("-r")
        .arg(&source)
        .arg(&into)
        .status_locked()
        .expect("run cp");
    let copy = started.elapsed();
    assert!(copied.success(), "cp -r into the mount failed: {copied:?}");

    let mut check = |root: &Path| -> (u64, u64) {
        let (mut missing, mut wrong) = (0_u64, 0_u64);
        for i in 0..files {
            let path = root.join("in").join(folder(i)).join(name(i));
            match std::fs::read(&path) {
                Ok(read) => {
                    fill(i, 0, &mut buf);
                    if read != buf {
                        wrong = wrong.saturating_add(1);
                    }
                }
                Err(_) => missing = missing.saturating_add(1),
            }
        }
        (missing, wrong)
    };
    let (missing, wrong) = check(&pair.host.mount);
    if missing + wrong > 0 {
        eprintln!(
            "copy: {} missing and {} wrong after the copy; writer's log:\n{}",
            missing,
            wrong,
            pair.host.dump()
        );
    }
    report(
        &rung_name(scenario, files),
        "copy_s",
        copy.as_secs_f64(),
        "s",
    );
    report(
        &rung_name(scenario, files),
        "files_per_s",
        files as f64 / copy.as_secs_f64(),
        "files/s",
    );
    report(
        &rung_name(scenario, files),
        "missing",
        missing as f64,
        "files",
    );
    report(&rung_name(scenario, files), "wrong", wrong as f64, "files");
    eprintln!(
        "copy: {files} files at {bytes} B took {copy:?} ({:.1} files/s); {missing} missing, \
         {wrong} with wrong bytes",
        files as f64 / copy.as_secs_f64()
    );

    let stopped = pair.stop_host();
    let restarted = pair.restart_host();
    let (missing_after, wrong_after) = check(&pair.host.mount);
    report(
        &rung_name(scenario, files),
        "restart_s",
        (stopped + restarted).as_secs_f64(),
        "s",
    );
    report(
        &rung_name(scenario, files),
        "missing_after_restart",
        missing_after as f64,
        "files",
    );
    report(
        &rung_name(scenario, files),
        "wrong_after_restart",
        wrong_after as f64,
        "files",
    );
    eprintln!(
        "copy: after restarting the writer ({stopped:?} to stop, {restarted:?} to serve), \
         {missing_after} missing, {wrong_after} with wrong bytes"
    );
    assert_eq!(
        (missing, wrong),
        (0, 0),
        "every file the copy wrote is in the workspace"
    );
    assert_eq!(
        (missing_after, wrong_after),
        (0, 0),
        "every file the copy wrote is still there after a restart"
    );
}

fn rung_name(scenario: &str, files: u64) -> String {
    format!("{scenario}/{files}")
}

/// A workspace holding a ladder's worth of files: many small ones and a few
/// huge ones. Everything here grows with the workspace's total size, so a
/// cost that grows with it instead of with what a request touches shows up as
/// the rungs' numbers diverging.
///
/// Tunables (environment):
///
/// - `TILO_FS_LADDER_TOTAL_MIB` (default 10240): the workspace's total bytes;
/// - `TILO_FS_LADDER_SMALL_FILES` (default four per MiB of the rung) and
///   `TILO_FS_LADDER_SMALL_BYTES` (default 4096): the many-small-files part;
/// - `TILO_FS_LADDER_HUGE_FILES` (default 4): the huge files the rest is
///   split across;
/// - `TILO_FS_LADDER_DIR_ENTRIES` (default 1000): the big folder's entries;
/// - `TILO_FS_BENCH_BACKEND=fuse` runs it on the portable backend, without
///   root or store volumes.
#[test]
fn a_workspace_of_files_on_a_size_ladder() {
    let scenario = "ladder";
    if !ladder_selected(scenario) {
        return;
    }
    let total_mib = env_u64("TILO_FS_LADDER_TOTAL_MIB", 10 * 1024);
    let small_files = env_u64("TILO_FS_LADDER_SMALL_FILES", total_mib.saturating_mul(4));
    let small_bytes = env_u64("TILO_FS_LADDER_SMALL_BYTES", 4096);
    let huge_files = env_u64("TILO_FS_LADDER_HUGE_FILES", 4).max(1);
    let dir_entries = env_u64("TILO_FS_LADDER_DIR_ENTRIES", 1_000);
    let total = total_mib.saturating_mul(1024 * 1024);
    let small_total = (small_files.saturating_mul(small_bytes)).min(total);
    let huge_total = total.saturating_sub(small_total);
    let huge_size = (huge_total / huge_files).max(1024 * 1024);
    let flat_entries = dir_entries.min(small_files.max(1));
    let rung = format!("ladder/{total_mib}mib");
    let (store_a, store_b) = match backend() {
        Backend::Native => {
            let (a, b) = stores(scenario).expect("the native ladder needs two store volumes");
            (Some(a), Some(b))
        }
        Backend::Fuse => (None, None),
    };
    let mut pair = start_pair_joining(store_a.as_deref(), store_b.as_deref(), 0x75, false, false);
    let m1 = pair.host.mount.clone();
    let host_rss_after_fill;
    let host_pss_after_fill;
    let host_disk_after_fill;

    // Many small files, one flat folder of `dir_entries` among them, then the
    // huge ones: the same shapes at every rung, more of them.
    let small_started = Instant::now();
    let mut buf = vec![0_u8; usize::try_from(small_bytes).unwrap_or(4096)];
    let mut small_written = 0_u64;
    for i in 0..small_files {
        let dir = if i < flat_entries {
            m1.join("flat")
        } else {
            m1.join(format!("small/d{:03}", i / 1000))
        };
        if let Err(error) = std::fs::create_dir_all(&dir) {
            panic!(
                "making {} before file {i} of {small_files}: {error} (the folder is {:?}); writer's log:\n{}",
                dir.display(),
                std::fs::metadata(&dir).map(|meta| meta.file_type()),
                pair.host.dump()
            );
        }
        fill(i, 0, &mut buf);
        let path = dir.join(format!("f{i:07}"));
        if let Err(error) = std::fs::write(&path, &buf) {
            panic!(
                "writing file {i} of {small_files}, {}: {error}; writer's log:\n{}",
                path.display(),
                pair.host.dump()
            );
        }
        // A file written and acknowledged is readable at once, or the write was lost where the
        // application could not see it.
        if i % 25 == 0 {
            match std::fs::read(&path) {
                Ok(read) if read == buf => {}
                Ok(read) => eprintln!(
                    "{}: readable right after writing, with {} B instead of {}",
                    path.display(),
                    read.len(),
                    buf.len()
                ),
                Err(error) => eprintln!(
                    "{}: not readable right after writing it: {error}",
                    path.display()
                ),
            }
        }
        small_written = small_written.saturating_add(small_bytes);
        if small_written >= small_total {
            break;
        }
    }
    let small_s = small_started.elapsed();
    let small_count = small_written / small_bytes.max(1);
    mark("every small file is written on the host");
    let huge_started = Instant::now();
    let mut huge_written = 0_u64;
    let mut huge = Vec::new();
    for i in 0..huge_files {
        if huge_written >= huge_total {
            break;
        }
        let size = huge_size.min(huge_total.saturating_sub(huge_written));
        let name = format!("huge{i:02}.bin");
        write_big(&m1.join(&name), 100 + i, size);
        huge.push((name, 100 + i, size));
        huge_written = huge_written.saturating_add(size);
        mark("a huge file is written on the host");
    }
    let huge_s = huge_started.elapsed();
    let expected = (small_written.saturating_add(huge_written)) as f64;
    report(rung.as_str(), "total_bytes", expected, "B");
    report(rung.as_str(), "put_small_s", small_s.as_secs_f64(), "s");
    report(
        rung.as_str(),
        "put_small_mib_s",
        mib(small_written) / small_s.as_secs_f64(),
        "MiB/s",
    );
    report(
        rung.as_str(),
        "put_small_files_s",
        (small_written / small_bytes.max(1)) as f64 / small_s.as_secs_f64(),
        "files/s",
    );
    report(rung.as_str(), "put_huge_s", huge_s.as_secs_f64(), "s");
    report(
        rung.as_str(),
        "put_huge_mib_s",
        mib(huge_written) / huge_s.as_secs_f64(),
        "MiB/s",
    );
    host_rss_after_fill = pair.host.rss_bytes();
    host_pss_after_fill = pair.host.pss_bytes();
    host_disk_after_fill = disk_bytes(&pair.dir.join("host-cache"));

    // What the writing device itself sees: a person browsing the folder they just filled.
    let started = Instant::now();
    let host_flat = list_directory(&m1.join("flat"));
    let host_list = started.elapsed();
    let host_root = list_directory(&m1);
    let host_small = read_range(&m1.join("flat").join("f0000000"), 0, 4096);
    if host_flat < flat_entries as usize || host_root < 3 {
        let written: std::collections::BTreeSet<String> =
            (0..flat_entries).map(|i| format!("f{i:07}")).collect();
        let mut missing = Vec::new();
        if let Ok(entries) = std::fs::read_dir(m1.join("flat")) {
            let seen: std::collections::BTreeSet<String> = entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect();
            missing.extend(written.difference(&seen).take(10).cloned());
        }
        eprintln!(
            "MISSING after writing {} files: {} of the flat folder, {} at the root; e.g. {:?}; \
             writer's log:\n{}",
            small_files,
            host_flat,
            host_root,
            missing,
            pair.host.dump()
        );
    }
    report(
        rung.as_str(),
        "host_flat_entries",
        host_flat as f64,
        "entries",
    );
    report(
        rung.as_str(),
        "host_list_flat_s",
        host_list.as_secs_f64(),
        "s",
    );
    report(
        rung.as_str(),
        "host_root_entries",
        host_root as f64,
        "entries",
    );
    report(
        rung.as_str(),
        "host_open_small_ok",
        f64::from(u8::from(host_small.is_ok())),
        "1",
    );

    // The second device joins over a filled workspace and is given its tree.
    let (serving, tree) = pair.join_peer();
    report(
        rung.as_str(),
        "peer_join_serving_s",
        serving.as_secs_f64(),
        "s",
    );
    report(rung.as_str(), "peer_join_tree_s", tree.as_secs_f64(), "s");
    let m2 = pair.peer_mount();

    // Listing a big folder and the root, on the device holding no content.
    let flat = m2.join("flat");
    let started = Instant::now();
    let cold_entries = list_directory(&flat);
    let cold_list = started.elapsed();
    let started = Instant::now();
    for _ in 0..8 {
        assert_eq!(list_directory(&flat), cold_entries);
    }
    let warm_list = started.elapsed().div_f64(8.0);
    let root_entries = list_directory(&m2);
    report(
        rung.as_str(),
        "list_flat_entries",
        cold_entries as f64,
        "entries",
    );
    report(
        rung.as_str(),
        "list_root_entries",
        root_entries as f64,
        "entries",
    );
    report(
        rung.as_str(),
        "peer_list_cold_s",
        cold_list.as_secs_f64(),
        "s",
    );
    report(
        rung.as_str(),
        "peer_list_warm_s",
        warm_list.as_secs_f64(),
        "s",
    );
    report(
        rung.as_str(),
        "peer_list_us_per_entry",
        cold_list.as_secs_f64() * 1e6 / cold_entries.max(1) as f64,
        "us",
    );

    // Opening one small file the device has never fetched, and one huge file:
    // the manifest and the first range, not the content.
    let peer_disk_before_reads = disk_bytes(&pair.dir.join("peer-cache"));
    // What joining cost the device: everything it holds once the tree shows, before it opens a file.
    report(
        rung.as_str(),
        "peer_join_fetched_bytes",
        peer_disk_before_reads as f64,
        "B",
    );
    let small_target = m2.join("flat").join("f0000000");
    let started = Instant::now();
    let small = read_range(&small_target, 0, 4096).unwrap_or_else(|error| {
        panic!(
            "the joining device holds {} of the {} files the writer holds ({error})",
            list_directory(&m2.join("flat")),
            host_flat
        )
    });
    let open_small = started.elapsed();
    assert_eq!(small.len(), 4096);
    let huge_target = m2.join("huge01.bin");
    let started = Instant::now();
    let first = read_range(&huge_target, 0, 65_536).unwrap();
    let open_huge = started.elapsed();
    assert_eq!(first.len(), 65_536);
    let after_first_read = disk_bytes(&pair.dir.join("peer-cache"));
    mark("one small file and one huge file's head are read on the peer");
    // A range the device has not touched at all, in a third of the file.
    let middle = huge_size / 2;
    let started = Instant::now();
    let mid = read_range(&huge_target, middle, 1024 * 1024).unwrap();
    let read_middle = started.elapsed();
    assert_eq!(mid.len(), 1024 * 1024);
    let after_middle = disk_bytes(&pair.dir.join("peer-cache"));
    report(
        rung.as_str(),
        "peer_open_small_s",
        open_small.as_secs_f64(),
        "s",
    );
    report(
        rung.as_str(),
        "peer_open_huge_s",
        open_huge.as_secs_f64(),
        "s",
    );
    report(
        rung.as_str(),
        "peer_read_unfetched_mib_s",
        1.0 / read_middle.as_secs_f64(),
        "MiB/s",
    );
    report(
        rung.as_str(),
        "peer_read_unfetched_s",
        read_middle.as_secs_f64(),
        "s",
    );
    report(
        rung.as_str(),
        "peer_fetched_bytes",
        after_middle.saturating_sub(peer_disk_before_reads) as f64,
        "B",
    );
    report(
        rung.as_str(),
        "peer_fetched_after_small_and_head",
        after_first_read.saturating_sub(peer_disk_before_reads) as f64,
        "B",
    );

    // What each end holds while the workspace is that big.
    let host_rss = pair.host.rss_bytes();
    let host_pss = pair.host.pss_bytes();
    let peer_rss = pair.peer.as_ref().expect("the peer is serving").rss_bytes();
    let peer_pss = pair.peer.as_ref().expect("the peer is serving").pss_bytes();
    report(rung.as_str(), "host_rss_mib", mib(host_rss), "MiB");
    report(rung.as_str(), "host_pss_mib", mib(host_pss), "MiB");
    report(rung.as_str(), "peer_rss_mib", mib(peer_rss), "MiB");
    report(rung.as_str(), "peer_pss_mib", mib(peer_pss), "MiB");
    report(
        rung.as_str(),
        "host_rss_after_fill_mib",
        mib(host_rss_after_fill),
        "MiB",
    );
    report(
        rung.as_str(),
        "host_pss_after_fill_mib",
        mib(host_pss_after_fill),
        "MiB",
    );
    report(
        rung.as_str(),
        "host_disk_after_fill_mib",
        mib(host_disk_after_fill),
        "MiB",
    );
    let side_bytes = |cache: &str, store: Option<&PathBuf>| {
        disk_bytes(&pair.dir.join(cache)).saturating_add(store.map_or(0, |store| disk_bytes(store)))
    };
    report(
        rung.as_str(),
        "host_disk_mib",
        mib(side_bytes("host-cache", pair.store_a.as_ref())),
        "MiB",
    );
    report(
        rung.as_str(),
        "peer_disk_mib",
        mib(side_bytes("peer-cache", pair.store_b.as_ref())),
        "MiB",
    );

    // Restarting both ends over the state they left.
    let stopped = pair.stop_host();
    let restarted = pair.restart_host();
    let started = Instant::now();
    let after_restart = list_directory(&pair.host.mount.join("flat"));
    let host_first_list = started.elapsed();
    assert_eq!(after_restart, cold_entries);
    report(rung.as_str(), "host_stop_s", stopped.as_secs_f64(), "s");
    report(
        rung.as_str(),
        "host_restart_s",
        restarted.as_secs_f64(),
        "s",
    );
    report(
        rung.as_str(),
        "host_restart_first_list_s",
        host_first_list.as_secs_f64(),
        "s",
    );
    let stopped = pair.stop_peer();
    let restarted = pair.restart_peer();
    let started = Instant::now();
    let _ = read_range(&pair.peer_mount().join("flat").join("f0000000"), 0, 4096).unwrap();
    let peer_first_read = started.elapsed();
    report(rung.as_str(), "peer_stop_s", stopped.as_secs_f64(), "s");
    report(
        rung.as_str(),
        "peer_restart_s",
        restarted.as_secs_f64(),
        "s",
    );
    report(
        rung.as_str(),
        "peer_restart_first_read_s",
        peer_first_read.as_secs_f64(),
        "s",
    );
    // Every file written, present and as written on both devices after both restarted
    // (`TILO_FS_LADDER_VERIFY_ALL=1`): it reads every small file and the ends and middle of each
    // huge one, so the joining device fetches them.
    if std::env::var("TILO_FS_LADDER_VERIFY_ALL").as_deref() == Ok("1") {
        let small = (small_count, flat_entries, small_bytes);
        for (side, root) in [
            ("host", pair.host.mount.clone()),
            ("peer", pair.peer_mount()),
        ] {
            let (missing, wrong) = verify_written(&root, small, &huge);
            report(
                rung.as_str(),
                &format!("{side}_files_checked"),
                (small_count + huge.len() as u64) as f64,
                "files",
            );
            report(
                rung.as_str(),
                &format!("{side}_missing_after_restart"),
                missing as f64,
                "files",
            );
            report(
                rung.as_str(),
                &format!("{side}_wrong_after_restart"),
                wrong as f64,
                "files",
            );
        }
    }
    mark("the ladder rung is measured");
    drop(pair);
}

/// How many of the ladder's files under `root` are missing or unreadable, and how many read back
/// other than written: each small file in full, and each huge file's size, first and last MiB and
/// a MiB from its middle.
fn verify_written(
    root: &Path,
    (count, flat_entries, bytes): (u64, u64, u64),
    huge: &[(String, u64, u64)],
) -> (u64, u64) {
    let (mut missing, mut wrong) = (0_u64, 0_u64);
    let mut want = vec![0_u8; usize::try_from(bytes).unwrap_or(4096)];
    for i in 0..count {
        let path = if i < flat_entries {
            root.join("flat").join(format!("f{i:07}"))
        } else {
            root.join(format!("small/d{:03}", i / 1000))
                .join(format!("f{i:07}"))
        };
        fill(i, 0, &mut want);
        match std::fs::read(&path) {
            Ok(got) if got == want => {}
            Ok(_) => {
                wrong += 1;
                if wrong <= 10 {
                    eprintln!("{}: reads back other than written", path.display());
                }
            }
            Err(error) => {
                missing += 1;
                if missing <= 10 {
                    eprintln!("{}: {error}", path.display());
                }
            }
        }
    }
    for (name, seed, size) in huge {
        let path = root.join(name);
        let len = std::fs::metadata(&path).map(|meta| meta.len());
        if len.as_ref().ok() != Some(size) {
            eprintln!("{}: {len:?} instead of {size} B", path.display());
            missing += 1;
            continue;
        }
        let span = (1024 * 1024).min(*size);
        let mut right = true;
        // The content is generated in 8-byte words, so a range starts on one.
        for offset in [
            0,
            (size / 2).saturating_sub(span / 2) & !7,
            (size - span) & !7,
        ] {
            let mut expected = vec![0_u8; usize::try_from(span).unwrap_or(0)];
            fill(*seed, offset, &mut expected);
            match read_range(&path, offset, expected.len()) {
                Ok(got) if got == expected => {}
                Ok(_) => right = false,
                Err(error) => {
                    eprintln!("{} at {offset}: {error}", path.display());
                    right = false;
                }
            }
        }
        if !right {
            eprintln!("{}: reads back other than written", path.display());
            wrong += 1;
        }
    }
    (missing, wrong)
}
