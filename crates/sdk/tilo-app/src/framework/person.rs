//! The records a person keeps for themselves, in their own scope rather than a
//! workspace's: presentation data only they use on their own devices, such as
//! the words they taught the on-screen keyboard or the signature they sign
//! documents with. A record the workspace keeps, one for every member, is
//! opened as the workspace's ([`super::WorkspaceResource`]).

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_actions::{ActionContext, ActionError, ActionInvoker, ChangeKind};

use super::live::Live;
use super::resources::{Held, ResourceApi};
use super::section::{SectionApi, SectionKind};
use super::workspace::Pending;
use tilo_resource::{Invoke, Resource};
use ui_core::LocalState;
use ui_core::history_mode::HistoryOwner;
use ui_core::resource::ResourceType;
use ui_core::resource::StoredResource;
use ui_core::resource::person::PersonResources;

/// A record the person keeps for themselves, in their own scope rather than a
/// workspace's: the words taught to the keyboard, the signature they sign with.
/// The same on each of the person's devices, carried by their personal store,
/// and no other member's.
///
/// It is a resource like the app's own: its edits are its actions, applied
/// by the replayable executor, attributed to the person, written into its
/// history and undone per step ([`Self::undo`]). It is created with the
/// resource's genesis ([`Resource::initialize`]) where it does not exist
/// yet. Read it through [`Self::state`] or [`Self::with`]; an edit from
/// another window or device reaches it on the next frame.
pub struct PersonResource<P: Resource> {
    shared: Rc<Shared<P>>,
}

impl<P: Resource> Clone for PersonResource<P> {
    fn clone(&self) -> Self {
        Self {
            shared: Rc::clone(&self.shared),
        }
    }
}

/// What a person's record is whatever its type: where it is kept, whose it is and the edits
/// that wait for its store. The typed record ([`PersonResource`]) hands its work here, so each
/// record type an app opens compiles only what reads its own state.
struct Core {
    record: StoredResource,
    app_id: &'static str,
    app_title: &'static str,
    me: tilo_resource::MemberId,
    /// The record's type, as its changes name it.
    kind: &'static str,
    label: &'static str,
    /// Edits asked for while the record's store was in use, applied in order
    /// once it is free.
    waiting: RefCell<std::collections::VecDeque<(Pending, ActionInvoker)>>,
    /// What the state shown was read with while edits waited: the record's
    /// heads and how many waited. The state then holds them, so what is
    /// read after an edit holds it although the store has not taken it yet.
    ahead: RefCell<Option<(Vec<ui_core::automerge::ChangeHash>, usize)>>,
    /// The window the record is opened for: woken while edits wait for the
    /// store, so a frame's settle makes them and its next pass shows them,
    /// rather than whichever pass comes next.
    wake: Option<ui_core::wake::WakeHandle>,
}

/// One typed record's [`Core`] and its state, followed change by change.
struct Shared<P: Resource> {
    core: Core,
    /// The state, followed change by change: what views of the record read.
    live: Live<P>,
    /// The same state, for controls bound to it.
    state: LocalState<Rc<P::State>>,
    /// The record as views and the API edit it.
    held: RefCell<Option<Rc<Held<P>>>>,
}

impl Core {
    /// Whether edits still wait for the store.
    fn owes(&self) -> bool {
        !self.waiting.borrow().is_empty()
    }

    /// Make what waited for the store, in a frame's settle, where the store
    /// is free, and wake the window so its next pass reads what it made.
    #[inline(never)]
    fn settle(&self) {
        if !self.owes() {
            return;
        }
        self.apply_waiting();
        self.wake();
    }

    fn wake(&self) {
        if let Some(wake) = &self.wake {
            wake.wake();
        }
    }

    /// `read` the record's document with the edits waiting for its store
    /// made in a copy of it, which is then let go here: the generic code
    /// that reads never holds a document, so a document's teardown is
    /// compiled once rather than in every app. Whether there was one.
    #[inline(never)]
    fn read_with_waiting(&self, read: &mut dyn FnMut(&ui_core::automerge::AutoCommit)) -> bool {
        let Some(mut doc) = self.record.read(Clone::clone) else {
            return false;
        };
        for (action, invoker) in self.waiting.borrow().iter() {
            if let Err(error) = self.apply_to(&mut doc, action, invoker) {
                tracing::warn!(kind = self.kind, %error, "a waiting edit does not apply");
            }
        }
        read(&doc);
        true
    }

