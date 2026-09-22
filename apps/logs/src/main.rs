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
    ui::launch::run_or_exit(&launch_spec(logs::app_root));
}

fn launch_spec(root: fn() -> Box<dyn Widget>) -> LaunchSpec {
    launch_spec_base(root).history(&logs::framework_history::HISTORY)
}

fn launch_spec_base(root: fn() -> Box<dyn Widget>) -> LaunchSpec {
    LaunchSpec::new("so.tilo.logs", root)
        .shot_root(shot_root)
        .allowed_flags(&[
            "--detail",
            "--error",
            "--filter-errors",
            "--loading",
            "--actions",
        ])
        .configure_app(configure_app)
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
        Box::new(logs::LogsApp::new(logs::journal::sample_entries(), false))
    }

    fn memory_state(_: &'static str) -> (ui::State, ui::persist::Autosave) {
        (
            ui::State::new(),
            ui::persist::Autosave::disabled("controlled Logs launch"),
        )
    }

    #[test]
    fn production_launch_spec_installs_the_root_history_definition() {
        let spec = launch_spec(inert_root).open_app_global_state(memory_state);
        spec.validate_history_declaration().unwrap();
        let mut app = spec.build_controlled_app_for_test().unwrap();
        let (installed, root) = app.controlled_history_definitions_for_test();
        assert!(std::ptr::eq(
            installed.unwrap(),
            &logs::framework_history::HISTORY
        ));
        assert!(std::ptr::eq(
            root.unwrap(),
            &logs::framework_history::HISTORY
        ));
        assert!(
            app.build_a11y_tree_update(1.0)
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("History"))
        );
    }

    #[test]
    fn missing_and_foreign_launch_declarations_are_not_admitted() {
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
            &logs::framework_history::HISTORY
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
