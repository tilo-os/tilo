//! Snippets — standard Tilo application entry point.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
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
    Ok(snippets::app_root_for_shot(
        args.scenario.as_deref().unwrap_or_default(),
    ))
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn install_locales() -> Result<(), ui::i18n::I18nError> {
    ui::i18n::I18nManager::install_system("en-US", snippets::LOCALES)
}

fn wants_standalone() -> bool {
    std::env::args().any(|arg| {
        [
            "--shots",
            "--responsive-matrix",
            "--native-audit",
            "--headless",
        ]
        .iter()
        .any(|mode| arg == *mode || arg.starts_with(&format!("{mode}=")))
    })
}

fn shot_spec() -> LaunchSpec {
    LaunchSpec::new("so.tilo.snippets", snippets::app_root)
        .history(&snippets::framework_history::HISTORY)
        .shot_root(shot_root)
        .scenarios(&["narrow", "detail", "empty", "expand", "language", "more"])
        .configure_app(|app| snippets::actions::register(app.action_registry_mut()))
}

/// Run the native Snippets application.
///
/// # Panics
/// Panics if the embedded locale catalogs are invalid (covered by a startup test).
#[allow(
    clippy::expect_used,
    reason = "the embedded locale catalogs are validated by the startup catalog test"
)]
pub fn main() {
    install_locales().expect("valid bundled Snippets locales");
    if wants_standalone() {
        ui::launch::run_or_exit(&shot_spec());
        return;
    }
    ui::launch::run_or_exit(&snippets::launch_spec());
}

/// Production launch registers history and installs the body editor and action labels.
#[test]
fn startup_registers_history_body_editor_and_action_labels() {
    // Case `production_launch_registers_history`.
    {
        snippets::launch_spec()
            .validate_history_declaration()
            .unwrap();
    }

    // Case `startup_installs_the_body_editor_and_action_labels`.
    {
        install_locales().unwrap();
        let locale = ui::i18n::I18nManager::current_locale();
        let expected = if locale.starts_with("de") {
            ("Snippet-Inhalt", "Inhalt bearbeiten")
        } else {
            ("Snippet body", "Edit body")
        };
        assert_eq!(ui::i18n!("snippets-body-placeholder"), expected.0);
        assert_eq!(ui::i18n!("snippets-edit-body"), expected.1);
    }
}
