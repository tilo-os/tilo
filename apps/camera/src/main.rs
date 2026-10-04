//! Camera — standard Tilo application entry point.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
#[allow(
    clippy::disallowed_methods,
    reason = "only reached from the --shots handler on the main thread before the \
              headless harness or any camera worker thread exists, so nothing reads \
              the environment concurrently; the widget reads these variables while \
              it is constructed"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    // These variables are consumed while the widget is constructed. Startup
    // is still single-threaded here, before any camera worker exists.
    unsafe {
        for (option, variable) in [
            ("--screen", "CAMERA_SCREEN"),
            // `--mode photo|video` poses the viewfinder's capture sub-mode so
            // the video shutter and switcher state can be captured headless.
            ("--mode", "CAMERA_MODE"),
            ("--dir", "CAMERA_DIR"),
            ("--scroll", "CAMERA_SCROLL"),
            ("--hover", "CAMERA_SHOT_HOVER"),
            ("--mode", "CAMERA_SHOT_MODE"),
        ] {
            if let Some(value) = args.value(option) {
                std::env::set_var(variable, value);
            }
        }
        if args.has("--scan-first") {
            std::env::set_var("CAMERA_SCAN_FIRST", "1");
        }
        if args.has("--timer-running") {
            std::env::set_var("CAMERA_SHOT_TIMER_RUNNING", "1");
        }
        if let Some(state) = args.value("--camera-state") {
            std::env::set_var("CAMERA_SHOT_CAMERA_STATE", state);
        }
        // Sample timestamps and relative labels must be byte-for-byte stable
        // across offscreen renders. A caller can still override this fixed
        // fixture clock explicitly when it needs a particular reference time.
        if args.shots.is_some() && std::env::var_os("CAMERA_SHOT_NOW").is_none() {
            std::env::set_var("CAMERA_SHOT_NOW", "1800000000");
        }
        if args.has("--crop") {
            std::env::set_var("CAMERA_CROP", "1");
        }
    }

    let disk = args.has("--disk") || std::env::var_os("CAMERA_SHOTS_DISK").is_some();
    Ok(if disk {
        camera::app_root()
    } else {
        camera::shots_root()
    })
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or(LaunchError::ShotsUnsupported)?;
    let (default_width, default_height) = args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT);
    let width = args
        .value("--width")
        .and_then(|value| value.parse().ok())
        .unwrap_or(default_width);
    let height = args
        .value("--height")
        .and_then(|value| value.parse().ok())
        .unwrap_or(default_height);
    let theme = ui::launch::shot_theme(args);
    ui::headless::FrameHarness::new_with_theme(
        ui::State::new(),
        shot_root(args)?,
        (width, height),
        theme,
    )
    .render_png(out);
    println!("so.tilo.camera: wrote shot to {}", out.display());
    Ok(())
}

pub fn main() {
    if !camera::standalone_launch() {
        ui::framework::run::<camera::CameraShell>();
        return;
    }
    ui::launch::run_or_exit(
        &LaunchSpec::new("so.tilo.camera", camera::app_root)
            .shot_handler(render_shot)
            .allowed_flags(&["--scan-first", "--disk", "--crop", "--timer-running"])
            .allowed_options(&[
                "--screen",
                "--mode",
                "--dir",
                "--scroll",
                "--hover",
                "--width",
                "--height",
                "--camera-state",
                "--mode",
            ])
            .configure_app(|app| camera::actions::register(app.action_registry_mut())),
    );
}
