//! The shell's Fluent catalogue.
//!
//! Chrome strings are keys resolved through the toolkit's [`mod@ui::i18n`]
//! manager, never literals. The catalogues are embedded with `include_str!`,
//! so a `--shots` render, a unit test and a live session resolve the same
//! copy with no file I/O on a draw path.

use ui::i18n::{ArgValue, EmbeddedCatalog};

/// Loaded per thread on first use; a host's manager is joined, never replaced.
/// The probe is a key every catalogue revision defines.
static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[
        ("en-US", include_str!("../../locales/en-US.ftl")),
        ("de-DE", include_str!("../../locales/de-DE.ftl")),
    ],
    "continuity-dismiss",
);

/// Review's step titles: the review contract owns their wording, so the
/// walk-through names a step exactly as the review model is told to.
static REVIEW_TITLES: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", tilo_feedback_review::FLUENT_EN_US)],
    "feedback-review-step-opened",
);

/// Resolve a shell string key.
pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// Resolve a shell string key that interpolates values.
pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

/// One text argument.
pub fn text(value: impl Into<String>) -> ArgValue {
    ArgValue::Text(value.into())
}

/// One number argument.
pub fn number(value: impl Into<f64>) -> ArgValue {
    ArgValue::Number(value.into())
}

/// A review step title built from the step, in the session's language.
pub fn step_title(label: &tilo_feedback_review::LabelTitle) -> String {
    let args: Vec<(&str, ArgValue)> = label
        .args()
        .iter()
        .map(|(name, value)| (*name, text(*value)))
        .collect();
    REVIEW_TITLES.t_args(label.fluent_key(), &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_the_shell_uses_resolves_to_a_real_string() {
        for key in [
            "continuity-continue",
            "continuity-continue-subtitle",
            "continuity-dismiss",
            "continuity-dismiss-title",
            "continuity-dismiss-subtitle",
            "continuity-other-device",
            "tab-closed-toast",
            "tab-closed-group-toast",
            "tab-closed-undo",
            "tab-closed-undo-hint",
            "tab-closed-undo-hint-unbound",
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
        let source = include_str!("../../locales/en-US.ftl");
        let keys: Vec<&str> = source
            .lines()
            .filter(|line| !line.starts_with('#') && !line.starts_with(' '))
            .filter_map(|line| line.split_once(" = ").map(|(key, _)| key.trim()))
            .collect();
        assert!(!keys.is_empty());
        for key in keys {
            let value = t(key);
            assert_ne!(value, key, "{key} has no entry in the catalogue");
            assert!(!value.trim().is_empty(), "{key} resolves to nothing");
        }
    }

    #[test]
    fn the_shell_catalogue_and_the_review_titles_resolve() {
        assert_eq!(t("remark-title"), "Say something about this");
        assert_eq!(
            t_args("remark-pick-count", &[("count", number(3))]),
            "3 picked"
        );
        assert_eq!(
            step_title(&tilo_feedback_review::LabelTitle::Failed {
                action: "Send".into()
            }),
            "You pressed Send; it failed"
        );
    }
}
