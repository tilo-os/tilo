//! Books — thin binary entry point. All logic lives in the library.
//!
//! Modes: default runs the live winit event loop via `ui::App`; `--shots
//! PATH` renders one offscreen frame to a PNG and exits (identical passes
//! to the live app). `--shots` also accepts `--scenario NAME` (see
//! [`ebook::app_root_for_shot`]) and `--viewport WxH` to capture states the
//! single-frame renderer can't reach through live input.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
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
    ui::launch::run_or_exit(
        &LaunchSpec::new_with_args("dev.qos.ebook", live_root)
            .max_positionals(1)
            .shot_root(shot_root)
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
                "type",
                "library-scrolled",
                "serif",
                "dark",
                "light",
                "sepia",
                "hicontrast",
                "font-picker",
                "settings-colours",
                "settings-fonts",
                "settings-fonts-rename",
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
    Ok(ebook::app_root_for_shot(
        args.scenario.as_deref().unwrap_or_default(),
    ))
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
