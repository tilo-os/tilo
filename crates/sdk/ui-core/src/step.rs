//! A step as a person reads it, in the language their window speaks.
//!
//! One place says what an actor is doing — the line beside its face, the
//! people list, the card drawn in the window the step happens in, the words
//! beside a run — from [`tilo_actions::step::Step`]'s data: the kind of work
//! the action declares, and the record or item it acts in. Each kind is a
//! message of the toolkit's own catalogue, so a language reads its own
//! sentence, with its own word order ("Writing in Trip plan", "Schreibt in
//! Trip plan"), rather than words assembled from an English verb and a name.

use crate::i18n::widget_format;
use tilo_actions::step::Step;

/// `step` as a person reads it, in this window's language: "Writing in Trip
/// plan", "Erstellt in Aufgaben".
#[must_use]
pub fn line(step: &Step<'_>) -> String {
    let (key, argument) = step.message();
    match argument {
        Some((name, value)) => widget_format(key, &[(name, value.into())]),
        None => widget_format(key, &[]),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test the unwrap is the assertion")]

    use super::*;
    use crate::i18n::I18nManager;
    use tilo_actions::StepKind;

    /// A window that ships English and German, speaking `locale`: the
    /// toolkit's own words follow the window's language, as in a real one.
    fn speak(locale: &str) {
        I18nManager::install(
            "en-US",
            locale,
            &[
                ("en-US", "step-probe = probe"),
                ("de-DE", "step-probe = probe"),
            ],
        )
        .unwrap();
    }

    /// The toolkit's own catalogue holds every kind's line, and German says
    /// its own word order: the point of asking the catalogue rather than
    /// assembling English words.
    #[test]
    fn every_kind_of_step_reads_in_the_window_language() {
        speak("en-US");
        assert_eq!(
            line(&Step::new(StepKind::Create, Some("Issues")).in_app(Some("Tasks"))),
            "Creating in Issues"
        );
        assert_eq!(
            line(&Step::new(StepKind::Write, Some("Trip plan"))),
            "Writing in Trip plan"
        );
        assert_eq!(
            line(&Step::new(StepKind::Delete, Some("Trip plan"))),
            "Deleting Trip plan"
        );
        assert_eq!(
            line(&Step::new(StepKind::Run, None).running(Some("cargo build"))),
            "Running cargo build"
        );
        assert_eq!(line(&Step::new(StepKind::Work, None)), "Working");

        speak("de-DE");
        assert_eq!(
            line(&Step::new(StepKind::Create, Some("Issues"))),
            "Erstellt in Issues"
        );
        assert_eq!(
            line(&Step::new(StepKind::Write, Some("Trip plan"))),
            "Schreibt in Trip plan"
        );
        assert_eq!(
            line(&Step::new(StepKind::Delete, Some("Trip plan"))),
            "Löscht Trip plan"
        );
        assert_eq!(
            line(&Step::new(StepKind::Run, None).running(Some("cargo build"))),
            "Führt cargo build aus",
            "the command sits where German puts it, never where English does"
        );
    }
}
