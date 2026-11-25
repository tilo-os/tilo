//! The Android entry point. [`crate::android_main!`], through an app library's
//! `dev_reload_entry!`, exports `android_main`, which android-activity calls on its own thread once
//! `TiloActivity` has created the native activity.

use std::ffi::{CString, c_void};
use std::io::{BufRead, BufReader};
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};

pub use winit::platform::android::activity::AndroidApp;

static APP: OnceLock<AndroidApp> = OnceLock::new();

/// The activity this process runs, once `android_main` has started.
pub fn app() -> Option<&'static AndroidApp> {
    APP.get()
}

/// The APK's asset `name` (`tilo/manifest.toml`), read: what an app reads of itself from the APK
/// it was installed from, which no file of the app's own can stand in for.
///
/// # Errors
/// No activity runs yet, or the APK has no such asset.
pub fn read_asset(name: &str) -> std::io::Result<Vec<u8>> {
    let app = app().ok_or_else(|| std::io::Error::other("no Android activity"))?;
    let c_name = CString::new(name).map_err(std::io::Error::other)?;
    let mut asset = app
        .asset_manager()
        .open(&c_name)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "missing from the APK"))?;
    let mut bytes = Vec::new();
    std::io::copy(&mut asset, &mut bytes)?;
    Ok(bytes)
}

/// Copy the APK's asset `name` (`tilo/fonts/manifest.toml`) to `to`, making its directory: what
/// an app copies out of its APK on first start, since Android hands assets out as streams only.
///
/// # Errors
/// No activity runs yet, the APK has no such asset, or `to` cannot be written.
pub fn copy_asset(name: &str, to: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write as _;
    let app = app().ok_or_else(|| std::io::Error::other("no Android activity"))?;
    let c_name = CString::new(name).map_err(std::io::Error::other)?;
    let mut asset = app
        .asset_manager()
        .open(&c_name)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "missing from the APK"))?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(to)?;
    std::io::copy(&mut asset, &mut file)?;
    file.flush()
}

/// Run the app's entry for the activity android-activity handed to `android_main`.
#[doc(hidden)]
pub fn run_main<T: std::process::Termination>(app: AndroidApp, main: fn() -> T) {
    forward_stdio_to_logcat();
    install_environment(&app);
    if APP.set(app).is_err() {
        crate::diag!("tilo: android_main started twice in one process");
        return;
    }
    let _ = main().report();
}

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(
        priority: i32,
        tag: *const std::ffi::c_char,
        text: *const std::ffi::c_char,
    ) -> i32;
}

const ANDROID_LOG_INFO: i32 = 4;

/// Android connects a process's stdout and stderr to `/dev/null`.
#[allow(
    clippy::disallowed_methods,
    reason = "the logcat pump is a dedicated blocking reader thread for the process's lifetime"
)]
fn forward_stdio_to_logcat() {
    let mut fds = [0; 2];
    // SAFETY: `fds` is a writable two-element array, as pipe(2) requires.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    let [read, write] = fds;
    // SAFETY: pipe(2) just returned these descriptors and nothing else owns
    // them; stdout and stderr are replaced, not closed, by dup2(2).
    let reader = unsafe {
        libc::dup2(write, libc::STDOUT_FILENO);
        libc::dup2(write, libc::STDERR_FILENO);
        libc::close(write);
        OwnedFd::from_raw_fd(read)
    };
    let _ = std::thread::Builder::new()
        .name("tilo-logcat".into())
        .spawn(move || {
            let tag = c"tilo";
            for line in BufReader::new(std::fs::File::from(reader)).split(b'\n') {
                let Ok(line) = line else { break };
                let text =
                    CString::new(line).unwrap_or_else(|_| c"(log line contained NUL)".to_owned());
                // SAFETY: both pointers are NUL-terminated strings that live
                // across the call; liblog copies the text.
                unsafe { __android_log_write(ANDROID_LOG_INFO, tag.as_ptr(), text.as_ptr()) };
            }
        });
}

/// The directories and locale a Linux session would have exported, placed in the app's sandbox so
/// `tilo-paths`, `dirs` and every store built on them resolve there unchanged.
fn install_environment(app: &AndroidApp) {
    let Some(files) = app.internal_data_path() else {
        crate::diag!("tilo: the activity reports no internal data path");
        return;
    };
    install_environment_in(&files);
    if let Some(locale) = locale(app) {
        set_var("LANG", format!("{locale}.UTF-8").into());
    }
}

/// [`install_environment`]'s directories for an app whose `getFilesDir()` is `files`: what a
/// process the system started without an activity installs, as a file provider's is, so the
/// device daemon it starts reads and keeps everything in the app's sandbox too.
pub fn install_environment_in(files: &std::path::Path) {
    let mut vars: Vec<(&str, std::ffi::OsString)> = super::android_env::directories(files)
        .into_iter()
        .map(|(name, dir)| {
            let _ = std::fs::create_dir_all(&dir);
            (name, dir.into_os_string())
        })
        .collect();
    // The process belongs to the activity; there is no runtime host to
    // re-exec under, as a Linux launch would.
    for (name, value) in vars {
        set_var(name, value);
    }
}

/// Put `name` in the process's environment before any Tilo code reads it: the JVM's own threads
/// were started by the system, and the C environment is read by Tilo's code alone.
fn set_var(name: &str, value: std::ffi::OsString) {
    #[allow(
        clippy::disallowed_methods,
        reason = "android_main runs before the app starts any thread that reads the environment; this is the process's launcher environment"
    )]
    // SAFETY: no Tilo thread runs yet — `android_main` calls this first, and a file provider
    // calls it from `onCreate` before it starts the daemon — and the threads the system started
    // (the JVM's, android-activity's) read no variable Tilo sets here.
    unsafe {
        std::env::set_var(name, value);
    }
}

fn locale(app: &AndroidApp) -> Option<String> {
    let config = app.config();
    let language = config.language().filter(|language| !language.is_empty())?;
    Some(
        match config.country().filter(|country| !country.is_empty()) {
            Some(country) => format!("{language}_{country}"),
            None => language,
        },
    )
}

static SAFE_AREA: [AtomicI32; 4] = [const { AtomicI32::new(0) }; 4];

static ON_SAFE_AREA_CHANGED: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Call `notify` whenever the window's insets change.
pub(crate) fn on_safe_area_changed(notify: Box<dyn Fn() + Send + Sync>) {
    let _ = ON_SAFE_AREA_CHANGED.set(notify);
}

/// The system bars', display cutout's and soft keyboard's current overlap of the window, in physical
/// pixels: `[left, top, right, bottom]`.
pub fn safe_area_insets() -> [i32; 4] {
    SAFE_AREA
        .each_ref()
        .map(|inset| inset.load(Ordering::Relaxed))
}

/// `TiloActivity.nativeSafeAreaChanged`, called on the UI thread whenever the window's insets change.
#[unsafe(no_mangle)]
pub extern "system" fn Java_so_tilo_android_TiloActivity_nativeSafeAreaChanged(
    _env: *mut c_void,
    _class: *mut c_void,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
) {
    for (slot, value) in SAFE_AREA.iter().zip([left, top, right, bottom]) {
        slot.store(value.max(0), Ordering::Relaxed);
    }
    if let Some(notify) = ON_SAFE_AREA_CHANGED.get() {
        notify();
    }
}
