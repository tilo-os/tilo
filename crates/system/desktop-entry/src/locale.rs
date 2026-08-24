//! The locale preference list a `Name[xx]=` lookup is resolved against.
//!
//! The spec's matching rule for `lang_COUNTRY.ENCODING@MODIFIER` is to
//! try, in order: `lang_COUNTRY@MODIFIER`, `lang_COUNTRY`,
//! `lang@MODIFIER`, `lang`. The encoding is never part of a key in the
//! file, so it is stripped before matching.

/// The current locale's key candidates, most specific first.
///
/// Read from `LC_ALL`, then `LC_MESSAGES`, then `LANG` — the precedence
/// POSIX gives the messages category. `LC_ALL` is first because it
/// *overrides* every category when it is set; reading `LC_MESSAGES`
/// ahead of it inverted that, so a harness that pinned `LC_ALL` to one
/// language still resolved `Name[xx]=` in whatever `LC_MESSAGES` the
/// machine happened to carry — an English session with German app
/// names in the palette, and no way to pin it from outside.
///
/// `C` and `POSIX` mean "no translation", and yield an empty list so the
/// unlocalised value is taken.
pub fn locale_candidates() -> Vec<String> {
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(raw) = std::env::var_os(var) {
            let raw = raw.to_string_lossy().to_string();
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
    let head = head.split_once('.').map(|(h, _)| h).unwrap_or(head);
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
    fn the_spec_fallback_order_is_followed() {
        assert_eq!(
            candidates_for("de_DE.UTF-8@euro"),
            vec!["de_DE@euro", "de_DE", "de@euro", "de"]
        );
        assert_eq!(candidates_for("de_AT"), vec!["de_AT", "de"]);
        assert_eq!(candidates_for("de.UTF-8"), vec!["de"]);
        assert_eq!(candidates_for("de"), vec!["de"]);
    }

    #[test]
    fn the_c_locale_means_take_the_unlocalised_value() {
        assert!(candidates_for("C").is_empty());
        assert!(candidates_for("POSIX").is_empty());
        assert!(candidates_for("").is_empty());
    }
}
