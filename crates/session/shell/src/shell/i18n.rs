//! The shell's Fluent catalogue.
//!
//! Chrome strings are keys resolved through the toolkit's [`mod@ui::i18n`]
//! manager, never literals. The catalogues are embedded with `include_str!`,
//! so a `--shots` render, a unit test and a live session resolve the same
//! copy. The user's locale comes from the environment (`LC_ALL`,
//! `LC_MESSAGES`, `LANGUAGE`, `LANG`), exactly as the toolkit's own widget
//! strings choose it; a message missing from a translation falls back to
//! `en-US`.

use ui::i18n::{ArgValue, EmbeddedCatalog};

const SOURCE_FTL: &str = include_str!("../../locales/en-US.ftl");
const DE_FTL: &str = include_str!("../../locales/de-DE.ftl");

/// Loaded per thread on first use; a host's manager is joined, never replaced.
/// The probe is a key every catalogue revision defines.
static CATALOG: EmbeddedCatalog = catalog();

const fn catalog() -> EmbeddedCatalog {
    let catalog = EmbeddedCatalog::new(
        "en-US",
        &[("en-US", SOURCE_FTL), ("de-DE", DE_FTL)],
        "continuity-continue",
    )
    .with_system_locale();
    // Unit tests assert source copy throughout the shell; keep them
    // deterministic on any developer or CI host locale.
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
}

/// Load the shell's catalogue and the toolkit's widget labels on this
/// thread, in the desktop's language. Startup calls it once; every lookup
/// repeats it for free, so a thread that draws chrome without having
/// resolved a shell string yet still gets translated toolkit widgets.
pub fn ensure() {
    CATALOG.ensure();
    ui::i18n::I18nManager::register_widget_messages();
}

/// Resolve a shell string key.
pub fn t(key: &str) -> String {
    ensure();
    CATALOG.t(key)
}

/// Resolve a shell string key that interpolates the given nouns.
pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    ensure();
    CATALOG.t_args(key, args)
}

/// The desktop's language (BCP 47), for content that is localised outside
/// the catalogue, such as emoji names.
pub fn current_locale() -> String {
    CATALOG.current_locale()
}

/// Select the locale written in `appearance.language`, if any.
/// A running compositor calls this after that key changes so chrome
/// strings switch without a restart. Missing leaves the current selection.
pub fn apply_configured_locale() {
    ensure();
    if let Some(language) =
        settings_store::str_at(&settings_store::load(), "appearance", "language")
    {
        CATALOG.set_locale(language);
    }
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
    fn every_key_the_shell_uses_resolves_to_a_real_string() {
        for key in [
            "continuity-continue",
            "continuity-continue-subtitle",
            "continuity-dismiss-title",
            "continuity-dismiss-subtitle",
            "continuity-other-device",
            "tab-closed-toast",
            "tab-closed-group-toast",
            "tab-closed-undo",
            "tab-closed-undo-hint",
            "tab-closed-undo-hint-unbound",
            "overview-windows-title",
            "overview-windows-description",
            "overview-window-move-failed",
        ] {
            let s = t(key);
            assert_ne!(s, key, "{key} has no entry in the catalogue");
            assert!(!s.trim().is_empty(), "{key} resolves to nothing");
        }
        assert_eq!(
            t_args(
                "continuity-continue",
                &[("app", "Notes".into()), ("device", "Laptop".into())]
            ),
            "Continue Notes from Laptop"
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
    fn every_shell_string_is_translated_in_every_locale() {
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

    /// Every literal key passed to [`t`] or [`t_args`] in `src/`.
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
                    for key in literal_keys(&source) {
                        found
                            .entry(key)
                            .or_insert_with(|| path.display().to_string());
                    }
                }
            }
        }
        let mut found = BTreeMap::new();
        visit(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut found,
        );
        found
    }

    /// The string literal that opens each `t(` / `t_args(` call in `source`.
    fn literal_keys(source: &str) -> Vec<String> {
        let mut keys = Vec::new();
        for call in ["t(", "t_args("] {
            for (start, _) in source.match_indices(call) {
                let preceded_by_ident = source[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
                if preceded_by_ident {
                    continue;
                }
                let Some(rest) = source[start..].strip_prefix(call) else {
                    continue;
                };
                let Some(literal) = rest.trim_start().strip_prefix('"') else {
                    continue;
                };
                let Some(end) = literal.find('"') else {
                    continue;
                };
                let key = &literal[..end];
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
            used.len() > 100,
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

    /// Toolkit widgets the shell draws take their labels from the toolkit's
    /// catalogue, which the shell loads with its own.
    #[test]
    fn toolkit_widgets_in_the_shell_follow_a_german_desktop() {
        use ui::widget::Widget as _;
        ensure();
        ui::i18n::I18nManager::set_locale("de-DE");
        let prompt = ui::widget::PermissionPrompt::new(
            "org.example.app".into(),
            t("capability-camera"),
            true,
        );
        let node = prompt.accessibility();
        ui::i18n::I18nManager::set_locale("en-US");
        let node = node.expect("a permission prompt is semantic");
        assert_eq!(
            node.name.as_deref(),
            Some("Kamerazugriff – angefragt von org.example.app")
        );
        let buttons: Vec<_> = node
            .children
            .iter()
            .map(|button| button.name.as_deref())
            .collect();
        assert_eq!(
            buttons,
            [
                Some("Erlauben"),
                Some("Diese Sitzung"),
                Some("Immer erlauben"),
                Some("Ablehnen")
            ]
        );
    }

    #[test]
    fn german_desktops_resolve_german_copy() {
        let english = t("tab-closed-undo");
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = t("tab-closed-undo");
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(english, "Undo");
        assert_eq!(german, "Rückgängig");
    }
}

#[cfg(test)]
#[path = "i18n_copy_lint_tests.rs"]
mod copy_lint_tests;
