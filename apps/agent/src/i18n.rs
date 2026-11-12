//! The agent's Fluent catalogue: every fixed sentence it says to a person.
//!
//! A run's reply, its progress and why it stopped are the agent's own words,
//! so they are messages here, resolved in the language the person chose for
//! their desktop: their `appearance.language` setting, read the way every
//! app's words are, and the machine's own locale when they chose none. What a
//! model answers is not here; it arrives in whatever language it was asked in.
//!
//! [`follow`] selects that language when a run starts, so a task begun after
//! the person changed it speaks the one they now read.

use tilo_actions::task_step::TaskStep;

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

/// Select the person's language before a run says anything: their settings if
/// they chose one, the machine's locale otherwise. A run starts long after the
/// app did, and the language may have changed in between.
pub fn follow() {
    CATALOG.ensure();
    ui::i18n::follow_configured_language();
}

/// One step of a run, said to the person: what it is doing, and to what.
///
/// The step itself is a small vocabulary a run keeps so it says each step once
/// (`tilo_actions::task_step`); its words are here, so the reply and the note
/// beside it read in the person's language.
#[must_use]
pub fn step(step: &TaskStep) -> String {
    let (key, subject) = match step {
        TaskStep::Searching(what) => ("agent-step-searching", what),
        TaskStep::Reading(what) => ("agent-step-reading", what),
        TaskStep::Editing(what) => ("agent-step-editing", what),
        TaskStep::Exploring(what) => ("agent-step-exploring", what),
    };
    t_args(key, &[("what", ui::i18n::ArgValue::from(bounded(subject)))])
}

/// The most characters of a query or an address a step's note carries: enough
/// to say what the step is about, short enough to read as one line.
const MOST_SUBJECT_CHARS: usize = 80;

/// `text` on one line (its runs of whitespace as one space each) of at most
/// [`MOST_SUBJECT_CHARS`], with an ellipsis where it was cut.
fn bounded(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut bounded: String = one_line.chars().take(MOST_SUBJECT_CHARS).collect();
    if one_line.chars().count() > MOST_SUBJECT_CHARS {
        bounded.push('\u{2026}');
    }
    bounded
}

/// Make `locale` the language the person chose, as their settings say it, for
/// as long as the returned scope lives: a test then drives the resolution a
/// desktop does, not only the catalogue.
#[cfg(test)]
pub(crate) fn chosen(locale: &str) -> settings_store::SourceScope {
    let document = serde_json::json!({"appearance": {"language": locale}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    settings_store::scoped_source(Some(std::sync::Arc::new(
        settings_store::HostDocument::new(document, |_| Ok(())),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every message the agent can say exists in every language it ships, so a
    /// German desktop never reads one English sentence among German ones.
    #[test]
    fn every_sentence_is_written_in_every_language() {
        let keys = [
            "agent-plan-model",
            "agent-plan-words-asked",
            "agent-plan-words-no-model",
            "agent-plan-no-action",
            "agent-plan-unmatched-asked",
            "agent-plan-no-model",
            "agent-done-model",
            "agent-done-words-asked",
            "agent-done-words-no-model",
            "agent-match-nothing",
            "agent-match-several",
            "agent-match-one-of",
            "agent-match-needs-text",
            "agent-run-reading-actions",
            "agent-run-step",
            "agent-model-turn-completed",
            "agent-step-searching",
            "agent-step-reading",
            "agent-step-editing",
            "agent-step-exploring",
            "agent-step-program",
            "agent-run-needs-decision",
            "agent-run-directions-require-review",
            "agent-run-cancelled-before-planning",
            "agent-run-cancelled-before-action",
            "agent-run-cancelled-in-action",
            "agent-cancel-task-center",
            "agent-cancel-thinking",
            "agent-cancel-superseded",
            "agent-fail-no-terminal-state",
            "agent-fail-local-only",
            "agent-fail-arguments",
            "agent-fail-turns",
            "agent-fail-time",
            "agent-fail-budget",
            "agent-fail-model-call",
            "agent-fail-model-answer",
            "agent-fail-turn-not-finished",
            "agent-fail-native-outcome",
            "agent-fail-direction-pages",
        ];
        for (locale, source) in [
            ("en-US", include_str!("../locales/en-US.ftl")),
            ("de-DE", include_str!("../locales/de-DE.ftl")),
        ] {
            let messages = tilo_copy_lint::catalog::messages(source);
            for key in keys {
                let text = messages.get(key).unwrap_or_else(|| {
                    panic!("{locale} has no {key}");
                });
                assert!(!text.trim().is_empty(), "{locale}: {key} says nothing");
            }
        }
    }

    /// The language a person chose in Settings is the one a run speaks: the
    /// choice is read through their settings, as every app reads it.
    #[test]
    fn the_persons_choice_is_the_language_a_run_speaks() {
        let _german = chosen("de-DE");
        follow();
        assert_eq!(ui::i18n::I18nManager::current_locale(), "de-DE");
        assert_eq!(
            t("agent-match-nothing"),
            "Das kann ich hier nicht erledigen."
        );
    }

    /// A step is said in the language the run is speaking, with the thing it is
    /// about inside the sentence.
    #[test]
    fn a_step_is_said_in_the_language_of_the_run() {
        ui::i18n::I18nManager::init("en-US");
        assert_eq!(
            step(&TaskStep::Editing("Notizen".into())),
            "Editing Notizen…"
        );
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_eq!(
            step(&TaskStep::Editing("Notizen".into())),
            "Ändert Notizen…"
        );
        assert_eq!(
            step(&TaskStep::Searching("borrow checker".into())),
            "Sucht im Web nach „borrow checker“…"
        );
    }

    /// A long query stays one line: the note is a line, not a paragraph.
    #[test]
    fn a_long_subject_is_cut_to_one_line() {
        ui::i18n::I18nManager::init("en-US");
        let note = step(&TaskStep::Reading(format!(
            "https://example.org/{}",
            "a".repeat(500)
        )));
        assert!(note.chars().count() <= 110, "{note}");
        assert!(!note.contains('\n'), "{note}");
    }
}
