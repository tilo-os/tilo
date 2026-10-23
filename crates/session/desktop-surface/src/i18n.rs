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

    /// A window whose app crashed says so in words, naming the app twice:
    /// what stopped, and what to open again.
    #[test]
    fn a_crashed_app_is_named_in_plain_words() {
        let notice = t_args("browser-app-crashed", &[("app", "Canvas".into())]);
        assert_eq!(
            notice,
            "Canvas stopped working, so it was closed to keep the rest of the desktop running. \
             Close this window, or open Canvas again."
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

    /// A key the code asks for but a catalogue lacks renders as the raw key
    /// (or English on a German desktop), so every one must be defined. The
    /// `activity-` namespace holds catalogue keys only: the pane picks many
    /// of them with a `match` before resolving.
    #[test]
    fn every_key_used_in_code_is_in_every_catalogue() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut dirs = vec![manifest.join("src")];
        let desktop = manifest.join("../desktop/src");
        if desktop.is_dir() {
            dirs.push(desktop);
        }
        catalog::UsedKeys::scan_with_namespaces(&dirs, &["activity-"])
            .min_keys(200)
            .check_defined(CATALOGS);
    }

    #[test]
    fn counts_choose_the_plural_form_in_every_locale() {
        let changes = |changes: usize, people: usize| {
            t_args(
                "activity-changes-workspace",
                &[
                    ("workspace", "Design".into()),
                    ("changes", changes.into()),
                    ("people", people.into()),
                ],
            )
        };
        let english = (changes(1, 1), changes(3, 2));
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = (changes(1, 1), changes(3, 2));
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(english.0, "Design · 1 change by 1 person");
        assert_eq!(english.1, "Design · 3 changes by 2 people");
        assert_eq!(german.0, "Design · 1 Änderung von 1 Person");
        assert_eq!(german.1, "Design · 3 Änderungen von 2 Personen");
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
