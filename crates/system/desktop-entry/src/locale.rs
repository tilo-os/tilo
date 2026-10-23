//! The locale preference list a `Name[xx]=` lookup is resolved against.
//!
//! The spec's matching rule for `lang_COUNTRY.ENCODING@MODIFIER` is to
//! try, in order: `lang_COUNTRY@MODIFIER`, `lang_COUNTRY`,
//! `lang@MODIFIER`, `lang`. The encoding is never part of a key in the
//! file, so it is stripped before matching.

/// The current locale's key candidates, most specific first.
///
/// `LC_ALL` first because POSIX lets it override every category; reading
/// `LC_MESSAGES` ahead of it inverts that, so a session pinning `LC_ALL`
/// still resolves `Name[xx]=` in whatever `LC_MESSAGES` the machine carries.
/// `C` and `POSIX` mean no translation and yield an empty list.
pub fn locale_candidates() -> Vec<String> {
    candidates_from(|var| std::env::var_os(var).map(|v| v.to_string_lossy().into_owned()))
}

/// Split from [`locale_candidates`] so the precedence is assertable without
/// mutating the environment of a parallel test binary — which is how the order
/// stayed inverted unnoticed.
fn candidates_from(lookup: impl Fn(&str) -> Option<String>) -> Vec<String> {
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(raw) = lookup(var) {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            return candidates_for(trimmed);
        }
    }
    Vec::new()
}

/// The candidate list for one locale string. Public so it is testable
/// without touching the process environment.
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
    let (lang, country) = match head.split_once('_') {
        Some((lang, country)) => (lang, Some(country)),
        None => (head, None),
    };

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
            ("the_c_locale_means_take_the_unlocalised_value", "C", &[]),
            (
                "the_c_locale_means_take_the_unlocalised_value",
                "POSIX",
                &[],
            ),
            ("the_c_locale_means_take_the_unlocalised_value", "", &[]),
        ] {
            assert_eq!(candidates_for(locale), want, "case {case}: {locale:?}");
        }
    }

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |var| owned.iter().find(|(k, _)| k == var).map(|(_, v)| v.clone())
    }

    #[test]
    fn the_environment_selects_lc_all_then_lc_messages_then_lang() {
        for (case, vars, want) in [
            (
                "lc_all_overrides_the_messages_category",
                &[("LC_ALL", "de_DE.UTF-8"), ("LC_MESSAGES", "en_US.UTF-8")][..],
                &["de_DE", "de"][..],
            ),
            (
                "lc_messages_is_taken_when_lc_all_is_unset_or_empty",
                &[("LC_MESSAGES", "fr_FR.UTF-8"), ("LANG", "en_US.UTF-8")],
                &["fr_FR", "fr"],
            ),
            (
                "lc_messages_is_taken_when_lc_all_is_unset_or_empty",
                &[("LC_ALL", "   "), ("LC_MESSAGES", "fr_FR")],
                &["fr_FR", "fr"],
            ),
            (
                "lang_is_the_last_resort_and_nothing_set_means_unlocalised",
                &[("LANG", "es_ES")],
                &["es_ES", "es"],
            ),
            (
                "lang_is_the_last_resort_and_nothing_set_means_unlocalised",
                &[],
                &[],
            ),
        ] {
            assert_eq!(candidates_from(env(vars)), want, "case {case}: {vars:?}");
        }
    }
}
