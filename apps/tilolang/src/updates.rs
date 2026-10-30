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
//! workspace record, as its next start would.

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
    let (replaced, replacements) = std::sync::mpsc::channel();
    let watch = tilo_files::native::watch_changes(
        dir,
        |path| path.file_name() == Some(std::ffi::OsStr::new(CURRENT_FILE)),
        move || {
            let _ = replaced.send(());
        },
    );
    let _watch = match watch {
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
        if replacements.recv().is_err() {
            eprintln!("tilolang: not following later builds: the watch ended");
            return;
        }
        // One read covers every replacement queued meanwhile.
        while replacements.try_recv().is_ok() {}
    }
}

#[cfg(not(target_os = "linux"))]
fn watch(_dir: &Path, _app: &str) {}

fn current_generation(dir: &Path) -> Option<String> {
    let path = dir.join(CURRENT_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // No build was published yet.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!("tilolang: {}: {error}", path.display());
            return None;
        }
    };
    let name = text.trim();
    if !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit()) {
        Some(name.to_owned())
    } else {
        eprintln!("tilolang: {} names no build: {name:?}", path.display());
        None
    }
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
