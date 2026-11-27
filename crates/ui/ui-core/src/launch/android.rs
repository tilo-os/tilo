//! The Android entry point. [`crate::android_main!`], through an app library's
//! `dev_reload_entry!`, exports `android_main`, which android-activity calls on its own thread once
//! `TiloActivity` has created the native activity.

use std::ffi::{CString, c_void};
use std::io::{BufRead, BufReader};
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};

pub use winit::platform::android::activity::AndroidApp;

/// Run the app's entry for the activity android-activity handed to `android_main`.
#[doc(hidden)]
pub fn run_main<T: std::process::Termination>(app: AndroidApp, main: fn() -> T) {
    forward_stdio_to_logcat();
    install_environment(&app);
    if tilo_ui_base::platform::android::APP.set(app).is_err() {
        tilo_ui_base::diag!("tilo: android_main started twice in one process");
        return;
    }
    // SAFETY: android-activity published the activity in `ndk_context` before it called
    // `android_main`, and takes it back only once this returns, after `activity_ended`.
    unsafe { tilo_process::android::activity_started() };
    let _ = main().report();
    tilo_process::android::activity_ended();
}

/// A launch that failed before its window opened, shown to the person: the activity shows
/// `message` and ends once they close it. Until then this thread answers the activity's
/// lifecycle, as the window's event loop would have, so the system never finds the app hung.
pub(crate) fn launch_failed(message: &str) {
    tilo_ui_base::diag!("tilo: the app did not open: {message}");
    let shown = tilo_process::android::with_activity(|env, activity| {
        let message = env.new_string(message)?;
        env.call_method(
            activity,
            "launchFailed",
            "(Ljava/lang/String;)V",
            &[jni::objects::JValue::Object(&message)],
        )
        .map(drop)
    });
    if let Err(error) = shown {
        tilo_ui_base::diag!("tilo: the launch's failure could not be shown: {error}");
        return;
    }
    let Some(app) = tilo_ui_base::platform::android::app() else {
        return;
    };
    let mut destroyed = false;
    while !destroyed {
        app.poll_events(None, |event| {
            if matches!(
                event,
                winit::platform::android::activity::PollEvent::Main(
                    winit::platform::android::activity::MainEvent::Destroy,
                )
            ) {
                destroyed = true;
            }
        });
    }
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
        tilo_ui_base::diag!("tilo: the activity reports no internal data path");
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
    let vars: Vec<(&str, std::ffi::OsString)> = super::android_env::directories(files)
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
    if let Some(filter) = log_filter_property() {
        set_var("RUST_LOG", filter.into());
    }
}

/// The log filter set on the device with `adb shell setprop debug.tilo.log <filter>`: an app on
/// Android starts with no shell environment a developer could put `RUST_LOG` in.
fn log_filter_property() -> Option<String> {
    let mut value = [0; libc::PROP_VALUE_MAX as usize];
    // SAFETY: the name is NUL-terminated, and `value` holds the PROP_VALUE_MAX bytes the call
    // writes at most, NUL included.
    let len =
        unsafe { libc::__system_property_get(c"debug.tilo.log".as_ptr(), value.as_mut_ptr()) };
    // SAFETY: the call NUL-terminated what it wrote.
    let filter = unsafe { std::ffi::CStr::from_ptr(value.as_ptr()) };
    (len > 0).then(|| filter.to_string_lossy().into_owned())
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

/// `TiloActivity.nativeProvideContext`, called on the UI thread in the activity's `onCreate`,
/// before the native activity starts: the application's context is published for the life of the
/// process, unless the app's file provider published it already, so android-activity publishes
/// the activity over it and the application's is there again once `android_main` returns.
#[unsafe(no_mangle)]
pub extern "system" fn Java_so_tilo_android_TiloActivity_nativeProvideContext(
    mut env: jni::JNIEnv<'_>,
    _class: jni::objects::JClass<'_>,
    activity: jni::objects::JObject<'_>,
) {
    if let Err(error) = tilo_process::android::provide_application_context(&mut env, &activity) {
        tilo_ui_base::diag!("tilo: the application's context was not published: {error}");
    }
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
