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

fn command(args: &LaunchArgs) -> Result<bool, LaunchError> {
    let Some(dir) = args.value("--emit-photos") else {
        return Ok(false);
    };
    emit_photos(std::path::Path::new(dir)).map_err(|error| {
        LaunchError::Operation(format!("could not emit sample photos: {error}"))
    })?;
    println!("camera: wrote sample photos to {dir}");
    Ok(true)
}

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
        if args.has("--recording") {
            std::env::set_var("CAMERA_SHOT_RECORDING", "1");
        }
        if let Some(state) = args.value("--camera-state") {
            std::env::set_var("CAMERA_SHOT_CAMERA_STATE", state);
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
    let mut harness = ui::headless::FrameHarness::new_with_theme(
        ui::State::new(),
        shot_root(args)?,
        (width, height),
        theme,
    );
    // A few frames let what settles on its own first frame (the settings
    // sheet fitting the window) take its place before the capture.
    for _ in 0..3 {
        harness.relayout();
    }
    harness.render_png(out);
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
            .allowed_flags(&[
                "--scan-first",
                "--disk",
                "--crop",
                "--timer-running",
                "--recording",
            ])
            .allowed_options(&[
                "--screen",
                "--mode",
                "--dir",
                "--hover",
                "--width",
                "--height",
                "--camera-state",
                "--mode",
            ])
            .allowed_live_options(&["--emit-photos"])
            .command_handler(command)
            .configure_app(|app| camera::actions::register(app.action_registry_mut())),
    );
}

/// Write procedural photos and a lossless QR poster for headless testing.
fn emit_photos(dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use camera::model::PhotoFormat;
    for seed in 0..5u8 {
        let frame = camera::frame::thumbnail(seed, 960, 640);
        camera::library::write_photo(dir, &frame, PhotoFormat::Jpeg)?;
        let _ = ui::rt::sleep_thread(std::time::Duration::from_millis(4));
    }
    let (qr, _) = camera::frame::scanner_frame(
        4,
        900,
        640,
        "WIFI:T:WPA2;S:Tilo Lounge;P:welcome-aboard;H:false;;",
    );
    camera::library::write_photo(dir, &qr, PhotoFormat::Png)?;
    Ok(())
}
