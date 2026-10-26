//! The person's own records, as views and surfaces reach them: a resource
//! kept in the person's own room, beside the record the app shows.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_actions::{Action as _, ActionContext, ActionError, ActionInvoker, ChangeKind};

use super::live::Live;
use super::record::{Held, RecordDoor};
use super::section::{Opener, SectionDoor, SectionKind, record_kind};
use tilo_resource::{Invoke, Resource};
use ui_core::LocalState;
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
    /// The app that keeps the record, when this app does not change it:
    /// it is read here and its edits are refused.
    owner: Option<&'static str>,
    app_id: &'static str,
    app_title: &'static str,
    me: String,
    /// The state, followed change by change: what views of the record read.
    live: Live<P>,
    /// The same state, for controls bound to it.
    state: LocalState<Rc<P::State>>,
    /// The record as views and the door edit it.
    held: RefCell<Option<Rc<Held<P>>>>,
}

impl<P: Resource> Shared<P> {
    /// Follow the record when it moved, reading only what changed. Whether
    /// it did.
    fn refresh(&self) -> bool {
        let moved = match self.record.read(|doc| self.live.sync(doc)) {
            Some(Ok(moved)) => moved,
            Some(Err(error)) => {
                unreadable_person_record(P::KIND, &error);
                false
            }
            None => false,
        };
        if !moved
            && self.live.unreadable().is_none()
            && let Some(Err(error)) = self.record.read(|doc| self.live.reread(doc))
        {
            // The record is small and read whole, so a change it cannot
            // read is noticed here rather than by whoever publishes it.
            unreadable_person_record(P::KIND, &error);
        }
        // Views of the record follow it whole: it is small.
        let _ = self.live.take_change();
        if moved {
            self.state.set(self.live.get());
        }
        moved
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

    /// Why the record stopped reading as `P`, if it did: the state is then
    /// the last one that read, and a view shows this instead of it. Its
    /// published actions say the same.
    #[must_use]
    pub fn unreadable(&self) -> Option<String> {
        self.shared.refresh();
        self.shared.live.unreadable().map(|why| why.reason)
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
        self.shared.live.revision()
    }

    /// Apply `action` now: one change of the record, attributed to the
    /// person, recorded in its history and — for content the person made —
    /// one undo step of theirs.
    pub fn apply(&self, action: &P::Action) -> Result<(), ActionError> {
        self.apply_as(action, &ActionInvoker::actor(&self.shared.me))
    }

    /// [`Self::apply`] by `invoker`: the person, or software acting for
    /// them.
    pub(crate) fn apply_as(
        &self,
        action: &P::Action,
        invoker: &ActionInvoker,
    ) -> Result<(), ActionError> {
        let shared = &self.shared;
        if let Some(owner) = shared.owner {
            return Err(ActionError::Refused(format!(
                "the person's {} are edited in {owner}",
                P::LABEL.to_lowercase()
            )));
        }
        let encoded = tilo_resource::encode::<P>(action)?;
        let change = action.change_kind();
        let context = ActionContext::for_resource(P::KIND, P::KIND, P::LABEL)
            .with_app(shared.app_id, shared.app_title)
            .with_invoker(invoker.clone())
            .with_change(change);
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

    /// Whether this app changes the record: its own, or a shared one it is
    /// an editor of; any other is read here.
    #[must_use]
    pub fn editable(&self) -> bool {
        self.shared.owner.is_none()
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

    /// Where the content this record references — a picture its state
    /// names by hash — is stored and read. It lives with the record, on the
    /// person's devices, never in the room of the window showing it; `None`
    /// on a device that knows no person, whose records keep no content.
    #[must_use]
    pub fn content(&self) -> Option<tilo_content::ContentStore> {
        self.shared.record.content()
    }

    /// The record underneath, for a test or host that carries it between
    /// devices itself.
    #[must_use]
    pub fn record(&self) -> &StoredRecord {
        &self.shared.record
    }
}

/// An open record: the app that owns it, and its kind.
type OpenKey = (&'static str, String);

/// The person's records a root's views opened, shared by every scope of the
/// root: one [`PersonRecord`] per resource kind.
pub(crate) struct PersonState {
    records: PersonRecords,
    app_id: &'static str,
    app_title: &'static str,
    me: String,
    /// The open records, by the app that owns them and their kind.
    opened: RefCell<HashMap<OpenKey, Rc<dyn Any>>>,
    /// The records of the person's other apps keep, held here too.
    others: RefCell<HashMap<&'static str, PersonRecords>>,
    owners: Rc<RefCell<Vec<HistoryOwner>>>,
    refreshers: RefCell<Vec<Box<dyn Fn() -> bool>>>,
    /// The open records' undo and redo, as the palette and agents reach
    /// them.
    published: RefCell<Vec<Published>>,
    /// The records that could not be opened as views asked for them, by
    /// what they are, with what they publish: their declared actions,
    /// unavailable for why. Those views are told now, and never again for
    /// the same record.
    unavailable: RefCell<HashMap<(String, &'static str), tilo_actions::Resource>>,
}

/// One open record as the root publishes it.
struct Published {
    id: String,
    label: String,
    /// Its field edits and verbs, as the door reaches them.
    door: Rc<dyn RecordDoor>,
    /// Take back (`true`) or redo (`false`) one of the person's steps, for
    /// the invoker named. Whether there was one.
    step: Step,
}

/// Take back (`true`) or redo (`false`) one step for the invoker named.
type Step = Box<dyn Fn(bool, &str) -> bool>;

/// A person record just opened as its type: its shared state, its door and
/// its undo and redo.
struct Opened {
    shared: Rc<dyn Any>,
    door: Rc<dyn RecordDoor>,
    step: Step,
}

/// The operation type of a person record's undo and redo.
pub(crate) const HISTORY_TYPE: &str = "tilo.app.person-history";

/// The most steps one undo or redo of a person record takes.
const MAX_STEPS: i64 = 50;

/// What a person record's undo or redo was asked.
#[derive(serde::Serialize, serde::Deserialize)]
struct HistoryRequest {
    record: String,
    undo: bool,
    steps: Option<i64>,
}

/// The address a person record of `kind` is published under.
#[must_use]
pub(crate) fn resource_id(kind: &str) -> String {
    format!("person:{kind}")
}

fn history_spec(record: &str, label: &str, undo: bool) -> tilo_actions::ActionSpec {
    let mut spec = history_operation(record.to_owned(), label, undo);
    spec.contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({
            "type": "object",
            "properties": { "steps": { "type": "integer" } },
        }),
    };
    spec
}

fn history_operation(record: String, label: &str, undo: bool) -> tilo_actions::ActionSpec {
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
                    record: record.clone(),
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
    .of_kind(tilo_actions::ActionKind::Read)
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
            others: RefCell::default(),
            owners: Rc::default(),
            refreshers: RefCell::default(),
            published: RefCell::default(),
            unavailable: RefCell::default(),
        }
    }

    /// The person's record of `P`, opened on first use.
    pub(crate) fn open<P: Resource>(&self) -> Option<PersonRecord<P>> {
        self.open_of::<P>(self.app_id, true, true)
    }

    /// The shared record `P`, opened on first use: kept by its owner, a copy
    /// of it here in every other app, changed where `P` says.
    pub(crate) fn open_shared<P: tilo_resource::SharedRecord>(&self) -> Option<PersonRecord<P>> {
        self.open_of::<P>(P::OWNER, P::edited_by(self.app_id), true)
    }

    /// The shared record `P`, opened on first use as [`Self::open_shared`]
    /// but never published: a host holds it for its own edits, and the app
    /// whose window shows it publishes its actions.
    pub(crate) fn hold_shared<P: tilo_resource::SharedRecord>(&self) -> Option<PersonRecord<P>> {
        self.open_of::<P>(P::OWNER, P::edited_by(self.app_id), false)
    }

    /// The person's record of `P` that the app `owner` keeps, opened on
    /// first use, whether this app changes it, and whether it is published
    /// when it is this app's own.
    fn open_of<P: Resource>(
        &self,
        owner: &'static str,
        edits: bool,
        publish: bool,
    ) -> Option<PersonRecord<P>> {
        let open = |record: StoredRecord| -> Result<Opened, String> {
            let shared = self.shared_of::<P>(record, (!edits).then_some(owner))?;
            let held = Rc::new(held::<P>(&shared, edits));
            *shared.held.borrow_mut() = Some(Rc::clone(&held));
            let weak = Rc::downgrade(&shared);
            Ok(Opened {
                shared: shared as Rc<dyn Any>,
                door: held,
                step: Box::new(move |undo, _| {
                    weak.upgrade().is_some_and(|shared| {
                        let record = PersonRecord { shared };
                        if undo { record.undo() } else { record.redo() }
                    })
                }),
            })
        };
        let shared = self.open_kind::<P>(
            owner,
            &|doc| P::initialize(doc).map_err(|error| error.to_string()),
            &open,
            publish,
        )?;
        shared
            .downcast::<Shared<P>>()
            .ok()
            .map(|shared| PersonRecord { shared })
    }

    /// The open record of `kind` that the app `owner` keeps, or the one
    /// `open` makes of it, created by `initialize` where it does not exist
    /// yet, kept and (for this app's own, when `publish`) published: the
    /// part of opening a person record that does not depend on its type.
    fn open_kind<P: Resource>(
        &self,
        owner: &'static str,
        initialize: &dyn Fn(&mut ui_core::automerge::AutoCommit) -> Result<(), String>,
        open: &dyn Fn(StoredRecord) -> Result<Opened, String>,
        publish: bool,
    ) -> Option<Rc<dyn Any>> {
        let (kind, label) = (P::KIND, P::LABEL);
        let key = (owner, kind.to_owned());
        if let Some(open) = self.opened.borrow().get(&key) {
            return Some(Rc::clone(open));
        }
        let records = self.records_of(owner)?;
        let record = match open_record(&records, kind.to_owned(), kind, initialize) {
            Ok(record) => record,
            Err(reason) => return self.note_unavailable::<P>(owner, &reason, publish),
        };
        let Opened { shared, door, step } = match open(record) {
            Ok(opened) => opened,
            Err(reason) => return self.note_unavailable::<P>(owner, &reason, publish),
        };
        if owner != self.app_id || !publish {
            self.opened.borrow_mut().insert(key, Rc::clone(&shared));
            return Some(shared);
        }
        self.publish(
            key,
            Rc::clone(&shared),
            Published {
                id: resource_id(kind),
                label: label.to_owned(),
                door,
                step,
            },
        );
        Some(shared)
    }

    /// `record` read as `P`, followed from now on.
    ///
    /// # Errors
    /// Why the record does not read as `P`: it could not be migrated to the
    /// storage version the app reads, or what it holds is not a `P`.
    fn shared_of<P: Resource>(
        &self,
        record: StoredRecord,
        owner: Option<&'static str>,
    ) -> Result<Rc<Shared<P>>, String> {
        // An older record is brought up to the resource's storage version
        // before it is read, as the app's record is when it opens: once, as
        // the same housekeeping change on every device, nobody's undo step.
        let stored = record
            .read(tilo_resource::evolution::stored_version)
            .ok_or_else(|| "the record cannot be read".to_owned())?;
        if stored < P::VERSION
            && let Err(error) = record.edit(false, |doc| {
                tilo_resource::evolution::migrate::<P>(doc).map_err(|error| error.to_string())
            })
        {
            return Err(format!(
                "it could not be brought up to this app's storage version: {error}"
            ));
        }
        let reader = record
            .read(tilo_resource::Reader::<P>::read)
            .ok_or_else(|| "the record cannot be read".to_owned())?
            .map_err(|error| format!("it does not read: {error}"))?;
        let live = Live::new(reader);
        self.owners.borrow_mut().push(record.history_owner());
        let shared = Rc::new(Shared::<P> {
            record,
            owner,
            app_id: self.app_id,
            app_title: self.app_title,
            me: self.me.clone(),
            state: LocalState::new(live.get()),
            live,
            held: RefCell::new(None),
        });
        let weak = Rc::downgrade(&shared);
        self.refreshers.borrow_mut().push(Box::new(move || {
            weak.upgrade().is_some_and(|shared| shared.refresh())
        }));
        Ok(shared)
    }

    /// The settings section `kind` held as a record of the person's, opened
    /// on first use: its edits are their steps, undone and redone like any
    /// of theirs, and each writes the section's value to the settings
    /// document with what made it and who. A device-local section has a
    /// record per device, as its values do.
    pub(crate) fn open_section(
        &self,
        kind: Rc<SectionKind>,
        device: Option<&str>,
    ) -> Option<Rc<SectionDoor>> {
        let name = record_kind(kind.kind, kind.device_local, device);
        let key = (self.app_id, name.clone());
        if let Some(open) = self.opened.borrow().get(&key) {
            return Rc::clone(open).downcast::<SectionDoor>().ok();
        }
        let label = kind.kind;
        let door = SectionDoor::person(
            kind,
            Opener {
                records: self.records.clone(),
                name,
                owners: Rc::clone(&self.owners),
            },
            self.app_id,
            self.app_title,
            self.me.clone(),
        )
        .map_err(|error| tracing::warn!(kind = label, %error, "settings section unavailable"))
        .ok()?;
        let door = Rc::new(door);
        let stepping = Rc::downgrade(&door);
        self.publish(
            key,
            Rc::clone(&door) as Rc<dyn Any>,
            Published {
                id: door.id().to_owned(),
                label: door.label().to_owned(),
                door: Rc::clone(&door) as Rc<dyn RecordDoor>,
                step: Box::new(move |undo, by| {
                    stepping.upgrade().is_some_and(|door| door.step(undo, by))
                }),
            },
        );
        Some(door)
    }

    /// The person's records `owner` keeps: this app's own, or another app's,
    /// opened once.
    fn records_of(&self, owner: &'static str) -> Option<PersonRecords> {
        if owner == self.app_id {
            return Some(self.records.clone());
        }
        let mut others = self.others.borrow_mut();
        if let Some(records) = others.get(owner) {
            return Some(records.clone());
        }
        let records = self
            .records
            .of_package(owner)
            .map_err(|error| {
                tracing::warn!(owner, %error, "the person's records of another app are unavailable");
            })
            .ok()?;
        others.insert(owner, records.clone());
        Some(records)
    }

    /// Note that the record `P` that `owner` keeps could not be opened, for
    /// `reason`: the root publishes it with its declared actions
    /// unavailable for that reason, rather than the record going quietly
    /// missing from the surface.
    fn note_unavailable<P: Resource>(
        &self,
        owner: &'static str,
        reason: &str,
        publish: bool,
    ) -> Option<Rc<dyn Any>> {
        unreadable_person_record(P::KIND, &reason);
        // Only the record this app would have published is published for
        // the reason: one another app keeps and this one reads is that
        // app's surface, which registers the actions its own record has.
        if owner == self.app_id && publish {
            let published = tilo_resource::resource::unreadable_parts::<P>(
                &resource_id(P::KIND),
                P::LABEL,
                reason,
            );
            self.unavailable
                .borrow_mut()
                .insert((owner.to_owned(), P::KIND), published);
        }
        None
    }

    /// Every record a view asked for that could not be opened, as it is
    /// published: its declared actions, unavailable for why.
    pub(crate) fn unavailable(&self) -> Vec<tilo_actions::Resource> {
        self.unavailable.borrow().values().cloned().collect()
    }

    /// Keep the record opened under `key`, publishing its door and its undo
    /// and redo.
    fn publish(&self, key: OpenKey, open: Rc<dyn Any>, published: Published) {
        self.published.borrow_mut().push(published);
        self.opened.borrow_mut().insert(key, open);
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
                published
                    .door
                    .publish()
                    .with_action(history_spec(&published.id, &published.label, true))
                    .with_action(history_spec(&published.id, &published.label, false))
            })
            .collect()
    }

