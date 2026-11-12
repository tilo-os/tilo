//! What an action is doing, in one line a person reads: the catalogue's own
//! name for the action, put in the present tense, on the record it acts in.
//!
//! One source for every surface that says what a step is at — the presence
//! line beside an agent's face, the activity card drawn in the window it works
//! in, the words a reviewer reads on a run. They ask here, so they never say
//! different things about the same step: "Editing Trip plan" on the record,
//! "Creating note from Markdown" where the record is the window the card
//! stands in.

/// The colour a verb's phrase is put in: the action's own name says what kind
/// of work it is, so "New note" is adding and "Replace text" is editing.
#[must_use]
pub fn verb_of(actions_words: &str) -> Option<&'static str> {
    let action = actions_words.to_lowercase();
    let any = |words: &[&str]| words.iter().any(|word| action.contains(word));
    Some(if any(&["run", "exec", "shell", "command"]) {
        "Running"
    } else if any(&["delete", "remove", "clear"]) {
        "Deleting"
    } else if any(&["new", "add", "create"]) {
        "Adding"
    } else if any(&[
        "edit", "insert", "append", "replace", "set", "update", "write", "type", "format",
        "rename", "move", "toggle",
    ]) {
        "Editing"
    } else if any(&["open", "start", "launch"]) {
        "Opening"
    } else {
        return None;
    })
}

/// The action's own name in the present tense, as a person reads it: its first
/// word turned into the -ing form, the rest as the catalogue words it
/// ("Create note from Markdown" is "Creating note from Markdown"). `working`
/// names what is said where the catalogue names nothing.
#[must_use]
pub fn doing_phrase(actions_words: &str, working: &str) -> String {
    let mut words = actions_words.split_whitespace();
    let Some(first) = words.next() else {
        return working.to_owned();
    };
    // "New note" names what the action makes, not a verb: whoever acts is
    // creating it, never "newing" it.
    let start = participle(if first.eq_ignore_ascii_case("new") {
        "create"
    } else {
        first
    });
    words.fold(start, |mut phrase, word| {
        phrase.push(' ');
        phrase.push_str(word);
        phrase
    })
}

/// What a step is at, on the record it acts in: its verb on that record
/// ("Editing Trip plan"), and the action's own words where no record is named
/// ("Creating note from Markdown"). A line of its own beside a face, so it
/// starts a sentence: [`doing_phrase`] carried it mid-sentence instead.
#[must_use]
pub fn step_line(actions_words: &str, record: Option<&str>) -> String {
    let record = record.map(str::trim).filter(|record| !record.is_empty());
    let line = match (record, verb_of(actions_words)) {
        (Some(record), Some(verb)) => format!("{verb} {record}"),
        _ => doing_phrase(actions_words, "Working"),
    };
    let mut characters = line.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(characters).collect()
    })
}

/// The -ing form of a verb as an app words it. Verbs that double their last
/// consonant are listed: no rule derives "setting" from "set" without also
/// turning "visit" into "visitting".
fn participle(verb: &str) -> String {
    const DOUBLING: &[&str] = &[
        "begin", "commit", "cut", "drop", "embed", "fit", "forget", "get", "hit", "log", "map",
        "permit", "pin", "plan", "prefer", "put", "quit", "refer", "run", "scan", "set", "ship",
        "shop", "sit", "skip", "stop", "submit", "tag", "trim", "wrap",
    ];
    let verb = verb.trim_end_matches(['.', ',', ':', ';', '!', '?']);
    let lower = verb.to_lowercase();
    if let Some(stem) = lower.strip_suffix("ie") {
        return format!("{stem}ying");
    }
    if let Some(stem) = lower.strip_suffix('e')
        && !stem.is_empty()
        && !stem.ends_with(['e', 'y', 'o'])
    {
        return format!("{stem}ing");
    }
    if let Some(last) = lower.chars().last()
        && DOUBLING.contains(&lower.as_str())
    {
        return format!("{lower}{last}ing");
    }
    format!("{lower}ing")
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test the unwrap is the assertion"
    )]

    use super::*;

    /// A step is said on its record in its verb, and in its own words where no
    /// record is named: the same catalogue words every surface reads.
    #[test]
    fn a_step_is_said_on_its_record_in_its_verbs() {
        assert_eq!(
            step_line("Replace text at a place", Some("Trip plan")),
            "Editing Trip plan"
        );
        assert_eq!(
            step_line("Rename note", Some("Trip plan")),
            "Editing Trip plan"
        );
        assert_eq!(step_line("New note", Some("Trip plan")), "Adding Trip plan");
        assert_eq!(
            step_line("Delete note", Some("Trip plan")),
            "Deleting Trip plan"
        );
        // No record: the action's own words, in the present tense.
        assert_eq!(
            step_line("Create note from Markdown", None),
            "Creating note from Markdown"
        );
        assert_eq!(step_line("New note", None), "Creating note");
        // A name that says no kind of work keeps its own words on the record.
        assert_eq!(
            step_line("Complete task", Some("Book the ferry")),
            "Completing task"
        );
        assert_eq!(step_line("", None), "Working");
        assert_eq!(step_line("   ", Some("  ")), "Working");
    }

    #[test]
    fn a_verbs_phrase_is_its_participle() {
        assert_eq!(participle("set"), "setting");
        assert_eq!(participle("make"), "making");
        assert_eq!(participle("tie"), "tying");
        assert_eq!(participle("visit"), "visiting");
    }
}
