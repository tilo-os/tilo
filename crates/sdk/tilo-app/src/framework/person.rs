//! The records a person keeps for themselves, in their own workspace rather than the one a window
//! shows: presentation data only they use, such as the signature they sign documents with. A
//! record the workspace keeps, one for every member, is opened as the workspace's
//! ([`super::WorkspaceResource`]).
//!
//! A window in the person's own workspace holds their records as it holds its own: the record is
//! a plain [`super::WorkspaceResource`] at the person's record id, so its publication, sync,
//! migration and history are the generic ones. A window in another workspace holds none of them:
//! it follows each through the record API, and its changes are invoked at the app that serves the
//! person's own workspace, which keeps and syncs the record there. With no identity on the device
//! there is no such workspace, and nothing of the person's is kept.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_actions::remote::{InvokeArgument, ReadAnswer, ResourceReadRequest};
use tilo_actions::{ActionError, ActionInvoker, InvocationUpdate};
use tilo_automerge::ApplyIncoming;

use super::live::Live;
use super::resources::{Held, ResourceApi};
use super::section::{SectionApi, SectionKind};
use super::workspace::{Unopened, WorkspaceResource};
use tilo_resource::{Invoke, Resource};
use tilo_ui_base::LocalState;
use ui_core::history_mode::HistoryOwner;
use ui_core::resource::StoredResource;
use ui_core::resource::workspace::WorkspaceResources;
use tilo_ui_base::{Arrival, FollowedRead, InvocationCall, RemoteActions};

/// A record the person keeps for themselves, in their own workspace rather than the one the
/// window shows: the signature they sign with. The same on each of the person's devices, and no
/// other member's.
///
/// In a window of their own workspace it is the record itself, a
/// [`WorkspaceResource`](super::WorkspaceResource), held and published as such. In a window of
/// another workspace it is followed through the record API and changed at the app serving their
/// own workspace.
pub struct PersonResource<P: Resource> {
    held: Rc<Held<P>>,
    me: tilo_resource::MemberId,
    backing: Backing<P>,
}

/// Where a window holds the person's record: its own workspace's copy, or one followed in the
/// person's own workspace.
enum Backing<P: Resource> {
    Own(Rc<WorkspaceResource<P>>),
    There(Rc<There<P>>),
}

impl<P: Resource> Clone for PersonResource<P> {
    fn clone(&self) -> Self {
        Self {
            held: Rc::clone(&self.held),
            me: self.me.clone(),
            backing: match &self.backing {
                Backing::Own(resource) => Backing::Own(Rc::clone(resource)),
                Backing::There(there) => Backing::There(Rc::clone(there)),
            },
        }
    }
}

/// A person's record in their own workspace, seen from a window in another one: followed
/// through the record API, changed by invoking its actions at the app that serves that
/// workspace, which holds, stores and syncs it.
struct Remote {
    /// The person's own workspace.
    workspace: String,
    /// The app whose record it is.
    app: &'static str,
    /// Its id there.
    id: ui_core::resource::DocumentId,
    actions: RemoteActions,
    /// The record, followed: answered again after every write to it.
    followed: FollowedRead,
    /// The record as read so far, from its genesis: each reading is merged in, so it only grows,
    /// as the record does in the person's workspace.
    doc: RefCell<ui_core::automerge::AutoCommit>,
    /// The revision of the readings `doc` holds.
    revision: RefCell<Option<String>>,
    /// The changes and steps sent and not yet answered, by what they were.
    sent: RefCell<Vec<(String, InvocationCall)>>,
}

impl Remote {
    /// The provider that serves the record: the app in the person's workspace, on this record.
    fn provider(&self) -> String {
        tilo_actions::remote::application_resource_target(
            &self.workspace,
            self.app,
            self.id.as_str(),
            "",
            "",
        )
        .provider
    }

