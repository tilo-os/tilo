//! The shell's own Fluent catalogue.
//!
//! Shell chrome passes keys, never literals, through the toolkit's
//! [`ui::i18n`] manager. The catalogues are embedded, so a `shots` render, a
//! unit test and a live session resolve the same strings with no file I/O
//! on a draw path.

use ui::i18n::{ArgValue, EmbeddedCatalog};

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[
        ("en-US", include_str!("../../locales/en-US.ftl")),
        ("de-DE", include_str!("../../locales/de-DE.ftl")),
    ],
    "remark-title",
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