    /// Refuse `action` now if, after the edits waiting for the store, it
    /// would be refused once the store takes it.
    #[inline(never)]
    fn admits_after_waiting(
        &self,
        action: &Pending,
        invoker: &ActionInvoker,
    ) -> Result<(), ActionError> {
        let Some(mut doc) = self.record.read(Clone::clone) else {
            return Ok(());
        };
        for (waiting, by) in self.waiting.borrow().iter() {
            let _ = self.apply_to(&mut doc, waiting, by);
        }
        self.apply_to(&mut doc, action, invoker)
            .map_err(ActionError::Refused)
    }

    /// Apply the edits that waited for the store, in the order they were
    /// asked for, while it is free.
    #[inline(never)]
    fn apply_waiting(&self) {
        loop {
            let Some((action, invoker)) = self.waiting.borrow_mut().pop_front() else {
                return;
            };
            match self.edit(&action, &invoker) {
                Err(unavailable) if unavailable.is_momentary() => {
                    self.waiting.borrow_mut().push_front((action, invoker));
                    return;
                }
                Err(unavailable) => {
                    tracing::warn!(kind = self.kind, %unavailable, "a waiting edit was not made");
                }
                Ok(Err(error)) => {
                    tracing::warn!(kind = self.kind, %error, "a waiting edit was refused");
                }
                Ok(Ok(())) => {}
            }
        }
    }

    /// Apply `action` as `invoker` after what waits for the store, or keep it
    /// for the store's next free moment when the store is in use.
    #[inline(never)]
    fn apply_as(&self, action: Pending, invoker: &ActionInvoker) -> Result<(), ActionError> {
        // What waited for the store goes first, so edits land in the order
        // they were asked for.
        self.apply_waiting();
        let waits = !self.waiting.borrow().is_empty()
            || match self.edit(&action, invoker) {
                Ok(result) => {
                    result.map_err(ActionError::Refused)?;
                    false
                }
                // The store is read by whatever runs now (a pass of the
                // app's view): the edit is made once it lets go, in the
                // window's next settle, and what is read until then holds it.
                Err(unavailable) if unavailable.is_momentary() => true,
                Err(unavailable) => return Err(ActionError::Refused(unavailable.to_string())),
            };
        if waits {
            self.admits_after_waiting(&action, invoker)?;
            self.waiting
                .borrow_mut()
                .push_back((action, invoker.clone()));
        }
        Ok(())
    }

    /// Make `action` one change of the record, as `invoker`; why not, when
    /// the record cannot be changed now.
    fn edit(
        &self,
        action: &Pending,
        invoker: &ActionInvoker,
    ) -> Result<Result<(), String>, ui_core::resource::HistoryUnavailable> {
        let content = action.action.change_kind() == ChangeKind::Content;
        self.record
            .try_edit(content, |doc| self.apply_to(doc, action, invoker))
    }

