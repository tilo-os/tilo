//! The desktop's Fluent catalogue.
//!
//! Top bar and activity pane strings are keys resolved through the toolkit's
//! [`mod@ui::i18n`] manager, never literals. The catalogues are embedded with
//! `include_str!`, so the native shell, the browser runtime (which has no
//! file system to read them from) and a unit test resolve the same copy. On a
//! thread with no manager yet, the user's locale comes from the environment,
//! exactly as the toolkit's own widget strings choose it; a host that already
//! installed a manager keeps its locale. A message missing from a
//! translation falls back to `en-US`.

#[cfg(test)]
const CATALOGS: &[(&str, &str)] = &[
    ("en-US", include_str!("../locales/en-US.ftl")),
    ("de-DE", include_str!("../locales/de-DE.ftl")),
];

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

/// Load the desktop's catalogue on this thread, so history entries the
/// desktop records read in its words.
pub fn ensure() {
    CATALOG.ensure();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_copy_lint::catalog;

    #[test]
    fn interpolated_keys_substitute_their_nouns() {
        assert_eq!(
            t_args(
                "topbar-workspace-position",
                &[("position", 2.into()), ("total", 3.into())]
            ),
            "Workspace 2 of 3"
        );
        assert_eq!(
            t_args("topbar-sharing-count", &[("viewers", 2.into())]),
            "2 watching"
        );
    }

    /// A workspace holds strips; the desktop names a strip a strip
    /// everywhere, as the native strip switcher does, never a "desk".
    #[test]
    fn a_strip_is_called_a_strip_in_every_locale() {
        for (locale, source) in CATALOGS {
            for line in source.lines().filter(|line| !line.starts_with('#')) {
                let value = line.split_once('=').map_or(line, |(_, value)| value);
                for word in value.split(|c: char| !c.is_alphanumeric()) {
                    assert!(
                        !matches!(
                            word.to_lowercase().as_str(),
                            "desk" | "desks" | "schreibtisch" | "schreibtische"
                        ),
                        "{locale}: {line}"
                    );
                }
            }
        }
        assert_eq!(t("workspace-menu-new-strip"), "New strip");
    }

    /// A window whose app stopped says so in words, naming the app, and says
    /// why without blaming the person for it.
    #[test]
    fn a_stopped_app_is_named_in_plain_words() {
        assert_eq!(
            crate::outcome::Closed::Crashed
                .outcome("Canvas", t("answer-reopen"))
                .detail(),
            "Canvas stopped working. It ended unexpectedly."
        );
        assert_eq!(
            crate::outcome::Closed::Memory
                .outcome("Canvas", t("answer-reopen"))
                .detail(),
            "Canvas was closed to free memory"
        );
        assert_eq!(
            crate::outcome::Closed::Runaway
                .outcome("Canvas", t("answer-reopen"))
                .detail(),
            "Canvas was using too much memory. It was closed so the rest keeps running."
        );
        assert_eq!(
            t_args("answer-app-not-opened", &[("app", "Notes".into())]),
            "Notes could not open"
        );
        assert_eq!(
            t_args("answer-app-stopped", &[("app", "Notes".into())]),
            "Notes stopped working"
        );
        assert_eq!(
            t("answer-why-not-opened"),
            "It was stopped before a window appeared."
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
    fn every_desktop_string_is_translated_in_every_locale() {
        catalog::check_translations(CATALOGS);
    }

    #[test]
    fn counts_choose_the_plural_form_in_every_locale() {
        let opened = |windows: usize| {
            t_args(
                "activity-event-opened-windows",
                &[("count", windows.into())],
            )
        };
        let english = (opened(1), opened(3));
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = (opened(1), opened(3));
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(english.0, "Opened 1 window");
        assert_eq!(english.1, "Opened 3 windows");
        assert_eq!(german.0, "1 Fenster geöffnet");
        assert_eq!(german.1, "3 Fenster geöffnet");
    }

    #[test]
    fn german_desktops_resolve_german_copy() {
        let english = t("workspace-menu-rename");
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = t("workspace-menu-rename");
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(english, "Rename workspace");
        assert_eq!(german, "Arbeitsbereich umbenennen");
    }
}

#[cfg(test)]
mod copy_lint_tests {
    /// Literals that look like prose but are not copy.
    const ALLOWED: &[(&str, &str)] = &[];

    /// The history pane and the notification views build every string from
    /// the catalogue; the surface's navigation contract is a wire format.
    #[test]
    fn copy_builders_take_catalogue_strings_not_english_literals() {
        for dir in ["activity", "notifications"] {
            tilo_copy_lint::CopyLint::new(
                concat!(env!("CARGO_MANIFEST_DIR"), "/src/").to_owned() + dir,
            )
            .allow(ALLOWED)
            // The activity pane's test fixture, compiled only under test.
            .skip_files(&["testing.rs"])
            .min_files(4)
            .check();
        }
    }
}
