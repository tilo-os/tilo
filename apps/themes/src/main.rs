//! Themes — standard Tilo application entry point.

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

// The signature is fixed by `ui::launch::ShotFactory`: other apps' shot roots
// may fail, so the alias is fallible even where this one isn't.
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is dictated by ui::launch::ShotFactory, which other apps' fallible shot roots need"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let width = args.viewport.unwrap_or(ui::launch::DEFAULT_VIEWPORT).0;
    Ok(themes::app_root_for_shot(width))
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(&launch_spec());
}

fn launch_spec() -> LaunchSpec {
    LaunchSpec::new("so.tilo.themes", themes::app_root)
        .history(&themes::framework_history::HISTORY)
        .shot_root(shot_root)
        .configure_app(|app| themes::actions::register(app.action_registry_mut()))
}

#[test]
fn production_launch_registers_history() {
    launch_spec().validate_history_declaration().unwrap();
}
