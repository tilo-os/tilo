//! Fluent localization for Files.
//!
//! Catalogs are embedded so standalone, sandboxed, headless and test runs all
//! resolve the same copy, through the toolkit's [`mod@ui::i18n`] manager. Locale
//! selection follows the usual POSIX variables (`LC_ALL`, `LC_MESSAGES`,
//! `LANGUAGE`, `LANG`) and falls back per-message to `en-US`.

use icu_datetime::{
    DateTimeFormatter,
    fieldsets::YMD,
    input::{Date, DateTime, Time},
};
use icu_decimal::{DecimalFormatter, input::Decimal};
use icu_locale::Locale;
use ui::i18n::{ArgValue, EmbeddedCatalog};
use unic_langid::LanguageIdentifier;

const SOURCE_FTL: &str = include_str!("../locales/en-US.ftl");
const DE_FTL: &str = include_str!("../locales/de-DE.ftl");

/// Region variants such as de-AT and de-CH negotiate to the complete German
/// catalog, while ICU and Fluent keep the requested locale for number,
/// plural and date rules.
static CATALOG: EmbeddedCatalog = catalog();

const fn catalog() -> EmbeddedCatalog {
    let catalog = EmbeddedCatalog::new(
        "en-US",
        &[("en-US", SOURCE_FTL), ("de-DE", DE_FTL)],
        "files-menu-open",
    )
    .with_system_locale();
    // Unit tests assert source-copy behavior throughout the widget. Keep them
    // deterministic across developer/CI host locales; catalog negotiation
    // itself is exercised through explicit-locale helpers below.
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
}

fn source_lang() -> LanguageIdentifier {
    "en-US".parse().unwrap_or_default()
}

fn active_locale() -> LanguageIdentifier {
    CATALOG
        .current_locale()
        .parse()
        .unwrap_or_else(|_| source_lang())
}

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

fn data_locale(locale: &LanguageIdentifier) -> Locale {
    // qps-ploc exercises copy expansion, not synthetic calendar data. Invalid
    // identifiers have already fallen back in `set_locale`, but keep this
    // boundary independently fail-safe for direct test helpers.
    let locale = if locale.language.as_str() == "qps" {
        source_lang()
    } else {
        locale.clone()
    };
    locale.to_string().parse().unwrap_or(Locale::UNKNOWN)
}

fn format_datetime_parts_for(
    locale: &LanguageIdentifier,
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
) -> Option<String> {
    let formatter =
        DateTimeFormatter::try_new(data_locale(locale).into(), YMD::medium().with_time_hm())
            .ok()?;
    let value = DateTime {
        date: Date::try_new_iso(year, month, day).ok()?,
        time: Time::try_new(hour, minute, 0, 0).ok()?,
    };
    Some(formatter.format(&value).to_string())
}

fn format_date_parts_for(
    locale: &LanguageIdentifier,
    year: i32,
    month: u8,
    day: u8,
) -> Option<String> {
    let formatter = DateTimeFormatter::try_new(data_locale(locale).into(), YMD::medium()).ok()?;
    let value = DateTime {
        date: Date::try_new_iso(year, month, day).ok()?,
        time: Time::try_new(0, 0, 0, 0).ok()?,
    };
    Some(formatter.format(&value).to_string())
}

/// Format the date of a local wall-clock value with ICU4X/CLDR data for the
/// active locale (the time of day left out, for a narrow column).
pub fn format_date(value: chrono::DateTime<chrono::Local>) -> String {
    use chrono::Datelike;

    format_date_parts_for(
        &active_locale(),
        value.year(),
        value.month() as u8,
        value.day() as u8,
    )
    .unwrap_or_else(|| value.format("%Y-%m-%d").to_string())
}

