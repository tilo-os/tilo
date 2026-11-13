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
/// plan", "Creating an issue in Issues", "Legt eine Aufgabe in Aufgaben an".
#[must_use]
pub fn line(step: &Step<'_>) -> String {
    if let Some(made) = making(step) {
        return made;
    }
    let (key, argument) = step.message();
    match argument {
        Some((name, value)) => widget_format(key, &[(name, value.into())]),
        None => widget_format(key, &[]),
    }
}

/// What a create makes, said by the noun the app's catalogue gives the
/// collection it adds to, and where it makes it: "Creating an issue in
/// Issues", "Legt eine Aufgabe in Aufgaben an". `None` where the step makes
/// nothing, or no catalogue of this window holds the collection's noun: the
/// step then says only where it works, as every other step of its kind does.
///
/// The noun is the one [`tilo_resource::words::item_words`] names a
/// collection's items by — the article is part of it, since a language whose
/// article agrees with the noun ("eine Aufgabe", "an issue") cannot have it
/// added outside.
fn making(step: &Step<'_>) -> Option<String> {
    let (kind, field) = step.makes?;
    let key = tilo_resource::label::noun_key(&format!("{kind}.{field}"));
    let noun = crate::i18n::noun(&key)?;
    let record = step.record?;
    Some(widget_format(
        tilo_actions::step::CREATING_ONE,
        &[("noun", noun.into()), ("record", record.into())],
    ))
}

/// The catalogue message `key` said with `name` as its one argument, in this
/// window's language: what a run is at when it is not a step of a record —
/// waiting, looking, searching.
#[must_use]
pub fn saying(key: &str, name: Option<&str>) -> String {
    match name {
        Some(name) => widget_format(key, &[("what", name.into())]),
        None => widget_format(key, &[]),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test the unwrap is the assertion")]

    use super::*;
    use crate::i18n::I18nManager;
    use tilo_actions::StepKind;

    /// What an app's catalogue says one of the issues its tracker makes is, with
    /// the article part of the noun: a language whose article agrees with the
    /// noun cannot have it added outside.
    fn issue_noun(locale: &str) -> &'static str {
        match locale {
            "de-DE" => {
                "tasks-tracker-issues-noun = { $count ->\n    [one] eine Aufgabe\n   *[other] \
                 Aufgaben\n}\n"
            }
            _ => {
                "tasks-tracker-issues-noun = { $count ->\n    [one] an issue\n   *[other] issues\n}\n"
            }
        }
    }

    /// A window that ships English and German, speaking `locale`: the
    /// toolkit's own words follow the window's language, as in a real one, and
    /// the app's own name what its tracker makes.
    fn speak(locale: &str) {
        let english = format!("step-probe = probe\n{}", issue_noun("en-US"));
        let german = format!("step-probe = probe\n{}", issue_noun("de-DE"));
        I18nManager::install("en-US", locale, &[("en-US", &english), ("de-DE", &german)]).unwrap();
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
            "Legt in Issues an",
            "German says the work happening now, never words that read as the past"
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

    /// A step on one of a collection's items says that item, as its app
    /// named it ("Editing Book room 4 for Friday"), in the window's language:
    /// the record's own name only stands where the step names no item.
    #[test]
    fn a_step_on_an_item_says_the_item_in_the_window_language() {
        let on = |kind| Step::new(kind, Some("Book room 4 for Friday"));
        speak("en-US");
        assert_eq!(line(&on(StepKind::Edit)), "Editing Book room 4 for Friday");
        assert_eq!(
            line(&on(StepKind::Delete)),
            "Deleting Book room 4 for Friday"
        );
        assert_eq!(line(&on(StepKind::Move)), "Moving Book room 4 for Friday");
        assert_eq!(
            line(&on(StepKind::Write)),
            "Writing in Book room 4 for Friday"
        );
        speak("de-DE");
        assert_eq!(
            line(&on(StepKind::Edit)),
            "Bearbeitet Book room 4 for Friday"
        );
        assert_eq!(line(&on(StepKind::Delete)), "Löscht Book room 4 for Friday");
        assert_eq!(
            line(&on(StepKind::Move)),
            "Verschiebt Book room 4 for Friday"
        );
        assert_eq!(
            line(&on(StepKind::Write)),
            "Schreibt in Book room 4 for Friday"
        );
    }

    /// A create that names the collection it adds to says what is being made,
    /// by the app's own noun for one of them, with its own article and word
    /// order: the line a person reads is a sentence of their language, never
    /// English words with their language's name in them.
    #[test]
    fn a_create_says_what_it_makes_in_the_window_language() {
        let made = |record: &'static str| {
            Step::new(StepKind::Create, Some(record))
                .in_app(Some("Tasks"))
                .making("tasks.tracker", "issues")
        };
        speak("en-US");
        assert_eq!(line(&made("Issues")), "Creating an issue in Issues");
        speak("de-DE");
        assert_eq!(line(&made("Aufgaben")), "Legt eine Aufgabe in Aufgaben an");
    }

    /// A collection no catalogue of the window names says only where the step
    /// works, rather than a sentence with a hole in it.
    #[test]
    fn a_create_no_catalogue_names_says_where_it_works() {
        speak("en-US");
        assert_eq!(
            line(&Step::new(StepKind::Create, Some("Notes")).making("notes.notebook", "pages")),
            "Creating in Notes"
        );
    }
}
