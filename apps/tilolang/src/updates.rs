//! Later builds of the package while it runs.
//!
//! Whoever starts the app may give it a directory its later builds are published into: a
//! development session's launcher follows reinstalls of the package
//! (`crates/packaging/launcher/src/live_module.rs`), and `tilolang dev` on a desktop publishes
//! each saved build itself (`crates/lang/tilolang-cli/src/dev.rs`). Both use the directory
//! protocol of `tilo_app_module`: the directory is named in `TILO_APP_MODULE_UPDATES`. Without
//! one, nothing here runs.
//!
//! A thread watches the directory, reads each new generation as a [`Bundle`] and wakes the
//! windows, which take it over on their own thread ([`crate::window::Window`]): in place when
//! the new build reads the state the old one wrote, and otherwise by starting the app over on the
//! workspace record, as its next start would.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use tilo_app_module::CURRENT_FILE;
use tilolang_tilo::bundle::Bundle;

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

/// Whether this process follows later builds: it was started with an updates directory.
#[must_use]
pub fn followed() -> bool {
    updates_dir().is_some()
}

fn updates_dir() -> Option<PathBuf> {
    tilo_app_module::dir_from_env()
}

/// Start watching for later builds of the installed package `app`, when this process was
/// started with an updates directory.
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

#[cfg(not(target_arch = "wasm32"))]
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
    let mut seen = started_generation(dir);
    loop {
        match tilo_app_module::current(dir) {
            Ok(Some(generation)) if Some(generation) != seen => {
                seen = Some(generation);
                take(dir, generation, app);
            }
            Ok(_) => {}
            Err(error) => eprintln!("tilolang: {error}"),
        }
        if replacements.recv().is_err() {
            eprintln!("tilolang: not following later builds: the watch ended");
            return;
        }
        // One read covers every replacement queued meanwhile.
        while replacements.try_recv().is_ok() {}
    }
}

#[cfg(target_arch = "wasm32")]
fn watch(_dir: &Path, _app: &str) {}

/// The generation this process was started on, when it was started on one: it runs that build
/// already, and taking it again would only redraw what is on screen.
fn started_generation(dir: &Path) -> Option<u64> {
    let module = std::env::var_os(tilolang_tilo::bundle::MODULE_ENV)?;
    let module = Path::new(&module);
    if module.parent() != Some(dir) {
        return None;
    }
    let name = module.file_name()?.to_str()?;
    match name.parse() {
        Ok(generation) => Some(generation),
        Err(error) => {
            eprintln!("tilolang: the build it started on, {name}, names no generation: {error}");
            None
        }
    }
}

/// Read generation `generation` and hand it to the windows.
fn take(dir: &Path, generation: u64, app: &str) {
    let read_at = Instant::now();
    let bundle = match Bundle::read(&dir.join(generation.to_string())) {
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
