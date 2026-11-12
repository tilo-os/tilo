//! What an agent's run is doing now, in a few words both sides understand.
//!
//! A run reports its progress as a public note on the task. The note is
//! plain English, so any reader of the task's history can show it as it is;
//! for the steps every run takes (looking something up on the web, reading a
//! document, changing one) it follows this vocabulary, which a person's own
//! surface reads back and says in their language ("Editing Notes…").

/// One step of a run, as the person is told about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskStep {
    /// Searching the web, for what.
    Searching(String),
    /// Reading what an application holds, or a web page at that address: the
    /// application's name or the page's address.
    Reading(String),
    /// Changing what an application holds; the application's name.
    Editing(String),
    /// Finding out what an application can do; the application's name.
    Exploring(String),
}

const SEARCHING: &str = "Searching the web for ";
const READING: &str = "Reading ";
const EDITING: &str = "Editing ";
const EXPLORING: &str = "Looking at what ";
const EXPLORING_END: &str = " can do…";
const ELLIPSIS: &str = "…";
/// The most characters of a query or an address a note carries: enough to
/// say what the step is about, short enough to read as one line.
const MOST_SUBJECT_CHARS: usize = 80;

impl TaskStep {
    /// The step as the run's public note says it.
    #[must_use]
    pub fn note(&self) -> String {
        match self {
            Self::Searching(what) => format!(
                "{SEARCHING}\u{201c}{}\u{201d}{ELLIPSIS}",
                bounded(what, MOST_SUBJECT_CHARS)
            ),
            Self::Reading(app) => {
                format!("{READING}{}{ELLIPSIS}", bounded(app, MOST_SUBJECT_CHARS))
            }
            Self::Editing(app) => {
                format!("{EDITING}{}{ELLIPSIS}", bounded(app, MOST_SUBJECT_CHARS))
            }
            Self::Exploring(app) => format!(
                "{EXPLORING}{}{EXPLORING_END}",
                bounded(app, MOST_SUBJECT_CHARS)
            ),
        }
    }

    /// The step a public note says, when it is one of these.
    #[must_use]
    pub fn parse(note: &str) -> Option<Self> {
        let named = |prefix: &str, suffix: &str| {
            note.strip_prefix(prefix)?
                .strip_suffix(suffix)
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
        };
        named(SEARCHING, ELLIPSIS)
            .map(|what| {
                let what = what.trim_start_matches('\u{201c}');
                Self::Searching(what.trim_end_matches('\u{201d}').to_owned())
            })
            .or_else(|| named(EXPLORING, EXPLORING_END).map(Self::Exploring))
            .or_else(|| named(READING, ELLIPSIS).map(Self::Reading))
            .or_else(|| named(EDITING, ELLIPSIS).map(Self::Editing))
    }
}

/// `text` on one line — its runs of whitespace as one space each — of at
/// most `most` characters, with an ellipsis where it was cut.
fn bounded(text: &str, most: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut bounded: String = one_line.chars().take(most).collect();
    if one_line.chars().count() > most {
        bounded.push_str(ELLIPSIS);
    }
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_reads_back_from_its_note_and_other_notes_do_not() {
        for step in [
            TaskStep::Searching("rust borrow checker".into()),
            TaskStep::Reading("Files".into()),
            TaskStep::Reading("https://example.org/a".into()),
            TaskStep::Editing("Notes".into()),
            TaskStep::Exploring("Code".into()),
        ] {
            assert_eq!(TaskStep::parse(&step.note()), Some(step));
        }
        assert_eq!(
            TaskStep::Searching("rust".into()).note(),
            "Searching the web for \u{201c}rust\u{201d}…"
        );
        assert_eq!(TaskStep::parse("Reviewing the catalogue"), None);
        assert_eq!(TaskStep::parse("Editing …"), None);
    }

    /// A note says what the step is about in one line, however long the
    /// query or address is.
    #[test]
    fn a_long_subject_is_cut_to_one_line() {
        let note = TaskStep::Searching("x".repeat(500)).note();
        assert!(note.chars().count() <= MOST_SUBJECT_CHARS + 40, "{note}");
        let url = format!("https://example.org/{}", "a".repeat(500));
        let note = TaskStep::Reading(url).note();
        assert!(note.chars().count() <= MOST_SUBJECT_CHARS + 20, "{note}");
    }
}
