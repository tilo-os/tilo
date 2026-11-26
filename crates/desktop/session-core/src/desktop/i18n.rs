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
    ("en-US", include_str!("../../locales/en-US.ftl")),
    ("de-DE", include_str!("../../locales/de-DE.ftl")),
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

    tilo_test_env::scenario! {
        fn the_catalogue_resolves_every_key_in_plain_words;

        phase a_strip_is_called_a_strip_in_every_locale;
        // A workspace holds strips; the desktop names a strip a strip
        // everywhere, as the native strip switcher does.
        {
            for (locale, source) in CATALOGS {
                for line in source.lines().filter(|line| !line.starts_with('#')) {
                    let value = line.split_once('=').map_or(line, |(_, value)| value);
                    for word in value.split(|c: char| !c.is_alphanumeric()) {
                        assert!(
                            !matches!(
                                word.to_lowercase().as_str(),
                                "schreibtisch" | "schreibtische"
                            ),
                            "{locale}: {line}"
                        );
                    }
                }
            }
        }

        phase a_stopped_app_is_named_in_plain_words;
        // A window whose app stopped says so in words, naming the app, and says
        // why without blaming the person for it.
        {
            assert_eq!(
                crate::desktop::outcome::Closed::Crashed
                    .outcome("Canvas", t("answer-reopen"))
                    .detail(),
                "Canvas stopped working. It ended unexpectedly."
            );
            assert_eq!(
                crate::desktop::outcome::Closed::Memory
                    .outcome("Canvas", t("answer-reopen"))
                    .detail(),
                "Canvas was closed to free memory"
            );
            assert_eq!(
                crate::desktop::outcome::Closed::Runaway
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
                "It stopped before its window appeared."
            );
        }

        phase every_catalogue_entry_resolves_to_a_real_string;
        {
            let keys = catalog::messages(CATALOGS[0].1);
            assert!(!keys.is_empty());
            for key in keys.keys() {
                let value = t(key);
                assert_ne!(&value, key, "{key} has no entry in the catalogue");
                assert!(!value.trim().is_empty(), "{key} resolves to nothing");
            }
        }

        phase every_desktop_string_is_translated_in_every_locale;
        // A key only English defines would show English on a German desktop,
        // and a German key English lacks is dead copy.
        {
            catalog::check_translations(CATALOGS);
        }
    }

    /// The tab showing another device's window says what it is called, whose window it is and
    /// how its pictures stand in every language the desktop ships.
    #[test]
    fn a_tab_showing_another_devices_window_reads_in_every_locale() {
        let words = || {
            [
                t_args(
                    "peer-window-name",
                    &[("window", "Writer".into()), ("device", "Laptop".into())],
                ),
                t_args("peer-window-of", &[("name", "Laptop".into())]),
                t("peer-window-first-picture"),
                t_args(
                    "peer-window-ended",
                    &[("why", t("peer-window-ended-sender-stopped").into())],
                ),
            ]
        };
        let english = words();
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = words();
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(
            english,
            [
                "Writer on Laptop",
                "Laptop's window",
                "Waiting for its first picture",
                "Ended: the sender stopped sharing it",
            ]
        );
        assert_eq!(
            german,
            [
                "Writer auf Laptop",
                "Fenster von Laptop",
                "Wartet auf das erste Bild",
                "Beendet: der Absender teilt es nicht mehr",
            ]
        );
    }

    /// The same tab while nothing of the window is shown in it says where the window runs and
    /// why it is not here, in every language the desktop ships: by the device's name, and for a
    /// device nothing names without one.
    #[test]
    fn a_tab_whose_window_is_not_shown_says_where_it_runs_and_why_in_every_locale() {
        let words = || {
            let named = |key: &str| t_args(key, &[("device", "Laptop".into())]);
            [
                t_args(
                    "peer-window-elsewhere",
                    &[("app", "Browser".into()), ("device", "Laptop".into())],
                ),
                t_args(
                    "peer-window-elsewhere-unnamed",
                    &[("app", "Browser".into())],
                ),
                t("peer-window-not-shared"),
                named("peer-window-awaited"),
                t("peer-window-awaited-unnamed"),
                named("peer-window-host-away"),
                t("peer-window-host-away-unnamed"),
                named("peer-window-host-left"),
                t("peer-window-host-left-unnamed"),
            ]
        };
        let english = words();
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = words();
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(
            english,
            [
                "Browser runs on Laptop",
                "Browser runs on another device of this workspace",
                "It is not shared with this workspace right now",
                "Waiting for Laptop to send it",
                "Waiting for its device to send it",
                "Laptop is not connected to this workspace right now",
                "Its device is not connected to this workspace right now",
                "Laptop is no longer in this workspace",
                "Its device is no longer in this workspace",
            ]
        );
        assert_eq!(
            german,
            [
                "Browser läuft auf Laptop",
                "Browser läuft auf einem anderen Gerät dieses Arbeitsbereichs",
                "Es wird gerade nicht mit diesem Arbeitsbereich geteilt",
                "Wartet darauf, dass Laptop es sendet",
                "Wartet darauf, dass sein Gerät es sendet",
                "Laptop ist gerade nicht mit diesem Arbeitsbereich verbunden",
                "Sein Gerät ist gerade nicht mit diesem Arbeitsbereich verbunden",
                "Laptop gehört nicht mehr zu diesem Arbeitsbereich",
                "Sein Gerät gehört nicht mehr zu diesem Arbeitsbereich",
            ]
        );
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
        for dir in ["desktop/activity", "desktop/notifications"] {
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
