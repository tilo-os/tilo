//! Settings' Fluent catalogue.
//!
//! User-facing strings are keys resolved through the toolkit's [`ui::i18n`]
//! manager. The catalogue is embedded with `include_str!`, so a `--shots`
//! render, a unit test and a live session resolve the same strings. Panes
//! move onto it as they are rewritten; the AI pane is fully on it.

use ui::i18n::{ArgValue, EmbeddedCatalog};

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", include_str!("../locales/en-US.ftl"))],
    "ai-pane-title",
);

/// Resolve a Settings string key.
pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// Resolve a Settings string key that interpolates variables. The sentence
/// stays in the catalogue; only the nouns are substituted.
pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

/// A string argument.
pub fn arg(value: impl Into<String>) -> ArgValue {
    ArgValue::from(value.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ai_key_resolves_to_a_real_string() {
        let catalogue = include_str!("../locales/en-US.ftl");
        let mut keys = Vec::new();
        for line in catalogue.lines() {
            if let Some((key, _)) = line.split_once(" = ")
                && !key.starts_with('#')
                && !key.starts_with(' ')
                && !key.is_empty()
            {
                keys.push(key.trim().to_string());
            }
        }
        assert!(keys.len() > 50, "catalogue parsed {} keys", keys.len());
        for key in keys {
            let value = t(&key);
            assert_ne!(value, key, "{key} has no catalogue entry");
        }
        // Every key the AI pane names in code resolves too.
        for source in [
            include_str!("panes/ai.rs"),
            include_str!("ai_settings.rs"),
            include_str!("actions.rs"),
            include_str!("../../../crates/agent/provider-config/src/tasks.rs"),
            include_str!("../../../crates/agent/provider-config/src/assign.rs"),
            include_str!("../../../crates/agent/provider-config/src/options.rs"),
            include_str!("../../../crates/agent/provider-config/src/lib.rs"),
        ] {
            for fragment in source.split('"').skip(1).step_by(2) {
                if fragment.starts_with("ai-")
                    && fragment
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit())
                    && !fragment.ends_with('-')
                {
                    assert!(
                        catalogue.contains(&format!("\n{fragment} =")),
                        "{fragment} is used but not in the catalogue"
                    );
                }
            }
        }
    }
}
