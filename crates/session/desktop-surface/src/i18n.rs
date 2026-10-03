//! The desktop's Fluent catalogue.
//!
//! Top bar and activity pane strings are keys resolved through the toolkit's
//! [`mod@ui::i18n`] manager, never literals. The catalogues are embedded with
//! `include_str!`, so the native shell, the browser runtime (which has no
//! file system to read them from) and a unit test resolve the same copy. On a
//! thread with no manager yet, the user's locale comes from the environment,
//! exactly as the toolkit's own widget strings choose it; a host that already
//! installed a manager keeps its locale. A message missing from a
//! translation falls back to `en-US`.

use ui::i18n::{ArgValue, EmbeddedCatalog};

const SOURCE_FTL: &str = include_str!("../locales/en-US.ftl");
const DE_FTL: &str = include_str!("../locales/de-DE.ftl");

/// Loaded per thread on first use; a host's manager is joined, never replaced.
/// The probe is a key every catalogue revision defines.
static CATALOG: EmbeddedCatalog = catalog();

const fn catalog() -> EmbeddedCatalog {
    let catalog = EmbeddedCatalog::new(
        "en-US",
        &[("en-US", SOURCE_FTL), ("de-DE", DE_FTL)],
        "topbar-control",
    )
    .with_system_locale();
    // Unit tests assert source copy throughout the desktop; keep them
    // deterministic on any developer or CI host locale.
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
}

