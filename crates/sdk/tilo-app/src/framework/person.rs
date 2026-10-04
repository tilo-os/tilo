//! The person's own records, as views and surfaces reach them: a resource
//! kept in the person's own room, beside the record the app shows.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use tilo_actions::{Action as _, ActionContext, ActionError, ActionInvoker, ChangeKind};
use tilo_resource::{Invoke, Resource};
use ui_core::LocalState;
use ui_core::automerge::ChangeHash;
use ui_core::history_mode::HistoryOwner;
use ui_core::record::RecordKind;
use ui_core::record::person::{PersonRecord as StoredRecord, PersonRecords};

/// A resource of the person's own, kept in their own room: what they
/// watched, the words they taught the keyboard, where they stopped
/// reading. It follows them to their other devices and never reaches a
/// collaborator, whatever room the app is open in.
///
/// It is a resource like the app's own: its edits are its actions, applied
/// by the replayable executor, attributed to the person, written into its
/// history and undone per step ([`Self::undo`]). One record per resource
/// kind and app, the same on every device, created with the resource's
/// genesis ([`Resource::initialize`]) where it does not exist yet. Read it
/// through [`Self::state`] or [`Self::with`]; an edit from another window or
/// device reaches it on the next frame.
pub struct PersonRecord<P: Resource> {
    shared: Rc<Shared<P>>,
}

impl<P: Resource> Clone for PersonRecord<P> {
    fn clone(&self) -> Self {
        Self {
            shared: Rc::clone(&self.shared),
        }
    }
}

struct Shared<P: Resource> {
    record: StoredRecord,
    app_id: &'static str,
    app_title: &'static str,
    me: String,
    /// The heads the state was read at.
    heads: RefCell<Vec<ChangeHash>>,
    state: LocalState<Rc<P::State>>,
    revision: Cell<u64>,
}

impl<P: Resource> Shared<P> {
    /// Read the state again when the record moved. Whether it did.
    fn refresh(&self) -> bool {
        let heads = self.record.heads();
        if *self.heads.borrow() == heads {
            return false;
        }
        let Some(Ok(state)) = self.record.read(P::read) else {
            return false;
        };
        *self.heads.borrow_mut() = heads;
        self.state.set(Rc::new(state));
        self.revision.set(self.revision.get().wrapping_add(1));
        true
    }
}

impl<P: Resource> PersonRecord<P> {
    /// The current state.
    #[must_use]
    pub fn get(&self) -> Rc<P::State> {
        self.shared.refresh();
        self.shared.state.with(Rc::clone)
    }

    /// Read the current state.
    pub fn with<T>(&self, read: impl FnOnce(&P::State) -> T) -> T {
        read(&self.get())
    }

    /// The state as view state that follows every change, for a control
    /// bound to it.
    #[must_use]
    pub fn state(&self) -> LocalState<Rc<P::State>> {
        self.shared.state.clone()
    }

