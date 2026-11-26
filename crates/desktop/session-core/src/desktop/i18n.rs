//! The desktop's Fluent catalogue.
//!
//! Top bar and activity pane strings are keys resolved through the toolkit's
//! [`mod@tilo_ui_base::i18n`] manager, never literals. The catalogues are embedded with
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

tilo_ui_base::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

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
