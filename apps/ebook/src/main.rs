//! Books — thin binary entry point. All logic lives in the library.
//!
//! Modes: default runs the live winit event loop via `ui::App`; `--shots
//! PATH` renders one offscreen frame to a PNG and exits (identical passes
//! to the live app). `--shots` also accepts `--scenario NAME` (see
//! [`ebook::app_root_for_shot`]) and `--viewport WxH` to capture states the
//! single-frame renderer can't reach through live input.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
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

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

/// # Errors
///
/// Returns an error if a `--shots` capture fails to render or write its PNG.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    if !ebook::standalone_launch() {
        ui::framework::run::<ebook::EbookShell>();
        return;
    }
    ui::launch::run_or_exit(
        &LaunchSpec::new_with_args("so.tilo.ebook", live_root)
            .max_positionals(1)
            .shot_root(shot_root)
            .shot_handler(shot)
            .shot_viewport(shot_viewport)
            .allowed_options(&["--size"])
            .scenarios(&[
                "onboarding",
                "read",
                "shelf",
                "library",
                "page-bubble",
                "settings-library",
                "narrow",
                "import-error",
                "import-loading",
                "type",
                "large-text",
                "library-scrolled",
                "long-toc",
                "long-annotations",
                "serif",
                "dark",
                "light",
                "sepia",
                "hicontrast",
                "reduced-motion",
                "font-picker",
                "settings-colours",
                "settings-fonts",
                "settings-fonts-rename",
                "settings-shortcuts",
                "scheme-editor",
                "picker",
                "word-select",
                "select",
                "highlight",
                "select-drag",
                "note",
                "comments",
                "continuous",
                "continuous-select",
                "two-up",
                "spread",
                "two-up-select",
                "spread-select",
            ])
            .configure_app(|app| ebook::actions::register(app.action_registry_mut())),
    );
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::LaunchSpec::new_with_args"
)]
fn live_root(args: &LaunchArgs) -> Result<Box<dyn ui::widget::Widget>, LaunchError> {
    let path = args.positionals().first().map(std::path::PathBuf::from);
    match path.filter(|path| {
        path.exists()
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("epub"))
    }) {
        Some(path) => Ok(ebook::app_root_for_path(&path)),
        None => Ok(ebook::app_root()),
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn ui::widget::Widget>, LaunchError> {
    if !args.positionals().is_empty() {
        return live_root(args);
    }
    Ok(ebook::app_root_for_shot(
        args.scenario.as_deref().unwrap_or_default(),
    ))
}

fn shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let mut harness = ui::headless::FrameHarness::new_with_theme(
        ui::State::new(),
        shot_root(args)?,
        args.viewport.unwrap_or_else(|| shot_viewport(args)),
        ui::launch::shot_theme(args),
    );
    if args.scenario.as_deref() != Some("import-loading") {
        harness
            .wait_for_visuals(std::time::Duration::from_secs(15))
            .map_err(LaunchError::Operation)?;
    }
    let out = args
        .shots
        .as_ref()
        .ok_or_else(|| LaunchError::Operation("Missing screenshot path".into()))?;
    harness.render_png(out);
    Ok(())
}

fn parse_viewport(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

fn shot_viewport(args: &LaunchArgs) -> (u32, u32) {
    args.value("--size")
        .and_then(parse_viewport)
        .unwrap_or(DEFAULT_SHOTS_VIEWPORT)
}