    /// Make `action` one change of `doc`, as `invoker`.
    #[inline(never)]
    fn apply_to(
        &self,
        doc: &mut ui_core::automerge::AutoCommit,
        action: &Pending,
        invoker: &ActionInvoker,
    ) -> Result<(), String> {
        let encoded = action.encoded.clone()?;
        let context = ActionContext::for_resource(self.kind, self.kind, self.label)
            .with_app(self.app_id, self.app_title)
            .with_invoker(invoker.clone())
            .with_change(action.action.change_kind());
        let writer =
            ui_core::history_source::authors::device_actor(&self.me.actor_bytes(), doc.get_actor());
        tilo_actions::apply_local_with_context(
            doc,
            &*action.action,
            encoded,
            Some(&writer),
            context,
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    /// What `refresh` sets the state to while edits wait for the store:
    /// `Some(read)` names the heads and count a new state is read for, `None`
    /// when the state shown already holds them or nothing waits.
    fn waiting_read(&self) -> Option<(Vec<ui_core::automerge::ChangeHash>, usize)> {
        let waiting = self.waiting.borrow().len();
        if waiting == 0 {
            return None;
        }
        let read = (self.record.heads(), waiting);
        (self.ahead.borrow().as_ref() != Some(&read)).then_some(read)
    }
}

impl<P: Resource> Shared<P> {
    /// Follow the record when it moved, reading only what changed. Whether
    /// it did.
    fn refresh(&self) -> bool {
        let core = &self.core;
        core.apply_waiting();
        if core.owes() {
            core.wake();
        }
        let mut moved = match core.record.read(|doc| self.live.sync(doc)) {
            Some(Ok(moved)) => moved,
            Some(Err(error)) => {
                unreadable_person_resource(P::KIND, &error);
                false
            }
            None => false,
        };
        if !moved && self.live.unreadable().is_none() {
            // The record is small and read whole, so a change it cannot
            // read is noticed here rather than by whoever publishes it. The
            // reader reads it again only when the document moved since it
            // last read it whole; what that read found is the state too.
            match core.record.read(|doc| self.live.reread(doc)) {
                Some(Ok(reread)) => moved = reread,
                Some(Err(error)) => unreadable_person_resource(P::KIND, &error),
                None => {}
            }
        }
        // Views of the record follow it whole: it is small.
        let _ = self.live.take_change();
        if core.owes() {
            let Some(read) = core.waiting_read() else {
                return moved;
            };
            if let Some(state) = self.with_waiting() {
                self.state.set(state);
                *core.ahead.borrow_mut() = Some(read);
                return true;
            }
        }
        if core.ahead.borrow_mut().take().is_some() || moved {
            self.state.set(self.live.get());
            return true;
        }
        moved
    }

    /// The state the record has once the edits waiting for the store are
    /// made: read from a copy of it they are made in, which is then let go.
    fn with_waiting(&self) -> Option<Rc<P::State>> {
        let mut state = None;
        self.core.read_with_waiting(&mut |doc| {
            state = match tilo_resource::Reader::<P>::read(doc) {
                Ok(reader) => Some(Rc::clone(reader.state())),
                Err(error) => {
                    unreadable_person_resource(P::KIND, &error);
                    None
                }
            };
        });
        state
    }
}

impl<P: Resource> PersonResource<P> {
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

    /// Read the record's document; `None` while its owner is busy.
    pub fn read<T>(&self, read: impl FnOnce(&ui_core::automerge::AutoCommit) -> T) -> Option<T> {
        self.record().read(read)
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
    /// person (or to whoever the invocation carried out here is for), recorded in its history and — for content the person made —
    /// one undo step of theirs.
    pub fn apply(&self, action: &P::Action) -> Result<(), ActionError> {
        self.apply_as(action, &super::resources::acting(&self.shared.core.me))
    }

    /// [`Self::apply`] by `invoker`: the person, or software acting for
    /// them.
    pub(crate) fn apply_as(
        &self,
        action: &P::Action,
        invoker: &ActionInvoker,
    ) -> Result<(), ActionError> {
        let encoded = tilo_resource::encode::<P>(action)?;
        let pending = Pending {
            action: Box::new(action.clone()),
            encoded: Ok(encoded),
        };
        self.shared.core.apply_as(pending, invoker)?;
        self.shared.refresh();
        Ok(())
    }

    /// Apply the action `invoke` names, as [`Self::apply`].
    ///
    /// This, with [`Self::invoke_as`], is how the app applies what its
    /// resource keeps for itself (`#[action(by_app)]`): such an action is
    /// declared to no caller, and the record's API refuses it.
    pub fn invoke(&self, invoke: &Invoke<P>) -> Result<(), ActionError> {
        let action = invoke
            .build(tilo_resource::origin_now_ms())
            .map_err(ActionError::Refused)?;
        self.apply(&action)
    }

    /// [`Self::invoke`] as `invoker` (an agent, a script the host admitted),
    /// which the change is attributed to in the record's history: what an
    /// agent did beside the person is set down as the agent's, not as theirs.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn invoke_as(
        &self,
        invoker: &ActionInvoker,
        invoke: &Invoke<P>,
    ) -> Result<(), ActionError> {
        let action = invoke
            .build(tilo_resource::origin_now_ms())
            .map_err(ActionError::Refused)?;
        self.apply_as(&action, invoker)
    }

    /// Undo the person's last edit of the record. Whether there was one.
    pub fn undo(&self) -> bool {
        let undone = self.shared.core.record.undo();
        self.shared.refresh();
        undone
    }

    /// Redo the person's last undone edit of the record.
    pub fn redo(&self) -> bool {
        let redone = self.shared.core.record.redo();
        self.shared.refresh();
        redone
    }

    /// Whether the record reaches other devices: it has a route. One kept
    /// on this device only has none.
    #[must_use]
    pub fn syncs(&self) -> bool {
        self.shared.core.record.syncs()
    }

    /// Where the content this record references — a picture its state
    /// names by hash — is stored and read: with the record, carried on its
    /// route. `None` for a record kept on this device only, which keeps no
    /// content.
    #[must_use]
    pub fn content(&self) -> Option<tilo_content::ContentStore> {
        self.shared.core.record.content()
    }

    /// The record underneath, for a test or host that carries it between
    /// devices itself.
    #[must_use]
    pub fn record(&self) -> &StoredResource {
        &self.shared.core.record
    }
}

/// An open record: the app that keeps it and its kind.
type OpenKey = (&'static str, String);

/// The person's records a root's views opened, shared by every scope of the
/// root: one [`PersonResource`] per resource kind.
pub(crate) struct PersonState {
    /// The person's records kept in their own scope; none for a host that
    /// keeps only the settings sections, whose history is the settings
    /// document's.
    records: Option<PersonResources>,
    app_id: &'static str,
    app_title: &'static str,
    me: tilo_resource::MemberId,
    /// The ids they went by before this device had an identity, under which
    /// what they changed is still theirs.
    earlier: Vec<String>,
    /// The window this host opens them for: woken when another replica's
    /// change to one of the records arrives in the store that holds it,
    /// which nothing else tells it of.
    wake: Option<ui_core::wake::WakeHandle>,
    /// The open records, by the app that owns them and their kind.
    opened: RefCell<HashMap<OpenKey, Rc<dyn Any>>>,
    /// The records of the person's other apps keep, held here too.
    others: RefCell<HashMap<&'static str, PersonResources>>,
    owners: Rc<RefCell<Vec<HistoryOwner>>>,
    refreshers: RefCell<Vec<Box<dyn Fn() -> bool>>>,
    /// What each open record makes in a frame's settle ([`Self::settle`]).
    settlers: RefCell<Vec<Box<dyn Fn()>>>,
    /// The open records' undo and redo, as the palette and agents reach
    /// them.
    published: RefCell<Vec<Published>>,
    /// The records that could not be opened as views asked for them, by
    /// what they are, with what they publish: their declared actions,
    /// unavailable for why. Those views are told now, and never again for
    /// the same record.
    unavailable: RefCell<HashMap<(String, &'static str), tilo_actions::ResourceSpec>>,
}

/// One open record as the root publishes it.
struct Published {
    id: String,
    label: String,
    /// Its field edits and verbs, as the API reaches them.
    api: Rc<dyn ResourceApi>,
    /// Take back (`true`) or redo (`false`) one of the person's steps, for
    /// the invoker named. Whether there was one.
    step: Step,
}

/// Take back (`true`) or redo (`false`) one step for the invoker named.
type Step = Box<dyn Fn(bool, &tilo_actions::Actor) -> bool>;

/// A person record just opened as its type: its shared state, its API and
/// its undo and redo.
struct Opened {
    shared: Rc<dyn Any>,
    api: Rc<dyn ResourceApi>,
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

/// The address the person's record of `kind` is published under: a record
/// address, not a person's id: a person is their bare root key
/// ([`tilo_resource::PersonId`]).
#[must_use]
pub(crate) fn resource_id(kind: &str) -> String {
    format!("person:{kind}")
}

/// A record an app keeps beside the one it shows, declared with the app
/// ([`ResourceApp::PERSON_RESOURCES`](super::ResourceApp::PERSON_RESOURCES)).
///
/// A shared one ([`Self::shared`]) is a record that one app keeps for every app
/// of the workspace ([`collab_types::WORKSPACE_DOCUMENTS`]): the same record
/// for every member, its changes decided by the workspace's grants. One this
/// app keeps itself ([`Self::profile`]) is the person's own: kept in their own
/// scope, carried to their other devices by their personal store, and never
/// reaching a member. The app's committed surface names either whether or not a
/// view of it is showing, so the host admits the app's changes to it — and its
/// reads of it — against the surface.
#[derive(Clone, Copy)]
pub struct PersonKind {
    kind: &'static str,
    /// The app that keeps a shared record of it; `None` for the person's own record, which is
    /// this app's.
    owner: Option<&'static str>,
    report: fn() -> tilo_resource::Surface,
    open: fn(&super::settings::ViewStorage),
}

impl PersonKind {
    /// The record `P` that one app keeps for every app of the workspace
    /// ([`SharedResource`](tilo_resource::SharedResource), its
    /// [`OWNER`](tilo_resource::SharedResource::OWNER) keeping it), opened as
    /// the workspace's.
    #[must_use]
    pub const fn shared<P: tilo_resource::SharedResource>() -> Self {
        Self {
            kind: P::KIND,
            owner: Some(P::OWNER),
            report: workspace_surface::<P>,
            open: open_shared::<P>,
        }
    }

    /// The record `P` the person keeps for themselves, in their own scope.
    #[must_use]
    pub const fn profile<P: Resource>() -> Self {
        Self {
            kind: P::KIND,
            owner: None,
            report: profile_surface::<P>,
            open: open_profile::<P>,
        }
    }

    /// The kind of record it is.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        self.kind
    }

    /// The app that keeps a shared record of it, when another app does; `None` for the app's
    /// own, the person's included.
    #[must_use]
    pub const fn owner(&self) -> Option<&'static str> {
        self.owner
    }

    /// Its surface, as the app's surface report lists it: `P`'s, marked as
    /// the person's own for a profile record, or as the workspace's for a
    /// shared one.
    #[must_use]
    pub fn surface(&self) -> tilo_resource::Surface {
        (self.report)()
    }

    /// Open it with the window, so it is published before any view shows it.
    pub(crate) fn open(&self, storage: &super::settings::ViewStorage) {
        (self.open)(storage);
    }
}

/// `P`'s surface, marked as the person's own.
fn profile_surface<P: Resource>() -> tilo_resource::Surface {
    tilo_resource::Surface {
        person: true,
        ..P::surface()
    }
}

/// `P`'s surface as a record the workspace keeps for every app of it.
fn workspace_surface<P: Resource>() -> tilo_resource::Surface {
    tilo_resource::Surface {
        person: false,
        ..P::surface()
    }
}

fn open_shared<P: tilo_resource::SharedResource>(storage: &super::settings::ViewStorage) {
    // The app's surface lists the record, so the host admits the app's changes
    // to it, whichever app of the workspace keeps it.
    super::controls::opened_shared::<P>();
    let _ = storage.shared_workspace::<P>(P::OWNER);
}

fn open_profile<P: Resource>(storage: &super::settings::ViewStorage) {
    let _ = storage.profile_resource::<P>();
}

fn history_spec(record: &str, label: &str, undo: bool) -> tilo_actions::ActionSpec {
    let mut spec = history_operation(record.to_owned(), label, undo);
    spec.type_mut().contract.result = tilo_actions::ResultSpec::Json {
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
        crate::copy_catalogue::t_args("tilo-app-undo-description", &[("label", label.into())])
    } else {
        crate::copy_catalogue::t_args("tilo-app-redo-description", &[("label", label.into())])
    })
}

impl PersonState {
    pub(crate) fn new(
        records: PersonResources,
        app_id: &'static str,
        app_title: &'static str,
        wake: Option<ui_core::wake::WakeHandle>,
    ) -> Self {
        let me = records.author().clone();
        Self::named(Some(records), me, Vec::new(), app_id, app_title, wake)
    }

    /// The person `me`, whose records this host does not hold: their
    /// undo and redo of the settings sections are published all the same,
    /// and read the sections' history in the settings document. `earlier`
    /// are the ids they went by before this device had an identity, whose
    /// changes are theirs too.
    pub(crate) fn without_resources(
        me: tilo_resource::MemberId,
        earlier: Vec<String>,
        app_id: &'static str,
        app_title: &'static str,
    ) -> Self {
        Self::named(None, me, earlier, app_id, app_title, None)
    }

    fn named(
        records: Option<PersonResources>,
        me: tilo_resource::MemberId,
        earlier: Vec<String>,
        app_id: &'static str,
        app_title: &'static str,
        wake: Option<ui_core::wake::WakeHandle>,
    ) -> Self {
        Self {
            records,
            app_id,
            app_title,
            me,
            earlier,
            wake,
            opened: RefCell::default(),
            others: RefCell::default(),
            owners: Rc::default(),
            refreshers: RefCell::default(),
            settlers: RefCell::default(),
            published: RefCell::default(),
            unavailable: RefCell::default(),
        }
    }

    /// The person's own workspace, which holds their profile records and decides their changes.
    fn workspace(&self) -> Option<String> {
        tilo_actions::policy::roles::workspace_of_person(self.me.person_id()?)
    }

    /// The record `P` the person keeps for themselves, in their own scope.
    pub(crate) fn open_profile<P: Resource>(&self) -> Option<PersonResource<P>> {
        debug_assert!(
            collab_types::workspace_document_of_kind(P::KIND).is_none(),
            "{} is a record the workspace keeps: open it with `shared_workspace`",
            P::KIND
        );
        self.open_of::<P>(self.app_id, true)
    }

    /// The record `P` the person keeps for themselves, opened on first use and
    /// published when it is this app's own (`publish`).
    fn open_of<P: Resource>(
        &self,
        owner: &'static str,
        publish: bool,
    ) -> Option<PersonResource<P>> {
        let open = |record: StoredResource| -> Result<Opened, String> {
            let shared = self.shared_of::<P>(record)?;
            let held = Rc::new(held::<P>(&shared).held_in(self.workspace()));
            *shared.held.borrow_mut() = Some(Rc::clone(&held));
            let weak = Rc::downgrade(&shared);
            Ok(Opened {
                shared: shared as Rc<dyn Any>,
                api: held,
                step: Box::new(move |undo, _| {
                    weak.upgrade().is_some_and(|shared| {
                        let record = PersonResource { shared };
                        if undo { record.undo() } else { record.redo() }
                    })
                }),
            })
        };
        let shared = self.open_kind(
            owner,
            &PersonResourceType {
                kind: P::KIND,
                label: P::LABEL,
                surface: P::surface,
                unreadable: &|reason| {
                    tilo_resource::resource::unreadable_parts::<P>(
                        &resource_id(P::KIND),
                        P::LABEL,
                        reason,
                    )
                },
            },
            &|doc| P::initialize(doc).map_err(|error| error.to_string()),
            &open,
            publish,
        )?;
        shared
            .downcast::<Shared<P>>()
            .ok()
            .map(|shared| PersonResource { shared })
    }

    /// The open record of `kind` that the app `owner` keeps, or the one
    /// `open` makes of it, created by `initialize` where it does not exist
    /// yet, kept and (for this app's own, when `publish`) published: the
    /// part of opening a record that does not depend on its type.
    fn open_kind(
        &self,
        owner: &'static str,
        of: &PersonResourceType<'_>,
        initialize: &dyn Fn(&mut ui_core::automerge::AutoCommit) -> Result<(), String>,
        open: &dyn Fn(StoredResource) -> Result<Opened, String>,
        publish: bool,
    ) -> Option<Rc<dyn Any>> {
        let (kind, label) = (of.kind, of.label);
        let key = (owner, kind.to_owned());
        if let Some(open) = self.opened.borrow().get(&key) {
            return Some(Rc::clone(open));
        }
        let opened = {
            let records = self.resources_of(owner)?;
            records.declare_type(kind, of.surface);
            open_resource(&records, kind.to_owned(), kind, initialize)
        };
        let record = match opened {
            Ok(record) => record,
            Err(reason) => return self.note_unavailable(owner, of, &reason, publish),
        };
        let Opened { shared, api, step } = match open(record) {
            Ok(opened) => opened,
            Err(reason) => return self.note_unavailable(owner, of, &reason, publish),
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
                api,
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
    fn shared_of<P: Resource>(&self, record: StoredResource) -> Result<Rc<Shared<P>>, String> {
        // An older record is brought up to the resource's storage version
        // before it is read, as the app's record is when it opens: once, as
        // the same housekeeping change on every device, nobody's undo step.
        let older = record
            .read(tilo_resource::evolution::needs_migrate::<P>)
            .ok_or_else(|| "the record cannot be read".to_owned())?;
        let id = record.checkout().id.route().to_owned();
        if older
            && let Err(error) = record.edit(false, |doc| {
                tilo_resource::founding::bring_up_resource::<P>(&id, doc)
                    .map_err(|error| error.to_string())
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
        if let Some(wake) = &self.wake {
            // The record is kept for the person, not the app's window: another
            // device's or app's change to it arrives in its store, and nothing
            // else would tell this window to read it.
            record.wake_on_arrival(wake.clone());
        }
        self.owners.borrow_mut().push(record.history_owner());
        let shared = Rc::new(Shared::<P> {
            core: Core {
                record,
                app_id: self.app_id,
                app_title: self.app_title,
                me: self.me.clone(),
                kind: P::KIND,
                label: P::LABEL,
                waiting: RefCell::default(),
                ahead: RefCell::default(),
                wake: self.wake.clone(),
            },
            state: LocalState::new(live.get()),
            live,
            held: RefCell::new(None),
        });
        let weak = Rc::downgrade(&shared);
        self.refreshers.borrow_mut().push(Box::new(move || {
            weak.upgrade().is_some_and(|shared| shared.refresh())
        }));
        let weak = Rc::downgrade(&shared);
        self.settlers.borrow_mut().push(Box::new(move || {
            if let Some(shared) = weak.upgrade() {
                shared.core.settle();
            }
        }));
        Ok(shared)
    }

    /// The settings section `kind` held as a record, opened on first use and
    /// published with the undo and redo of each invoker's steps on it.
    pub(crate) fn open_section(&self, kind: Rc<SectionKind>) -> Rc<SectionApi> {
        let key = (self.app_id, kind.kind.to_owned());
        if let Some(api) = self
            .opened
            .borrow()
            .get(&key)
            .and_then(|open| Rc::clone(open).downcast::<SectionApi>().ok())
        {
            return api;
        }
        let api = Rc::new(SectionApi::new(
            kind,
            Some(self.me.clone()),
            self.earlier.clone(),
        ));
        let stepping = Rc::downgrade(&api);
        self.publish(
            key,
            Rc::clone(&api) as Rc<dyn Any>,
            Published {
                id: api.id().to_owned(),
                label: api.label().to_owned(),
                api: Rc::clone(&api) as Rc<dyn ResourceApi>,
                step: Box::new(move |undo, by| {
                    stepping.upgrade().is_some_and(|api| api.step(undo, by))
                }),
            },
        );
        api
    }

    /// The person's records `owner` keeps: this app's own, or another app's,
    /// opened once.
    fn resources_of(&self, owner: &'static str) -> Option<PersonResources> {
        // A host that keeps the person's shared records lends this app its
        // own as well: a second writer of them here would store nowhere the
        // host stores or syncs.
        if owner == self.app_id && !self.records.as_ref()?.shared_by_host() {
            return self.records.clone();
        }
        let mut others = self.others.borrow_mut();
        if let Some(records) = others.get(owner) {
            return Some(records.clone());
        }
        let records = self
            .records
            .as_ref()?
            .of_app(owner)
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
    fn note_unavailable(
        &self,
        owner: &'static str,
        of: &PersonResourceType<'_>,
        reason: &str,
        publish: bool,
    ) -> Option<Rc<dyn Any>> {
        unreadable_person_resource(of.kind, &reason);
        // Only the record this app would have published is published for
        // the reason: one another app keeps and this one reads is that
        // app's surface, which registers the actions its own record has.
        if owner == self.app_id && publish {
            let published = (of.unreadable)(reason);
            self.unavailable
                .borrow_mut()
                .insert((owner.to_owned(), of.kind), published);
        }
        None
    }

    /// Every record a view asked for that could not be opened, as it is
    /// published: its declared actions, unavailable for why.
    pub(crate) fn unavailable(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.unavailable.borrow().values().cloned().collect()
    }

    /// Keep the record opened under `key`, publishing its API and its undo
    /// and redo.
    fn publish(&self, key: OpenKey, open: Rc<dyn Any>, published: Published) {
        self.published.borrow_mut().push(published);
        self.opened.borrow_mut().insert(key, open);
    }

    /// Make what waited for each open record's store (an edit asked for
    /// during a pass of the view): in a frame's settle, where the store is
    /// free.
    pub(crate) fn settle(&self) {
        for settle in self.settlers.borrow().iter() {
            settle();
        }
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
    pub(crate) fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.published
            .borrow()
            .iter()
            .map(|published| {
                published
                    .api
                    .publish()
                    .with_action(history_spec(&published.id, &published.label, true))
                    .with_action(history_spec(&published.id, &published.label, false))
            })
            .collect()
    }

    /// The open records of the person's own, as the API changes them.
    pub(crate) fn apis(&self) -> Vec<Rc<dyn ResourceApi>> {
        self.published
            .borrow()
            .iter()
            .map(|published| Rc::clone(&published.api))
            .collect()
    }

    pub(crate) fn me(&self) -> &tilo_resource::MemberId {
        &self.me
    }

    /// Answer an undo or redo of an open record: how many steps it took.
    pub(crate) fn answer(&self, payload: &[u8]) -> Result<tilo_actions::ActionOutput, String> {
        let invoker = super::resources::acting(&self.me);
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
        let theirs = invoker.acting.by.member_id() == Some(&self.me)
            || self
                .me
                .person_id()
                .is_some_and(|person| invoker.acting.acted_for_person() == Some(person));
        if !theirs {
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
        while taken < steps && (record.step)(request.undo, &invoker.acting.by) {
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

    /// The routes of the open records, which the runtime syncs: a record
    /// kept on this device only has none.
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
fn open_resource(
    records: &PersonResources,
    kind: String,
    of: &str,
    initialize: &dyn Fn(&mut ui_core::automerge::AutoCommit) -> Result<(), String>,
) -> Result<StoredResource, String> {
    records
        .open_resource(ResourceType::new(kind), initialize)
        .map_err(|error| format!("{of}'s record cannot be opened: {error}"))
}

/// The record `shared` as views and the API edit it: read into a state of
/// its own that follows the record, changed as the person.
fn held<P: Resource>(shared: &Rc<Shared<P>>) -> Held<P> {
    let following = Rc::downgrade(shared);
    let applying = Rc::downgrade(shared);
    let reading = Rc::downgrade(shared);
    Held::new(
        resource_id(P::KIND),
        format!("Your {}", P::LABEL.to_lowercase()),
        true,
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
            PersonResource { shared }.apply_as(action, invoker)
        },
    )
    .reading(move |read| {
        if let Some(shared) = reading.upgrade() {
            shared.core.record.read(|doc| read(doc));
        }
    })
}

impl<P: Resource> super::resources::sealed::Hold<P> for PersonResource<P> {
    fn held(&self, _storage: super::resources::sealed::Storage<'_>) -> Option<Rc<Held<P>>> {
        self.shared.held.borrow().clone()
    }

    fn api(
        &self,
        _storage: super::resources::sealed::Storage<'_>,
    ) -> Option<super::resources::sealed::Api> {
        self.shared
            .held
            .borrow()
            .clone()
            .map(|held| super::resources::sealed::Api(held as Rc<dyn ResourceApi>))
    }
}

impl Drop for PersonState {
    /// The window closes: what the person did is stored before it goes.
    fn drop(&mut self) {
        if let Some(Err(error)) = self.records.as_ref().map(PersonResources::flush) {
            tracing::warn!(%error, "the records beside the app's own could not be stored");
        }
    }
}

/// Log that the person's record of `kind` does not read: one event site in
/// the runtime rather than one in every app for each record type.
#[cold]
#[inline(never)]
fn unreadable_person_resource(kind: &'static str, error: &dyn std::fmt::Display) {
    tracing::warn!(kind, %error, "person record unreadable");
}

/// A person record's type, as the part of opening one that does not depend
/// on it reads it: compiled once, not into every app.
struct PersonResourceType<'a> {
    kind: &'static str,
    label: &'static str,
    /// The record's type, which it is founded by.
    surface: fn() -> tilo_resource::surface::Surface,
    /// What the record publishes when it could not be opened, for why.
    unreadable: &'a dyn Fn(&str) -> tilo_actions::ResourceSpec,
}