/// Resolve a desktop string key.
pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// Resolve a desktop string key that interpolates the given nouns.
pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluent::{FluentBundle, FluentResource};
    use std::collections::{BTreeMap, BTreeSet};

    /// Every message in a catalogue, keyed by id, with its full pattern
    /// (continuation lines included).
    fn catalog_messages(source: &str) -> BTreeMap<String, String> {
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
                current = Some((key.trim().to_string(), value.to_string()));
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

    fn strict_bundle(locale: &str, source: &str) -> FluentBundle<FluentResource> {
        let resource = FluentResource::try_new(source.to_string())
            .unwrap_or_else(|(_, errors)| panic!("{locale}.ftl has syntax errors: {errors:?}"));
        let mut bundle = FluentBundle::new(vec![locale.parse().expect("locale id")]);
        bundle.set_use_isolating(false);
        bundle
            .add_resource(resource)
            .unwrap_or_else(|errors| panic!("{locale}.ftl defines a key twice: {errors:?}"));
        bundle
    }

    #[test]
    fn interpolated_keys_substitute_their_nouns() {
        assert_eq!(
            t_args(
                "topbar-workspace-position",
                &[("position", 2.into()), ("total", 3.into())]
            ),
            "Workspace 2 of 3"
        );
        assert_eq!(
            t_args("topbar-sharing-count", &[("viewers", 2.into())]),
            "2 VIEWING"
        );
    }

    #[test]
    fn every_catalogue_entry_resolves_to_a_real_string() {
        let keys = catalog_messages(SOURCE_FTL);
        assert!(!keys.is_empty());
        for key in keys.keys() {
            let value = t(key);
            assert_ne!(&value, key, "{key} has no entry in the catalogue");
            assert!(!value.trim().is_empty(), "{key} resolves to nothing");
        }
    }

    /// A key only English defines would show English on a German desktop,
    /// and a German key English lacks is dead copy. Both catalogues must
    /// define the same keys with the same variables, and parse cleanly.
    #[test]
    fn every_desktop_string_is_translated_in_every_locale() {
        let source = catalog_messages(SOURCE_FTL);
        for (locale, catalogue) in [("de-DE", DE_FTL)] {
            let translated = catalog_messages(catalogue);
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
                "{locale}.ftl has keys en-US lacks: {extra:?}"
            );
            for (key, pattern) in &source {
                assert_eq!(
                    variables(pattern),
                    variables(&translated[key]),
                    "{locale}.ftl changes the variables of {key}"
                );
            }
            let bundle = strict_bundle(locale, catalogue);
            for key in source.keys() {
                assert!(
                    bundle
                        .get_message(key)
                        .and_then(|message| message.value())
                        .is_some(),
                    "{locale}.ftl has no value for {key}"
                );
            }
        }
        strict_bundle("en-US", SOURCE_FTL);
    }

    /// Every literal key passed to [`t`] or [`t_args`] in `src/`, and every
    /// literal in the `activity-` namespace, which holds catalogue keys only:
    /// the pane picks many of them with a `match` before resolving.
    fn keys_used_in_code() -> BTreeMap<String, String> {
        fn visit(dir: &std::path::Path, found: &mut BTreeMap<String, String>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .expect("read source directory")
                .map(|entry| entry.expect("source entry").path())
                .collect();
            entries.sort();
            for path in entries {
                if path.is_dir() {
                    visit(&path, found);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let source = std::fs::read_to_string(&path).expect("read source file");
                    for key in literal_keys(&source)
                        .into_iter()
                        .chain(namespaced_keys(&source, "activity-"))
                    {
                        found
                            .entry(key)
                            .or_insert_with(|| path.display().to_string());
                    }
                }
            }
        }
        let mut found = BTreeMap::new();
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        visit(&manifest.join("src"), &mut found);
        // The native desktop resolves the rest of this catalogue.
        let desktop = manifest.join("../desktop/src");
        if desktop.is_dir() {
            visit(&desktop, &mut found);
        }
        found
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
                let Some(rest) = source.get(start..).and_then(|rest| rest.strip_prefix(call))
                else {
                    continue;
                };
                let Some(literal) = rest.trim_start().strip_prefix('"') else {
                    continue;
                };
                let Some(key) = literal.split_once('"').map(|(key, _)| key) else {
                    continue;
                };
                if !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                {
                    keys.push(key.to_string());
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
                (key.len() > prefix.len()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
                .then(|| key.to_string())
            })
            .collect()
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
    fn literal_key_scan_finds_direct_and_wrapped_calls() {
        let source = "t(\"a-b\") + crate::shell::i18n::t_args(\n    \"c-d\", &[]) + format(\"x\") + tt(\"no\")";
        assert_eq!(literal_keys(source), vec!["a-b", "c-d"]);
    }

    /// A key the code asks for but a catalogue lacks renders as the raw key
    /// (or English on a German desktop), so every one must be defined.
    #[test]
    fn every_key_used_in_code_is_in_every_catalogue() {
        let used = keys_used_in_code();
        assert!(
            used.len() > 200,
            "the source scan found only {} keys",
            used.len()
        );
        for (locale, catalogue) in [("en-US", SOURCE_FTL), ("de-DE", DE_FTL)] {
            let defined = catalog_messages(catalogue);
            let missing: Vec<_> = used
                .iter()
                .filter(|(key, _)| !defined.contains_key(*key))
                .map(|(key, file)| format!("{key} ({file})"))
                .collect();
            assert!(missing.is_empty(), "{locale}.ftl lacks {missing:?}");
        }
    }

    #[test]
    fn counts_choose_the_plural_form_in_every_locale() {
        let changes = |changes: usize, people: usize| {
            t_args(
                "activity-changes-workspace",
                &[
                    ("workspace", "Design".into()),
                    ("changes", changes.into()),
                    ("people", people.into()),
                ],
            )
        };
        let english = (changes(1, 1), changes(3, 2));
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = (changes(1, 1), changes(3, 2));
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(english.0, "Design · 1 change by 1 person");
        assert_eq!(english.1, "Design · 3 changes by 2 people");
        assert_eq!(german.0, "Design · 1 Änderung von 1 Person");
        assert_eq!(german.1, "Design · 3 Änderungen von 2 Personen");
    }

    #[test]
    fn german_desktops_resolve_german_copy() {
        let english = t("topbar-open-overview");
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = t("topbar-open-overview");
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(english, "Open overview");
        assert_eq!(german, "Übersicht öffnen");
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
        for dir in ["activity", "notifications"] {
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
