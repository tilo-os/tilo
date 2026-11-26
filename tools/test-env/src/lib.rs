//! Process-environment fixtures for tests. The environment is process-global, and `cargo test` runs
//! every test of a binary as a thread of one process. A test that sets `XDG_DATA_HOME` and forgets
//! to put it back, or restores it by removing a value that was set before it ran, leaks into
//! whichever test runs next, and a test that reads the variable while another thread is writing it
//! sees either value. Prefer not to need this crate: an API that takes its root directory as an
//! argument (`open_at(root, …)`, `*_in(root, …)`) needs no environment at all and its tests can run
//! in parallel. Use the fixtures here only where the environment is the interface under test, such
//! as the function that resolves `XDG_DATA_HOME` itself. [`ScopedEnv`], [`pin`], [`TestHome`],
//! [`TestEnv`] and [`lock`] cover the environment; the other modules cover the machine a test used
//! to reach for: [`net`], [`idle`], [`hang`], [`script`], [`swtpm`], [`capability`] and
//! [`desktop`]. The lock only orders code that takes it. Code under test that reads the environment
//! from a thread of its own is still racing any other test that mutates the variable without this
//! crate.

pub mod allocation;
mod binary;
#[cfg(unix)]
mod bus;
mod capability;
mod desktop;
#[cfg(target_os = "linux")]
pub mod disk_sync;
pub mod environment;
#[cfg(target_os = "linux")]
pub mod file_mode;
mod fixture;
pub mod hang;
pub mod held;
pub mod idle;
pub mod net;
mod own_process;
pub mod requested_allocations;
pub mod scenario;
#[cfg(unix)]
mod stand_in;
pub mod swtpm;
mod wall;

pub use binary::{binary_if_built, sibling_binary};
#[cfg(unix)]
pub use bus::PrivateBus;
pub use capability::{Capability, Probe, capability_unavailable};
#[cfg(target_os = "linux")]
pub use capability::{
    NamedProbe, PROBES, bubblewrap, bubblewrap_overlay, bubblewrap_with_proc, mount_namespace,
    network_tunnel, non_root_identity, root_identity, user_namespace,
};
#[cfg(unix)]
pub use capability::{chown_to, switch_identity};
pub use desktop::{TEST_ENVIRONMENT, desktop_isolation, require_isolated_desktop};
pub use environment::{Clock, Network, TestEnv};
pub use fixture::{Fixture, WORKSPACE_DOCUMENT};
#[cfg(target_os = "linux")]
pub use hang::exited;
pub use hang::{
    Within, hang_bound, hang_deadline, recv, wait_for, wait_for_within, wait_until, within,
};
#[cfg(feature = "document")]
pub use net::document::{FakeDocumentNet, at_path};
pub use net::{Converged, FakeNet, Peer};
pub use own_process::{in_own_process, in_own_process_here};
#[cfg(unix)]
pub use stand_in::{StandIn, stand_in};
pub use wall::held_wall_clock;

use hang::HANG_BOUND_ENV;
use std::ffi::{OsStr, OsString};
use std::marker::PhantomData;
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};
use std::{env, fs, io};
use tilo_lock::MutexRecover as _;

