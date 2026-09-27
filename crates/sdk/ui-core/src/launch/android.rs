//! The Android entry point.
//!
//! [`crate::android_main!`] in an app's `main.rs` exports `android_main`,
//! which android-activity calls on its own thread once `TiloActivity` has
//! created the native activity. It prepares the process the way a desktop
//! launcher would and then runs the app's ordinary `main`, so the Android app
//! takes the same [`super::run`] path as every other native host.
//!
//! Android starts a process with no launcher environment, so this module
//! supplies one: stdout and stderr go to logcat, `HOME`, `TMPDIR` and the XDG
//! base directories point into the app's private storage, `LANG` follows the
//! device locale, and the window's system-bar insets reach the runner.
//!
//! Java calls into Rust through `static native` methods on the Java classes in
//! `so.tilo.android`. `TiloActivity` loads the app library with
//! `System.loadLibrary` before the native activity starts, so the JVM resolves
//! those methods by their exported `Java_so_tilo_android_<Class>_<method>`
//! symbols; see `nativeSafeAreaChanged` below.

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

/// Run the app's `main` for the activity android-activity handed to
/// `android_main`. Called by [`crate::android_main!`]; not an app API.
#[doc(hidden)]
pub fn run_main<T: std::process::Termination>(app: AndroidApp, main: fn() -> T) {
    forward_stdio_to_logcat();
    install_environment(&app);
    if APP.set(app).is_err() {
        // `TiloActivity` ends the process when its activity is destroyed:
        // winit's event loop and the toolkit's process state run once.
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

/// Android connects a process's stdout and stderr to `/dev/null`. Everything
/// the toolkit reports goes through them (`tracing`'s formatter, `diag!`,
/// panic messages), so pipe both into logcat line by line.
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

/// The directories and locale a desktop session would have exported, placed
/// in the app's sandbox so `xdg-paths`, `dirs` and every store built on them
/// resolve there unchanged.
fn install_environment(app: &AndroidApp) {
    let Some(files) = app.internal_data_path() else {
        crate::diag!("tilo: the activity reports no internal data path");
        return;
    };
    let mut vars: Vec<(&str, std::ffi::OsString)> = super::android_env::directories(&files)
        .into_iter()
        .map(|(name, dir)| {
            let _ = std::fs::create_dir_all(&dir);
            (name, dir.into_os_string())
        })
        .collect();
    if let Some(locale) = locale(app) {
        vars.push(("LANG", format!("{locale}.UTF-8").into()));
    }
    // The process belongs to the activity; there is no runtime host to
    // re-exec under, as a desktop launch would.
    vars.push(("TILO_RUNTIME_STANDALONE", "1".into()));
    for (name, value) in vars {
        #[allow(
            clippy::disallowed_methods,
            reason = "android_main runs before the app starts any thread that reads the environment; this is the process's launcher environment"
        )]
        // SAFETY: called first thing in `android_main`, before the app or the
        // toolkit spawns threads; the only other threads are the JVM's and
        // android-activity's, which do not read the C environment.
        unsafe {
            std::env::set_var(name, value);
        }
    }
}

/// The device locale as a POSIX `language_COUNTRY`.
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

/// The system bars', display cutout's and soft keyboard's current overlap of
/// the window, in physical pixels: `[left, top, right, bottom]`.
pub fn safe_area_insets() -> [i32; 4] {
    SAFE_AREA
        .each_ref()
        .map(|inset| inset.load(Ordering::Relaxed))
}

/// `TiloActivity.nativeSafeAreaChanged`, called on the UI thread whenever the
/// window's insets change. It can run before `android_main` has started.
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
    if let Some(app) = APP.get() {
        app.create_waker().wake();
    }
}