    /// Invoke `action` with `arguments` on the record's published resource `resource`, at the
    /// app in the person's workspace.
    fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) {
        let target = tilo_actions::remote::application_resource_target(
            &self.workspace,
            self.app,
            self.id.as_str(),
            resource,
            action,
        );
        let call = self
            .actions
            .invoke(target, InvokeArgument::Json(arguments), None);
        self.sent.borrow_mut().push((action.to_owned(), call));
    }

    /// Take up what the person's workspace answered since: a newer reading of the record merged
    /// into the copy read so far, and each answered change let go, a refused one told. Whether
    /// the copy moved.
    fn take_answers(&self, kind: &'static str) -> bool {
        self.sent.borrow_mut().retain(|(action, call)| {
            call.with(|answer| match answer {
                Arrival::Pending { .. }
                | Arrival::Ready(
                    InvocationUpdate::Accepted { .. }
                    | InvocationUpdate::Progress { .. }
                    | InvocationUpdate::Item { .. },
                ) => true,
                Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
                    refused_person_change(kind, action, &error.message);
                    false
                }
                Arrival::Failed(error) => {
                    refused_person_change(kind, action, &error.to_string());
                    false
                }
                Arrival::Ready(_) => false,
            })
        });
        let Some((revision, document)) = self.followed.call().with(|answer| match answer {
            Arrival::Ready(readings) => Some(reading_of(readings, self.id.as_str())),
            Arrival::Pending { .. } | Arrival::Failed(_) => None,
        }) else {
            return false;
        };
        if self.revision.borrow().as_deref() == Some(revision.as_str()) {
            return false;
        }
        *self.revision.borrow_mut() = Some(revision);
        // Absent or still arriving there: the genesis read so far is what it holds.
        let Some(document) = document else {
            return false;
        };
        match ui_core::automerge::AutoCommit::load(&document) {
            Ok(mut read) => {
                let mut doc = self.doc.borrow_mut();
                let before = doc.get_heads();
                if let Err(error) = doc.merge_incoming(&mut read) {
                    unreadable_person_resource(kind, &error);
                    return false;
                }
                doc.get_heads() != before
            }
            Err(error) => {
                unreadable_person_resource(kind, &error);
                false
            }
        }
    }
}

/// The person's record followed in their own workspace, as a window in another workspace reads
/// it: the readings merged so far, read as `P` and followed change by change.
struct There<P: Resource> {
    remote: Remote,
    live: Live<P>,
    state: LocalState<Rc<P::State>>,
    /// The window this record is opened for: woken when a change of it was asked and when a
    /// reading shows it moved.
    wake: Option<ui_core::wake::WakeHandle>,
}

impl<P: Resource> There<P> {
    fn wake(&self) {
        if let Some(wake) = &self.wake {
            wake.wake();
        }
    }

    /// Follow the record when it moved. Whether it did.
    fn refresh(&self) -> bool {
        self.remote.take_answers(P::KIND);
        let mut moved = match self.live.sync(&self.remote.doc.borrow()) {
            Ok(moved) => moved,
            Err(error) => {
                unreadable_person_resource(P::KIND, &error);
                false
            }
        };
        if !moved && self.live.unreadable().is_none() {
            // The record is small and read whole, so a change it cannot read is noticed here.
            match self.live.reread(&self.remote.doc.borrow()) {
                Ok(reread) => moved = reread,
                Err(error) => unreadable_person_resource(P::KIND, &error),
            }
        }
        // Views of the record follow it whole: it is small.
        let _ = self.live.take_change();
        if moved {
            self.state.set(self.live.get());
        }
        moved
    }
}

/// Take back (`undo`) or bring back `author`'s last step on `record`. Whether there was one.
fn step_as(record: &StoredResource, author: &tilo_resource::MemberId, undo: bool) -> bool {
    if undo {
        record.undo_as(author)
    } else {
        record.redo_as(author)
    }
}

