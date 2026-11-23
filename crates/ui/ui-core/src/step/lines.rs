//! What a run is at, in a person's words, said beside its face: the host's
//! own word for each request a run makes ([`looking`], [`reading`],
//! [`searching`], [`doing`]), cut where a person stops reading.

use collab_types::presence::MAX_ACTIVITY_BYTES;
use serde_json::Value;
use tilo_actions::StepKind;
use tilo_actions::app_label;
use tilo_actions::step::{COMMAND_PARAMS, Step};

/// The longest line said beside a run's face, the ellipsis included: what a
/// person reads at a glance where the roster draws it.
const SHOWN_CHARS: usize = 64;

/// Connected, and asked nothing yet: an agent thinking, or waiting on
/// its person.
#[must_use]
pub fn waiting() -> String {
    clip(&super::saying("step-waiting", None))
}

/// Looking at the apps, or at the one `app` names.
#[must_use]
pub fn looking(app: Option<&str>) -> String {
    match app.map(app_label).filter(|app| !app.is_empty()) {
        Some(app) => clip(&super::saying("step-looking-at", Some(&app))),
        None => clip(&super::saying("step-looking-at-apps", None)),
    }
}

/// Reading what `app` shows.
#[must_use]
pub fn reading(app: &str) -> String {
    let record = app_label(app);
    clip(&super::line(&Step::new(StepKind::Read, Some(&record))))
}

/// Searching for `needle`.
#[must_use]
pub fn searching(needle: &str) -> String {
    clip(&super::saying("step-searching", Some(needle.trim())))
}

/// Taking the action `action` in `app` with `args`: what kind of step its
/// own declaration names, on the app's record, a shell command as the
/// command line itself. Said in the window's language by the one function
/// every surface asks ([`super`]).
#[must_use]
pub fn doing(app: &str, action: &str, args: Option<&Value>) -> String {
    let command = COMMAND_PARAMS
        .iter()
        .find_map(|key| args.and_then(|args| args.get(*key)))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty());
    let kind = if action == tilo_actions::CREATE_ACTION {
        StepKind::Create
    } else if command.is_some() {
        StepKind::Run
    } else {
        StepKind::Work
    };
    let record = app_label(app);
    let step = Step::new(kind, Some(&record)).running(command);
    clip(&super::line(&step))
}

/// `line`, for an action that failed: never said as one that worked.
#[must_use]
pub fn failed(line: &str) -> String {
    clip(&format!("{line} (failed)"))
}

/// `step` as a person reads it beside a run's face: the one line every
/// surface says of a step ([`super`]), cut where a person stops
/// reading.
#[must_use]
pub fn step(step: &Step<'_>) -> String {
    clip(&super::line(step))
}

/// `line` as far as a person reads it at a glance, and within the bytes
/// a presence frame carries.
fn clip(line: &str) -> String {
    let mut kept = String::new();
    for character in line.chars() {
        if kept.chars().count().saturating_add(2) > SHOWN_CHARS
            || kept.len().saturating_add(character.len_utf8()) > MAX_ACTIVITY_BYTES
        {
            kept.push('…');
            return kept;
        }
        kept.push(character);
    }
    kept.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_runs_lines_are_said_in_a_persons_words;

        phase a_request_is_said_as_what_the_run_is_doing;
        // Each request is said as what the run is at now, on what.
        {
            assert_eq!(looking(None), "Looking at the apps");
            assert_eq!(looking(Some("so.tilo.terminal")), "Looking at Terminal");
            assert_eq!(reading("so.tilo.notes"), "Reading Notes");
            assert_eq!(searching(" invoices "), "Searching for invoices");
            assert_eq!(waiting(), "Waiting");
            assert_eq!(
                doing("so.tilo.sheets", tilo_actions::CREATE_ACTION, None),
                "Creating in Sheets"
            );
            assert_eq!(
                doing("so.tilo.word-processor", "append-paragraph", None),
                "Working in Word processor",
                "an app is named in words: its id's last part as a person reads it"
            );
            let command = serde_json::json!({"command": "cargo build"});
            assert_eq!(
                doing("so.tilo.terminal", "run", Some(&command)),
                "Running cargo build"
            );
            assert_eq!(
                doing("so.tilo.terminal", "run", None),
                "Working in Terminal"
            );
            assert_eq!(failed("Working in Sheets"), "Working in Sheets (failed)");
        }

        phase a_step_is_said_on_the_record_it_acts_in;
        // A step is said in the catalogue's words for its kind, on the record it acts in:
        // "Writing in Trip plan", never words assembled from the action's English label.
        {
            assert_eq!(
                step(&Step::new(StepKind::Write, Some("Trip plan"))),
                "Writing in Trip plan"
            );
            assert_eq!(
                step(&Step::new(StepKind::Delete, Some("Trip plan"))),
                "Deleting Trip plan"
            );
            let long = step(&Step::new(StepKind::Write, Some(&"x".repeat(200))));
            assert!(long.ends_with('…') && long.chars().count() <= SHOWN_CHARS);
        }

        phase a_long_line_is_cut_where_a_person_stops_reading;
        // A long line is cut where a person stops reading.
        {
            let long = searching(&"x".repeat(200));
            assert!(long.ends_with('…') && long.chars().count() <= SHOWN_CHARS);
        }
    }
}
