//! Video Editor — thin binary entry point. All logic lives in the library.
//!
//! Every mode runs the editor on the app framework: the live window, and the
//! `--shots` / `--responsive-matrix` / `--native-audit` / `--headless`
//! captures, which stage their scenario through the same host (see
//! `video_editor::launch_spec`).

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    use_bundled_sample_media_for_captures();
    let spec: ui::launch::LaunchSpec = video_editor::launch_spec();
    ui::launch::run_or_exit(&spec);
}

/// Point the FUSE-media seam at the bundled `sample-media/` directory for a
/// capture, so the demo project's media resolve to local (hydrated) sources
/// and the capture exercises the hydration branch without a live mount. An
/// existing override is honoured.
#[allow(
    clippy::disallowed_methods,
    reason = "runs first in main, before the launcher or anything else has spawned a thread"
)]
fn use_bundled_sample_media_for_captures() {
    let capture = std::env::args().any(|arg| {
        [
            "--shots",
            "--responsive-matrix",
            "--native-audit",
            "--headless",
        ]
        .iter()
        .any(|mode| arg == *mode || arg.starts_with(&format!("{mode}=")))
    });
    if !capture || std::env::var_os("TILO_VE_MEDIA_DIR").is_some() {
        return;
    }
    let bundled = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sample-media");
    if bundled.exists() {
        // SAFETY: no other thread exists yet to read the environment.
        unsafe {
            std::env::set_var("TILO_VE_MEDIA_DIR", bundled);
        }
    }
}
