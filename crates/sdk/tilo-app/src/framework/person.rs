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
    /// The open records' undo and redo, as the palette and agents reach
    /// them.
    published: RefCell<Vec<Published>>,
}

/// One open record as the root publishes it.
struct Published {
    kind: &'static str,
    label: &'static str,
    /// Take back (`true`) or redo (`false`) one of the person's steps.
    /// Whether there was one.
    step: Box<dyn Fn(bool) -> bool>,
}

/// The operation type of a person record's undo and redo.
pub(crate) const HISTORY_TYPE: &str = "tilo.app.person-history";

/// The most steps one undo or redo of a person record takes.
const MAX_STEPS: i64 = 50;

/// What a person record's undo or redo was asked.
#[derive(serde::Serialize, serde::Deserialize)]
struct HistoryRequest {
    kind: String,
    undo: bool,
    steps: Option<i64>,
}

/// The address a person record of `kind` is published under.
#[must_use]
pub(crate) fn resource_id(kind: &str) -> String {
    format!("person:{kind}")
}

fn history_spec(kind: &'static str, label: &'static str, undo: bool) -> tilo_actions::ActionSpec {
    let mut spec = history_operation(kind, label, undo);
    spec.contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({
            "type": "object",
            "properties": { "steps": { "type": "integer" } },
        }),
    };
    spec
}

fn history_operation(
    kind: &'static str,
    label: &'static str,
    undo: bool,
) -> tilo_actions::ActionSpec {
    let (id, verb) = if undo {
        ("undo", "Undo")
    } else {
        ("redo", "Redo")
    };
    tilo_actions::ActionSpec::typed_operation(
        id,
        verb,
        verb,
        tilo_actions::OperationKind::Effect,
        HISTORY_TYPE,
        vec![
            tilo_actions::ActionParam::optional(
                "steps",
                tilo_actions::ParamType::Number,
                "How many of your steps (default 1, at most 50)",
            )
            .nullable(),
        ],
        move |args| {
            tilo_actions::EncodedAction::encode_operation(
                HISTORY_TYPE,
                &HistoryRequest {
                    kind: kind.to_owned(),
                    undo,
                    steps: args.integer("steps"),
                },
            )
            .unwrap_or_else(|_| tilo_actions::EncodedAction {
                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
                payload: Vec::new(),
            })
        },
    )
    .requiring(tilo_actions::PermissionRequirement::Read)
    .with_description(if undo {
        format!(
            "Take back your last change to your {label}: it is yours alone, on all of your devices."
        )
    } else {
        format!("Make again the change to your {label} you last took back.")
    })
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
            published: RefCell::default(),
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
        let weak = Rc::downgrade(&shared);
        self.published.borrow_mut().push(Published {
            kind: P::KIND,
            label: P::LABEL,
            step: Box::new(move |undo| {
                weak.upgrade().is_some_and(|shared| {
                    let record = PersonRecord { shared };
                    if undo { record.undo() } else { record.redo() }
                })
            }),
        });
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

    /// The open records as resources of their own: the person's undo and
    /// redo of each, which the palette and agents reach like the app's own
    /// history.
    pub(crate) fn resources(&self) -> Vec<tilo_actions::Resource> {
        self.published
            .borrow()
            .iter()
            .map(|published| {
                tilo_actions::Resource::new(
                    resource_id(published.kind),
                    published.kind,
                    format!("Your {}", published.label.to_lowercase()),
                )
                .with_action(history_spec(published.kind, published.label, true))
                .with_action(history_spec(published.kind, published.label, false))
            })
            .collect()
    }

    /// Answer an undo or redo of an open record: how many steps it took.
    pub(crate) fn answer(&self, payload: &[u8]) -> Result<tilo_actions::ActionOutput, String> {
        let request: HistoryRequest =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let steps = request.steps.unwrap_or(1);
        if !(1..=MAX_STEPS).contains(&steps) {
            return Err(format!("steps is 1 to {MAX_STEPS}"));
        }
        let published = self.published.borrow();
        let record = published
            .iter()
            .find(|published| published.kind == request.kind)
            .ok_or_else(|| format!("no record of yours is open as {}", request.kind))?;
        let mut taken = 0;
        while taken < steps && (record.step)(request.undo) {
            taken = taken.saturating_add(1);
        }
        if taken == 0 {
            return Err(if request.undo {
                "nothing of yours to undo".to_owned()
            } else {
                "nothing of yours to redo".to_owned()
            });
        }
        Ok(tilo_actions::ActionOutput {
            value: Some(serde_json::json!({ "steps": taken })),
            resources: vec![resource_id(record.kind)],
        })
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