    /// The open records of the person's own, as the door changes them.
    pub(crate) fn doors(&self) -> Vec<Rc<dyn RecordDoor>> {
        self.published
            .borrow()
            .iter()
            .map(|published| Rc::clone(&published.door))
            .collect()
    }

    pub(crate) fn me(&self) -> &str {
        &self.me
    }

    /// Answer an undo or redo of an open record: how many steps it took.
    pub(crate) fn answer(&self, payload: &[u8]) -> Result<tilo_actions::ActionOutput, String> {
        let invoker = ui_core::invocation_scope::current_invoker()
            .unwrap_or_else(|| ActionInvoker::actor(&self.me));
        self.answer_for(payload, &invoker)
    }

    /// [`Self::answer`] for `invoker`.
    pub(crate) fn answer_for(
        &self,
        payload: &[u8],
        invoker: &ActionInvoker,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let request: HistoryRequest =
            tilo_actions::decode_json(payload).map_err(|error| error.to_string())?;
        let steps = request.steps.unwrap_or(1);
        if !(1..=MAX_STEPS).contains(&steps) {
            return Err(format!("steps is 1 to {MAX_STEPS}"));
        }
        if invoker.actor != self.me && invoker.on_behalf_of.as_deref() != Some(self.me.as_str()) {
            return Err(format!(
                "`{}` is {}'s own; only they change it",
                request.record, self.me
            ));
        }
        let published = self.published.borrow();
        let record = published
            .iter()
            .find(|published| published.id == request.record)
            .ok_or_else(|| format!("no record of yours is open as {}", request.record))?;
        let mut taken = 0;
        while taken < steps && (record.step)(request.undo, &invoker.actor) {
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
            resources: vec![record.id.clone()],
        })
    }

    /// The routes of the open records, which the runtime syncs.
    pub(crate) fn owners(&self) -> Vec<HistoryOwner> {
        self.owners.borrow().clone()
    }
}

