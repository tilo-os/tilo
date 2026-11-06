//! The locale preference list a `Name[xx]=` lookup is resolved against.
//!
//! The spec's matching rule for `lang_COUNTRY.ENCODING@MODIFIER` is to
//! try, in order: `lang_COUNTRY@MODIFIER`, `lang_COUNTRY`,
//! `lang@MODIFIER`, `lang`. The encoding is never part of a key in the
//! file, so it is stripped before matching.
//!
//! The list is always the caller's, never this crate's: every caller asks
//! [`language_candidates`] for the interface language the desktop resolves
//! (the settings' language, else the device's locale, which that one answer
//! reads for itself), so an English session on a German machine lists English
//! names and a German one lists German names. Nothing here reads the process
//! environment.

/// The `Name[xx]` key candidates for the interface language `preferred`: a
/// BCP 47 tag (`en-US`) or the POSIX locale it came from (`en_US.UTF-8`), both
/// accepted. `None` means no language is named (`C`), so the unlocalised keys
/// are the ones read.
#[must_use]
pub fn language_candidates(preferred: Option<&str>) -> Vec<String> {
    preferred.map_or_else(Vec::new, candidates_for)
}

/// The candidate list for one locale string. Public so it is testable without
/// the process environment, and so both spellings of the same language — the
/// POSIX `de_DE.UTF-8@euro` and the BCP 47 `de-DE` the settings store keeps —
/// expand the same way.
#[must_use]
pub fn candidates_for(locale: &str) -> Vec<String> {
    let locale = locale.trim();
    if locale.is_empty() || locale == "C" || locale == "POSIX" {
        return Vec::new();
    }
    // Strip the encoding: `de_DE.UTF-8@euro` → `de_DE@euro`.
    let (head, modifier) = match locale.split_once('@') {
        Some((head, modifier)) => (head, Some(modifier)),
        None => (locale, None),
    };
    let head = head.split_once('.').map_or(head, |(h, _)| h);
    // A BCP 47 tag spells the same parts with `-`; the file's keys never do.
    let head = head.replace('-', "_");
    if matches!(head.as_str(), "C" | "POSIX") {
        return Vec::new();
    }
    let mut parts = head.split('_');
    let lang = parts.next().unwrap_or_default();
    // The region is the two-letter or three-digit subtag after the language: a
    // script subtag (`Hant`) names no key the format has.
    let country = parts.find(|part| {
        (part.len() == 2 && part.bytes().all(|b| b.is_ascii_alphabetic()))
            || (part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit()))
    });

    let mut out = Vec::new();
    let mut push = |s: String| {
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    match (country, modifier) {
        (Some(c), Some(m)) => {
            push(format!("{lang}_{c}@{m}"));
            push(format!("{lang}_{c}"));
            push(format!("{lang}@{m}"));
        }
        (Some(c), None) => push(format!("{lang}_{c}")),
        (None, Some(m)) => push(format!("{lang}@{m}")),
        (None, None) => {}
    }
    push(lang.to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locale_name_expands_to_the_spec_fallback_order() {
        for (case, locale, want) in [
            (
                "the_spec_fallback_order_is_followed",
                "de_DE.UTF-8@euro",
                &["de_DE@euro", "de_DE", "de@euro", "de"][..],
            ),
            (
                "the_spec_fallback_order_is_followed",
                "de_AT",
                &["de_AT", "de"],
            ),
            ("the_spec_fallback_order_is_followed", "de.UTF-8", &["de"]),
            ("the_spec_fallback_order_is_followed", "de", &["de"]),
            // The C locale means take the unlocalised value.
            ("the_c_locale_means_take_the_unlocalised_value", "C", &[]),
            (
                "the_c_locale_means_take_the_unlocalised_value",
                "POSIX",
                &[],
            ),
            ("the_c_locale_means_take_the_unlocalised_value", "", &[]),
            (
                "the_c_locale_means_take_the_unlocalised_value",
                "C.UTF-8",
                &[],
            ),
            // The settings store keeps BCP 47; the same language must expand to
            // the keys the file has.
            (
                "a_bcp_47_tag_expands_to_the_same_keys_as_its_posix_locale",
                "de-DE",
                &["de_DE", "de"],
            ),
            (
                "a_bcp_47_tag_expands_to_the_same_keys_as_its_posix_locale",
                "de",
                &["de"],
            ),
            (
                "a_bcp_47_tag_expands_to_the_same_keys_as_its_posix_locale",
                "es-419",
                &["es_419", "es"],
            ),
            // A script subtag names no key the format has, so the region after
            // it is the one tried.
            (
                "a_bcp_47_tag_expands_to_the_same_keys_as_its_posix_locale",
                "zh-Hant-TW",
                &["zh_TW", "zh"],
            ),
        ] {
            assert_eq!(candidates_for(locale), want, "case {case}: {locale:?}");
        }
    }

    /// The person's choice is the interface language; the machine's locale is
    /// never consulted when there is one, and a choice that names no language
    /// leaves the unlocalised keys.
    #[test]
    fn the_interface_language_decides_the_candidates() {
        assert_eq!(language_candidates(Some("en-US")), ["en_US", "en"]);
        assert_eq!(language_candidates(Some("de-DE")), ["de_DE", "de"]);
        assert_eq!(language_candidates(Some("C")), Vec::<String>::new());
        assert_eq!(language_candidates(None), Vec::<String>::new());
    }
}
