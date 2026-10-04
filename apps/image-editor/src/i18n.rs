//! The Image Editor's Fluent catalogue.
//!
//! Every string a person reads is a key resolved here, never a literal. The
//! catalogues are embedded, so a `--shots` render, a unit test and a live
//! session resolve the same copy.

use ui::i18n::{ArgValue, EmbeddedCatalog};

/// Loaded per thread on first use; a host's manager is joined, never replaced.
static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[
        ("en-US", include_str!("../locales/en-US.ftl")),
        ("en-GB", include_str!("../locales/en-GB.ftl")),
        ("de-DE", include_str!("../locales/de-DE.ftl")),
    ],
    "image-editor-adjustments",
)
.with_system_locale();

/// Resolve a key in the active locale.
pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// Resolve a key that interpolates the app's own words or numbers.
pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_locale_defines_every_key() {
        let keys = |source: &str| {
            source
                .lines()
                .filter_map(|line| line.split_once(" = ").map(|(key, _)| key.trim()))
                .filter(|key| key.starts_with("image-editor-"))
                .map(str::to_owned)
                .collect::<std::collections::BTreeSet<_>>()
        };
        let source = keys(include_str!("../locales/en-US.ftl"));
        assert!(source.len() > 40, "the source catalogue parses: {source:?}");
        for (locale, text) in [
            ("en-GB", include_str!("../locales/en-GB.ftl")),
            ("de-DE", include_str!("../locales/de-DE.ftl")),
        ] {
            assert_eq!(keys(text), source, "{locale} defines the same keys");
        }
    }

    #[test]
    fn keys_resolve_to_copy() {
        for key in [
            "image-editor-adjustments",
            "image-editor-empty-title",
            "image-editor-colour",
        ] {
            assert_ne!(super::t(key), key);
        }
    }
}
