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

pub fn format_date(value: chrono::DateTime<chrono::Local>) -> String {
    ui::format::date(value.date_naive())
}

pub fn format_datetime(value: chrono::DateTime<chrono::Local>) -> String {
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
    #[test]
    fn catalogs_are_complete_registered_and_negotiated() {
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

    #[test]
    fn localised_formatting_plurals_and_pseudolocale() {
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

    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn surfaces_use_catalog_labels_and_preserve_data() {
        {
            let sources = [
                include_str!("files_app.rs"),
                include_str!("views/file_view.rs"),
                include_str!("history_control.rs"),
                include_str!("main.rs"),
                include_str!("model.rs"),
            ];
            let production = sources.map(|source| {
                source
                    .split_once("#[cfg(test)]")
                    .map_or(source, |(production, _)| production)
            });
            for phrase in [
                "Keep offline",
                "Use automatic storage",
                "Retry download",
                "Copy one-time share link",
                "Sharing and access…",
                "Availability…",
                "Version history…",
                "Shared with 1 recipient",
                "Transfer failed",
                "Replica status unknown",
                "Has a version conflict",
            ] {
                assert!(
                    production.iter().all(|source| !source.contains(phrase)),
                    "migrated collaborative label remains hard-coded: {phrase}"
                );
            }

            for value in SOURCE_FTL
                .split("## Application chrome")
                .next()
                .unwrap_or(SOURCE_FTL)
                .lines()
                .filter_map(|line| {
                    let (key, value) = line.split_once(" = ")?;
                    (!key.starts_with("files-history-action-")
                        && !value.is_empty()
                        && !value.contains('{'))
                    .then_some(value)
                })
            {
                let literal = format!("\"{value}\"");
                assert!(
                    production[..5]
                        .iter()
                        .all(|source| !source.contains(&literal)),
                    "catalog value remains a production string literal: {value}"
                );
            }

            for prefix in [
                "\"Sharing with ",
                "\"Shared with ",
                "\"Collaborative record: ",
                "\"Replica safety ",
                "\"Conflict resolved ",
                "\"Received-file service ",
                "\"Share service ",
            ] {
                assert!(
                    production.iter().all(|source| !source.contains(prefix)),
                    "interpolated collaborative copy remains hard-coded: {prefix}"
                );
            }
        }

        {
            let sources = [
                include_str!("breadcrumbs.rs"),
                include_str!("favorites.rs"),
                include_str!("fileops.rs"),
                include_str!("files_app.rs"),
                include_str!("path_bar.rs"),
                include_str!("places_sidebar.rs"),
                include_str!("search_provider.rs"),
                include_str!("templates.rs"),
                include_str!("toolbar.rs"),
                include_str!("views/file_view.rs"),
            ];
            let production = sources.map(|source| {
                source
                    .split_once("#[cfg(test)]")
                    .map_or(source, |(production, _)| production)
                    .lines()
                    .filter(|line| !line.trim_start().starts_with("//") && !line.contains(".join("))
                    .collect::<Vec<_>>()
                    .join("\n")
            });

            for (key, value) in SOURCE_FTL.lines().filter_map(|line| {
                let (key, value) = line.split_once(" = ")?;
                let is_generic = key.starts_with("files-place-")
                    || key.starts_with("files-column-")
                    || key.starts_with("files-empty-")
                    || key.starts_with("files-property-")
                    || key.starts_with("files-context-")
                    || key.starts_with("files-sort-")
                    || key.starts_with("files-view-")
                    || matches!(
                        key,
                        "files-app-title"
                            | "files-back"
                            | "files-forward"
                            | "files-up"
                            | "files-try-again"
                            | "files-loading"
                            | "files-new-name-placeholder"
                            | "files-new-folder-default-name"
                            | "files-a11y-activity"
                            | "files-a11y-places"
                            | "files-a11y-favourite"
                            | "files-a11y-system-location"
                    );
                (is_generic && !value.is_empty() && !value.contains('{')).then_some((key, value))
            }) {
                let literal = format!("\"{value}\"");
                assert!(
                    production.iter().all(|source| !source.contains(&literal)),
                    "generic catalog value {key} remains a production string literal: {value}"
                );
            }

            for prefix in [
                "\"Could not open this folder:",
                "\"Could not load folder:",
                "\"Current folder:",
                "\"Type: ",
                "\"Location: ",
                "\"Modified: ",
                "\"New folder failed:",
                "\"Delete failed:",
                "\"Moved to Trash",
                "\"Restore failed:",
                "\"Empty Trash failed:",
                "\"New file failed:",
                "\"Rename failed:",
            ] {
                assert!(
                    production.iter().all(|source| !source.contains(prefix)),
                    "interpolated generic copy remains hard-coded: {prefix}"
                );
            }
        }

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
