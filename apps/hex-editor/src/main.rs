//! Hex editor — standard Tilo application entry point.

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
use ui::widget::Widget;

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match ui::launch::ShotFactory's fn(&LaunchArgs) -> Result<..> signature"
)]
fn shot_root(_: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(hex_editor::app_root())
}

fn wants_standalone() -> bool {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--shots" {
            let _ = args.next();
            return true;
        }
        if ["--responsive-matrix", "--native-audit", "--headless"]
            .iter()
            .any(|mode| arg == *mode || arg.starts_with(&format!("{mode}=")))
            || arg.starts_with("--shots=")
        {
            return true;
        }
        if arg.starts_with('-') {
            continue;
        }
        if std::path::Path::new(&arg).exists() {
            return true;
        }
    }
    false
}

fn shot_spec() -> LaunchSpec {
    LaunchSpec::new("so.tilo.hex-editor", hex_editor::app_root)
        .max_positionals(1)
        .shot_root(shot_root)
        .configure_app(|app| hex_editor::actions::register(app.action_registry_mut()))
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    // A positional file or `--shots` stays on the standalone widget path.
    if wants_standalone() {
        ui::launch::run_or_exit(&shot_spec());
        return;
    }
    ui::launch::run_or_exit(&hex_editor::launch_spec());
}