/// Whether the record kept in the person's own workspace has a step of theirs to take back,
/// as the copy this window follows holds its history: the record is undone where it is kept,
/// so what it holds decides whether that is asked at all. A redo can only bring back an undo
/// taken where the record is kept, so it is always asked, and the keeper answers what it took.
fn step_held<P: Resource>(
    there: &There<P>,
    author: &tilo_resource::MemberId,
    undo: bool,
) -> bool {
    // Opening a history opens an undo step on this thread; this one takes no step, so the
    // step open before it stays open.
    let open = tilo_actions::current_step();
    let mut doc = there.remote.doc.borrow_mut();
    let history = ui_core::UndoLog::new(&mut doc).on_any_device();
    let held = !undo || history.can_undo_as(&doc, Some(author));
    tilo_actions::open_step(open);
    held
}

/// The revision of `readings` and the document of the record `record` in them, when the
/// workspace holds one.
fn reading_of(readings: &serde_json::Value, record: &str) -> (String, Option<Vec<u8>>) {
    let revision = readings["revision"].as_str().unwrap_or_default().to_owned();
    let document = readings["records"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|reading| reading["record"].as_str() == Some(record))
        .and_then(|reading| reading["document"].as_str())
        .and_then(unhex);
    (revision, document)
}

/// The bytes `text` writes in hexadecimal, as a reading carries a document.
fn unhex(text: &str) -> Option<Vec<u8>> {
    let digits = text.as_bytes();
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    digits
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

impl<P: Resource> PersonResource<P> {
    /// The current state.
    #[must_use]
    pub fn get(&self) -> Rc<P::State> {
        match &self.backing {
            Backing::Own(resource) => resource.get(),
            Backing::There(there) => {
                there.refresh();
                there.state.with(Rc::clone)
            }
        }
    }

    /// Read the current state.
    pub fn with<T>(&self, read: impl FnOnce(&P::State) -> T) -> T {
        read(&self.get())
    }

    /// Read the record's document; `None` while its owner is busy.
    pub fn read<T>(&self, read: impl FnOnce(&ui_core::automerge::AutoCommit) -> T) -> Option<T> {
        match &self.backing {
            Backing::Own(resource) => resource.read(read),
            Backing::There(there) => Some(read(&there.remote.doc.borrow())),
        }
    }

    /// Why the record stopped reading as `P`, if it did: the state is then
    /// the last one that read, and a view shows this instead of it. Its
    /// published actions say the same.
    #[must_use]
    pub fn unreadable(&self) -> Option<String> {
        match &self.backing {
            Backing::Own(resource) => resource.unreadable(),
            Backing::There(there) => {
                there.refresh();
                there.live.unreadable().map(|why| why.reason)
            }
        }
    }

    /// The state as view state that follows every change, for a control
    /// bound to it.
    #[must_use]
    pub fn state(&self) -> LocalState<Rc<P::State>> {
        match &self.backing {
            Backing::Own(resource) => resource.state(),
            Backing::There(there) => there.state.clone(),
        }
    }

    /// Increases whenever the state changes.
    #[must_use]
    pub fn revision(&self) -> u64 {
        match &self.backing {
            Backing::Own(resource) => resource.revision(),
            Backing::There(there) => {
                there.refresh();
                there.live.revision()
            }
        }
    }

    /// Apply `action` now: one change of the record, attributed to the
    /// person (or to whoever the invocation carried out here is for), recorded in its history and — for content the person made —
    /// one undo step of theirs.
    pub fn apply(&self, action: &P::Action) -> Result<(), ActionError> {
        self.apply_as(action, &super::resources::acting(&self.me))
    }

    /// [`Self::apply`] by `invoker`: the person, or software acting for
    /// them.
    pub(crate) fn apply_as(
        &self,
        action: &P::Action,
        invoker: &ActionInvoker,
    ) -> Result<(), ActionError> {
        match &self.backing {
            Backing::Own(resource) => resource.act_as(invoker, action),
            Backing::There(there) => {
                let invoke = Invoke::<P>::apply(action)
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                send_there(there, &self.me, &invoke, invoker)
            }
        }
    }

    /// Apply the action `invoke` names, as [`Self::apply`].
    ///
    /// This, with [`Self::invoke_as`], is how the app applies what its
    /// resource keeps for itself (`#[action(by_app)]`): such an action is
    /// declared to no caller, and the record's API refuses it.
    pub fn invoke(&self, invoke: &Invoke<P>) -> Result<(), ActionError> {
        self.invoke_as(&super::resources::acting(&self.me), invoke)
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
        match &self.backing {
            Backing::Own(resource) => resource.invoke_as(invoker, invoke),
            Backing::There(there) => send_there(there, &self.me, invoke, invoker),
        }
    }

    /// Undo the person's last edit of the record. Whether there was one, or, for a record
    /// kept in their own workspace while the window shows another, whether the undo was asked
    /// of it.
    pub fn undo(&self) -> bool {
        self.step(true)
    }

    /// Redo the person's last undone edit of the record, as [`Self::undo`].
    pub fn redo(&self) -> bool {
        self.step(false)
    }

    fn step(&self, undo: bool) -> bool {
        match &self.backing {
            Backing::Own(resource) => {
                let by = super::resources::acting(&self.me);
                let stepped = step_as(
                    resource.record(),
                    by.acting.by.member_id().unwrap_or(&self.me),
                    undo,
                );
                resource.refresh();
                stepped
            }
            Backing::There(there) => {
                let held = step_held(there, &self.me, undo);
                if held {
                    there.remote.invoke(
                        &resource_id(P::KIND),
                        if undo { "undo" } else { "redo" },
                        serde_json::Map::new(),
                    );
                    there.wake();
                }
                held
            }
        }
    }

    /// Where the content this record references — a picture its state
    /// names by hash — is stored and read: with the record, carried on its
    /// route. `None` for a record kept on this device only, which keeps no
    /// content.
    #[must_use]
    pub fn content(&self) -> Option<tilo_content::ContentStore> {
        match &self.backing {
            Backing::Own(resource) => resource.record().content(),
            Backing::There(there) => ui_core::resource::stored::content_of(
                there.remote.app.to_owned(),
                &there.remote.workspace,
                &there.remote.id,
            ),
        }
    }

    /// The record underneath where the window holds it, in the person's own workspace, for a
    /// test or host that carries it between devices itself. `None` in a window of another
    /// workspace, which holds none of it.
    #[must_use]
    pub fn record(&self) -> Option<&StoredResource> {
        match &self.backing {
            Backing::Own(resource) => Some(resource.record()),
            Backing::There(_) => None,
        }
    }
}

/// Send `invoke` to the app that keeps the record in the person's own workspace, which
/// applies it as whoever this window's connection is: the person. The record shows it once
/// that workspace answers the next reading.
fn send_there<P: Resource>(
    there: &There<P>,
    me: &tilo_resource::MemberId,
    invoke: &Invoke<P>,
    invoker: &ActionInvoker,
) -> Result<(), ActionError> {
    let action = invoke
        .build(tilo_resource::origin_now_ms())
        .map_err(ActionError::Refused)?;
    // What the app keeps for itself it applies only where it holds the record: the app in
    // the person's workspace takes from a caller only what the record offers callers.
    if P::by_app(&action) {
        return Err(ActionError::Refused(format!(
            "Your {} changes this way only in a window of your own workspace",
            P::LABEL.to_lowercase()
        )));
    }
    if invoker.acting.by.member_id() != Some(me) {
        return Err(ActionError::Refused(format!(
            "Only you change your {} from a window in another workspace",
            P::LABEL.to_lowercase()
        )));
    }
    there
        .remote
        .invoke(&resource_id(P::KIND), invoke.id, invoke.arguments.clone());
    there.wake();
    Ok(())
}

/// The record `resource` as views and the API edit it: read as the window reads it, changed as
/// the person.
fn held<P: Resource>(resource: &Rc<WorkspaceResource<P>>) -> Rc<Held<P>> {
    let following = Rc::downgrade(resource);
    let applying = Rc::downgrade(resource);
    let reading = Rc::downgrade(resource);
    Rc::new(
        Held::new(
            resource_id(P::KIND),
            format!("Your {}", P::LABEL.to_lowercase()),
            true,
            resource.live(),
            move |_: &Live<P>| {
                if let Some(resource) = following.upgrade() {
                    resource.refresh();
                }
            },
            move |action: &P::Action, invoker: &ActionInvoker| {
                let resource = applying
                    .upgrade()
                    .ok_or_else(|| ActionError::Refused("the record is closed".into()))?;
                resource.act_as(invoker, action)
            },
        )
        .reading(move |read| {
            if let Some(resource) = reading.upgrade() {
                resource.read(|doc| read(doc));
            }
        }),
    )
}

/// The record `there` as views and the API edit it: read as this window read it, changed by
/// invoking its actions at the app in the person's own workspace.
fn held_there<P: Resource>(there: &Rc<There<P>>, me: &tilo_resource::MemberId) -> Rc<Held<P>> {
    let following = Rc::downgrade(there);
    let applying = Rc::downgrade(there);
    let reading = Rc::downgrade(there);
    let me = me.clone();
    let workspace = there.remote.workspace.clone();
    Rc::new(
        Held::new(
            resource_id(P::KIND),
            format!("Your {}", P::LABEL.to_lowercase()),
            true,
            there.live.clone(),
            move |_: &Live<P>| {
                if let Some(there) = following.upgrade() {
                    there.refresh();
                }
            },
            move |action: &P::Action, invoker: &ActionInvoker| {
                let there = applying
                    .upgrade()
                    .ok_or_else(|| ActionError::Refused("the record is closed".into()))?;
                let invoke = Invoke::<P>::apply(action)
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                send_there(&there, &me, &invoke, invoker)
            },
        )
        .reading(move |read| {
            if let Some(there) = reading.upgrade() {
                read(&there.remote.doc.borrow());
            }
        })
        .held_in(Some(workspace)),
    )
}

/// An open record: the app that keeps it and its kind.
type OpenKey = (&'static str, String);

/// The person's records a root's views opened, shared by every scope of the
/// root: one [`PersonResource`] per resource kind.
pub(crate) struct PersonState {
    /// Where the person's records are, seen from this window; none for a host that keeps only
    /// the settings sections, whose history is the settings document's.
    records: Option<PersonRecords>,
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
    /// The client the person's records in their own workspace are followed and changed over,
    /// for a window in another one.
    remote: Option<RemoteActions>,
    /// The open records, by the app that owns them and their kind.
    opened: RefCell<HashMap<OpenKey, Rc<dyn Any>>>,
    owners: Rc<RefCell<Vec<HistoryOwner>>>,
    refreshers: RefCell<Vec<Box<dyn Fn() -> bool>>>,
    /// The open records' undo and redo, as the palette and agents reach
    /// them.
    published: RefCell<Vec<Published>>,
    /// The records that could not be opened as views asked for them, by
    /// what they are, with what they publish: their declared actions,
    /// unavailable for why. Those views are told now, and never again for
    /// the same record.
    unavailable: RefCell<HashMap<(String, &'static str), tilo_actions::ResourceSpec>>,
}

/// Where a window finds the person's records.
pub(crate) enum PersonRecords {
    /// The device has no identity: the person has no workspace of their own, and keeps nothing.
    Unsigned,
    /// The window is in the person's own workspace: their records are the app's records there.
    Here(WorkspaceResources),
    /// The window is in another workspace: their records are in this one, their own.
    There(String),
}

impl PersonRecords {
    /// Where `me`'s records are for a window in `workspace`, which holds its records in `here`;
    /// `None` for a window in the person's own workspace that holds none.
    pub(crate) fn of(
        me: &tilo_resource::MemberId,
        workspace: &str,
        here: impl FnOnce() -> Option<WorkspaceResources>,
    ) -> Option<Self> {
        let own = me
            .person_id()
            .and_then(tilo_actions::policy::roles::workspace_of_person);
        Some(match own {
            None => Self::Unsigned,
            Some(own) if own.eq_ignore_ascii_case(workspace) => Self::Here(here()?),
            Some(own) => Self::There(own),
        })
    }
}

/// One open record as the root publishes it.
struct Published {
    id: String,
    label: String,
    /// The provider that runs its actions, when another one does: the app in the person's own
    /// workspace.
    run_by: Option<String>,
    /// Its field edits and verbs, as the API reaches them.
    api: Rc<dyn ResourceApi>,
    /// Take back (`true`) or redo (`false`) one of the person's steps, for
    /// the invoker named. Whether there was one.
    step: Step,
}

/// Take back (`true`) or redo (`false`) one step for the invoker named.
type Step = Box<dyn Fn(bool, &tilo_actions::Actor) -> bool>;

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
/// workspace, carried to their devices from there, and never reaching a member. The app's committed surface names either whether or not a
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

    /// The record `P` the person keeps for themselves, in their own workspace.
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
fn workspace_surface<P: tilo_resource::SharedResource>() -> tilo_resource::Surface {
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
        records: PersonRecords,
        me: tilo_resource::MemberId,
        app_id: &'static str,
        app_title: &'static str,
        wake: Option<ui_core::wake::WakeHandle>,
    ) -> Self {
        let mut state = Self::named(Some(records), me, Vec::new(), app_id, app_title, wake);
        if matches!(state.records, Some(PersonRecords::There(_))) {
            // The window's own client: the root carries its requests and answers.
            state.remote = Some(RemoteActions::current());
        }
        state
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
        records: Option<PersonRecords>,
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
            remote: None,
            opened: RefCell::default(),
            owners: Rc::default(),
            refreshers: RefCell::default(),
            published: RefCell::default(),
            unavailable: RefCell::default(),
        }
    }

    /// The record `P` the person keeps for themselves, in their own workspace.
    pub(crate) fn open_profile<P: Resource>(&self) -> Option<PersonResource<P>> {
        debug_assert!(
            collab_types::workspace_document_of_kind(P::KIND).is_none(),
            "{} is a record the workspace keeps: open it with `shared_workspace`",
            P::KIND
        );
        let kind = P::KIND;
        let key = (self.app_id, kind.to_owned());
        if let Some(opened) = self.opened.borrow().get(&key) {
            return Rc::clone(opened)
                .downcast::<PersonResource<P>>()
                .ok()
                .map(|person| (*person).clone());
        }
        let id = collab_types::person_document_id(kind);
        let of = PersonResourceType {
            kind,
            unreadable: &|reason| {
                tilo_resource::resource::unreadable_parts::<P>(
                    &resource_id(P::KIND),
                    P::LABEL,
                    reason,
                )
            },
        };
        match self.records.as_ref() {
            None => {
                let _ = self.note_unavailable(
                    self.app_id,
                    &of,
                    "this window keeps none of your records",
                );
                None
            }
            Some(PersonRecords::Unsigned) => {
                let reason = format!(
                    "Sign in to keep your {}: it is kept with your identity, and this device has none",
                    P::LABEL.to_lowercase()
                );
                let _ = self.note_unavailable(self.app_id, &of, &reason);
                None
            }
            Some(PersonRecords::Here(records)) => self.open_here::<P>(records, key, &id, &of),
            Some(PersonRecords::There(workspace)) => self.open_there::<P>(workspace, key, &id, &of),
        }
    }


    /// The record `P` in the person's own workspace, which this window is in: the app's record
    /// there, published with each invoker's undo and redo of it.
    fn open_here<P: Resource>(
        &self,
        records: &WorkspaceResources,
        key: OpenKey,
        id: &str,
        of: &PersonResourceType<'_>,
    ) -> Option<PersonResource<P>> {
        let kind = P::KIND;
        let resource = match WorkspaceResource::try_open_at(
            records,
            self.app_id,
            id,
            (self.app_id, self.app_title),
            None,
            self.wake.clone(),
        ) {
            Ok(resource) => Rc::new(resource),
            Err(why) => {
                let reason = match why {
                    Unopened::Arriving => "it has not reached this device yet".to_owned(),
                    Unopened::InUse => "it cannot be read right now".to_owned(),
                    Unopened::Failed => {
                        format!("it does not read as {}", P::LABEL.to_lowercase())
                    }
                };
                let _ = self.note_unavailable(self.app_id, of, &reason);
                return None;
            }
        };
        let held = held(&resource);
        let me = self.me.clone();
        let stepping = Rc::downgrade(&resource);
        let following = Rc::downgrade(&resource);
        self.owners
            .borrow_mut()
            .push(resource.record().history_owner());
        self.refreshers.borrow_mut().push(Box::new(move || {
            following
                .upgrade()
                .is_some_and(|resource| resource.refresh())
        }));
        let opened = PersonResource {
            held: Rc::clone(&held),
            me: self.me.clone(),
            backing: Backing::Own(resource),
        };
        self.publish(
            key,
            Rc::new(opened.clone()) as Rc<dyn Any>,
            Published {
                id: resource_id(kind),
                label: P::LABEL.to_owned(),
                run_by: None,
                api: Rc::clone(&held) as Rc<dyn ResourceApi>,
                step: Box::new(move |undo, by| {
                    stepping.upgrade().is_some_and(|resource| {
                        let stepped =
                            step_as(resource.record(), by.member_id().unwrap_or(&me), undo);
                        resource.refresh();
                        stepped
                    })
                }),
            },
        );
        Some(opened)
    }

    /// The record `P` in the person's own `workspace`, seen from a window in another one:
    /// followed there and changed by its actions at the app that keeps it.
    fn open_there<P: Resource>(
        &self,
        workspace: &str,
        key: OpenKey,
        id: &str,
        of: &PersonResourceType<'_>,
    ) -> Option<PersonResource<P>> {
        let kind = P::KIND;
        let Some(remote_actions) = self.remote.clone() else {
            let _ =
                self.note_unavailable(self.app_id, of, "this window reaches no workspace");
            return None;
        };
        // The record is the keeper's: this window reads a replica of it, and writes no
        // genesis of its own, which would meet the keeper's as a second document.
        let doc = ui_core::automerge::AutoCommit::new();
        let live = match tilo_resource::Reader::<P>::read(&doc) {
            Ok(reader) => Live::new(reader),
            Err(error) => {
                let reason = format!("it does not read: {error}");
                let _ = self.note_unavailable(self.app_id, of, &reason);
                return None;
            }
        };
        let followed = remote_actions.follow_resource(
            None,
            ResourceReadRequest {
                workspace: workspace.to_owned(),
                app: self.app_id.to_owned(),
                record: id.to_owned(),
                joined: false,
                after: None,
                wait_ms: None,
                answer: ReadAnswer::Document,
            },
        );
        let Some(document) = ui_core::resource::DocumentId::parse(id) else {
            let reason = format!("{kind} names no record");
            let _ = self.note_unavailable(self.app_id, of, &reason);
            return None;
        };
        let there = Rc::new(There {
            remote: Remote {
                workspace: workspace.to_owned(),
                app: self.app_id,
                id: document,
                actions: remote_actions,
                followed,
                doc: RefCell::new(doc),
                revision: RefCell::default(),
                sent: RefCell::default(),
            },
            state: LocalState::new(live.get()),
            live,
            wake: self.wake.clone(),
        });
        let provider = there.remote.provider();
        let held = held_there(&there, &self.me);
        let stepping = Rc::downgrade(&there);
        let following = Rc::downgrade(&there);
        let mine = self.me.clone();
        self.refreshers.borrow_mut().push(Box::new(move || {
            following.upgrade().is_some_and(|there| there.refresh())
        }));
        let opened = PersonResource {
            held: Rc::clone(&held),
            me: self.me.clone(),
            backing: Backing::There(there),
        };
        self.publish(
            key,
            Rc::new(opened.clone()) as Rc<dyn Any>,
            Published {
                id: resource_id(kind),
                label: P::LABEL.to_owned(),
                run_by: Some(provider),
                api: Rc::clone(&held) as Rc<dyn ResourceApi>,
                step: Box::new(move |undo, by| {
                    stepping.upgrade().is_some_and(|there| {
                        let held = step_held(&there, by.member_id().unwrap_or(&mine), undo);
                        if held {
                            there.remote.invoke(
                                &resource_id(P::KIND),
                                if undo { "undo" } else { "redo" },
                                serde_json::Map::new(),
                            );
                            there.wake();
                            there.refresh();
                        }
                        held
                    })
                }),
            },
        );
        Some(opened)
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
                run_by: None,
                api: Rc::clone(&api) as Rc<dyn ResourceApi>,
                step: Box::new(move |undo, by| {
                    stepping.upgrade().is_some_and(|api| api.step(undo, by))
                }),
            },
        );
        api
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
    ) -> Option<Rc<dyn Any>> {
        unreadable_person_resource(of.kind, &reason);
        // Only the record this app would have published is published for
        // the reason: one another app keeps and this one reads is that
        // app's surface, which registers the actions its own record has.
        if owner == self.app_id {
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
                let mut resource = published
                    .api
                    .publish()
                    .with_action(history_spec(&published.id, &published.label, true))
                    .with_action(history_spec(&published.id, &published.label, false));
                // Kept in the person's own workspace: this window still publishes and answers
                // what the record offers, sending each change to the app there, and that
                // workspace decides it.
                resource.workspace = published.run_by.as_deref().and_then(|provider| {
                    tilo_actions::remote::application_provider(provider)
                        .map(|(workspace, _)| workspace.to_owned())
                });
                resource
            })
            .collect()
    }

    /// The open records of the person's own, as they are published.
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