#[macro_export]
macro_rules! scenario {
    ($(#[$meta:meta])* fn $name:ident; $($rest:tt)*) => {
        #[test]
        // Clippy exempts a test function's body from arithmetic_side_effects but not the
        // closure each phase runs in, and a scenario is its phases one after another.
        #[allow(
            clippy::arithmetic_side_effects,
            clippy::too_many_lines,
            reason = "an overflow in a phase fails the test, and a scenario's length is its phases"
        )]
        $(#[$meta])*
        fn $name() {
            $crate::__scenario! {
                @cases [] @rest
                $($rest)*
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scenario {
    (
        @cases [$($case:tt)*] @rest
        $kind:ident $($name:ident)? ; { $($body:tt)* } $($rest:tt)*
    ) => {
        $crate::__scenario! {
            @cases [
                $($case)*
                (
                    $crate::__scenario_name!($kind $($name)?),
                    ::core::concat!(::core::file!(), ":", ::core::line!()),
                    { $($body)* }
                )
            ]
            @rest $($rest)*
        }
    };

    (@cases [$(($name:expr, $location:expr, $body:tt))*] @rest) => {
        $crate::scenario::run(::std::vec![
            $(
                $crate::__scenario_phase!($name, $location, $body)
            ),*
        ])
        .assert_green()
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scenario_phase {
    ($name:expr, $location:expr, { $($body:tt)* }) => {
        $crate::scenario::Phase::new($name, $location, || { $($body)* })
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __scenario_name {
    (phase $name:ident) => {
        ::core::stringify!($name)
    };
    (step $name:ident) => {
        ::core::stringify!($name)
    };
    (steps) => {
        "steps"
    };
}

#[macro_export]
macro_rules! package_binary {
    ($name:literal) => {
        $crate::package_binary_at($name, env!(concat!("CARGO_BIN_EXE_", $name)))
    };
}

#[must_use]
pub fn package_binary_at(name: &str, built_at: &str) -> PathBuf {
    env::var_os(format!("NEXTEST_BIN_EXE_{name}"))
        .map_or_else(|| PathBuf::from(built_at), PathBuf::from)
}

/// The build of this package's own binary `bin` that the run made with another
/// profile and features, as a `starts` entry in `t.toml` asks for it
/// (`package:bin@profile=features`): t hands it to the tests as
/// `T_STARTED_BIN_<bin>_<profile>_<features>`, and leaves the plain build where
/// [`package_binary!`] finds it, for the tests that start that one. `None` when
/// the run made no such build, as a plain `cargo test` does not.
#[must_use]
pub fn started_variant(bin: &str, profile: &str, features: &str) -> Option<PathBuf> {
    env::var_os(format!("T_STARTED_BIN_{bin}_{profile}_{features}")).map(PathBuf::from)
}

/// Give `command` the environment a launcher gives the app it starts: none of
/// the `TILO_*` or `QUICK_*` choices this test process was started with, and
/// the prepared fonts the run draws with, which a launch hands on
/// (`crates/desktop/app-launch/src/env.rs` names them). A test that starts an
/// app as a process of its own calls this instead of clearing the variables
/// itself, so an app that draws text finds the catalog under nextest too.
pub fn launch_env(command: &mut std::process::Command) {
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        let drawn_with = name == "TILO_FONT_ASSET_CACHE" || name == "TILO_FONT_ASSETS_OFFLINE";
        if !drawn_with && (name.starts_with("TILO_") || name.starts_with("QUICK_")) {
            command.env_remove(&key);
        }
    }
}

struct LockState {
    owner: Option<ThreadId>,
    depth: usize,
    /// Who holds it: a test waiting on it names them, so a lock kept past the test that took it
    /// (a [`TestHome`] leaked for the process, a [`ScopedEnv`] in a static) fails the next test
    /// that needs the environment legibly instead of hanging it.
    holder: Option<Holder>,
}

/// The thread holding the lock, where it took it, and since when.
struct Holder {
    thread: String,
    at: &'static Location<'static>,
    since: Instant,
}

static STATE: Mutex<LockState> = Mutex::new(LockState {
    owner: None,
    depth: 0,
    holder: None,
});
static RELEASED: Condvar = Condvar::new();

#[must_use = "the environment is only serialised while the lock is held"]
pub struct EnvLock {
    _not_send: PhantomData<*const ()>,
}

/// Take the environment lock, waiting while another thread holds it.
///
/// # Panics
/// Another thread has held it for longer than [`hang_bound`]: the message names that thread and
/// where it took the lock.
#[track_caller]
#[allow(
    clippy::panic,
    reason = "test support: a lock that is never let go fails the test that waits for it"
)]
pub fn lock() -> EnvLock {
    let me = thread::current();
    let at = Location::caller();
    let mut state = STATE.lock_recover();
    loop {
        match state.owner {
            None => {
                state.owner = Some(me.id());
                state.depth = 1;
                state.holder = Some(Holder {
                    thread: me.name().unwrap_or("an unnamed thread").to_owned(),
                    at,
                    since: Instant::now(),
                });
                break;
            }
            Some(owner) if owner == me.id() => {
                state.depth = state.depth.saturating_add(1);
                break;
            }
            Some(_) => {
                let held = state
                    .holder
                    .as_ref()
                    .map_or(Duration::ZERO, |holder| holder.since.elapsed());
                let Some(left) = hang_bound()
                    .checked_sub(held)
                    .filter(|left| !left.is_zero())
                else {
                    let holder = state.holder.as_ref().map_or_else(
                        || "a thread that did not say".to_owned(),
                        |holder| format!("{:?}, which took it at {}", holder.thread, holder.at),
                    );
                    drop(state);
                    panic!(
                        "the environment lock {at} waits for has been held for {held:?} by \
                         {holder}: what took it outlived its test, or that test hangs (set \
                         {HANG_BOUND_ENV} to wait longer)"
                    );
                };
                state = RELEASED
                    .wait_timeout(state, left)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
    }
    drop(state);
    EnvLock {
        _not_send: PhantomData,
    }
}

impl Drop for EnvLock {
    fn drop(&mut self) {
        let mut state = STATE.lock_recover();
        state.depth = state.depth.saturating_sub(1);
        let released = state.depth == 0;
        if released {
            state.owner = None;
            state.holder = None;
        }
        drop(state);
        if released {
            RELEASED.notify_one();
        }
    }
}

/// How long [`SpawnUnderEnvLock::output_locked`] reads a program's streams after it ended.
const OUTLIVED_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

/// A stream read to its end on a thread of its own, into a buffer the reader can take early.
struct Drained {
    read: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    ended: std::sync::mpsc::Receiver<()>,
}

impl Drained {
    fn read(stream: Option<impl io::Read + Send + 'static>) -> Self {
        let read = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (done, ended) = std::sync::mpsc::channel();
        if let Some(mut stream) = stream {
            let into = std::sync::Arc::clone(&read);
            std::thread::spawn(move || {
                let mut chunk = [0_u8; 8192];
                while let Ok(n) = stream.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    into.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .extend_from_slice(chunk.get(..n).unwrap_or_default());
                }
                let _ = done.send(());
            });
        } else {
            let _ = done.send(());
        }
        Self { read, ended }
    }

    /// What was read by [`OUTLIVED_AFTER`] after `since`, and whether the stream ended by then.
    fn take(self, since: std::time::Instant) -> (Vec<u8>, bool) {
        let left = OUTLIVED_AFTER.saturating_sub(since.elapsed());
        let ended = self.ended.recv_timeout(left).is_ok();
        let read = std::mem::take(
            &mut *self
                .read
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        (read, ended)
    }
}

pub trait SpawnUnderEnvLock {
    fn spawn_locked(&mut self) -> io::Result<std::process::Child>;
    fn output_locked(&mut self) -> io::Result<std::process::Output>;
    fn status_locked(&mut self) -> io::Result<std::process::ExitStatus>;
}

/// A program a test starts ends with the test that started it.
///
/// A test's process can end without running a destructor: the runner kills a test that ran past
/// its bound. A program the test started in a process group of its own is out of that kill's
/// reach, so it keeps running for the rest of the run, holding its sockets and whatever it serves.
/// The kernel's parent-death signal ends it when the test's process ends, whichever way it ends.
pub trait DiesWithTheTest {
    /// Make the program end when the process that runs it ends, however that happens.
    ///
    /// The signal is the one a stop of the program's own group sends (SIGTERM), so a session ends
    /// the way a session ends: it puts away its sockets and files and ends its apps, rather than
    /// being cut off where its destructors would not run either. A program whose answer the test
    /// waits for (`output`, `status`) ends with it already; this is for the ones a test leaves
    /// running, and start them from the thread that runs the test, as the signal is delivered
    /// when that thread ends.
    fn dies_with_the_test(&mut self) -> &mut Self;
}

#[cfg(target_os = "linux")]
impl DiesWithTheTest for std::process::Command {
    fn dies_with_the_test(&mut self) -> &mut Self {
        use std::os::unix::process::CommandExt as _;
        // The process that runs the test. A child forked as that process is being killed finds a
        // different parent, and the signal it would have been sent is already behind it.
        let test_process = i32::try_from(std::process::id()).unwrap_or(0);
        // SAFETY: `pre_exec` runs between fork and exec in the child, where the only work done is
        // a `prctl`, a `getppid` and `_exit`, all async-signal-safe.
        unsafe {
            self.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != test_process {
                    libc::_exit(0);
                }
                Ok(())
            })
        }
    }
}

impl SpawnUnderEnvLock for std::process::Command {
    fn spawn_locked(&mut self) -> io::Result<std::process::Child> {
        let _env = lock();
        self.spawn()
    }

    /// The program's answer once it ends. Its streams are read until it ends and then for a
    /// short while longer: a program it started that outlives it and keeps the streams open
    /// would otherwise keep the test waiting for an end that never comes, without a word. Its
    /// output then says so, after what it said.
    fn output_locked(&mut self) -> io::Result<std::process::Output> {
        self.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = self.spawn_locked()?;
        let stdout = Drained::read(child.stdout.take());
        let stderr = Drained::read(child.stderr.take());
        let status = child.wait()?;
        let ended = std::time::Instant::now();
        let (stdout, out_ended) = stdout.take(ended);
        let (mut stderr, err_ended) = stderr.take(ended);
        if !(out_ended && err_ended) {
            stderr.extend_from_slice(
                format!(
                    "\n[the program ended, and a program it started still holds its output {} s \
                     later: it outlived it]\n",
                    OUTLIVED_AFTER.as_secs()
                )
                .as_bytes(),
            );
        }
        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    }

    fn status_locked(&mut self) -> io::Result<std::process::ExitStatus> {
        self.spawn_locked()?.wait()
    }
}

#[cfg(feature = "tokio")]
pub trait SpawnUnderEnvLockAsync {
    fn spawn_locked(&mut self) -> io::Result<tokio::process::Child>;
    fn output_locked(
        &mut self,
    ) -> impl std::future::Future<Output = io::Result<std::process::Output>> + Send;
    fn status_locked(
        &mut self,
    ) -> impl std::future::Future<Output = io::Result<std::process::ExitStatus>> + Send;
}

#[cfg(feature = "tokio")]
impl SpawnUnderEnvLockAsync for tokio::process::Command {
    fn spawn_locked(&mut self) -> io::Result<tokio::process::Child> {
        let _env = lock();
        self.spawn()
    }

    fn output_locked(
        &mut self,
    ) -> impl std::future::Future<Output = io::Result<std::process::Output>> + Send {
        self.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = self.spawn_locked();
        async move { child?.wait_with_output().await }
    }

    fn status_locked(
        &mut self,
    ) -> impl std::future::Future<Output = io::Result<std::process::ExitStatus>> + Send {
        let child = self.spawn_locked();
        async move { child?.wait().await }
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "this crate is the one sanctioned writer of the process environment"
)]
fn write_var(key: &OsStr, value: Option<&OsStr>) {
    unsafe {
        match value {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
    }
}

#[must_use = "the changes are undone as soon as the scope is dropped"]
pub struct ScopedEnv {
    saved: Vec<(OsString, Option<OsString>)>,
    _lock: EnvLock,
}

impl ScopedEnv {
    #[track_caller]
    pub fn new() -> Self {
        Self {
            saved: Vec::new(),
            _lock: lock(),
        }
    }

    pub fn set(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        let key = key.as_ref();
        self.remember(key);
        write_var(key, Some(value.as_ref()));
        self
    }

    pub fn remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        let key = key.as_ref();
        self.remember(key);
        write_var(key, None);
        self
    }

    fn remember(&mut self, key: &OsStr) {
        if !self.saved.iter().any(|(saved, _)| saved == key) {
            self.saved.push((key.to_owned(), env::var_os(key)));
        }
    }
}

impl Default for ScopedEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        for (key, prior) in self.saved.drain(..).rev() {
            write_var(&key, prior.as_deref());
        }
    }
}

pub fn pin(key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
    let _lock = lock();
    write_var(key.as_ref(), Some(value.as_ref()));
}

pub struct TestHome {
    env: ScopedEnv,
    home: PathBuf,
    data: PathBuf,
    config: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    runtime: PathBuf,
    root: tempfile::TempDir,
    _runtime_root: tempfile::TempDir,
}

impl TestHome {
    #[track_caller]
    pub fn new() -> io::Result<Self> {
        let root = tempfile::Builder::new()
            .prefix("tilo-test-home-")
            .tempdir()?;
        let home = root.path().join("home");
        let data = home.join(".local/share");
        let config = home.join(".config");
        let state = home.join(".local/state");
        let cache = home.join(".cache");
        let runtime_root = runtime_tempdir()?;
        let runtime = runtime_root.path().to_owned();
        for dir in [&data, &config, &state, &cache] {
            fs::create_dir_all(dir)?;
        }
        let mut env = ScopedEnv::new();
        env.set("HOME", &home)
            .set("XDG_DATA_HOME", &data)
            .set("XDG_CONFIG_HOME", &config)
            .set("XDG_STATE_HOME", &state)
            .set("XDG_CACHE_HOME", &cache)
            .set("XDG_RUNTIME_DIR", &runtime);
        Ok(Self {
            env,
            home,
            data,
            config,
            state,
            cache,
            runtime,
            root,
            _runtime_root: runtime_root,
        })
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn data(&self) -> &Path {
        &self.data
    }

    pub fn config(&self) -> &Path {
        &self.config
    }

    pub fn state(&self) -> &Path {
        &self.state
    }

    pub fn cache(&self) -> &Path {
        &self.cache
    }

    pub fn runtime(&self) -> &Path {
        &self.runtime
    }

    pub fn env(&mut self) -> &mut ScopedEnv {
        &mut self.env
    }
}

const LOCAL_SOCKET_PATH_MAX: usize = 103;

const RUNTIME_NAMES_BUDGET: usize = 64;

pub fn runtime_tempdir() -> io::Result<tempfile::TempDir> {
    const PREFIX: &str = "tilo-rt-";
    const LONGEST_BASE: usize =
        LOCAL_SOCKET_PATH_MAX - RUNTIME_NAMES_BUDGET - (1 + PREFIX.len() + 6);
    let builder = {
        let mut builder = tempfile::Builder::new();
        builder.prefix(PREFIX);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(fs::Permissions::from_mode(0o700));
        }
        builder
    };
    let temp = env::temp_dir();
    let resolved = fs::canonicalize(&temp).unwrap_or_else(|_| temp.clone());
    if cfg!(unix) && resolved.as_os_str().len() > LONGEST_BASE {
        builder.tempdir_in("/tmp")
    } else {
        builder.tempdir_in(temp)
    }
}

pub fn memory_tempdir() -> io::Result<tempfile::TempDir> {
    match tilo_paths::memory_temp_dir() {
        Some(memory) => tempfile::tempdir_in(memory),
        None => tempfile::tempdir(),
    }
}

#[must_use]
#[expect(
    clippy::expect_used,
    reason = "a fixture's key has no fallback: a machine whose entropy source fails cannot run the test it is minted for"
)]
pub fn node_key() -> [u8; 32] {
    let mut seed = [0; 32];
    getrandom::fill(&mut seed).expect("the system's entropy source");
    seed
}

#[derive(Debug, Clone)]
pub struct Script {
    path: PathBuf,
}

pub const SHELL: &str = "/bin/sh";

impl Script {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exec_argv(&self) -> [&OsStr; 2] {
        [OsStr::new(SHELL), self.path.as_os_str()]
    }

    pub fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(SHELL);
        command.arg(&self.path);
        command
    }
}

