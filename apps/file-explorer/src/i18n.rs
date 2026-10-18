//! Fluent localization for Files.
//!
//! Catalogs are embedded so standalone, sandboxed, headless and test runs all
//! resolve the same copy, through the toolkit's [`mod@ui::i18n`] manager. Locale
//! selection follows the usual POSIX variables (`LC_ALL`, `LC_MESSAGES`,
//! `LANGUAGE`, `LANG`) and falls back per-message to `en-US`.

use ui::i18n::ArgValue;

#[cfg(test)]
const SOURCE_FTL: &str = include_str!("../locales/en-US.ftl");
#[cfg(test)]
const DE_FTL: &str = include_str!("../locales/de-DE.ftl");

// Region variants such as de-AT and de-CH negotiate to the complete German
// catalog, while ICU and Fluent keep the requested locale for number,
// plural and date rules.
ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

/// Change the Files locale without rebuilding the widget tree. The host can
/// call this when the desktop locale changes; the next structural render
/// resolves fresh strings. Unknown/invalid locales safely use `en-US`.
pub fn set_locale(locale: &str) {
    CATALOG.set_locale(locale);
}

/// Active BCP-47 language identifier, for diagnostics and host integration.
pub fn current_locale() -> String {
    CATALOG.current_locale()
}

/// A local modification time in the toolkit's date style (the time of day
/// left out, for a narrow column).
pub fn format_date(value: chrono::DateTime<chrono::Local>) -> String {
    ui::format::date(value.date_naive())
}

/// A local modification time in the toolkit's timestamp style.
pub fn format_datetime(value: chrono::DateTime<chrono::Local>) -> String {
    ui::format::timestamp(&value)
}

/// A file size in the toolkit's size style (SI units).
pub fn format_bytes(bytes: u64) -> String {
    ui::format::size(bytes)
}

pub fn arg<V: Into<ArgValue>>(name: &str, value: V) -> (&str, ArgValue) {
    (name, value.into())
}

/// Resolve `key` under `locale` without leaving it selected.
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
    /// The German catalogue is complete and structurally matches the source, every key is registered, and negotiation falls back correctly.
    #[test]
    fn catalogs_are_complete_registered_and_negotiated() {
        tilo_copy_lint::catalog::check_translations(&[("en-US", SOURCE_FTL), ("de-DE", DE_FTL)]);

        // --- phase: catalog_negotiation_uses_german_regions_and_source_fallback ---
        {
            assert_eq!(t_args_for("de-DE", "files-menu-open", &[]), "Öffnen");
            assert_eq!(t_args_for("de-AT", "files-menu-open", &[]), "Öffnen");
            assert_eq!(t_args_for("fr-FR", "files-menu-open", &[]), "Open");
            assert_eq!(t_args_for("C.UTF-8", "files-menu-open", &[]), "Open");
            assert_eq!(
                t_args_for(
                    "de-DE",
                    "files-share-created",
                    &[arg("person", "Zoë <Admin>")]
                ),
                "Für Zoë <Admin> freigegeben; heruntergeladene Kopien können nicht zurückgerufen werden"
            );
        }
    }

    /// Pseudolocale expansion, plural grammar, locale plural rules and CLDR date and byte formatting.
    #[test]
    fn localised_formatting_plurals_and_pseudolocale() {
        // --- phase: pseudolocale_expands_copy_and_preserves_nonlinguistic_data ---
        {
            let rendered = t_args_for(
                "qps-ploc",
                "files-share-created",
                &[arg("person", "Zoë 🔗")],
            );
            assert!(rendered.starts_with('⟦'));
            assert!(rendered.ends_with('⟧'));
            assert!(
                rendered.contains('🔗'),
                "non-letter data survives: {rendered}"
            );
            assert!(!rendered.contains("Shared with"));
        }

        // --- phase: plural_messages_are_grammatical_and_interpolation_is_not_executable ---
        {
            assert_eq!(
                t_args_for("en-US", "files-sharing-summary", &[arg("count", 1)]),
                "1 active grant; copied bytes cannot be recalled"
            );
            assert_eq!(
                t_args_for("en-US", "files-sharing-summary", &[arg("count", 3)]),
                "3 active grants; copied bytes cannot be recalled"
            );
            let hostile = "{ $count } <b>not markup</b>";
            assert_eq!(
                t_args_for(
                    "en-US",
                    "files-share-with-progress",
                    &[arg("person", hostile)]
                ),
                format!("Sharing with {hostile}…")
            );
        }

        // --- phase: fluent_plural_rules_follow_the_active_locale_even_during_english_fallback ---
        {
            assert_eq!(
                t_args_for("en-US", "files-keeping-offline", &[arg("count", 21)]),
                "Keeping 21 items offline"
            );
            assert_eq!(
                t_args_for("ru-RU", "files-keeping-offline", &[arg("count", 21)]),
                "Keeping 21 item offline"
            );
        }

    }

    /// Collaboration and generic surfaces take labels from the catalogue and the pseudolocale preserves paths and hostile names.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn surfaces_use_catalog_labels_and_preserve_data() {
        // --- phase: migrated_collaboration_surfaces_do_not_embed_source_labels ---
        {
            let sources = [
                include_str!("files_app.rs"),
                include_str!("views/file_view.rs"),
                include_str!("history_control.rs"),
                include_str!("sharing.rs"),
                include_str!("received_shares.rs"),
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

            // Cover every non-interpolated message, rather than relying only on a
            // hand-picked phrase list. Matching the quoted literal avoids false
            // positives from prose comments while still catching UI constructors,
            // status assignments and accessibility values.
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

            // Interpolated messages do not have one complete static catalog value,
            // so guard their distinctive literal prefixes separately.
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

        // --- phase: generic_files_surfaces_do_not_embed_catalog_labels ---
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

        // --- phase: generic_pseudolocale_preserves_paths_and_hostile_names ---
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