impl<P: Resource> super::resources::sealed::Hold<P> for PersonResource<P> {
    fn held(&self, _storage: super::resources::sealed::Storage<'_>) -> Option<Rc<Held<P>>> {
        Some(Rc::clone(&self.held))
    }

    fn api(
        &self,
        _storage: super::resources::sealed::Storage<'_>,
    ) -> Option<super::resources::sealed::Api> {
        Some(super::resources::sealed::Api(
            Rc::clone(&self.held) as Rc<dyn ResourceApi>
        ))
    }
}

/// Log that the person's record of `kind` does not read: one event site in
/// the runtime rather than one in every app for each record type.
#[cold]
#[inline(never)]
fn unreadable_person_resource(kind: &'static str, error: &dyn std::fmt::Display) {
    tracing::warn!(kind, %error, "person record unreadable");
}

/// Log that the person's own workspace refused a change to their record of `kind`: the app that
/// keeps it there decided, and the record shows what it holds.
#[cold]
#[inline(never)]
fn refused_person_change(kind: &'static str, action: &str, reason: &str) {
    tracing::warn!(
        kind,
        action,
        reason,
        "the person's own workspace refused a change"
    );
}

/// A person record's type, as the part of opening one that does not depend
/// on it reads it: compiled once, not into every app.
struct PersonResourceType<'a> {
    kind: &'static str,
    /// What the record publishes when it could not be opened, for why.
    unreadable: &'a dyn Fn(&str) -> tilo_actions::ResourceSpec,
}
