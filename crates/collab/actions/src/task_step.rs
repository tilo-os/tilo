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
    /// Searching the web.
    Searching,
    /// Reading what an application holds; the application's name.
    Reading(String),
    /// Changing what an application holds; the application's name.
    Editing(String),
    /// Finding out what an application can do; the application's name.
    Exploring(String),
}

const SEARCHING: &str = "Searching the web…";
const READING: &str = "Reading ";
const EDITING: &str = "Editing ";
const EXPLORING: &str = "Looking at what ";
const EXPLORING_END: &str = " can do…";
const ELLIPSIS: &str = "…";

impl TaskStep {
    /// The step as the run's public note says it.
    #[must_use]
    pub fn note(&self) -> String {
        match self {
            Self::Searching => SEARCHING.to_owned(),
            Self::Reading(app) => format!("{READING}{app}{ELLIPSIS}"),
            Self::Editing(app) => format!("{EDITING}{app}{ELLIPSIS}"),
            Self::Exploring(app) => format!("{EXPLORING}{app}{EXPLORING_END}"),
        }
    }

    /// The step a public note says, when it is one of these.
    #[must_use]
    pub fn parse(note: &str) -> Option<Self> {
        if note == SEARCHING {
            return Some(Self::Searching);
        }
        let named = |prefix: &str, suffix: &str| {
            note.strip_prefix(prefix)?
                .strip_suffix(suffix)
                .filter(|app| !app.trim().is_empty())
                .map(str::to_owned)
        };
        named(EXPLORING, EXPLORING_END)
            .map(Self::Exploring)
            .or_else(|| named(READING, ELLIPSIS).map(Self::Reading))
            .or_else(|| named(EDITING, ELLIPSIS).map(Self::Editing))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_reads_back_from_its_note_and_other_notes_do_not() {
        for step in [
            TaskStep::Searching,
            TaskStep::Reading("Files".into()),
            TaskStep::Editing("Notes".into()),
            TaskStep::Exploring("Code".into()),
        ] {
            assert_eq!(TaskStep::parse(&step.note()), Some(step));
        }
        assert_eq!(TaskStep::parse("Reviewing the catalogue"), None);
        assert_eq!(TaskStep::parse("Editing …"), None);
    }
}
