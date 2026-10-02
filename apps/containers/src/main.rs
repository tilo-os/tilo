//! Containers — standard Tilo application entry point.

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

// `shot_root` must keep this exact `Result` return type to satisfy
// `ui::launch::ShotFactory` (`fn(&LaunchArgs) -> Result<Box<dyn Widget>,
// LaunchError>`), even though this app's shot scenarios never fail.
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(match args.scenario.as_deref() {
        Some(scenario) => containers::app_root_shot(scenario),
        None => containers::app_root_offline(),
    })
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn wants_standalone(args: impl Iterator<Item = String>) -> bool {
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" => return true,
            flag if flag.starts_with('-') => {}
            _ => {}
        }
    }
    false
}

pub fn main() {
    if !wants_standalone(std::env::args().skip(1)) {
        ui::framework::run::<containers::ContainersShell>();
        return;
    }
    ui::launch::run_or_exit(&launch_spec(containers::app_root));
}

fn launch_spec(root: fn() -> Box<dyn Widget>) -> LaunchSpec {
    LaunchSpec::new("so.tilo.containers", root)
        .shot_root(shot_root)
        .scenarios(&["confirm", "prompt"])
        .history(&containers::framework_history::HISTORY)
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    fn inert_root() -> Box<dyn Widget> {
        Box::new(containers::history_test_root())
    }

    fn memory_state(_: &'static str) -> (ui::State, ui::persist::DocWriter) {
        (
            ui::State::new(),
            ui::persist::DocWriter::disabled("controlled Containers launch"),
        )
    }

    #[test]
    fn production_launch_installs_containers_history() {
        let spec = launch_spec(inert_root).open_app_global_state(memory_state);
        let mut app = spec.build_controlled_app_for_test().unwrap();
        let (installed, root) = app.controlled_history_definitions_for_test();
        assert!(std::ptr::eq(
            installed.unwrap(),
            std::ptr::addr_of!(containers::framework_history::HISTORY)
        ));
        assert!(std::ptr::eq(
            root.unwrap(),
            std::ptr::addr_of!(containers::framework_history::HISTORY)
        ));
        assert!(
            app.build_a11y_tree_update(1.0)
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("History"))
        );
    }
}
