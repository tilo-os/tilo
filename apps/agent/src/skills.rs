//! A person's own instructions for the resources they pick: what they want
//! done with a book, a folder of invoices, a page of notes.
//!
//! A skill is not a setting of one app. It is the person's own record, kept
//! in their room, holding a name, the resources it is for
//! ([`Selector`](tilo_selector::Selector): the kinds and the
//! fields that pick them out), the instruction that goes to the model, the
//! style and guidance the answer should keep to, examples of answers they
//! accepted and rejected, the model to ask, and whether asking it does the
//! one thing, works on the task or leaves the choice to the agent.
//!
//! What a skill holds is data, and the record is the skill's one home: the
//! composer that offers it and the agent that reads it both read here, and a
//! skill changes where a person changes anything else of theirs, with the
//! same history and undo.
//!
//! What a person reads of a skill is a Fluent key like `Skill.noSubject`, so
//! every surface says the same thing about the same refusal.
//!
//! Nothing here runs anything, and nothing here decides what a person may
//! do: a skill names the work and how to do it, while the resources it runs
//! on carry the permissions. A skill's [`RunMode`] is a preference, not a
//! grant.

use serde::{Deserialize, Serialize};
use tilo_selector::{Selector, Subject};

/// The longest a skill's name may be.
pub const MAX_NAME_CHARS: usize = 80;

/// The most skills one person keeps.
pub const MAX_SKILLS: usize = 200;

/// The longest a skill's instruction or guidance may be.
pub const MAX_INSTRUCTION_BYTES: usize = 16 * 1024;

/// How far an ask should go once a skill is picked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunMode {
    /// Answer this once, here, and that is all.
    #[default]
    OneShot,
    /// Carry the work out as a task, with its own run and its history.
    Task,
    /// Answer if the ask is a question, work it as a task if it is work.
    Decide,
}

/// A skill: a person's instruction for the resources a selector picks out.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Skill {
    /// What the person calls it: "Book report".
    pub name: String,
    /// The resources it is for.
    pub selector: Selector,
    /// The instruction the model is given, with placeholders for what the
    /// ask itself brings.
    pub instruction: String,
    /// How the answer should read: a voice, a length, a form.
    pub style: String,
    /// What the person cares about and how the answer is built.
    pub guidance: String,
    /// Answers the person accepted, kept as examples.
    pub accepted: Vec<String>,
    /// Answers the person rejected, kept as examples.
    pub rejected: Vec<String>,
    /// The model to ask, by its published name; empty for the person's own
    /// default.
    pub model: String,
    /// How far the ask should go.
    pub run_mode: RunMode,
}

impl Skill {
    /// A skill called `name` for the resources `selector` picks out, with
    /// no instruction yet.
    #[must_use]
    pub fn new(name: impl Into<String>, selector: Selector) -> Self {
        Self {
            name: name.into(),
            selector,
            ..Self::default()
        }
    }

    /// Whether this skill is for the resource whose surface `matcher` reads.
    #[must_use]
    pub fn is_for(&self, subject: &(impl Subject + ?Sized)) -> bool {
        self.selector.matches(subject)
    }

    /// The check the model keeps, so a skill is never stored unusable: it
    /// has a name to be picked by, and a selector that names something.
    ///
    /// # Errors
    /// The Fluent key of why it cannot be stored, said in the person's words.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Skill.needsName".to_owned());
        }
        if self.name.chars().count() > MAX_NAME_CHARS {
            return Err("Skill.nameTooLong".to_owned());
        }
        if self.selector.kinds.is_empty() && self.selector.predicates.is_empty() {
            return Err("Skill.noSubject".to_owned());
        }
        if self.instruction.len() > MAX_INSTRUCTION_BYTES
            || self.guidance.len() > MAX_INSTRUCTION_BYTES
        {
            return Err("Skill.tooLong".to_owned());
        }
        Ok(())
    }
}

/// What the skill is for, in the selector's syntax; empty when it picks out every resource.
#[must_use]
pub fn subject_of(skill: &Skill) -> String {
    let syntax = skill.selector.to_syntax();
    if skill.selector.kinds.is_empty() && skill.selector.predicates.is_empty() {
        String::new()
    } else {
        syntax
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_selector::Predicate;

    fn books() -> Selector {
        Selector::kinds(["ebook.book".to_owned()]).where_(Predicate::new(
            "content-type",
            ["application/epub+zip".to_owned()],
        ))
    }

    /// A resource of `kind` saying `content_type` in its content type.
    struct Sample {
        kind: &'static str,
        content_type: &'static str,
    }

    impl Subject for Sample {
        fn kind(&self) -> &str {
            self.kind
        }

        fn says(&self, field: &str) -> Vec<&str> {
            if field == "content-type" {
                vec![self.content_type]
            } else {
                Vec::new()
            }
        }
    }

    #[test]
    fn a_skill_is_for_the_resources_its_selector_picks_out() {
        let skill = Skill::new("Book report", books());
        assert!(skill.is_for(&Sample {
            kind: "ebook.book",
            content_type: "application/epub+zip",
        }));
        assert!(!skill.is_for(&Sample {
            kind: "ebook.book",
            content_type: "application/pdf",
        }));
        assert!(!skill.is_for(&Sample {
            kind: "files.file",
            content_type: "application/epub+zip",
        }));
    }

    #[test]
    fn a_skill_needs_a_name_and_something_to_be_for() {
        assert!(Skill::new("Book report", books()).validate().is_ok());
        assert!(Skill::new("  ", books()).validate().is_err());
        assert!(
            Skill::new("Anything", Selector::default())
                .validate()
                .is_err()
        );
    }

    #[test]
    fn the_composer_says_what_the_skill_is_for() {
        let skill = Skill::new("Book report", books());
        assert_eq!(
            subject_of(&skill),
            "ebook.book { content-type in [\"application/epub+zip\"] }"
        );
        assert_eq!(subject_of(&Skill::new("Anything", Selector::default())), "");
    }
}
