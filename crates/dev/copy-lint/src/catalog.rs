//! Checks that a crate's Fluent catalogues agree with each other and with
//! the keys its code asks for.
//!
//! A key only English defines shows English on a German desktop, a key the
//! source lacks is dead copy, and a translation that drops a variable loses
//! words. Each crate states its catalogues once:
//!
//! ```no_run
//! const CATALOGS: &[(&str, &str)] = &[("en-US", "a = A\n"), ("de-DE", "a = Ä\n")];
//! tilo_copy_lint::catalog::check_translations(CATALOGS);
//! tilo_copy_lint::catalog::UsedKeys::scan(&[concat!(env!("CARGO_MANIFEST_DIR"), "/src")])
//!     .min_keys(100)
//!     .check_defined(CATALOGS);
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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
            && let Some((key, value)) = line.split_once(" = ")
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

/// The `$variables` a pattern interpolates.
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

/// The catalogue keys a crate's sources ask for, each with the first file
/// that names it.
#[derive(Debug)]
pub struct UsedKeys {
    found: BTreeMap<String, String>,
    min_keys: usize,
}

impl UsedKeys {
    /// Every literal key passed to a `t(` or `t_args(` call in the `.rs`
    /// files under `dirs`.
    #[must_use]
    pub fn scan(dirs: &[impl AsRef<Path>]) -> Self {
        Self::scan_with_namespaces(dirs, &[])
    }

    /// As [`Self::scan`], plus every string literal spelled like a key that
    /// starts with one of `namespaces`, for keys a `match` picks before the
    /// lookup.
    ///
    /// # Panics
    ///
    /// When a directory or file under `dirs` cannot be read.
    #[must_use]
    pub fn scan_with_namespaces(dirs: &[impl AsRef<Path>], namespaces: &[&str]) -> Self {
        let mut found = BTreeMap::new();
        for dir in dirs {
            for path in rust_files(dir.as_ref()) {
                let source = std::fs::read_to_string(&path).expect("read source file");
                let keys = literal_keys(&source).into_iter().chain(
                    namespaces
                        .iter()
                        .flat_map(|prefix| namespaced_keys(&source, prefix)),
                );
                for key in keys {
                    found
                        .entry(key)
                        .or_insert_with(|| path.display().to_string());
                }
            }
        }
        Self { found, min_keys: 1 }
    }

    /// Fail when the scan finds fewer keys than this, so a broken scan
    /// cannot pass by finding nothing.
    #[must_use]
    pub fn min_keys(mut self, min_keys: usize) -> Self {
        self.min_keys = min_keys;
        self
    }

    /// Panic unless every catalogue defines every key the code asks for: a
    /// missing one renders as the raw key, or as English in a translation.
    ///
    /// # Panics
    ///
    /// When the scan found too few keys or a catalogue lacks one.
    pub fn check_defined(&self, catalogs: &[(&str, &str)]) {
        assert!(
            self.found.len() >= self.min_keys,
            "the source scan found only {} keys",
            self.found.len()
        );
        for (locale, catalogue) in catalogs {
            let defined = messages(catalogue);
            let missing: Vec<_> = self
                .found
                .iter()
                .filter(|(key, _)| !defined.contains_key(*key))
                .map(|(key, file)| format!("{key} ({file})"))
                .collect();
            assert!(missing.is_empty(), "{locale}.ftl lacks {missing:?}");
        }
    }
}

/// Every `.rs` file under `dir`, in a stable order.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .map(|entry| entry.expect("source entry").path())
        .collect();
    entries.sort();
    entries
        .into_iter()
        .flat_map(|path| {
            if path.is_dir() {
                rust_files(&path)
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                vec![path]
            } else {
                Vec::new()
            }
        })
        .collect()
}

fn is_key(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The string literal that opens each `t(` / `t_args(` call in `source`.
fn literal_keys(source: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for call in ["t(", "t_args("] {
        for (start, _) in source.match_indices(call) {
            let preceded_by_ident = source
                .get(..start)
                .and_then(|before| before.chars().next_back())
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            if preceded_by_ident {
                continue;
            }
            let Some(rest) = source.get(start..).and_then(|rest| rest.strip_prefix(call)) else {
                continue;
            };
            let Some(literal) = rest.trim_start().strip_prefix('"') else {
                continue;
            };
            if let Some((key, _)) = literal.split_once('"')
                && is_key(key)
            {
                keys.push(key.to_owned());
            }
        }
    }
    keys
}

/// Every string literal in `source` that starts with `prefix` and is
/// spelled like a catalogue key.
fn namespaced_keys(source: &str, prefix: &str) -> Vec<String> {
    let opening = format!("\"{prefix}");
    source
        .match_indices(&opening)
        .filter_map(|(start, _)| {
            let literal = source.get(start.saturating_add(1)..)?;
            let (key, _) = literal.split_once('"')?;
            (key.len() > prefix.len() && is_key(key)).then(|| key.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_key_scan_finds_direct_and_wrapped_calls() {
        let source = "t(\"a-b\") + crate::shell::i18n::t_args(\n    \"c-d\", &[]) + format(\"x\") + tt(\"no\")";
        assert_eq!(literal_keys(source), vec!["a-b", "c-d"]);
    }

    #[test]
    fn namespaced_key_scan_finds_keys_picked_by_a_match() {
        let source = "t(match v { A => \"activity-view-tasks\", B => \"activity-view-replay\" }) + \"activity x\"";
        assert_eq!(
            namespaced_keys(source, "activity-"),
            vec!["activity-view-tasks", "activity-view-replay"]
        );
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
