//! Video Editor — thin binary entry point. All logic lives in the library.
//!
//! Every mode runs the editor on the app framework: the live window, and the
//! `--shots` / `--responsive-matrix` / `--native-audit` / `--headless`
//! captures, which stage their scenario through the same host (see
//! [`launch_spec`]).

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    use_bundled_sample_media_for_captures();
    let spec = launch_spec();
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

/// The editor's launch: the framework's, plus the capture flags. Every mode
/// — the live window, `--shots`, `--responsive-matrix`, `--native-audit`,
/// `--headless` — runs the framework-hosted editor:
///
/// * `--shots [--scenario NAME]` loads the demo project and stages the
///   scenario's view state;
/// * `--empty` captures the live app on a blank project;
/// * `--narrow` captures the collapsed media bin at a narrow width;
/// * `--import PATH` imports a real file, places it and waits for its frame.
fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<video_editor::VideoEditor>()
        .capture_scenario(capture)
        .shot_viewport(shot_viewport)
        .allowed_flags(&["--empty", "--narrow"])
        .allowed_options(&["--data-dir", "--import"])
}

fn shot_viewport(args: &ui::launch::LaunchArgs) -> (u32, u32) {
    const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);
    /// Narrow capture viewport: below the bin's collapse breakpoint, so
    /// `--shots --narrow` shows the overlay-flap layout.
    const NARROW_SHOTS_VIEWPORT: (u32, u32) = (720, 820);
    if args.has("--narrow") {
        NARROW_SHOTS_VIEWPORT
    } else {
        DEFAULT_SHOTS_VIEWPORT
    }
}

fn capture(
    harness: &mut ui::headless::FrameHarness,
    args: &ui::launch::LaunchArgs,
) -> Result<(), ui::launch::LaunchError> {
    let import = args
        .value("--import")
        .map(|path| format!("{}{path}", video_editor::app::IMPORT_CAPTURE));
    let scenario = if let Some(import) = &import {
        Some(import.as_str())
    } else if args.has("--empty") {
        Some(video_editor::app::EMPTY_CAPTURE)
    } else if args.has("--narrow") {
        Some(video_editor::app::NARROW_CAPTURE)
    } else {
        args.scenario.as_deref()
    };
    ui::framework::stage::<video_editor::VideoEditor>(harness, scenario)?;
    if import.is_some() {
        // A clip that never decodes (no `decode` feature, an unreadable file)
        // is still captured, showing what the editor shows for it.
        let _ = harness.wait_for_visuals(std::time::Duration::from_secs(6));
    }
    Ok(())
}
