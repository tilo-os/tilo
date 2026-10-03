//! Settings — standard Tilo application entry point.
//!
//! The live window runs on the app framework (`SettingsShell`). Format-specific
//! `--shots` scenarios keep the standalone screenshot pipeline.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop. Crash-producing lints remain denied.
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

#[cfg(not(feature = "thin"))]
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
#[cfg(not(feature = "thin"))]
use ui::widget::Widget;

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    #[cfg(not(feature = "thin"))]
    if standalone_capture() {
        let spec = LaunchSpec::new("so.tilo.settings", || {
            Box::new(settings::SettingsApp::new()) as Box<dyn Widget>
        })
        .shot_handler(render_shot)
        .configure_app(configure);
        ui::launch::run_or_exit(&spec);
        return;
    }
    ui::launch::run_or_exit(&ui::framework::launch_spec::<settings::SettingsShell>());
}

#[cfg(not(feature = "thin"))]
fn configure(app: &mut ui::App) {
    // Sidebar filtering rebuilds the widget tree; restore its first focusable
    // field after each structural change so typing retains the caret.
    app.autofocus_on_structure_change = true;
    // Apply Appearance-pane scale, theme, contrast and motion changes live.
    app.appearance_reconcile = true;
}

#[cfg(not(feature = "thin"))]
fn standalone_capture() -> bool {
    std::env::args().any(|arg| {
        arg == "--shots"
            || arg.starts_with("--shots=")
            || arg == "--responsive-matrix"
            || arg.starts_with("--responsive-matrix=")
    })
}

#[cfg(not(feature = "thin"))]
fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or(LaunchError::ShotsUnsupported)?;
    let result = args.viewport.map_or_else(
        || settings::shots::render_for_path_with_text_scale(out, args.text_scale),
        |viewport| {
            settings::shots::render_responsive_to_path_with_text_scale(
                viewport,
                out,
                args.text_scale,
            )
        },
    );
    result.map_err(|error| LaunchError::Operation(format!("settings screenshot failed: {error}")))
}
