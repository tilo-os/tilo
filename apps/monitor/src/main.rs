//! Monitor — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — the framework app in a session.
//!   * `--shots PATH` — capture the sample snapshot to a PNG and exit;
//!     `--scenario NAME` stages one of [`SCENARIOS`].

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

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// The capture scenarios `--shots --scenario <name>` stages.
const SCENARIOS: &[&str] = &[
    "detail",
    "resources",
    "resources-many",
    "empty",
    "process-loading",
    "process-permission-error",
    "no-results",
    "flat",
    "tree-expanded",
    "per-process",
    "kernel-threads",
    "context-menu",
    "interval",
];

/// Monitor's binary entry point: the framework app in a session, the
/// launcher for captures, audits and headless runs.
pub fn main() {
    if !wants_standalone(std::env::args().skip(1)) {
        ui::framework::run::<monitor::MonitorShell>();
        return;
    }
    ui::launch::run_or_exit(&launch_spec(monitor::app_root));
}

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

/// A capture shows the sample snapshot, never the machine it runs on.
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::ShotFactory"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let mut app = monitor::MonitorApp::new();
    if let Some(scenario) = args.scenario.as_deref() {
        app.seed_shot_scenario(scenario);
    }
    Ok(Box::new(app))
}

fn launch_spec(root: fn() -> Box<dyn Widget>) -> LaunchSpec {
    launch_spec_base(root).history(&monitor::framework_history::HISTORY)
}

fn launch_spec_base(root: fn() -> Box<dyn Widget>) -> LaunchSpec {
    LaunchSpec::new("so.tilo.monitor", root)
        .shot_root(shot_root)
        .scenarios(SCENARIOS)
        .configure_app(configure_app)
}

fn configure_app(app: &mut ui::App) {
    let _ = app;
    #[cfg(not(debug_assertions))]
    {
        app.debug_mode = false;
    }
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    use ui::history_mode::{HistoryDefinition, HistoryPresentation, HistoryScopeDefinition};

    static FOREIGN_HISTORY: HistoryDefinition = HistoryDefinition {
        app_id: "so.tilo.foreign",
        scopes: &[HistoryScopeDefinition {
            key: "foreign",
            title: "Foreign",
            presentation: HistoryPresentation::External { reason: "foreign" },
            content_schema: None,
        }],
    };

    fn inert_root() -> Box<dyn Widget> {
        Box::new(monitor::MonitorApp::new())
    }

    fn memory_state(_: &'static str) -> (ui::State, ui::persist::DocWriter) {
        (
            ui::State::new(),
            ui::persist::DocWriter::disabled("controlled Monitor launch"),
        )
    }

    #[test]
    fn launch_admits_only_the_root_history_declaration() {
        // Phase `production_launch_spec_installs_the_root_history_definition`.
        {
            let spec = launch_spec(inert_root).open_app_global_state(memory_state);
            spec.validate_history_declaration().unwrap();
            let mut app = spec.build_controlled_app_for_test().unwrap();
            let (installed, root) = app.controlled_history_definitions_for_test();
            assert!(std::ptr::eq(
                installed.unwrap(),
                std::ptr::from_ref(&monitor::framework_history::HISTORY)
            ));
            assert!(std::ptr::eq(
                root.unwrap(),
                std::ptr::from_ref(&monitor::framework_history::HISTORY)
            ));
            assert!(
                app.build_a11y_tree_update(1.0)
                    .nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some("History"))
            );
        }

        // Phase `missing_and_foreign_launch_declarations_are_not_admitted`.
        {
            let missing = launch_spec_base(inert_root).open_app_global_state(memory_state);
            assert_eq!(
                missing.validate_history_declaration(),
                Err(ui::history_mode::HistoryRegistrationError::Missing)
            );
            let app = missing.build_controlled_app_for_test().unwrap();
            let (installed, root) = app.controlled_history_definitions_for_test();
            assert!(installed.is_none());
            assert!(std::ptr::eq(
                root.unwrap(),
                std::ptr::from_ref(&monitor::framework_history::HISTORY)
            ));

            let foreign = launch_spec_base(inert_root)
                .history(&FOREIGN_HISTORY)
                .open_app_global_state(memory_state);
            assert_eq!(
                foreign.validate_history_declaration(),
                Err(ui::history_mode::HistoryRegistrationError::ForeignSource)
            );
            assert!(matches!(
                foreign.build_controlled_app_for_test(),
                Err(LaunchError::Operation(message)) if message.contains("different installed application")
            ));
        }
    }
}