    /// Increases whenever the state changes.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.shared.refresh();
        self.shared.revision.get()
    }

    /// Apply `action` now: one change of the record, attributed to the
    /// person, recorded in its history and — for content the person made —
    /// one undo step of theirs.
    ///
    /// # Errors
    /// The action refused to apply, or the record is unavailable.
    pub fn apply(&self, action: &P::Action) -> Result<(), ActionError> {
        let shared = &self.shared;
        let encoded = tilo_resource::encode::<P>(action)?;
        let label = tilo_resource::action_label::<P>(action)
            .unwrap_or_else(|| ActionContext::for_action("", "", action).action_label);
        let change = action.change_kind();
        let context = ActionContext::for_resource(label, P::KIND, P::KIND, P::LABEL)
            .with_app(shared.app_id, shared.app_title)
            .with_invoker(ActionInvoker::actor(&shared.me))
            .with_change(change)
            .with_args_from_payload(&encoded.payload);
        shared
            .record
            .edit(change == ChangeKind::Content, |doc| {
                let writer =
                    ui_core::history_source::authors::device_actor(&shared.me, doc.get_actor());
                tilo_actions::apply_local_with_context(doc, action, encoded, Some(&writer), context)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
            .map_err(ActionError::Refused)?;
        shared.refresh();
        Ok(())
    }

    /// Apply the action `invoke` names, as [`Self::apply`].
    ///
    /// # Errors
    /// The arguments do not build the action, or it refused to apply.
    pub fn invoke(&self, invoke: &Invoke<P>) -> Result<(), ActionError> {
        let action = invoke
            .build(tilo_resource::origin_now_ms())
            .map_err(ActionError::Refused)?;
        self.apply(&action)
    }

    /// Undo the person's last edit of the record. Whether there was one.
    pub fn undo(&self) -> bool {
        let undone = self.shared.record.undo();
        self.shared.refresh();
        undone
    }

    /// Redo the person's last undone edit of the record.
    pub fn redo(&self) -> bool {
        let redone = self.shared.record.redo();
        self.shared.refresh();
        redone
    }

    /// Whether the record reaches the person's other devices: they have a
    /// root identity, so it lives in their room. Without one it is kept on
    /// this device only.
    #[must_use]
    pub fn syncs(&self) -> bool {
        self.shared.record.syncs()
    }

    /// The record underneath, for a test or host that carries it between
    /// devices itself.
    #[must_use]
    pub fn record(&self) -> &StoredRecord {
        &self.shared.record
    }
}

/// The person's records a root's views opened, shared by every scope of the
/// root: one [`PersonRecord`] per resource kind.
pub(crate) struct PersonState {
    records: PersonRecords,
    app_id: &'static str,
    app_title: &'static str,
    me: String,
    opened: RefCell<HashMap<&'static str, Rc<dyn Any>>>,
    owners: RefCell<Vec<HistoryOwner>>,
    refreshers: RefCell<Vec<Box<dyn Fn() -> bool>>>,
}

impl PersonState {
    pub(crate) fn new(
        records: PersonRecords,
        app_id: &'static str,
        app_title: &'static str,
    ) -> Self {
        let me = records.author().0.clone();
        Self {
            records,
            app_id,
            app_title,
            me,
            opened: RefCell::default(),
            owners: RefCell::default(),
            refreshers: RefCell::default(),
        }
    }

    /// The person's record of `P`, opened on first use.
    pub(crate) fn open<P: Resource>(&self) -> Option<PersonRecord<P>> {
        if let Some(open) = self.opened.borrow().get(P::KIND) {
            return open
                .clone()
                .downcast::<Shared<P>>()
                .ok()
                .map(|shared| PersonRecord { shared });
        }
        let record = self
            .records
            .open_record(RecordKind::new(P::KIND), |doc| {
                P::initialize(doc).map_err(|error| error.to_string())
            })
            .map_err(|error| tracing::warn!(kind = P::KIND, %error, "person record unavailable"))
            .ok()?;
        let state = record.read(P::read)?.ok()?;
        let heads = record.heads();
        self.owners.borrow_mut().push(record.history_owner());
        let shared = Rc::new(Shared::<P> {
            record,
            app_id: self.app_id,
            app_title: self.app_title,
            me: self.me.clone(),
            heads: RefCell::new(heads),
            state: LocalState::new(Rc::new(state)),
            revision: Cell::new(0),
        });
        let weak = Rc::downgrade(&shared);
        self.refreshers.borrow_mut().push(Box::new(move || {
            weak.upgrade().is_some_and(|shared| shared.refresh())
        }));
        self.opened
            .borrow_mut()
            .insert(P::KIND, Rc::clone(&shared) as Rc<dyn Any>);
        Some(PersonRecord { shared })
    }

    /// Read every open record that moved. Whether one did.
    pub(crate) fn refresh(&self) -> bool {
        self.refreshers
            .borrow()
            .iter()
            .fold(false, |moved, refresh| refresh() | moved)
    }

    /// The routes of the open records, which the runtime syncs.
    pub(crate) fn owners(&self) -> Vec<HistoryOwner> {
        self.owners.borrow().clone()
    }
}

impl Drop for PersonState {
    /// The window closes: what the person did is stored before it goes.
    fn drop(&mut self) {
        if let Err(error) = self.records.flush() {
            tracing::warn!(%error, "the person's records could not be stored");
        }
    }
}
