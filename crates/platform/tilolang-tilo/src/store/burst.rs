//! **The burst of typing a person is in**: the keystrokes of one field that history and undo
//! take as one step.
//!
//! A bound control hands over the whole of what it holds on every keystroke, so typing into one
//! field is one change per letter. Each of them is a change of the record as any other — written
//! into the chunk that holds the resource, kept on disk with it, and sent on the route in the
//! frame that made it — so the person, every other view of it and every peer sees the letter at
//! once, and nothing typed is ever only in memory. What makes the burst read as one act is its
//! *gesture*, which every keystroke of it names: the workspace's history lists it as one entry,
//! and undo takes it back as one step, on whichever replica reads it
//! ([`tilolang_runtime::store::ChangeNote`] → the change's `ActionContext.gesture`).
//!
//! A keystroke continues the burst held ([`Inner::burst_gesture`]) when it is the next letter of
//! the same field of the same resource, typed by the same person in the same app, less than the
//! pause ([`TiloStore::set_edit_pause`]) after the one before it; anything else starts a burst of
//! its own. So a person who stops typing for a moment, turns to another field or another person,
//! or runs anything else, has made a step of what came before — and their next letter opens the
//! next one. A change that is no letter of a text field — a press, a command, an undo, a
//! collection's change — ends the burst too.
//!
//! A trial run ([`Inner::trials`]) neither joins nor ends a burst: planning an action writes the
//! mirror and is taken back, and what a person is typing is no part of that.

use std::time::Instant;

use tilo_actions::ActionId;
use tilo_resource::EditOp;
use tilolang_runtime::ids::AppId;
use tilolang_runtime::store::ChangeNote;

use super::{DynEdit, Inner, TxnState};

/// What a burst's keystrokes type into: one field of one resource, by one person, in one app.
/// A keystroke that is not of it is a change of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Typing {
    app: AppId,
    /// The resource the field is of, as a [`tilolang_runtime::value::ResourceRef`] writes it
    /// (`doc` or `doc/item`).
    resource: String,
    field: String,
    by: String,
}

impl Typing {
    /// What the transaction `state`, committed with `note`, types into: the field a bound
    /// control handed over a whole text as (a keystroke, a paste, a mark, a paragraph), and
    /// nothing else. `None` for anything else — an action, a toggle, a collection's change, a
    /// write that reaches outside the record.
    pub(super) fn of(state: &TxnState, note: &ChangeNote) -> Option<Self> {
        if note.action != super::RUNTIME_EDIT
            || !note.effects.is_empty()
            || note.count > 1
            || note.approved_by.is_some()
            || note.suggested_by.is_some()
            || state.saved == 0
            || state.derived > 0
            || state.edits.is_empty()
            || !state.edits.iter().all(writes_text)
        {
            return None;
        }
        let [resource] = note.touched.as_slice() else {
            return None;
        };
        let field = note.args.get(0)?.as_str()?;
        Some(Self {
            app: state.app.clone()?,
            resource: resource.clone(),
            field: field.to_owned(),
            by: note.invoker.clone(),
        })
    }
}

/// Whether the edit writes a text field whole: what a bound control hands over on each
/// keystroke, and what a typing burst is made of.
fn writes_text(edit: &DynEdit) -> bool {
    matches!(
        edit.bare(),
        DynEdit::Edit { edit, .. }
            if matches!(edit.op(), EditOp::Replace { .. } | EditOp::Rich { .. })
    )
}

/// The burst of typing the person is in: what its next keystroke continues, the gesture all of
/// its changes name, and when the last of them was made — the pause that ends it runs from there.
pub(super) struct Burst {
    what: Typing,
    gesture: ActionId,
    at: Instant,
}

impl Inner {
    /// The gesture the change `state`, committed with `note`, is a change of: the burst's own
    /// when it is the next letter of the field the person is typing into, and a fresh one
    /// otherwise. `None`, and the burst ended, for a change that is no letter of a text field.
    pub(super) fn burst_gesture(
        &mut self,
        state: &TxnState,
        note: &ChangeNote,
    ) -> Option<ActionId> {
        let Some(what) = Typing::of(state, note) else {
            self.burst = None;
            return None;
        };
        let gesture = match self.burst.take() {
            Some(burst) if burst.what == what && burst.at.elapsed() < self.edit_pause => {
                burst.gesture
            }
            _ => ActionId::new(),
        };
        self.burst = Some(Burst {
            what,
            gesture,
            at: Instant::now(),
        });
        Some(gesture)
    }
}
