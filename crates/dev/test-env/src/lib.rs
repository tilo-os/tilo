//! Process-environment fixtures for tests.
//!
//! The environment is process-global, and `cargo test` runs every test of a
//! binary as a thread of one process. A test that sets `XDG_DATA_HOME` and
//! forgets to put it back, or restores it by removing a value that was set
//! before it ran, leaks into whichever test runs next — and a test that reads
//! the variable while another thread is writing it sees either value.
//!
//! Prefer not to need this crate: an API that takes its root directory as an
//! argument (`open_at(root, …)`, `*_in(root, …)`) needs no environment at all
//! and its tests can run in parallel. Use the fixtures here only where the
//! environment *is* the interface under test, such as the function that
//! resolves `XDG_DATA_HOME` itself.
//!
//! - [`ScopedEnv`] sets and removes variables and restores every prior value
//!   when dropped, including while unwinding from a failed assertion.
//! - [`TestHome`] points `HOME` and every XDG base directory at a fresh
//!   temporary tree.
//! - [`runtime_tempdir`] is a private temporary directory with a path as
//!   short as a real runtime directory's, for tests that bind sockets.
//! - [`lock`] is the single process-wide lock both of them hold. Every
//!   scope in the process is serialised on it, and it is re-entrant, so
//!   scopes nest on one thread.
//! - [`script`] writes an executable fixture script without racing
//!   concurrent `fork`/`exec` in sibling tests.
//! - [`require_capability!`] skips a test that needs a GPU, audio, codecs,
//!   FUSE, fanotify, chown or namespaces where the host lacks it, and fails it
//!   instead on a CI lane that sets `TILO_REQUIRE_<CAP>=1`.
//! - [`hang`] waits for workers, daemons, sockets and events against one
//!   workspace-wide bound that only catches a hang: [`wait_until`],
//!   [`wait_for`], [`recv`] and [`within`].
//! - [`idle`] counts how often idle threads wake: one thread of the test
//!   process, or every thread of a process tree the test spawned.
//! - [`require_isolated_desktop`] refuses to go on when a test binary was
//!   started outside the test environment, where it could reach the caller's
//!   session bus, portals, media players or display.
//! - [`package_binary!`] finds a binary of the package under test where the
//!   test run put it, which an archived run moves away from Cargo's path.
//! - [`in_own_process`] runs a test that changes what the whole process
//!   sees, such as a held wall clock, in a process no other test shares.
//! - [`scenario!`] folds a group of related tests into one test with a
//!   labelled phase per former test, each phase in its own scope, and still
//!   reports a failure by the case it came from.
//!
//! ## Scenarios
//!
//! A scenario is the one test a group of related tests becomes. Every case
//! keeps its own fixture and every assertion; it is called a *phase*, and a
//! second group of statements in the same scenario is a *step*. Both keep their
//! own scope, so two of them may bind the same names, as two tests did:
//!
//! ```ignore
//! scenario! {
//!     fn a_saved_project_opens_as_the_same_timeline;
//!
//!     phase saving_writes_a_file_that_opens_back;
//!     /// What a file is for, end to end through the app.
//!     {
//!         let (app, doc) = app_and_doc();
//!         assert!(path.is_file(), "the file is on disk");
//!     }
//!
//!     steps {
//!         // Between two phases: a fixture the phases below share, run once.
//!     }
//! }
//! ```
//!
//! [`scenario!`] writes the case function itself: a `#[test]` that runs every
//! case in one pass, through the hook in [`scenario`]. `#[scenario]` is for a
//! group that already has its test and only needs its cases collected. A case
//! that fails is reported by phase, with the line inside it that raised, and
//! the scenario's cases after it still run, so one pass shows every failure. A
//! `#[cfg]` above the `fn` in [`scenario!`] applies to the function, as it
//! would above any other `fn`.
//!
//! The lock only orders code that takes it. Code under test that reads the
//! environment from a thread of its own is still racing any other test that
//! mutates the variable without this crate.

pub mod allocation;
mod capability;
mod desktop;
pub mod hang;
pub mod idle;
mod own_process;
pub mod scenario;
pub mod swtpm;

pub use capability::{Capability, Probe, capability_unavailable};
#[cfg(target_os = "linux")]
pub use capability::{bubblewrap, mount_namespace, user_namespace};
#[cfg(unix)]
pub use capability::{chown_to, switch_identity};
pub use desktop::{TEST_ENVIRONMENT, desktop_isolation, require_isolated_desktop};
pub use hang::{Within, hang_bound, hang_deadline, recv, wait_for, wait_until, within};
pub use own_process::in_own_process;

