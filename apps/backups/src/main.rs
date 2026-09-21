//! Backups — standard Tilo application entry point.

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

// `ShotFactory` (in `ui::launch`) is `fn(&LaunchArgs) -> Result<Box<dyn
// Widget>, LaunchError>` — a fixed callback type shared by every app's
// `--shots` entry point, so this can't drop the `Result` even though this
// particular app never fails to build its shot root.
#[allow(
    clippy::unnecessary_wraps,
    reason = "must match ui::launch's ShotFactory = fn(&LaunchArgs) -> Result<..>"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    if args.has("--empty") {
        Ok(backups::app_root())
    } else if args.scenario.as_deref() == Some("status") {
        let mut app = backups::BackupsApp::new_demo();
        app.arm_status_bubble_for_shots();
        Ok(Box::new(app))
    } else {
        let mut app = backups::BackupsApp::new_demo();
        if let Some(scenario) = args.scenario.as_deref() {
            app.stage_shot_scenario(scenario);
        }
        Ok(Box::new(app))
    }
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(&launch_spec(backups::app_root));
}

fn launch_spec(root: fn() -> Box<dyn Widget>) -> LaunchSpec {
    LaunchSpec::new("dev.tilo.backups", root)
        .shot_root(shot_root)
        .scenarios(&[
            "status",
            "progress",
            "failure",
            "restore",
            "sources",
            "destinations",
            "schedules",
            "unavailable",
        ])
        .allowed_flags(&["--empty"])
        .configure_app(|app| backups::actions::register(app.action_registry_mut()))
        .history(&backups::framework_history::HISTORY)
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    fn inert_root() -> Box<dyn Widget> {
        Box::new(backups::history_test_root())
    }

    fn memory_state(_: &'static str) -> (ui::State, ui::persist::Autosave) {
        (
            ui::State::new(),
            ui::persist::Autosave::disabled("controlled Backups launch"),
        )
    }

    #[test]
    fn production_launch_installs_backups_history() {
        let spec = launch_spec(inert_root).open_app_global_state(memory_state);
        let mut app = spec.build_controlled_app_for_test().unwrap();
        let (installed, root) = app.controlled_history_definitions_for_test();
        assert!(std::ptr::eq(
            installed.unwrap(),
            std::ptr::addr_of!(backups::framework_history::HISTORY)
        ));
        assert!(std::ptr::eq(
            root.unwrap(),
            std::ptr::addr_of!(backups::framework_history::HISTORY)
        ));
        assert!(
            app.build_a11y_tree_update(1.0)
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("History"))
        );
    }
}
