//! Settings — standard qOS application entry point.
//!
//! Settings keeps its format-specific screenshot scenario pipeline, while the
//! shared runner owns CLI validation, headless driving, persistence and the
//! live window lifecycle.

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

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

const APP_ID: &str = "dev.qos.settings";

fn root() -> Box<dyn Widget> {
    Box::new(settings::SettingsApp::new())
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or(LaunchError::ShotsUnsupported)?;
    settings::shots::render_for_path(out)
        .map_err(|error| LaunchError::Operation(format!("settings screenshot failed: {error}")))
}

fn configure(app: &mut ui::App) {
    settings::actions::register(app.action_registry_mut());
    // Sidebar filtering rebuilds the widget tree; restore its first focusable
    // field after each structural change so typing retains the caret.
    app.autofocus_on_structure_change = true;
    // Apply Appearance-pane scale, theme, contrast and motion changes live.
    app.appearance_reconcile = true;
}

pub fn main() {
    ui::launch::run_or_exit(
        LaunchSpec::new(APP_ID, root)
            .shot_handler(render_shot)
            .configure_app(configure),
    );
}
