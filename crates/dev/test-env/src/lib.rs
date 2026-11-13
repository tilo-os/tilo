//! Process-environment fixtures for tests. The environment is process-global, and `cargo test` runs
//! every test of a binary as a thread of one process. A test that sets `XDG_DATA_HOME` and forgets
//! to put it back, or restores it by removing a value that was set before it ran, leaks into
//! whichever test runs next, and a test that reads the variable while another thread is writing it
//! sees either value. Prefer not to need this crate: an API that takes its root directory as an
//! argument (`open_at(root, …)`, `*_in(root, …)`) needs no environment at all and its tests can run
//! in parallel. Use the fixtures here only where the environment is the interface under test, such
//! as the function that resolves `XDG_DATA_HOME` itself. [`ScopedEnv`], [`pin`], [`TestHome`],
//! [`TestEnv`] and [`lock`] cover the environment; the other modules cover the machine a test used
//! to reach for: [`net`], [`idle`], [`hang`], [`relay`], [`script`], [`swtpm`], [`capability`] and
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
mod fixture;
pub mod hang;
pub mod idle;
pub mod net;
mod own_process;
#[cfg(feature = "iroh-relay")]
pub mod relay;
pub mod requested_allocations;
pub mod scenario;
#[cfg(target_os = "linux")]
mod stand_in;
pub mod swtpm;
mod wall;

pub use binary::sibling_binary;
#[cfg(unix)]
pub use bus::PrivateBus;
pub use capability::{Capability, Probe, capability_unavailable};
#[cfg(target_os = "linux")]
pub use capability::{
    NamedProbe, PROBES, bubblewrap, bubblewrap_overlay, bubblewrap_with_proc, mount_namespace,
    network_tunnel, user_namespace,
};
#[cfg(unix)]
pub use capability::{chown_to, switch_identity};
pub use desktop::{TEST_ENVIRONMENT, desktop_isolation, require_isolated_desktop};
pub use environment::{Clock, Network, TestEnv};
pub use fixture::{Fixture, WORKSPACE_DOCUMENT};
#[cfg(target_os = "linux")]
pub use hang::exited;
pub use hang::{Within, hang_bound, hang_deadline, recv, wait_for, wait_until, within};
#[cfg(feature = "document")]
pub use net::document::{FakeDocumentNet, at_path};
pub use net::{Converged, FakeNet, Peer};
pub use own_process::{in_own_process, in_own_process_here};
#[cfg(target_os = "linux")]
pub use stand_in::{StandIn, stand_in};
pub use wall::held_wall_clock;

use std::ffi::{OsStr, OsString};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};
use std::thread::{self, ThreadId};
use std::{env, fs, io};
use tilo_lock::MutexRecover as _;

#[macro_export]
macro_rules! scenario {
    ($(#[$meta:meta])* fn $name:ident; $($rest:tt)*) => {
        #[test]
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

struct LockState {
    owner: Option<ThreadId>,
    depth: usize,
}

static STATE: Mutex<LockState> = Mutex::new(LockState {
    owner: None,
    depth: 0,
});
static RELEASED: Condvar = Condvar::new();

#[must_use = "the environment is only serialised while the lock is held"]
pub struct EnvLock {
    _not_send: PhantomData<*const ()>,
}

pub fn lock() -> EnvLock {
    let me = thread::current().id();
    let mut state = STATE.lock_recover();
    loop {
        match state.owner {
            None => {
                state.owner = Some(me);
                state.depth = 1;
                break;
            }
            Some(owner) if owner == me => {
                state.depth = state.depth.saturating_add(1);
                break;
            }
            Some(_) => {
                state = RELEASED.wait(state).unwrap_or_else(PoisonError::into_inner);
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
        }
        drop(state);
        if released {
            RELEASED.notify_one();
        }
    }
}

pub trait SpawnUnderEnvLock {
    fn spawn_locked(&mut self) -> io::Result<std::process::Child>;
    fn output_locked(&mut self) -> io::Result<std::process::Output>;
    fn status_locked(&mut self) -> io::Result<std::process::ExitStatus>;
}

impl SpawnUnderEnvLock for std::process::Command {
    fn spawn_locked(&mut self) -> io::Result<std::process::Child> {
        let _env = lock();
        self.spawn()
    }

    fn output_locked(&mut self) -> io::Result<std::process::Output> {
        self.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        self.spawn_locked()?.wait_with_output()
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