use std::ffi::{OsStr, OsString};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};
use std::thread::{self, ThreadId};
use std::{env, fs, io};
use tilo_lock::MutexRecover as _;

/// One test with a labelled case per former test.
///
/// Its body holds three kinds of statement. Anything else it holds — a
/// constant, a helper, a declaration — stays exactly where it was written,
/// ahead of the cases.
///
/// - `phase NAME;` — the case `NAME`, which starts here and runs the
///   statements in the braces that follow it;
/// - `step NAME;` — the same, kept apart for a fixture written between cases
///   rather than a test of its own;
/// - `steps;` — the same, named for the file it came from.
///
/// Each case runs in a scope of its own, so two may bind the same names as two
/// separate tests did, and the whole scenario runs in one pass: a failure is
/// reported by the phase it came from, and the cases after it still run.
///
/// ```ignore
/// scenario! {
///     fn a_saved_project_opens_as_the_same_timeline;
///
///     phase saving_writes_a_file_that_opens_back;
///     {
///         let (app, doc) = app_and_doc();
///         assert!(app.save(&doc, path).is_ok(), "the file is on disk");
///     }
///
///     phase an_open_is_one_undoable_action;
///     {
///         // A fresh fixture, as the test this came from built.
///         let (app, doc) = app_and_doc();
///         assert!(opened_in_one_edit(&app), "an open is one edit");
///     }
/// }
/// ```
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

/// The scenario's cases so far, and the statements after them.
///
/// A case is buffered as its name, the `file!()` and `line!()` it is written
/// at, and the tokens of its body — never as the expression that becomes it.
/// A path written into a buffer with `$crate` and spliced back out is matched
/// by this macro's `$crate`, which is the crate the case ends up in only by
/// accident. Everything is expanded once, at the end, in the caller's file.
#[doc(hidden)]
#[macro_export]
macro_rules! __scenario {
    // A case named `NAME`, of kind `phase` or `step` (or `steps`), which runs
    // the statements in the braces that follow it, in a closure that keeps its
    // bindings inside — inside a scope of its own, as the test it came from
    // had, so two cases may bind the same names.
    //
    // The braces are captured as tokens and become the closure's body where
    // they were written. A `block` fragment would be checked against the
    // closure's own context before it could be handed on, and a block checked
    // there does not fit back into a body.
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

    // Nothing left: make a phase of each case and run them together.
    (@cases [$(($name:expr, $location:expr, $body:tt))*] @rest) => {
        $crate::scenario::run(::std::vec![
            $(
                $crate::__scenario_phase!($name, $location, $body)
            ),*
        ])
        .assert_green()
    };
}

/// One buffered case, as a phase.
#[doc(hidden)]
#[macro_export]
macro_rules! __scenario_phase {
    ($name:expr, $location:expr, { $($body:tt)* }) => {
        $crate::scenario::Phase::new($name, $location, || { $($body)* })
    };
}

/// The name a case is reported by.
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

/// A binary of the package under test, by its Cargo target name: where an
/// archived nextest run relocated it (`NEXTEST_BIN_EXE_<name>`), else where
/// Cargo built it (`CARGO_BIN_EXE_<name>`). A test that starts the build path
/// directly finds nothing once the run moves its binaries.
#[macro_export]
macro_rules! package_binary {
    ($name:literal) => {
        $crate::package_binary_at($name, env!(concat!("CARGO_BIN_EXE_", $name)))
    };
}

/// [`package_binary!`] with Cargo's path given: `NEXTEST_BIN_EXE_<name>`
/// when the run relocated the binary, else `built_at`.
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

/// Holding this means no other thread holds an [`EnvLock`], a [`ScopedEnv`]
/// or a [`TestHome`].
///
/// Re-entrant: the thread that holds it may take it again, so scopes nest.
/// A test that panics while holding it releases it during unwinding rather
/// than poisoning every later test.
#[must_use = "the environment is only serialised while the lock is held"]
pub struct EnvLock {
    // Released on the thread that took it; the owner bookkeeping relies on it.
    _not_send: PhantomData<*const ()>,
}

/// Take the process-wide environment lock, waiting for any other thread
/// that holds it.
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

