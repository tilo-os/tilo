//! Snippets — standard Tilo application entry point.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
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

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn install_locales() -> Result<(), ui::i18n::I18nError> {
    ui::i18n::I18nManager::install_system("en-US", snippets::LOCALES)
}

/// Run Snippets: the live window, `--shots` and the other launch modes all
/// go through the framework.
///
/// # Panics
/// Panics if the embedded locale catalogs are invalid (covered by a startup test).
#[allow(
    clippy::expect_used,
    reason = "the embedded locale catalogs are validated by the startup catalog test"
)]
pub fn main() {
    install_locales().expect("valid bundled Snippets locales");
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
