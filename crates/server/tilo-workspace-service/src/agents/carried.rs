//! What a request carries, said to a turn in words.
//!
//! What asks an agent says it in words and refers to resources inside them: a message that
//! carries a note, an ask whose text links a task. On a device such a resource is what the
//! agent's work is started on. A turn this server carries out is started with words alone, so
//! each resource is said in them here: what it is called where it has a name of its own, its
//! kind and id, and the app that keeps it. The turn reads it through its tools, as its rules
//! allow.

use std::fmt::Write as _;

use crate::asks::Reference;

/// One resource a request carries.
pub(super) struct Carried<'a> {
    /// What it is called, where the request names it apart from its words; empty where its
    /// words say it.
    pub(super) label: &'a str,
    pub(super) kind: &'a str,
    pub(super) id: &'a str,
    /// The app that keeps it.
    pub(super) app: &'a str,
}

impl<'a> Carried<'a> {
    /// The resource `reference` names; none for a member, whom nothing is said of here.
    pub(super) fn of(reference: &'a Reference) -> Option<Self> {
        let at = reference.as_resource()?;
        Some(Self {
            label: &at.reference.label,
            kind: &at.reference.kind,
            id: &at.reference.id,
            app: &at.app,
        })
    }
}

/// `instruction`, with each resource of `carried` said after it.
pub(super) fn told<'a>(
    mut instruction: String,
    carried: impl IntoIterator<Item = Carried<'a>>,
) -> String {
    for carried in carried {
        if !instruction.is_empty() {
            instruction.push('\n');
        }
        let Carried {
            label,
            kind,
            id,
            app,
        } = carried;
        if label.is_empty() {
            let _ = write!(instruction, "It carries {kind} `{id}`, in `{app}`.");
        } else {
            let _ = write!(
                instruction,
                "It carries {label:?} ({kind} `{id}`, in `{app}`)."
            );
        }
    }
    instruction
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resource a request carries is said after its words, by its name where the request
    /// names it apart; a request that carries none says its words alone.
    #[test]
    fn what_a_request_carries_is_said_in_words() {
        let note = || Carried {
            label: "Launch plan",
            kind: "notes.note",
            id: "note:7",
            app: "so.tilo.notes",
        };
        assert_eq!(
            told("Is this ready?".to_owned(), [note()]),
            "Is this ready?\nIt carries \"Launch plan\" (notes.note `note:7`, in `so.tilo.notes`)."
        );
        assert_eq!(
            told(String::new(), [note()]),
            "It carries \"Launch plan\" (notes.note `note:7`, in `so.tilo.notes`)."
        );
        assert_eq!(told("Is this ready?".to_owned(), []), "Is this ready?");

        let task = Reference::resource(
            "so.tilo.lang.tasks",
            "tl.so.tilo.lang.tasks.Task",
            "tl:so.tilo.lang.tasks:t1",
        );
        let member = Reference::Member(tilo_actions::MemberId::agent("scout"));
        assert_eq!(
            told(
                "@Scout look at Ship it".to_owned(),
                [&member, &task].into_iter().filter_map(Carried::of)
            ),
            "@Scout look at Ship it\nIt carries tl.so.tilo.lang.tasks.Task \
             `tl:so.tilo.lang.tasks:t1`, in `so.tilo.lang.tasks`."
        );
    }
}