#[cfg(unix)]
pub fn script(path: impl AsRef<Path>, body: &str) -> io::Result<Script> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

    let path = path.as_ref();
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let mut staged = tempfile::Builder::new()
        .prefix(".tilo-test-script-")
        .tempfile_in(dir)?;
    staged.write_all(body.as_bytes())?;
    staged.as_file().sync_all()?;
    staged
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o755))?;
    let written = staged.persist(path).map_err(|error| error.error)?;
    drop(written);
    fs::File::open(dir)?.sync_all()?;
    Ok(Script {
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const KEY: &str = "TILO_TEST_ENV_SELFTEST";
    const OTHER: &str = "TILO_TEST_ENV_SELFTEST_OTHER";

    /// A program a test leaves running ends when the thread that started it does. The signal
    /// stands in for the destructor a test the runner killed never runs; checked here on its own,
    /// where the test is still alive to see the program end.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_program_a_test_starts_ends_with_the_test() {
        use std::os::unix::process::ExitStatusExt as _;
        use std::process::Command;
        use std::thread;

        let started = thread::spawn(|| {
            let mut command = Command::new("sleep");
            command.arg("60").dies_with_the_test();
            command
                .spawn_locked()
                .expect("a program to outlive its test")
        });
        let mut program = started.join().expect("the thread that started it");
        // A pidfd says the process ended; the wait then reaps it and says how.
        exited("the program a test started", program.id());
        let status = program.wait().expect("the program's end");
        assert_eq!(
            status.signal(),
            Some(libc::SIGTERM),
            "it ends on the signal a stop of its group sends: {status:?}"
        );
    }

    fn read(key: &str) -> Option<String> {
        env::var(key).ok()
    }

    #[test]
    fn a_runtime_tempdir_leaves_room_for_sockets_and_is_private() {
        let dir = runtime_tempdir().unwrap();
        let resolved = fs::canonicalize(dir.path()).unwrap();
        assert!(
            resolved.as_os_str().len() + RUNTIME_NAMES_BUDGET <= LOCAL_SOCKET_PATH_MAX,
            "{} leaves no room for a socket below it",
            resolved.display()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
            let socket = dir.path().join(format!("{:0>48}", "s.sock"));
            std::os::unix::net::UnixListener::bind(&socket).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_child_started_beside_another_scope_inherits_none_of_it() {
        const LEAK: &str = "TILO_TEST_ENV_SELFTEST_LEAK";
        let (opened, on_open) = std::sync::mpsc::channel();
        let scope = thread::spawn(move || {
            let mut env = ScopedEnv::new();
            env.set(LEAK, "the other test's value");
            opened.send(()).unwrap();
            thread::sleep(std::time::Duration::from_millis(200));
            drop(env);
        });
        on_open.recv().unwrap();
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", "printf %s \"${TILO_TEST_ENV_SELFTEST_LEAK-}\""])
            .output_locked()
            .unwrap();
        scope.join().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    }

    #[test]
    fn scoped_env_restores_every_prior_value_and_serialises_scopes() {
        {
            {
                let mut env = ScopedEnv::new();
                env.remove(KEY);
            }
            {
                let mut env = ScopedEnv::new();
                env.set(KEY, "inside");
                assert_eq!(read(KEY).as_deref(), Some("inside"));
            }
            let _guard = lock();
            assert_eq!(read(KEY), None);
        }

        {
            let mut outer = ScopedEnv::new();
            outer.set(KEY, "prior");
            {
                let mut env = ScopedEnv::new();
                env.set(KEY, "first").set(KEY, "second");
                assert_eq!(read(KEY).as_deref(), Some("second"));
            }
            assert_eq!(
                read(KEY).as_deref(),
                Some("prior"),
                "set twice, restored once"
            );
            {
                let mut env = ScopedEnv::new();
                env.remove(KEY);
                assert_eq!(read(KEY), None);
            }
            assert_eq!(read(KEY).as_deref(), Some("prior"), "removal is undone");
        }

        {
            let mut outer = ScopedEnv::new();
            outer.remove(KEY).set(OTHER, "outer");
            {
                let mut inner = ScopedEnv::new();
                inner.set(KEY, "inner").set(OTHER, "inner");
                assert_eq!(read(KEY).as_deref(), Some("inner"));
                assert_eq!(read(OTHER).as_deref(), Some("inner"));
            }
            assert_eq!(read(KEY), None);
            assert_eq!(read(OTHER).as_deref(), Some("outer"));
            drop(outer);
            let _guard = lock();
            assert_eq!(read(OTHER), None);
        }

        {
            let result = thread::spawn(|| {
                let mut env = ScopedEnv::new();
                env.set(KEY, "leaked?");
                panic!("assertion failure inside a scope");
            })
            .join();
            assert!(result.is_err(), "the scope's thread panicked");
            {
                let _guard = lock();
                assert_eq!(read(KEY), None);
            }
            let caught = std::panic::catch_unwind(|| {
                let mut env = ScopedEnv::new();
                env.set(KEY, "leaked?");
                panic!("same-thread failure");
            });
            assert!(caught.is_err());
            let _guard = lock();
            assert_eq!(read(KEY), None);
        }

        {
            const THREADS: usize = 8;
            const ROUNDS: usize = 50;
            let inside = Arc::new(AtomicUsize::new(0));
            let handles: Vec<_> = (0..THREADS)
                .map(|index| {
                    let inside = Arc::clone(&inside);
                    thread::spawn(move || {
                        let mine = format!("thread-{index}");
                        for _ in 0..ROUNDS {
                            let mut env = ScopedEnv::new();
                            assert_eq!(
                                inside.fetch_add(1, Ordering::SeqCst),
                                0,
                                "two scopes at once"
                            );
                            env.set(KEY, &mine);
                            thread::yield_now();
                            assert_eq!(read(KEY).as_deref(), Some(mine.as_str()));
                            inside.fetch_sub(1, Ordering::SeqCst);
                        }
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
            let _guard = lock();
            assert_eq!(read(KEY), None, "every scope restored the absent value");
        }
    }

    #[test]
    fn a_pinned_variable_outlives_every_scope_taken_after_it() {
        const PINNED: &str = "TILO_TEST_ENV_SELFTEST_PINNED";
        pin(PINNED, "/tmp/shared");
        assert_eq!(read(PINNED).as_deref(), Some("/tmp/shared"));
        {
            let mut env = ScopedEnv::new();
            env.set(PINNED, "/tmp/one-test");
            assert_eq!(read(PINNED).as_deref(), Some("/tmp/one-test"));
        }
        assert_eq!(
            read(PINNED).as_deref(),
            Some("/tmp/shared"),
            "a scope restores the pinned value, not the one before it"
        );
        {
            let mut env = ScopedEnv::new();
            env.remove(PINNED);
            assert_eq!(read(PINNED), None);
        }
        assert_eq!(
            read(PINNED).as_deref(),
            Some("/tmp/shared"),
            "a scope's removal is undone too"
        );
    }

    #[test]
    fn test_home_redirects_every_base_directory_and_restores_home() {
        {
            let root;
            let runtime;
            {
                let home = TestHome::new().unwrap();
                root = home.root().to_path_buf();
                runtime = home.runtime().to_path_buf();
                for (key, dir) in [
                    ("HOME", home.home()),
                    ("XDG_DATA_HOME", home.data()),
                    ("XDG_CONFIG_HOME", home.config()),
                    ("XDG_STATE_HOME", home.state()),
                    ("XDG_CACHE_HOME", home.cache()),
                ] {
                    assert_eq!(env::var_os(key).as_deref(), Some(dir.as_os_str()), "{key}");
                    assert!(dir.is_dir(), "{key} exists");
                    assert!(dir.starts_with(&root), "{key} is inside the fixture");
                }
                assert_eq!(
                    env::var_os("XDG_RUNTIME_DIR").as_deref(),
                    Some(runtime.as_os_str())
                );
                assert!(runtime.is_dir(), "XDG_RUNTIME_DIR exists");
                assert!(
                    fs::canonicalize(&runtime).unwrap().as_os_str().len() + RUNTIME_NAMES_BUDGET
                        <= LOCAL_SOCKET_PATH_MAX,
                    "XDG_RUNTIME_DIR leaves room for sockets"
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = fs::metadata(home.runtime()).unwrap().permissions().mode();
                    assert_eq!(mode & 0o777, 0o700, "runtime directory is private");
                }
            }
            assert!(!root.exists(), "the tree is removed on drop");
            assert!(
                !runtime.exists(),
                "the runtime directory is removed on drop"
            );
            let _guard = lock();
            assert!(
                env::var_os("XDG_DATA_HOME").is_none_or(|dir| !Path::new(&dir).starts_with(&root))
            );
        }

        {
            let mut outer = ScopedEnv::new();
            outer.set("XDG_CONFIG_HOME", "/nonexistent/prior-config");
            {
                let mut home = TestHome::new().unwrap();
                home.env().set(KEY, "with-home");
                assert_ne!(
                    read("XDG_CONFIG_HOME").as_deref(),
                    Some("/nonexistent/prior-config")
                );
            }
            assert_eq!(
                read("XDG_CONFIG_HOME").as_deref(),
                Some("/nonexistent/prior-config")
            );
            assert_eq!(read(KEY), None, "extra variables share the home's scope");
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_scripts_are_written_whole_and_start_under_concurrency() {
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("fixture");
            fs::write(&path, "stale").unwrap();
            let script = script(&path, "#!/bin/sh\nprintf 'ran %s' \"$1\"\n").unwrap();
            assert_eq!(script.path(), path);
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o111,
                0o111,
                "executable by everyone who can read it"
            );
            let names: Vec<_> = fs::read_dir(dir.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(
                names,
                [OsString::from("fixture")],
                "no staging file left behind"
            );
            let [shell, arg] = script.exec_argv();
            assert_eq!(shell, OsStr::new("/bin/sh"));
            let output = std::process::Command::new(shell)
                .arg(arg)
                .arg("once")
                .output_locked()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"ran once");
        }

        {
            let dir = Arc::new(tempfile::tempdir().unwrap());
            let handles: Vec<_> = (0..8)
                .map(|thread_index| {
                    let dir = Arc::clone(&dir);
                    thread::spawn(move || {
                        for round in 0..25 {
                            let path = dir.path().join(format!("fixture-{thread_index}-{round}"));
                            let body = format!("#!/bin/sh\nprintf '{thread_index}-{round}'\n");
                            let output = script(&path, &body)
                                .unwrap()
                                .command()
                                .output_locked()
                                .unwrap();
                            assert!(output.status.success());
                            assert_eq!(output.stdout, format!("{thread_index}-{round}").as_bytes());
                        }
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
        }
    }
    fn note(ran: &std::sync::Mutex<Vec<&'static str>>, case: &'static str) {
        ran.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(case);
    }

    #[test]
    fn a_scenario_runs_every_case_in_its_own_scope_and_reports_each_by_name() {
        let ran = std::sync::Mutex::new(Vec::new());
        let report = scenario::run(vec![
            scenario::Phase::new("first", "here:1", || note(&ran, "first")),
            scenario::Phase::new("fails", "here:20", || {
                note(&ran, "fails");
                assert_eq!(1, 2, "a case that fails says which one");
            }),
            scenario::Phase::new("after_a_failure", "here:30", || {
                note(&ran, "after_a_failure");
            }),
        ]);
        assert_eq!(
            ran.into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ["first", "fails", "after_a_failure"],
            "a failing case does not stop the ones after it",
        );
        assert_eq!(report.total(), 3);
        let failed: Vec<_> = report
            .cases()
            .filter(|(_, _, failure)| failure.is_some())
            .collect();
        assert_eq!(failed.len(), 1, "only the case that failed fails");
        assert_eq!(failed[0].0, "fails");
        let shown = std::panic::catch_unwind(|| report.assert_green());
        let message = shown.expect_err("a failed case fails the scenario");
        let message = message
            .downcast_ref::<String>()
            .map_or_else(String::new, Clone::clone);
        assert!(
            message.contains("phase `fails`") && message.contains("here:20"),
            "the report names the case and where it starts: {message}",
        );
    }

    #[test]
    fn a_scenario_with_no_case_proves_nothing_and_says_so() {
        let empty = scenario::run(Vec::new());
        assert_eq!(empty.total(), 0);
        assert!(
            std::panic::catch_unwind(|| empty.assert_green()).is_err(),
            "a scenario that ran nothing must not pass",
        );
    }

    scenario! {
        fn a_scenario_collects_phases_steps_and_declarations;

        phase a_case_binds_a_name_the_next_one_binds_too;
        {
            const SHARED: usize = 7;
            let value = SHARED;
            assert_eq!(value, 7, "a declaration beside the cases is read");
        }

        step a_fixture_between_cases;
        {
            let shared = 1 + 1;
            assert_eq!(shared, 2, "a step runs on its own");
        }

        steps;
        {
            let shared = "a name the cases also bind";
            assert_eq!(shared.len(), 26, "a file of steps runs on its own");
        }

        phase the_last_case_runs_last;
        {
            let shared = [3, 1, 2];
            assert_eq!(shared.len(), 3, "a case's bindings do not outlive it");
        }
    }
}
