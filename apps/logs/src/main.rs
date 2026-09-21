//! Logs — standard Tilo application entry point.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
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
use ui::widget::Widget;

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let entries = if std::env::var("TILO_LOGS_EMPTY").is_ok() || args.has("--error") {
        Vec::new()
    } else {
        logs::journal::sample_entries()
    };
    let mut app = if args.has("--error") {
        logs::LogsApp::new_with_source_error(
            entries,
            true,
            logs::journal::JournalLoadError::AccessDenied,
        )
    } else {
        logs::LogsApp::new(entries, args.has("--actions"))
    };
    if args.has("--detail") {
        app.shots_show_detail();
    }
    if args.has("--filter-errors") {
        app.shots_filter_errors();
    }
    if args.has("--loading") {
        app.shots_show_loading();
    }
    if args.has("--actions") {
        app.shots_show_actions();
    }
    Ok(Box::new(app))
}

fn configure_app(app: &mut ui::App) {
    logs::actions::register(app.action_registry_mut());
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new("so.tilo.logs", logs::app_root)
            .shot_root(shot_root)
            .allowed_flags(&[
                "--detail",
                "--error",
                "--filter-errors",
                "--loading",
                "--actions",
            ])
            .configure_app(configure_app),
    );
}
