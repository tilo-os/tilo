//! A person typing in one field, or setting one value again and again, is
//! one change in History: the keystrokes of a formula, a title or a
//! paragraph, however many words and undo steps they took, are one row
//! until the person stops; so are the calls of a verb a run of setting one
//! item's value made ([`tilo_actions::run`]).
//!
//! Undo keeps its own, finer steps for typing (a word at a time): this
//! decides only what History lists as one change. A change continues its
//! author's typing when it is a typing edit (a text replacement or splice
//! made as part of a gesture, by a person) of the same field as that
//! author's previous change, made within [`TYPING_PAUSE_MS`] of it. A run of
//! setting a value is decided as it is written: its calls share a gesture,
//! the step undo takes back, so History joins the calls of one verb on one
//! item that share one. Any other change by the author (an edit elsewhere,
//! another verb, a version they named) ends the run; another person's
//! changes in between do not end typing. Every input is in the change
//! itself, so every peer, and every reader of the log (the toolkit's
//! `HistorySource` rows, restoring one row's changes), finds the same runs.

use std::collections::HashMap;

use tilo_actions::{ActionContext, ActionId, ActionKey, Did, EditOp, PathStep};

pub use tilo_actions::run::TYPING_PAUSE_MS;
use tilo_actions::run::within_pause;

/// What a change runs on in: the field a person typed into, or the run of
/// setting one item's value with a verb.
#[derive(Clone, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum TypedField {
    Field {
        ty: String,
        field: String,
        path: Vec<PathStep>,
    },
    Setting {
        action: ActionKey,
        item: String,
        gesture: ActionId,
    },
}

impl TypedField {
    /// What `context`'s change runs on in: the field of a text replacement
    /// or splice made as part of a gesture by a person, or the verb, item
    /// and gesture of a person's call setting an item's value. A change that
    /// is a step of its own (an agent's edit, a paste through a verb) runs
    /// on in nothing.
    pub fn of(context: &ActionContext) -> Option<Self> {
        if let Some((call, item)) = tilo_actions::run::setting(context) {
            return Some(Self::Setting {
                action: call.action.clone(),
                item: item.to_owned(),
                gesture: context.gesture?,
            });
        }
        if !tilo_actions::run::by_a_person(context) {
            return None;
        }
        match context.what.as_ref()? {
            Did::Edit(edit) if matches!(edit.op, EditOp::Replace | EditOp::Splice) => {
                Some(Self::Field {
                    ty: edit.ty.clone(),
                    field: edit.field.clone(),
                    path: edit.path.clone(),
                })
            }
            _ => None,
        }
    }
}

/// Each author's typing as a log is read in order: where their current run
/// is (`T`, a row or its changes), which field it types in and when its last
/// change was.
#[doc(hidden)]
pub struct TypingRuns<T> {
    open: HashMap<String, Run<T>>,
}

struct Run<T> {
    field: TypedField,
    time_ms: i64,
    at: T,
}

impl<T> Default for TypingRuns<T> {
    fn default() -> Self {
        Self {
            open: HashMap::new(),
        }
    }
}

impl<T> TypingRuns<T> {
    /// The next change of the log, by `actor` at `time_ms`, typing into
    /// `field` (`None` for any other change). Returns the run it continues,
    /// with its time moved on, or `None` when it starts a run of its own or
    /// types nothing; the caller then [`Self::start`]s one for a typing
    /// change.
    pub fn continues(
        &mut self,
        actor: &str,
        field: Option<&TypedField>,
        time_ms: i64,
    ) -> Option<&mut T> {
        let Some(field) = field else {
            self.open.remove(actor);
            return None;
        };
        let continues = self
            .open
            .get(actor)
            .is_some_and(|run| run.field == *field && within_pause(run.time_ms, time_ms));
        if !continues {
            self.open.remove(actor);
            return None;
        }
        let run = self.open.get_mut(actor)?;
        run.time_ms = time_ms;
        Some(&mut run.at)
    }

    pub fn start(&mut self, actor: &str, field: TypedField, time_ms: i64, at: T) {
        self.open
            .insert(actor.to_owned(), Run { field, time_ms, at });
    }

    /// Every open run's place, to adjust when the places move.
    pub fn places_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.open.values_mut().map(|run| &mut run.at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::{ActionId, FieldEdit};

    fn typing(field: &str, gesture: bool) -> ActionContext {
        ActionContext {
            what: Some(Did::Edit(FieldEdit {
                ty: "plot-fn".into(),
                field: "expr".into(),
                path: vec![
                    PathStep::Field("funcs".into()),
                    PathStep::Item(field.into()),
                    PathStep::Field("expr".into()),
                ],
                op: EditOp::Replace,
                old: None,
                new: None,
                args: std::collections::BTreeMap::new(),
            })),
            gesture: gesture.then(ActionId::new),
            ..ActionContext::default()
        }
    }

    #[test]
    fn typing_in_one_field_is_one_run_until_the_person_stops() {
        let row = TypedField::of(&typing("row", true)).expect("a gesture's replacement types");
        let other = TypedField::of(&typing("other", true)).expect("so does another field's");
        assert!(
            TypedField::of(&typing("row", false)).is_none(),
            "a step of its own types nothing"
        );
        let mut runs = TypingRuns::default();
        assert!(runs.continues("ann", Some(&row), 1_000).is_none());
        runs.start("ann", row.clone(), 1_000, 0);
        assert_eq!(runs.continues("ann", Some(&row), 2_000).copied(), Some(0));
        assert!(runs.continues("bob", Some(&row), 2_500).is_none());
        runs.start("bob", row.clone(), 2_500, 1);
        assert_eq!(
            runs.continues("ann", Some(&row), 3_000).copied(),
            Some(0),
            "another person's typing does not end hers"
        );
        assert!(
            runs.continues("ann", Some(&row), 3_000 + TYPING_PAUSE_MS + 1)
                .is_none(),
            "a pause ends it"
        );
        runs.start("ann", row.clone(), 40_000, 2);
        assert!(runs.continues("ann", Some(&other), 41_000).is_none());
        runs.start("ann", other, 41_000, 3);
        assert!(runs.continues("ann", None, 42_000).is_none());
        assert!(
            runs.continues("ann", Some(&row), 43_000).is_none(),
            "a change that is not typing ended the run"
        );
    }
}
