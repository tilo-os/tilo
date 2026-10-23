//! A person typing in one field is one change in History: the keystrokes of
//! a formula, a title or a paragraph, however many words and undo steps
//! they took, are one row until the person stops.
//!
//! Undo keeps its own, finer steps (a word at a time): this decides only
//! what History lists as one change. A change continues its author's typing
//! when it is a typing edit (a text replacement or splice made as part of a
//! gesture, by a person) of the same field as that author's previous change,
//! made within [`TYPING_PAUSE_MS`] of it. Any other change by the author (an
//! edit elsewhere, a verb, a version they named) ends the typing; another
//! person's changes in between do not. Every input is in the change itself,
//! so every peer, and every reader of the log ([`super::HistorySource`]'s
//! rows, restoring one row's changes), finds the same runs.

use std::collections::HashMap;

use tilo_actions::{ActionContext, ActorKind, Did, EditOp, PathStep};

/// How long a person may stop typing in a field and still be writing the
/// same thing: long enough to think through the rest of a formula or a
/// sentence, short enough that coming back to the field later is a change
/// of its own.
pub const TYPING_PAUSE_MS: i64 = 30_000;

/// The field a change typed into, when it is a person's typing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TypedField {
    ty: String,
    field: String,
    path: Vec<PathStep>,
}

impl TypedField {
    /// The field `context`'s change typed into: a text replacement or
    /// splice made as part of a gesture by a person. A change that is a step
    /// of its own (an agent's edit, a paste through a verb) types nothing.
    pub(crate) fn of(context: &ActionContext) -> Option<Self> {
        context.gesture?;
        if context
            .invoker
            .as_ref()
            .is_some_and(|invoker| invoker.kind != ActorKind::Person)
        {
            return None;
        }
        match context.what.as_ref()? {
            Did::Edit(edit) if matches!(edit.op, EditOp::Replace | EditOp::Splice) => Some(Self {
                ty: edit.ty.clone(),
                field: edit.field.clone(),
                path: edit.path.clone(),
            }),
            _ => None,
        }
    }
}

/// Each author's typing as a log is read in order: where their current run
/// is (`T`, a row or its changes), which field it types in and when its last
/// change was.
pub(crate) struct TypingRuns<T> {
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
    pub(crate) fn continues(
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

    pub(crate) fn start(&mut self, actor: &str, field: TypedField, time_ms: i64, at: T) {
        self.open
            .insert(actor.to_owned(), Run { field, time_ms, at });
    }

    /// Every open run's place, to adjust when the places move.
    pub(crate) fn places_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.open.values_mut().map(|run| &mut run.at)
    }
}

/// Whether a change at `next` is within the pause of one at `previous`. A
/// change committed without a time is no pause: splitting on it would split
/// differently per peer.
fn within_pause(previous: i64, next: i64) -> bool {
    previous <= 0 || next <= 0 || next.saturating_sub(previous) <= TYPING_PAUSE_MS
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