#[allow(
    clippy::disallowed_methods,
    reason = "this crate is the one sanctioned writer of the process environment"
)]
fn write_var(key: &OsStr, value: Option<&OsStr>) {
    // SAFETY: every caller holds the process-wide `EnvLock`, so no other
    // cooperating test mutates or reads the environment concurrently.
    unsafe {
        match value {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
    }
}

/// Environment changes that are undone on drop.
///
/// Holds [`lock`] for its whole lifetime. Dropping it restores each touched
/// variable to the value it had before this scope first touched it — set if
/// it was set, absent if it was absent — in reverse order, and then releases
/// the lock. Drop also runs while a failing test unwinds, so an assertion
/// failure cannot leak a value into the next test.
///
/// ```
/// let mut env = tilo_test_env::ScopedEnv::new();
/// env.set("TILO_EXAMPLE_ROOT", "/tmp/example")
///     .remove("TILO_EXAMPLE_OVERRIDE");
/// assert_eq!(std::env::var("TILO_EXAMPLE_ROOT").as_deref(), Ok("/tmp/example"));
/// drop(env);
/// assert!(std::env::var_os("TILO_EXAMPLE_ROOT").is_none());
/// ```
#[must_use = "the changes are undone as soon as the scope is dropped"]
pub struct ScopedEnv {
    saved: Vec<(OsString, Option<OsString>)>,
    _lock: EnvLock,
}

impl ScopedEnv {
    /// Open a scope, waiting for the environment lock.
    pub fn new() -> Self {
        Self {
            saved: Vec::new(),
            _lock: lock(),
        }
    }

    /// Set `key` to `value` until this scope is dropped.
    pub fn set(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        let key = key.as_ref();
        self.remember(key);
        write_var(key, Some(value.as_ref()));
        self
    }

    /// Remove `key` until this scope is dropped.
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

/// A temporary home directory with every XDG base directory inside it.
///
/// Sets `HOME`, `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`,
/// `XDG_CACHE_HOME` and `XDG_RUNTIME_DIR` through a [`ScopedEnv`], so the
/// process sees none of the developer's real configuration, keyring
/// records or runtime sockets. The runtime directory is mode `0700`, as the
/// XDG specification requires and as runtime-directory checks verify.
///
/// On drop the environment is restored first, then the tree is deleted.
pub struct TestHome {
    // Field order is drop order: restore the environment before the
    // directory it points at disappears.
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
    /// Create the tree and point the environment at it.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the temporary tree cannot be created.
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

    /// The temporary directory holding the home and runtime trees.
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// `$XDG_DATA_HOME`.
    pub fn data(&self) -> &Path {
        &self.data
    }

    /// `$XDG_CONFIG_HOME`.
    pub fn config(&self) -> &Path {
        &self.config
    }

    /// `$XDG_STATE_HOME`.
    pub fn state(&self) -> &Path {
        &self.state
    }

    /// `$XDG_CACHE_HOME`.
    pub fn cache(&self) -> &Path {
        &self.cache
    }

    /// `$XDG_RUNTIME_DIR`: a [`runtime_tempdir`], outside [`Self::root`].
    pub fn runtime(&self) -> &Path {
        &self.runtime
    }

    /// The scope holding the variables above; more changes made through it
    /// are undone with them.
    pub fn env(&mut self) -> &mut ScopedEnv {
        &mut self.env
    }
}

/// The longest socket path, in bytes, a local socket address holds on every
/// Unix Tilo runs on: `sun_path` is 104 bytes on macOS and 108 on Linux, one
/// of them the terminating NUL.
const LOCAL_SOCKET_PATH_MAX: usize = 103;

/// Room left below a [`runtime_tempdir`] for the names tests put there (a
/// service directory and its socket, a uuid-named socket).
const RUNTIME_NAMES_BUDGET: usize = 64;

/// A fresh private (`0700`) temporary directory whose path is short enough
/// that sockets below it fit a local socket address, as below a real runtime
/// directory (`/run/user/<uid>`).
///
/// A test that binds a socket in [`tempfile::tempdir`] inherits the length of
/// `$TMPDIR`, which on macOS (`/var/folders/…/T/`) and on CI runners' per-job
/// scratch directories leaves too little of the 104 bytes macOS allows. This
/// is `$TMPDIR` when that leaves room for `RUNTIME_NAMES_BUDGET` bytes of
/// names below it, else `/tmp`, where the directory's random name and mode
/// keep it private like any other temporary directory.
///
/// # Errors
///
/// Returns the I/O error if the directory cannot be created.
pub fn runtime_tempdir() -> io::Result<tempfile::TempDir> {
    const PREFIX: &str = "tilo-rt-";
    // `/<prefix><6 random characters>`, then the names below it.
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
    // Code that canonicalizes a path sees macOS's `/var` as `/private/var`.
    let resolved = fs::canonicalize(&temp).unwrap_or_else(|_| temp.clone());
    if cfg!(unix) && resolved.as_os_str().len() > LONGEST_BASE {
        builder.tempdir_in("/tmp")
    } else {
        builder.tempdir_in(temp)
    }
}

/// An executable fixture script written by [`script`].
#[derive(Debug, Clone)]
pub struct Script {
    path: PathBuf,
}

/// The interpreter [`Script::exec_argv`] runs fixtures through.
pub const SHELL: &str = "/bin/sh";

impl Script {
    /// Where the script was written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `["/bin/sh", path]`: the argv to run this script with.
    ///
    /// Run fixtures through the shell rather than executing the path. While
    /// any thread holds a file open for writing, `execve` of that file fails
    /// with `ETXTBSY` — and a sibling test that forks between this script's
    /// `open` and `close` leaves a copy of the write descriptor in its child
    /// until that child calls `exec`. `/bin/sh` only reads the script, so a
    /// stray writer is harmless.
    pub fn exec_argv(&self) -> [&OsStr; 2] {
        [OsStr::new(SHELL), self.path.as_os_str()]
    }

    /// A [`std::process::Command`] that runs this script through
    /// [`exec_argv`](Self::exec_argv).
    pub fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(SHELL);
        command.arg(&self.path);
        command
    }
}

/// Write `body` to `path` as an executable script.
///
/// The body goes to a uniquely named temporary file beside `path`, is
/// flushed to disk, made executable and then renamed over `path`, so no
/// reader ever observes a partial or non-executable script and an existing
/// file is replaced atomically. Run the result with
/// [`Script::exec_argv`] or [`Script::command`], not by executing `path`
/// directly; see [`Script::exec_argv`] for why.
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

    /// Setting restores an absent variable to absent; set and remove restore the prior value; nested
    /// scopes unwind in order on one thread; a panicking scope restores and releases the lock;
    /// concurrent scopes are serialised.
    #[test]
    fn scoped_env_restores_every_prior_value_and_serialises_scopes() {
        // Phase: set_restores_an_absent_variable_to_absent.
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

        // Phase: set_and_remove_restore_the_prior_value.
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

        // Phase: nested_scopes_unwind_in_order_on_one_thread.
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

        // Phase: a_panicking_scope_restores_and_releases_the_lock.
        {
            // Every scope in this module leaves `KEY` absent, so absent is the
            // prior value each panicking scope below must restore.
            let result = thread::spawn(|| {
                let mut env = ScopedEnv::new();
                env.set(KEY, "leaked?");
                panic!("assertion failure inside a scope");
            })
            .join();
            assert!(result.is_err(), "the scope's thread panicked");
            {
                // Taking the lock again proves the panic released it; the value
                // proves the unwinding drop restored it.
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

        // Phase: concurrent_scopes_are_serialised.
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
    fn test_home_redirects_every_base_directory_and_restores_home() {
        // Phase: test_home_points_every_base_directory_into_one_temporary_tree.
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
                // The runtime directory is a runtime_tempdir of its own, so
                // its sockets fit a local socket address.
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

        // Phase: test_home_restores_the_previous_home.
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

    /// A script is executable and complete and leaves no staging file; scripts written and run
    /// concurrently never fail to start.
    #[cfg(unix)]
    #[test]
    fn test_scripts_are_written_whole_and_start_under_concurrency() {
        // Phase: script_is_executable_complete_and_leaves_no_staging_file.
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
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"ran once");
        }

        // Phase: scripts_written_and_run_concurrently_never_fail_to_start.
        // Many threads writing and running fixtures at once is what made
        // directly executed fixtures fail with `ETXTBSY`.
        {
            let dir = Arc::new(tempfile::tempdir().unwrap());
            let handles: Vec<_> = (0..8)
                .map(|thread_index| {
                    let dir = Arc::clone(&dir);
                    thread::spawn(move || {
                        for round in 0..25 {
                            let path = dir.path().join(format!("fixture-{thread_index}-{round}"));
                            let body = format!("#!/bin/sh\nprintf '{thread_index}-{round}'\n");
                            let output = script(&path, &body).unwrap().command().output().unwrap();
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
    /// The cases a probe scenario ran, in order, so the test below can say
    /// that a failure did not stop the cases after it.
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

    // A `scenario!` body collects its `phase`, `step` and `steps` statements
    // into one test, in order, keeps a binding two phases share apart, and
    // reads a declaration beside them without collecting it.
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
