//! Files' Fluent catalogues, and the toolkit's date and size styles its rows show.

use ui::i18n::ArgValue;

#[cfg(test)]
const SOURCE_FTL: &str = include_str!("../locales/en-US.ftl");
#[cfg(test)]
const DE_FTL: &str = include_str!("../locales/de-DE.ftl");

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

pub fn set_locale(locale: &str) {
    CATALOG.set_locale(locale);
}

pub fn current_locale() -> String {
    CATALOG.current_locale()
}

pub fn format_datetime(value: jiff::Timestamp) -> String {
    ui::format::timestamp(&value)
}

pub fn format_bytes(bytes: u64) -> String {
    ui::format::size(bytes)
}

pub fn arg<V: Into<ArgValue>>(name: &str, value: V) -> (&str, ArgValue) {
    (name, value.into())
}

#[cfg(test)]
fn t_args_for(locale: &str, key: &str, args: &[(&str, ArgValue)]) -> String {
    let previous = current_locale();
    set_locale(locale);
    let value = t_args(key, args);
    set_locale(&previous);
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    tilo_test_env::scenario! {
        fn files_copy_is_complete_localised_and_keeps_data;

        phase catalogs_are_complete_registered_and_negotiated;
        {
            tilo_copy_lint::catalog::check_translations(&[("en-US", SOURCE_FTL), ("de-DE", DE_FTL)]);

            {
                assert_eq!(t_args_for("de-DE", "files-menu-open", &[]), "Öffnen");
                assert_eq!(t_args_for("de-AT", "files-menu-open", &[]), "Öffnen");
                assert_eq!(t_args_for("fr-FR", "files-menu-open", &[]), "Open");
                assert_eq!(t_args_for("C.UTF-8", "files-menu-open", &[]), "Open");
                assert_eq!(
                    t_args_for(
                        "de-DE",
                        "files-share-failed",
                        &[arg("reason", "Zoë <Admin>")]
                    ),
                    "Das Einladungsfenster wurde nicht geöffnet: Zoë <Admin>"
                );
            }
        }

        phase localised_formatting_plurals_and_pseudolocale;
        {
            {
                let rendered = t_args_for("qps-ploc", "files-share-failed", &[arg("reason", "Zoë 🔗")]);
                assert!(rendered.starts_with('⟦'));
                assert!(rendered.ends_with('⟧'));
                assert!(
                    rendered.contains('🔗'),
                    "non-letter data survives: {rendered}"
                );
                assert!(!rendered.contains("Making"));
            }

            {
                assert_eq!(
                    t_args_for("en-US", "files-item-count", &[arg("count", 1)]),
                    "1 item"
                );
                assert_eq!(
                    t_args_for("en-US", "files-item-count", &[arg("count", 3)]),
                    "3 items"
                );
                let hostile = "{ $count } <b>not markup</b>";
                assert_eq!(
                    t_args_for("en-US", "files-share-failed", &[arg("reason", hostile)]),
                    format!("The invite sheet did not open: {hostile}")
                );
            }

            {
                assert_eq!(
                    t_args_for("en-US", "files-keeping-offline", &[arg("count", 21)]),
                    "Keeping 21 items offline"
                );
                assert_eq!(
                    t_args_for("de-DE", "files-keeping-offline", &[arg("count", 21)]),
                    "21 Elemente werden offline verfügbar gehalten"
                );
                assert_eq!(
                    t_args_for("ru-RU", "files-keeping-offline", &[arg("count", 21)]),
                    "Keeping 21 items offline"
                );
            }
        }

        phase interpolated_data_survives_the_pseudolocale;
        {
            {
                let path = "/home/Zoë/<script> & notes";
                let rendered = t_args_for(
                    "qps-ploc",
                    "files-a11y-current-folder",
                    &[arg("path", path)],
                );
                assert!(rendered.starts_with('⟦') && rendered.ends_with('⟧'));
                assert!(
                    rendered.contains(path),
                    "interpolated path is preserved: {rendered}"
                );
                assert!(!rendered.contains("Current folder"));
            }
        }
    }
}
