//! **The change a person is making**: the typing burst a store holds open.
//!
//! A bound control hands over the whole of what it holds on every keystroke, so typing into one
//! field is one write per letter. Each is in the mirror and the overlay as it is made, and told
//! to whoever reads it ([`super::unsaved`]), so the person at the window, every other view, and
//! every peer sees the letter at once. What waits is the *change*: [`Inner::hold`] keeps the
//! keystrokes of one field open, and [`Inner::land_open`] commits them as one change — one
//! action, one history entry, one undo step — when the burst ends. Its boundaries:
//!
//! * another field, another resource or another person: the change it is not part of lands
//!   first ([`super::TiloStore::commit`]);
//! * anything else the person does: a press, a command, an undo, the field losing focus
//!   ([`crate::instance::Instance`] lands it there);
//! * a pause in the typing ([`TiloStore::edit_wakes_in`], which the window waits on);
//! * the window closing ([`TiloStore::close_edit`], which the host calls before it ends the
//!   instance).
//!
//! Nothing typed is ever only held: the writes are the mirror's open transaction, which every
//! read answers from, and a record that moves on while the burst is open leaves them where they
//! are until the change that carries them comes back ([`Inner::reapply_kept`]). A trial run
//! ([`Inner::trials`]) neither lands nor joins it: planning an action writes the mirror and is
//! taken back, and what a person is typing is no part of that.

use std::time::{Duration, Instant};

use tilo_resource::EditOp;
use tilolang_runtime::ids::{AppId, DocId};
use tilolang_runtime::store::ChangeNote;

use super::{Authorship, DynAction, DynEdit, Inner, TxnState, published_name};

/// What the change held open edits: one field of one resource, by one person, in one app. A
/// keystroke that is not of it is a change of its own.
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

/// One keystroke of the change held open.
struct Typed {
    /// The transaction it committed, as the store wrote it: what the change takes.
    state: TxnState,
    /// The note of its commit: the change's own is the last of them.
    note: ChangeNote,
    /// The documents the store told it reached.
    docs: Vec<DocId>,
    /// How many writes the store held when it was made: the last of them is the change's, and
    /// the versions the change makes are kept with it.
    writes: usize,
}

/// The change a person is making, held open until it lands.
pub(super) struct Open {
    /// What its next keystroke continues.
    what: Typing,
    /// The keystrokes so far, in the order they were made, oldest first.
    typed: Vec<Typed>,
    /// When the last of them was made: the pause that lands the change runs from here.
    at: Instant,
}

impl Inner {
    /// Whether `state`, committed with `note`, is the next keystroke of the change held open:
    /// the same field of the same resource, typed by the same person in the same app.
    pub(super) fn opens_typing(&self, state: &TxnState, note: &ChangeNote) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| Typing::of(state, note) == Some(open.what.clone()))
    }

    /// Whether `state`, committed with `note`, may be held open at all: a keystroke of a bound
    /// text field.
    pub(super) fn types_text(state: &TxnState, note: &ChangeNote) -> bool {
        Typing::of(state, note).is_some()
    }

    /// Keep `state`, committed with `note`, as the change a person is making: joined to the one
    /// held open when it is a keystroke of that field, and the first keystroke of a new one
    /// otherwise. Its write stays in the mirror and the overlay, and `docs` were told what it
    /// reached.
    pub(super) fn hold(
        &mut self,
        state: TxnState,
        note: &ChangeNote,
        docs: Vec<DocId>,
        writes: usize,
    ) {
        let Some(what) = Typing::of(&state, note) else {
            return;
        };
        let typed = Typed {
            state,
            note: note.clone(),
            docs,
            writes,
        };
        let at = Instant::now();
        match self.open.as_mut() {
            Some(open) if open.what == what => {
                open.typed.push(typed);
                open.at = at;
            }
            _ => {
                self.open = Some(Open {
                    what,
                    typed: vec![typed],
                    at,
                });
            }
        }
    }

    /// Land the change a person is making as one change of the record, if one is held open: the
    /// edits of its keystrokes as one action, queued for the host to commit as any action is.
    pub(super) fn land_open(&mut self) {
        let Some(Open { what, typed, .. }) = self.open.take() else {
            return;
        };
        let mut edits: Vec<DynEdit> = Vec::new();
        let mut docs: Vec<DocId> = Vec::new();
        let mut kind = String::new();
        let mut note: Option<ChangeNote> = None;
        // Where its writes stand in the store's: the first keystroke's, to the last one's
        // ([`Inner::provisional_versions`] keeps the change's versions with the last of them).
        let mut start = usize::MAX;
        let mut end = 0;
        for Typed {
            state,
            note: typed_note,
            docs: reached,
            writes,
        } in typed
        {
            let TxnState {
                edits: wrote,
                start: began,
                kind: of,
                ..
            } = state;
            start = start.min(began);
            end = end.max(writes);
            if let Some(of) = of {
                kind = of;
            }
            edits.extend(wrote);
            for doc in reached {
                if !docs.contains(&doc) {
                    docs.push(doc);
                }
            }
            note = Some(typed_note);
        }
        let Some(note) = note else {
            return;
        };
        let (action_id, args) = published_name(&note);
        let action = DynAction {
            app: what.app.clone(),
            kind,
            action_id,
            args,
            authorship: Authorship {
                invoker: note.invoker.clone(),
                chain: note.chain.clone(),
                module: note.module.clone(),
                at: note.at.clone(),
                from: note.from.clone(),
                suggested_by: note.suggested_by.clone(),
                count: note.count.max(1),
                invocation: note.invocation.clone(),
            },
            edits,
        };
        let made = action.made();
        self.enqueue(action);
        for doc in self
            .provisional_versions(&what.app, start, end, &note, &made)
            .into_keys()
        {
            if !docs.contains(&doc) {
                docs.push(doc);
            }
        }
        for doc in docs {
            self.notes.entry(doc).or_default().push(note.clone());
        }
    }

    /// Whether a change is held open, and how long until the pause in its typing lands it.
    pub(super) fn open_wakes_in(&self) -> Option<Duration> {
        let open = self.open.as_ref()?;
        Some(self.edit_pause.saturating_sub(open.at.elapsed()))
    }
}
