//! The shell's Fluent catalogue.
//!
//! Chrome strings are keys resolved through the toolkit's [`mod@ui::i18n`]
//! manager, never literals. The catalogues are embedded with `include_str!`,
//! so a `--shots` render, a unit test and a live session resolve the same
//! copy. The user's locale comes from the environment (`LC_ALL`,
//! `LC_MESSAGES`, `LANGUAGE`, `LANG`), exactly as the toolkit's own widget
//! strings choose it; a message missing from a translation falls back to
//! `en-US`.

use ui::i18n::{ArgValue, EmbeddedCatalog};

const CATALOGS: &[(&str, &str)] = &[
    ("en-US", include_str!("../../locales/en-US.ftl")),
    ("de-DE", include_str!("../../locales/de-DE.ftl")),
];

/// Loaded per thread on first use; a host's manager is joined, never replaced.
/// The probe is a key every catalogue revision defines.
static CATALOG: EmbeddedCatalog = catalog();

const fn catalog() -> EmbeddedCatalog {
    let catalog =
        EmbeddedCatalog::new("en-US", CATALOGS, "continuity-continue").with_system_locale();
    // Unit tests assert source copy throughout the shell; keep them
    // deterministic on any developer or CI host locale.
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
}

/// Load the shell's catalogue and the toolkit's widget labels on this
/// thread, in the desktop's language. Startup calls it once; every lookup
/// repeats it for free, so a thread that draws chrome without having
/// resolved a shell string yet still gets translated toolkit widgets.
pub fn ensure() {
    CATALOG.ensure();
    ui::i18n::I18nManager::register_widget_messages();
}

/// Resolve a shell string key.
pub fn t(key: &str) -> String {
    ensure();
    CATALOG.t(key)
}

/// Resolve a shell string key that interpolates the given nouns.
pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    ensure();
    CATALOG.t_args(key, args)
}

/// The desktop's language (BCP 47), for content that is localised outside
/// the catalogue, such as emoji names.
pub fn current_locale() -> String {
    CATALOG.current_locale()
}

/// Select the locale written in `appearance.language`, if any.
/// A running compositor calls this after that key changes so chrome
/// strings switch without a restart. Missing leaves the current selection.
pub fn apply_configured_locale() {
    ensure();
    if let Some(language) =
        settings_store::str_at(&settings_store::load(), "appearance", "language")
    {
        CATALOG.set_locale(language);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_copy_lint::catalog;

    #[test]
    fn every_key_the_shell_uses_resolves_to_a_real_string() {
        for key in [
            "continuity-continue",
            "continuity-continue-subtitle",
            "continuity-dismiss-title",
            "continuity-dismiss-subtitle",
            "continuity-other-device",
            "tab-closed-toast",
            "tab-closed-group-toast",
            "tab-closed-undo",
            "tab-closed-undo-hint",
            "tab-closed-undo-hint-unbound",
            "overview-windows-title",
            "overview-windows-description",
            "overview-window-move-failed",
        ] {
            let s = t(key);
            assert_ne!(s, key, "{key} has no entry in the catalogue");
            assert!(!s.trim().is_empty(), "{key} resolves to nothing");
        }
        assert_eq!(
            t_args(
                "continuity-continue",
                &[("app", "Notes".into()), ("device", "Laptop".into())]
            ),
            "Continue Notes from Laptop"
        );
    }

    #[test]
    fn every_catalogue_entry_resolves_to_a_real_string() {
        let keys = catalog::messages(CATALOGS[0].1);
        assert!(!keys.is_empty());
        for key in keys.keys() {
            let value = t(key);
            assert_ne!(&value, key, "{key} has no entry in the catalogue");
            assert!(!value.trim().is_empty(), "{key} resolves to nothing");
        }
    }

    /// A key only English defines would show English on a German desktop,
    /// and a German key English lacks is dead copy.
    #[test]
    fn every_shell_string_is_translated_in_every_locale() {
        catalog::check_translations(CATALOGS);
    }

    /// A key the code asks for but a catalogue lacks renders as the raw key
    /// (or English on a German desktop), so every one must be defined.
    #[test]
    fn every_key_used_in_code_is_in_every_catalogue() {
        catalog::UsedKeys::scan(&[concat!(env!("CARGO_MANIFEST_DIR"), "/src")])
            .min_keys(100)
            .check_defined(CATALOGS);
    }

    /// Toolkit widgets the shell draws take their labels from the toolkit's
    /// catalogue, which the shell loads with its own.
    #[test]
    fn toolkit_widgets_in_the_shell_follow_a_german_desktop() {
        use ui::widget::Widget as _;
        ensure();
        ui::i18n::I18nManager::set_locale("de-DE");
        let prompt = ui::widget::PermissionPrompt::new(
            "org.example.app".into(),
            t("capability-camera"),
            true,
        );
        let node = prompt.accessibility();
        ui::i18n::I18nManager::set_locale("en-US");
        let node = node.expect("a permission prompt is semantic");
        assert_eq!(
            node.name.as_deref(),
            Some("Kamerazugriff – angefragt von org.example.app")
        );
        let buttons: Vec<_> = node
            .children
            .iter()
            .map(|button| button.name.as_deref())
            .collect();
        assert_eq!(
            buttons,
            [
                Some("Erlauben"),
                Some("Diese Sitzung"),
                Some("Immer erlauben"),
                Some("Ablehnen")
            ]
        );
    }

    #[test]
    fn german_desktops_resolve_german_copy() {
        let english = t("tab-closed-undo");
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = t("tab-closed-undo");
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(english, "Undo");
        assert_eq!(german, "Rückgängig");
    }
}

#[cfg(test)]
#[path = "i18n_copy_lint_tests.rs"]
mod copy_lint_tests;
