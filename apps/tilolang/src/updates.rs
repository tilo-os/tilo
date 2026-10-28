//! Later builds of the installed package, in a development session.
//!
//! A development session's launcher follows reinstalls of a module payload's package
//! (`crates/packaging/launcher/src/live_module.rs`): it verifies each reinstall as a launch does,
//! copies its payload into a directory bound read-only into this sandbox and names that
//! generation in its `current` file. The launcher names the directory with [`UPDATES_ENV`];
//! outside a development session there is no such directory, and nothing here runs.
//!
//! A thread watches the directory, reads each new generation as a [`Bundle`] and wakes the
//! windows, which take it over on their own thread ([`crate::window::Window`]): in place when
//! the new build reads the state the old one wrote, and otherwise by starting the app over on the
//! room record, as its next start would.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use tilolang_tilo::bundle::Bundle;

/// The variable the launcher names the updates directory with.
pub const UPDATES_ENV: &str = "TILO_APP_MODULE_UPDATES";
/// The file in it that names the current generation.
pub const CURRENT_FILE: &str = "current";

/// A later build, read.
#[derive(Clone)]
pub struct Update {
    /// Counts the builds taken since launch, from 1.
    pub generation: u64,
    pub bundle: Arc<Bundle>,
    /// When the build was read: what the time a window takes to show it is counted from.
    pub read_at: Instant,
}

#[derive(Default)]
struct Shared {
    latest: Option<Update>,
    wakes: Vec<ui::wake::WakeHandle>,
}

fn shared() -> &'static Mutex<Shared> {
    static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
    SHARED.get_or_init(Mutex::default)
}

fn lock() -> std::sync::MutexGuard<'static, Shared> {
    shared()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The latest later build, once there is one.
#[must_use]
pub fn latest() -> Option<Update> {
    #[cfg(test)]
    {
        HANDED.with(|handed| handed.borrow().clone())
    }
    #[cfg(not(test))]
    {
        lock().latest.clone()
    }
}

#[cfg(test)]
thread_local! {
    /// The window tests run side by side on threads of one process: each hands its own later
    /// builds over.
    static HANDED: std::cell::RefCell<Option<Update>> = const { std::cell::RefCell::new(None) };
}

/// Hand `bundle` to the windows of this thread as the next later build, as the watch does.
#[cfg(test)]
pub fn hand_over(bundle: Bundle) {
    HANDED.with(|handed| {
        let mut handed = handed.borrow_mut();
        let generation = handed
            .as_ref()
            .map_or(1, |update| update.generation.saturating_add(1));
        *handed = Some(Update {
            generation,
            bundle: Arc::new(bundle),
            read_at: Instant::now(),
        });
    });
}

/// Wake `wake` whenever a later build arrives.
pub fn subscribe(wake: ui::wake::WakeHandle) {
    let mut shared = lock();
    shared.wakes.retain(ui::wake::WakeHandle::is_active);
    shared.wakes.push(wake);
}

/// Whether this process follows later builds: the launcher gave it an updates directory.
#[must_use]
pub fn followed() -> bool {
    updates_dir().is_some()
}

fn updates_dir() -> Option<PathBuf> {
    std::env::var_os(UPDATES_ENV)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

/// Start watching for later builds of the installed package `app`, when the launcher gave this
/// process an updates directory.
pub fn start(app: &str) {
    let Some(dir) = updates_dir() else {
        return;
    };
    let app = app.to_owned();
    let started = std::thread::Builder::new()
        .name("tilolang-updates".into())
        .spawn(move || watch(&dir, &app));
    if let Err(error) = started {
        eprintln!("tilolang: not following later builds: {error}");
    }
}

#[cfg(target_os = "linux")]
fn watch(dir: &Path, app: &str) {
    let watch = match DirWatch::new(dir) {
        Ok(watch) => watch,
        Err(error) => {
            eprintln!("tilolang: not following later builds: {error}");
            return;
        }
    };
    let mut seen = String::new();
    loop {
        if let Some(name) = current_generation(dir)
            && name != seen
        {
            seen.clone_from(&name);
            take(dir, &name, app);
        }
        if let Err(error) = watch.wait() {
            eprintln!("tilolang: not following later builds: {error}");
            return;
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn watch(_dir: &Path, _app: &str) {}

fn current_generation(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(CURRENT_FILE)).ok()?;
    let name = text.trim();
    (!name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit())).then(|| name.to_owned())
}

/// Read generation `name` and hand it to the windows.
fn take(dir: &Path, name: &str, app: &str) {
    let read_at = Instant::now();
    let bundle = match Bundle::read(&dir.join(name)) {
        Ok(bundle) => bundle,
        Err(error) => {
            eprintln!("tilolang: the new build does not read: {error}");
            return;
        }
    };
    if bundle.meta.app != app {
        eprintln!(
            "tilolang: the new build is the app {}, not {app}; it is not taken",
            bundle.meta.app
        );
        return;
    }
    let mut shared = lock();
    let generation = shared
        .latest
        .as_ref()
        .map_or(1, |update| update.generation.saturating_add(1));
    shared.latest = Some(Update {
        generation,
        bundle: Arc::new(bundle),
        read_at,
    });
    shared.wakes.retain(ui::wake::WakeHandle::is_active);
    for wake in &shared.wakes {
        wake.wake();
    }
}

/// An inotify watch of the updates directory: wakes when `current` is replaced.
#[cfg(target_os = "linux")]
struct DirWatch {
    fd: std::os::fd::OwnedFd,
}

#[cfg(target_os = "linux")]
impl DirWatch {
    fn new(dir: &Path) -> std::io::Result<Self> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        use std::os::unix::ffi::OsStrExt as _;
        // SAFETY: a plain syscall; the result is checked before it is owned.
        let raw = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a fresh descriptor this function owns.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        let path = std::ffi::CString::new(dir.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        // SAFETY: a valid descriptor and a NUL-terminated path.
        let added =
            unsafe { libc::inotify_add_watch(fd.as_raw_fd(), path.as_ptr(), libc::IN_MOVED_TO) };
        if added < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { fd })
    }

    /// Block until something was moved into the directory.
    fn wait(&self) -> std::io::Result<()> {
        use std::os::fd::AsRawFd as _;
        let mut buffer = [0_u8; 4096];
        loop {
            // SAFETY: reading into a buffer this function owns.
            let read = unsafe {
                libc::read(
                    self.fd.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if read > 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}
