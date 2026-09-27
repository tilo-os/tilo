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
//! - [`lock`] is the single process-wide lock both of them hold. Every
//!   scope in the process is serialised on it, and it is re-entrant, so
//!   scopes nest on one thread.
//! - [`script`] writes an executable fixture script without racing
//!   concurrent `fork`/`exec` in sibling tests.
//! - [`require_capability!`] skips a test that needs a GPU, audio, codecs,
//!   FUSE or fanotify where the host lacks it, and fails it instead on a CI
//!   lane that sets `TILO_REQUIRE_<CAP>=1`.
//! - [`hang`] waits for workers, daemons, sockets and events against one
//!   workspace-wide bound that only catches a hang: [`wait_until`],
//!   [`wait_for`], [`recv`] and [`within`].
//! - [`require_isolated_desktop`] refuses to go on when a test binary was
//!   started outside the test environment, where it could reach the caller's
//!   session bus, portals, media players or display.
//!
//! The lock only orders code that takes it. Code under test that reads the
//! environment from a thread of its own is still racing any other test that
//! mutates the variable without this crate.

pub mod allocation;
mod capability;
mod desktop;
pub mod hang;

pub use capability::{Capability, Probe, capability_unavailable};
pub use desktop::{TEST_ENVIRONMENT, desktop_isolation, require_isolated_desktop};
pub use hang::{Within, hang_bound, hang_deadline, recv, wait_for, wait_until, within};

use std::ffi::{OsStr, OsString};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};
use std::thread::{self, ThreadId};
use std::{env, fs, io};

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
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
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
        let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
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
        let runtime = root.path().join("runtime");
        for dir in [&data, &config, &state, &cache, &runtime] {
            fs::create_dir_all(dir)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))?;
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
        })
    }

    /// The temporary directory holding the home and runtime trees.
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// `$HOME`.
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

    /// `$XDG_RUNTIME_DIR`.
    pub fn runtime(&self) -> &Path {
        &self.runtime
    }

    /// The scope holding the variables above; more changes made through it
    /// are undone with them.
    pub fn env(&mut self) -> &mut ScopedEnv {
        &mut self.env
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
///
/// # Errors
///
/// Returns the I/O error from creating, writing, syncing or renaming the file.
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const KEY: &str = "TILO_TEST_ENV_SELFTEST";
    const OTHER: &str = "TILO_TEST_ENV_SELFTEST_OTHER";

    fn read(key: &str) -> Option<String> {
        env::var(key).ok()
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

    /// Test home points every base directory into one temporary tree and restores the previous home.
    #[test]
    fn test_home_redirects_every_base_directory_and_restores_home() {
        // Phase: test_home_points_every_base_directory_into_one_temporary_tree.
        {
            let root;
            {
                let home = TestHome::new().unwrap();
                root = home.root().to_path_buf();
                for (key, dir) in [
                    ("HOME", home.home()),
                    ("XDG_DATA_HOME", home.data()),
                    ("XDG_CONFIG_HOME", home.config()),
                    ("XDG_STATE_HOME", home.state()),
                    ("XDG_CACHE_HOME", home.cache()),
                    ("XDG_RUNTIME_DIR", home.runtime()),
                ] {
                    assert_eq!(env::var_os(key).as_deref(), Some(dir.as_os_str()), "{key}");
                    assert!(dir.is_dir(), "{key} exists");
                    assert!(dir.starts_with(&root), "{key} is inside the fixture");
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = fs::metadata(home.runtime()).unwrap().permissions().mode();
                    assert_eq!(mode & 0o777, 0o700, "runtime directory is private");
                }
            }
            assert!(!root.exists(), "the tree is removed on drop");
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
}
