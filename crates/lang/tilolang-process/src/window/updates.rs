//! Later builds of the package while it runs, as `tilolang dev` publishes them without a session
//! (`crates/lang/tilolang-cli/src/dev.rs`) in the directory protocol of `tilo_app_module`: the
//! directory is named in `TILO_APP_MODULE_UPDATES`. Without one, nothing here runs. In a
//! session the workspace's tilolang host runs the instance, and the session hands it later
//! builds over the host's socket.
//!
//! A thread watches the directory and reads each new generation as a [`Bundle`], which it hands
//! over to the later builds the window's instance follows ([`builds`]).

use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

use tilo_app_module::CURRENT_FILE;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::instance::LaterBuilds;

struct Shared {
    /// Whether the watch runs.
    started: bool,
    /// The generation of the directory's build handed over last, so a read that finds the same
    /// one again takes nothing.
    taken: Option<u64>,
}

fn lock() -> std::sync::MutexGuard<'static, Shared> {
    static SHARED: Mutex<Shared> = Mutex::new(Shared {
        started: false,
        taken: None,
    });
    SHARED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The later builds this process took, which a window's instance follows.
#[must_use]
pub fn builds() -> Arc<LaterBuilds> {
    static BUILDS: LazyLock<Arc<LaterBuilds>> = LazyLock::new(Arc::default);
    Arc::clone(&BUILDS)
}

/// Start watching for later builds of the installed package `app`, when this process was
/// started with an updates directory and does not watch already.
pub fn start(app: &str) {
    let Some(dir) = tilo_app_module::dir_from_env() else {
        return;
    };
    if std::mem::replace(&mut lock().started, true) {
        return;
    }
    let app = app.to_owned();
    let started = std::thread::Builder::new()
        .name("tilolang-updates".into())
        .spawn(move || watch(&dir, &app));
    if let Err(error) = started {
        eprintln!("tilolang: not following later builds: {error}");
    }
}

/// Take the build of `app` the directory publishes now, as the watch does when one is
/// published: code that starts a window in this process after taking a rebuild — the reload in
/// place of `tilolang dev` — shows the build the running window showed, not the one the process
/// was started on. Then [`start`] following the directory takes the builds after it.
pub fn catch_up(app: &str) {
    let Some(dir) = tilo_app_module::dir_from_env() else {
        return;
    };
    let Ok(Some(generation)) = tilo_app_module::current(&dir) else {
        return;
    };
    if lock().taken == Some(generation) {
        return;
    }
    take(&dir, generation, app);
}

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
    let mut seen = lock().taken.or_else(|| started_generation(dir));
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

/// Read the build the directory published as `published` and hand it over.
fn take(dir: &Path, published: u64, app: &str) {
    let bundle = match Bundle::read(&dir.join(published.to_string())) {
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
    lock().taken = Some(published);
    builds().hand_over(bundle);
}