/// Format a local wall-clock value with ICU4X/CLDR data for the active locale.
/// The stable numeric fallback is used only if locale data or input validation
/// unexpectedly fails.
pub fn format_datetime(value: chrono::DateTime<chrono::Local>) -> String {
    use chrono::{Datelike, Timelike};

    format_datetime_parts_for(
        &active_locale(),
        value.year(),
        value.month() as u8,
        value.day() as u8,
        value.hour() as u8,
        value.minute() as u8,
    )
    .unwrap_or_else(|| value.format("%Y-%m-%d %H:%M").to_string())
}

fn format_bytes_for(locale: &LanguageIdentifier, bytes: u64) -> String {
    // Decimal SI units preserve Files' existing 1000-byte thresholds. The
    // value is computed as integer tenths, avoiding binary floating-point
    // artifacts before ICU applies locale digits and decimal separators.
    const UNITS: [&str; 7] = ["B", "kB", "MB", "GB", "TB", "PB", "EB"];
    let mut unit = 0usize;
    let mut divisor = 1u64;
    while unit + 1 < UNITS.len() && bytes >= divisor.saturating_mul(1_000) {
        unit += 1;
        divisor = divisor.saturating_mul(1_000);
    }

    let mut decimal = if unit == 0 {
        Decimal::from(bytes)
    } else {
        let tenths = ((u128::from(bytes) * 10) + u128::from(divisor / 2)) / u128::from(divisor);
        let tenths = u64::try_from(tenths).unwrap_or(u64::MAX);
        if tenths % 10 == 0 {
            Decimal::from(tenths / 10)
        } else {
            let mut value = Decimal::from(tenths);
            value.multiply_pow10(-1);
            value
        }
    };
    // Do not expose insignificant fractional zeros if a future Decimal
    // constructor changes its default display magnitude.
    decimal.trim_end();

    let number = DecimalFormatter::try_new(
        data_locale(locale).into(),
        icu_decimal::options::DecimalFormatterOptions::default(),
    )
    .map_or_else(
        |_| decimal.to_string(),
        |formatter| formatter.format(&decimal).to_string(),
    );
    let unit = UNITS.get(unit).copied().unwrap_or("B");
    format!("{number}\u{a0}{unit}")
}

/// Human-readable decimal byte quantity using the active locale's numbering
/// system and separators. Unit choice and symbols follow SI, not a hand-built
/// per-language table.
pub fn format_bytes(bytes: u64) -> String {
    format_bytes_for(&active_locale(), bytes)
}

pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
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
    use fluent::{FluentBundle, FluentResource};
    use std::collections::{BTreeMap, BTreeSet};

    /// Parse a catalog the way Fluent will, keeping entries past a syntax
    /// error so the assertions below name every affected key.
    fn make_bundle(locale: LanguageIdentifier, source: &str) -> FluentBundle<FluentResource> {
        let mut bundle = FluentBundle::new(vec![locale]);
        bundle.set_use_isolating(false);
        let resource =
            FluentResource::try_new(source.to_string()).unwrap_or_else(|(partial, _)| partial);
        let _ = bundle.add_resource(resource);
        bundle
    }

    fn catalog_messages(source: &str) -> BTreeMap<String, String> {
        let mut messages = BTreeMap::new();
        let mut current: Option<(String, String)> = None;
        for line in source.lines() {
            if !line.starts_with(char::is_whitespace)
                && let Some((key, value)) = line.split_once(" = ")
            {
                if let Some((key, value)) = current.take() {
                    messages.insert(key, value);
                }
                current = Some((key.to_string(), value.to_string()));
            } else if let Some((_, value)) = current.as_mut()
                && (line.starts_with(char::is_whitespace) || line.trim() == "}")
            {
                value.push('\n');
                value.push_str(line);
            }
        }
        if let Some((key, value)) = current {
            messages.insert(key, value);
        }
        messages
    }

    fn variables(pattern: &str) -> BTreeSet<String> {
        let mut variables = BTreeSet::new();
        let bytes = pattern.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'$' {
                let start = index + 1;
                index = start;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'-' | b'_'))
                {
                    index += 1;
                }
                variables.insert(pattern[start..index].to_string());
            } else {
                index += 1;
            }
        }
        variables
    }

    fn selector_variants(pattern: &str) -> Vec<String> {
        let mut variants: Vec<_> = pattern
            .lines()
            .filter_map(|line| {
                let line = line.trim_start();
                let line = line.strip_prefix('*').unwrap_or(line);
                line.strip_prefix('[')
                    .and_then(|line| line.split_once(']'))
                    .map(|(variant, _)| variant.to_string())
            })
            .collect();
        variants.sort();
        variants
    }

    /// The German catalogue is complete and structurally matches the source, every key is registered, and negotiation falls back correctly.
    #[test]
    fn catalogs_are_complete_registered_and_negotiated() {
        // --- phase: german_catalog_is_valid_complete_and_structurally_matches_source ---
        {
            assert!(
                FluentResource::try_new(DE_FTL.to_string()).is_ok(),
                "de-DE.ftl must parse without Fluent syntax recovery"
            );
            let source = catalog_messages(SOURCE_FTL);
            let german = catalog_messages(DE_FTL);
            assert_eq!(
                source.keys().collect::<Vec<_>>(),
                german.keys().collect::<Vec<_>>(),
                "German and source catalogs must have exact key parity"
            );
            for (key, source_pattern) in &source {
                let german_pattern = &german[key];
                assert_eq!(
                    variables(source_pattern),
                    variables(german_pattern),
                    "Fluent variables changed for {key}"
                );
                assert_eq!(
                    selector_variants(source_pattern),
                    selector_variants(german_pattern),
                    "Fluent selector branches changed for {key}"
                );
            }

            let locale: LanguageIdentifier = "de-DE".parse().unwrap();
            let bundle = make_bundle(locale, DE_FTL);
            for key in source.keys() {
                assert!(
                    bundle
                        .get_message(key)
                        .and_then(|message| message.value())
                        .is_some(),
                    "de-DE would fall back to source English for {key}"
                );
            }
        }

        // --- phase: every_source_key_is_registered ---
        {
            let bundle = make_bundle(source_lang(), SOURCE_FTL);
            for key in include_str!("../locales/en-US.ftl")
                .lines()
                .filter_map(|line| line.split_once(" = ").map(|(key, _)| key.trim()))
            {
                assert!(
                    bundle
                        .get_message(key)
                        .and_then(|message| message.value())
                        .is_some(),
                    "missing or valueless Fluent message: {key}"
                );
            }
        }

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

        // --- phase: cldr_formats_dates_for_en_us_de_de_and_invalid_locale_fallback ---
        {
            let en: LanguageIdentifier = "en-US".parse().unwrap();
            let de: LanguageIdentifier = "de-DE".parse().unwrap();
            let fallback: LanguageIdentifier =
                "not a locale!".parse().unwrap_or_else(|_| source_lang());

            assert_eq!(
                format_datetime_parts_for(&en, 2025, 1, 15, 16, 9).unwrap(),
                "Jan 15, 2025, 4:09\u{202f}PM"
            );
            assert_eq!(
                format_datetime_parts_for(&de, 2025, 1, 15, 16, 9).unwrap(),
                "15.01.2025, 16:09"
            );
            assert_eq!(
                format_datetime_parts_for(&fallback, 2025, 1, 15, 16, 9).unwrap(),
                format_datetime_parts_for(&en, 2025, 1, 15, 16, 9).unwrap()
            );
        }

        // --- phase: cldr_formats_decimal_byte_quantities_without_float_artifacts ---
        {
            let en: LanguageIdentifier = "en-US".parse().unwrap();
            let de: LanguageIdentifier = "de-DE".parse().unwrap();

            assert_eq!(format_bytes_for(&en, 999), "999\u{a0}B");
            assert_eq!(format_bytes_for(&en, 1_500), "1.5\u{a0}kB");
            assert_eq!(format_bytes_for(&de, 1_500), "1,5\u{a0}kB");
            assert_eq!(format_bytes_for(&de, 1_000_000), "1\u{a0}MB");
            assert_eq!(format_bytes_for(&en, u64::MAX), "18.4\u{a0}EB");
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
