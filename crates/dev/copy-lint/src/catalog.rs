//! Checks that a crate's Fluent catalogues agree with each other.
//!
//! A key only English defines shows English on a German desktop, a key the
//! source lacks is dead copy, and a translation that drops a variable loses
//! words. Each crate states its catalogues once:
//!
//! ```no_run
//! const CATALOGS: &[(&str, &str)] = &[("en-US", "a = A\n"), ("de-DE", "a = Ä\n")];
//! tilo_copy_lint::catalog::check_translations(CATALOGS);
//! ```

use std::collections::{BTreeMap, BTreeSet};

use fluent::{FluentBundle, FluentResource};

/// Every message in a catalogue, keyed by id, with its full pattern
/// (continuation lines included).
#[must_use]
pub fn messages(source: &str) -> BTreeMap<String, String> {
    let mut messages = BTreeMap::new();
    let mut current: Option<(String, String)> = None;
    for line in source.lines() {
        if !line.starts_with(char::is_whitespace)
            && !line.starts_with('#')
            && let Some((key, value)) = line
                .split_once(" = ")
                .or_else(|| line.strip_suffix(" =").map(|key| (key, "")))
        {
            if let Some((key, value)) = current.take() {
                messages.insert(key, value);
            }
            current = Some((key.trim().to_owned(), value.to_owned()));
        } else if let Some((_, value)) = current.as_mut()
            && (line.starts_with(char::is_whitespace) || line.trim() == "}")
        {
            value.push('\n');
            value.push_str(line);
        } else if let Some((key, value)) = current.take() {
            messages.insert(key, value);
        }
    }
    if let Some((key, value)) = current {
        messages.insert(key, value);
    }
    messages
}

fn variables(pattern: &str) -> BTreeSet<String> {
    pattern
        .split('$')
        .skip(1)
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                .collect()
        })
        .collect()
}

/// Parse `source` with no syntax recovery and no duplicate ids.
fn strict_bundle(locale: &str, source: &str) -> FluentBundle<FluentResource> {
    let resource = FluentResource::try_new(source.to_owned())
        .unwrap_or_else(|(_, errors)| panic!("{locale}.ftl has syntax errors: {errors:?}"));
    let mut bundle = FluentBundle::new(vec![locale.parse().expect("locale id")]);
    bundle.set_use_isolating(false);
    bundle
        .add_resource(resource)
        .unwrap_or_else(|errors| panic!("{locale}.ftl defines a key twice: {errors:?}"));
    bundle
}

/// Panic unless every catalogue parses cleanly and every translation defines
/// exactly the source's keys, each with a value and the same variables.
/// Selector branches may differ: each language has its own plural forms. `catalogs[0]` is the source.
///
/// # Panics
///
/// On the first catalogue that breaks one of those rules.
pub fn check_translations(catalogs: &[(&str, &str)]) {
    let [(source_locale, source_ftl), translations @ ..] = catalogs else {
        panic!("no catalogues to check");
    };
    let source = messages(source_ftl);
    assert!(
        !source.is_empty(),
        "{source_locale}.ftl defines no messages"
    );
    assert_values(source_locale, source_ftl, source.keys());
    for (locale, catalogue) in translations {
        let translated = messages(catalogue);
        let missing: Vec<_> = source
            .keys()
            .filter(|key| !translated.contains_key(*key))
            .collect();
        assert!(missing.is_empty(), "{locale}.ftl lacks {missing:?}");
        let extra: Vec<_> = translated
            .keys()
            .filter(|key| !source.contains_key(*key))
            .collect();
        assert!(
            extra.is_empty(),
            "{locale}.ftl has keys {source_locale} lacks: {extra:?}"
        );
        for (key, pattern) in &source {
            let translation = &translated[key];
            assert_eq!(
                variables(pattern),
                variables(translation),
                "{locale}.ftl changes the variables of {key}"
            );
        }
        assert_values(locale, catalogue, source.keys());
    }
}

/// Panic unless `catalogue` parses strictly and gives each of `keys` a value.
fn assert_values<'a>(locale: &str, catalogue: &str, keys: impl IntoIterator<Item = &'a String>) {
    let bundle = strict_bundle(locale, catalogue);
    for key in keys {
        assert!(
            bundle
                .get_message(key)
                .and_then(|message| message.value())
                .is_some(),
            "{locale}.ftl has no value for {key}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_may_start_on_the_line_after_the_equals_sign() {
        let found = messages("a =\n    one\n    two\nb = B\n");
        assert_eq!(found["a"], "\n    one\n    two");
        assert_eq!(found["b"], "B");
    }

    #[test]
    fn matching_translations_pass() {
        check_translations(&[
            (
                "en-US",
                "# note\ngreet = Hi { $name }\ncount = { $n ->\n    [one] One\n   *[other] Many\n}\n",
            ),
            (
                "de-DE",
                "greet = Hallo { $name }\ncount = { $n ->\n    [one] Eins\n   *[other] Viele\n}\n",
            ),
        ]);
    }

    #[test]
    #[should_panic(expected = "de-DE.ftl changes the variables of greet")]
    fn a_dropped_variable_fails() {
        check_translations(&[
            ("en-US", "greet = Hi { $name }\n"),
            ("de-DE", "greet = Hallo\n"),
        ]);
    }

    #[test]
    #[should_panic(expected = "de-DE.ftl lacks")]
    fn a_missing_translation_fails() {
        check_translations(&[("en-US", "a = A\nb = B\n"), ("de-DE", "a = Ä\n")]);
    }
}
