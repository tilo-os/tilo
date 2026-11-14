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

    /// The rows a window's menu offers for sharing it with its workspace read the same in every
    /// language the desktop ships, and the row that shares is never the row that stops.
    #[test]
    fn a_windows_menu_names_the_rows_that_share_it_in_every_locale() {
        let words = || {
            (
                t("window-menu-share-with-workspace"),
                t("window-menu-stop-sharing-with-workspace"),
                t_args("window-menu-stop-sharing-watched", &[("viewers", 2.into())]),
            )
        };
        let english = words();
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = words();
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(english.0, "Share with workspace");
        assert_eq!(english.1, "Stop sharing with workspace");
        assert_eq!(english.2, "Stop sharing with workspace (2 watching)");
        assert_eq!(german.0, "Mit dem Arbeitsbereich teilen");
        assert_eq!(german.1, "Teilen mit dem Arbeitsbereich beenden");
        assert_eq!(
            german.2,
            "Teilen mit dem Arbeitsbereich beenden (2 sehen zu)"
        );
        for (locale, (share, stop, _)) in [("en-US", &english), ("de-DE", &german)] {
            assert_ne!(share, stop, "{locale}");
        }
    }

    /// A device that asked to see a shared window nothing goes out of waits for it, in every
    /// language the desktop ships: neither the bar nor the window's menu says it watches.
    #[test]
    fn viewers_sent_nothing_are_said_to_wait_in_every_locale() {
        let words = |count: i64| {
            [
                t_args("topbar-sharing-count-waiting", &[("waiting", count.into())]),
                t_args(
                    "topbar-sharing-count-waiting-control",
                    &[("waiting", count.into()), ("controllers", 1.into())],
                ),
                t_args(
                    "topbar-sharing-count-mixed",
                    &[("viewers", 1.into()), ("waiting", count.into())],
                ),
                t_args(
                    "topbar-sharing-count-mixed-controlling",
                    &[
                        ("viewers", 2.into()),
                        ("controllers", 1.into()),
                        ("waiting", count.into()),
                    ],
                ),
                t_args(
                    "window-menu-stop-sharing-waiting",
                    &[("viewers", count.into())],
                ),
                t_args("topbar-sharing-person-waiting", &[("name", "Ada".into())]),
                t_args(
                    "topbar-sharing-agent-waiting-control",
                    &[("name", "Scribe".into())],
                ),
            ]
        };
        let english = [words(1), words(2)];
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = [words(1), words(2)];
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(
            english[0],
            [
                "1 waiting",
                "1 waiting, 1 may control",
                "1 watching, 1 waiting",
                "2 watching, 1 in control, 1 waiting",
                "Stop sharing with workspace (1 waiting)",
                "Waiting: Ada",
                "Agent waiting, may control: Scribe",
            ]
        );
        assert_eq!(
            english[1],
            [
                "2 waiting",
                "2 waiting, 1 may control",
                "1 watching, 2 waiting",
                "2 watching, 1 in control, 2 waiting",
                "Stop sharing with workspace (2 waiting)",
                "Waiting: Ada",
                "Agent waiting, may control: Scribe",
            ]
        );
        assert_eq!(
            german[0],
            [
                "1 wartet",
                "1 wartet, 1 darf steuern",
                "1 sieht zu, 1 wartet",
                "2 sehen zu, 1 steuert, 1 wartet",
                "Teilen mit dem Arbeitsbereich beenden (1 wartet)",
                "Wartet: Ada",
                "Agent wartet, darf steuern: Scribe",
            ]
        );
        assert_eq!(
            german[1],
            [
                "2 warten",
                "2 warten, 1 darf steuern",
                "1 sieht zu, 2 warten",
                "2 sehen zu, 1 steuert, 2 warten",
                "Teilen mit dem Arbeitsbereich beenden (2 warten)",
                "Wartet: Ada",
                "Agent wartet, darf steuern: Scribe",
            ]
        );
    }

    /// A screen reader is told who views a shared window in the words the bar's pill uses: what
    /// the workspace says they are, their name alone where only that is known, and for a viewer
    /// nothing names that it has no name. No reader is read a stand-in for a name.
    #[test]
    fn a_viewer_is_named_to_a_reader_or_said_to_have_no_name_in_every_locale() {
        let entries = || {
            let access = t_args("topbar-viewer-view-only", &[("targets", "Budget".into())]);
            ["agent", "person", "named", "unnamed"].map(|kind| {
                t_args(
                    "topbar-viewer-entry",
                    &[
                        ("name", "Framework".into()),
                        ("kind", kind.into()),
                        ("access", access.as_str().into()),
                    ],
                )
            })
        };
        let english = entries();
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = entries();
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(
            english,
            [
                "Framework (agent): view only Budget",
                "Framework (person): view only Budget",
                "Framework: view only Budget",
                "a device with no name: view only Budget",
            ]
        );
        assert_eq!(
            german,
            [
                "Framework (Agent): sieht nur Budget",
                "Framework (Person): sieht nur Budget",
                "Framework: sieht nur Budget",
                "ein Gerät ohne Namen: sieht nur Budget",
            ]
        );
        for (locale, source) in CATALOGS {
            for stand_in in ["unidentified", "unbekannte Sitzung"] {
                assert!(!source.contains(stand_in), "{locale} says {stand_in:?}");
            }
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