/// The record `kind` that `records` keeps, made by `initialize` where it
/// does not exist yet.
///
/// # Errors
/// Why the record cannot be opened: the store refused it, or it cannot be
/// created.
fn open_record(
    records: &PersonRecords,
    kind: String,
    of: &str,
    initialize: &dyn Fn(&mut ui_core::automerge::AutoCommit) -> Result<(), String>,
) -> Result<StoredRecord, String> {
    records
        .open_record(RecordKind::new(kind), initialize)
        .map_err(|error| format!("{of}'s record cannot be opened: {error}"))
}

/// The record `shared` as views and the door edit it: read into a state of
/// its own that follows the record, changed as the person.
fn held<P: Resource>(shared: &Rc<Shared<P>>, edits: bool) -> Held<P> {
    let following = Rc::downgrade(shared);
    let applying = Rc::downgrade(shared);
    Held::new(
        resource_id(P::KIND),
        format!("Your {}", P::LABEL.to_lowercase()),
        edits,
        shared.live.clone(),
        move |_: &Live<P>| {
            if let Some(shared) = following.upgrade() {
                shared.refresh();
            }
        },
        move |action: &P::Action, invoker: &ActionInvoker| {
            let shared = applying
                .upgrade()
                .ok_or_else(|| ActionError::Refused("the record is closed".into()))?;
            PersonRecord { shared }.apply_as(action, invoker)
        },
    )
}

impl<P: Resource> super::record::sealed::Hold<P> for PersonRecord<P> {
    fn held(&self, _storage: super::record::sealed::Storage<'_>) -> Option<Rc<Held<P>>> {
        self.shared.held.borrow().clone()
    }

    fn door(
        &self,
        _storage: super::record::sealed::Storage<'_>,
    ) -> Option<super::record::sealed::Door> {
        self.shared
            .held
            .borrow()
            .clone()
            .map(|held| super::record::sealed::Door(held as Rc<dyn RecordDoor>))
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

/// Log that the person's record of `kind` does not read: one event site in
/// the runtime rather than one in every app for each record type.
#[cold]
#[inline(never)]
fn unreadable_person_record(kind: &'static str, error: &dyn std::fmt::Display) {
    tracing::warn!(kind, %error, "person record unreadable");
}
