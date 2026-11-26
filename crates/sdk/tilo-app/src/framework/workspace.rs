//! The records every app of the window's workspace opens beside its own
//! ([`ui_core::resource::workspace`]): the workspace's emoji among them,
//! which any view of any app draws for `:name:`, and a conversation, which
//! the two people it is between open from either side.
//!
//! Such a record is its owner's resource: the workspace's grants on the owner's
//! node decide every change, here at the API as on the server, whichever
//! app's window makes it. The window opens it on first use, follows it
//! change by change, and reports its route so the runtime syncs it; only
//! the owner's own window publishes it (and lists it in its surface).

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use tilo_actions::{Action as _, ActionContext, ActionError, ActionInvoker, ChangeKind};
use tilo_resource::{Invoke, Resource};
use ui_core::LocalState;
use ui_core::arriving::{Arriving, Source};
use ui_core::history_mode::HistoryOwner;
use ui_core::permissions::{LivePermissions, PermissionScope, TreePath};
use ui_core::resource::StoredResource;
use ui_core::resource::workspace::WorkspaceResources;
use ui_core::wake::WakeHandle;
use ui_core::workspace_sync::RecordNotHere;

use super::live::Live;
use super::resources::{Held, ResourceApi};

/// A record every app of the window's workspace opens, held for the window
/// ([`ResourceEnv::workspace`](super::ResourceEnv::workspace)).
pub struct WorkspaceResource<P: Resource> {
    shared: Rc<Shared<P>>,
}

/// Opens the record `id` of `P` that one owner keeps, later, from a widget that outlives the
/// view that built it: a page's Files window opens each chunk of the workspace's tree as the
/// tree lists it ([`ResourceEnv::resource_opener`](super::ResourceEnv::resource_opener)).
pub struct ResourceOpener<P: Resource> {
    storage: Rc<super::settings::ViewStorage>,
    owner: &'static str,
    resource: std::marker::PhantomData<fn() -> P>,
}

impl<P: Resource> Clone for ResourceOpener<P> {
    fn clone(&self) -> Self {
        Self {
            storage: Rc::clone(&self.storage),
            owner: self.owner,
            resource: std::marker::PhantomData,
        }
    }
}

impl<P: Resource> ResourceOpener<P> {
    pub(crate) fn new(storage: Rc<super::settings::ViewStorage>, owner: &'static str) -> Self {
        Self {
            storage,
            owner,
            resource: std::marker::PhantomData,
        }
    }

    /// The record `id` of `P` that the owner keeps, opened on first use; `None` where the host
    /// keeps no records in a workspace (a view of a past version), for an id that names no
    /// record, or while the workspace has not delivered it.
    pub fn open(&self, id: &str) -> Option<WorkspaceResource<P>> {
        self.storage.shared_resource::<P>(self.owner, id)
    }

    /// The current member's private record in this workspace.
    pub fn open_own(&self) -> Option<WorkspaceResource<P>> {
        self.storage.own_workspace_resource::<P>(self.owner)
    }
}

impl<P: Resource> Clone for WorkspaceResource<P> {
    fn clone(&self) -> Self {
        Self {
            shared: Rc::clone(&self.shared),
        }
    }
}

/// What a held record is whatever its type: where it is kept, whose it is, the grants that
/// decide its changes and the edits that wait for its store. The typed record
/// ([`WorkspaceResource`]) hands its work here, so each record type an app opens compiles only
/// what reads its own state and builds its own actions.
struct Core {
    record: StoredResource,
    /// The app that keeps it: whose node the workspace's grants decide on.
    owner: &'static str,
    /// The app whose window holds it.
    app_id: &'static str,
    /// Where its changes are made, as their history names it.
    app_title: &'static str,
    me: tilo_resource::MemberId,
    permissions: Option<LivePermissions>,
    /// Wakes the window: for an action that waits for the record.
    wake: Option<WakeHandle>,
    /// The record's type, as its changes and the grants name it.
    kind: &'static str,
    label: &'static str,
    /// The verbs the record's type declares.
    declarations: fn() -> Vec<tilo_resource::Declaration>,
    /// Actions asked for while the record could not be changed (its store
    /// in use by whatever runs now), applied in order once it can.
    waiting: RefCell<Vec<Waiting>>,
    /// What the state shown was read with while edits waited: the record's
    /// heads and how many waited. The state then holds them, so what is read
    /// after an edit holds it although the store has not taken it yet.
    ahead: RefCell<Option<(Vec<ui_core::automerge::ChangeHash>, usize)>>,
}

/// One typed record's [`Core`] and its state, followed change by change.
struct Shared<P: Resource> {
    core: Core,
    /// The state, followed change by change.
    live: Live<P>,
    /// The same state as view state: a view that read it is built again
    /// when another app's or device's change arrives.
    state: LocalState<Rc<P::State>>,
}

/// An action that waits for its record ([`WorkspaceResource::invoke_then`]),
/// with what to do once it was applied or refused.
struct Waiting {
    action: Pending,
    /// Who it is by, as it was asked: it is applied later, outside the
    /// invocation it was asked in.
    invoker: ActionInvoker,
    then: Box<dyn FnOnce(Result<(), ActionError>)>,
    /// The window's refreshes it asked for: a few, so a record held
    /// throughout them waits for the window's next refresh of its own
    /// instead of keeping the window busy.
    woken: u8,
}

/// An action of the record's type as the record applies it: the action, and its encoded form
/// or why it has none.
pub(super) struct Pending {
    pub(super) action: Box<dyn tilo_actions::Action>,
    pub(super) encoded: Result<tilo_actions::EncodedAction, String>,
}

impl Pending {
    fn of<P: Resource>(action: P::Action) -> Self {
        let encoded = tilo_resource::encode::<P>(&action).map_err(|error| error.to_string());
        Self {
            action: Box::new(action),
            encoded,
        }
    }
}

/// How many refreshes an action that waits for its record asks for.
const WAITING_WAKES: u8 = 4;

/// What became of an action applied to the record.
enum Applied {
    Done(Result<(), ActionError>),
    /// The record cannot be changed right now; nothing ran.
    Waits,
}

impl Core {
    /// `read` the record's document with the edits waiting for its store
    /// made in a copy of it: what a read answers although the store has not
    /// taken them yet. The copy is let go here: the generic code that reads
    /// never holds a document, so a document's teardown is compiled once
    /// rather than in every app. Whether there was one.
    #[inline(never)]
    fn read_with_waiting(&self, read: &mut dyn FnMut(&ui_core::automerge::AutoCommit)) -> bool {
        let waiting = self.waiting.borrow();
        let Some(mut doc) = self.record.read(Clone::clone) else {
            return false;
        };
        for entry in waiting.iter() {
            if let Err(error) = self.apply_to(&mut doc, &entry.action, &entry.invoker) {
                tracing::warn!(kind = self.kind, %error, "a waiting edit does not apply");
            }
        }
        drop(waiting);
        read(&doc);
        true
    }

    /// Apply the actions that wait for the record, in order, while it can
    /// be changed: what still cannot waits on, asking the window for a
    /// refresh a few times.
    fn apply_waiting(&self) {
        let Ok(mut waiting) = self.waiting.try_borrow_mut() else {
            return;
        };
        if waiting.is_empty() {
            return;
        }
        let queue = std::mem::take(&mut *waiting);
        drop(waiting);
        let mut left = Vec::new();
        for entry in queue {
            // Order is kept: once one waits, the ones after it wait too.
            if !left.is_empty() {
                left.push(entry);
                continue;
            }
            match self.apply_as(&entry.action, &entry.invoker) {
                Applied::Done(result) => (entry.then)(result),
                Applied::Waits => left.push(entry),
            }
        }
        let Some(first) = left.first_mut() else {
            return;
        };
        let wake = first.woken < WAITING_WAKES;
        if wake {
            first.woken = first.woken.saturating_add(1);
        }
        let mut waiting = self.waiting.borrow_mut();
        left.append(&mut waiting);
        *waiting = left;
        drop(waiting);
        if wake && let Some(window) = &self.wake {
            window.wake();
        }
    }

    /// Keep `action` for the record's next free moment, then `then` with what became of it.
    fn wait(
        &self,
        action: Pending,
        invoker: ActionInvoker,
        then: Box<dyn FnOnce(Result<(), ActionError>)>,
    ) {
        self.waiting.borrow_mut().push(Waiting {
            action,
            invoker,
            then,
            woken: 0,
        });
        if let Some(window) = &self.wake {
            window.wake();
        }
    }

    /// Apply `action` as `invoker` (the person, an agent, a script), which
    /// the change is attributed to and which
    /// [`ActionCx::author`](tilo_resource::action) answers inside the action.
    fn apply_as(&self, action: &Pending, invoker: &ActionInvoker) -> Applied {
        let change = action.action.change_kind();
        let edited = self.record.try_edit(change == ChangeKind::Content, |doc| {
            self.apply_to(doc, action, invoker)
        });
        match edited {
            Ok(result) => Applied::Done(result.map_err(ActionError::Refused)),
            Err(unavailable) if unavailable.is_momentary() => Applied::Waits,
            Err(unavailable) => Applied::Done(Err(ActionError::Refused(unavailable.to_string()))),
        }
    }

    /// Make `action` one change of `doc` as `invoker`: what the record holds
    /// once its store takes the edit, and what a copy of one it waits in
    /// holds meanwhile.
    fn apply_to(
        &self,
        doc: &mut ui_core::automerge::AutoCommit,
        action: &Pending,
        invoker: &ActionInvoker,
    ) -> Result<(), String> {
        let encoded = action.encoded.clone()?;
        let change = action.action.change_kind();
        // Who the change is: the invoker's own actor (an agent acting in a turn, attributed as it
        // is at an app's record origin), or the person at this device for an invoker naming nobody.
        let invoker = if invoker.acting.by.id().is_empty() {
            ActionInvoker::member(self.me.clone())
        } else {
            invoker.clone()
        };
        let actor = invoker.acting.by.actor_bytes();
        // The change is the owner's action, whichever window or tab makes it: every receiver
        // decides it against the owner's surface and refuses a verb named under another app. The
        // title still says where it was made ("Workspace settings").
        let context = ActionContext::for_resource(self.kind, self.kind, self.label)
            .with_app(self.owner, self.app_title)
            .with_invoker(invoker)
            .with_change(change);
        let writer = ui_core::history_source::authors::device_actor(&actor, doc.get_actor());
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

    /// The record's id, which names it in the workspace.
    fn id(&self) -> &ui_core::resource::DocumentId {
        &self.record.checkout().id
    }

    fn may_read(&self) -> bool {
        if self.permissions.is_none()
            && tilo_actions::policy::roles::has_member_reader(self.owner, self.kind)
        {
            return false;
        }
        let node = tilo_actions::resource_node(
            &TreePath::root(),
            self.owner,
            self.kind,
            self.id().route(),
        );
        self.answer(&node, tilo_actions::ActionKind::Read) == tilo_actions::policy::Answer::Allow
    }

    fn answer(
        &self,
        node: &TreePath,
        kind: tilo_actions::ActionKind,
    ) -> tilo_actions::policy::Answer {
        use tilo_actions::policy::Answer;
        let invoker = ui_core::invocation_scope::current_invoker();
        let Some(permissions) = &self.permissions else {
            return if invoker.is_none() {
                Answer::Allow
            } else {
                Answer::Deny
            };
        };
        let current = permissions.current();
        let current = if let Some(invoker) = invoker {
            let Some(access) = current.access().invoked_by(&invoker.acting) else {
                return Answer::Deny;
            };
            current.with_access(access)
        } else {
            current
        };
        current.answer(node, kind)
    }

    fn independently_sealed(&self) -> bool {
        use tilo_actions::policy::roles;
        let (app, route) = (self.owner, self.id().route());
        collab_types::sealed_kind(app, route).is_some()
            || roles::has_member_reader(app, self.kind)
            || collab_types::decided_as_app_record(app, route)
    }

    fn may(&self, verb: &str) -> bool {
        let Some(declared) = (self.declarations)()
            .into_iter()
            .find(|decl| decl.id == verb)
        else {
            return self.permissions.is_none() && !self.independently_sealed();
        };
        let node = tilo_actions::action_node(
            &TreePath::root(),
            self.owner,
            self.kind,
            self.id().route(),
            declared.field,
            verb,
        );
        if self.independently_sealed() {
            return self.answer(&node, declared.kind) == tilo_actions::policy::Answer::Allow;
        }
        self.permissions.as_ref().is_none_or(|permissions| {
            permissions.check(&PermissionScope::Action {
                node,
                kind: declared.kind,
            })
        })
    }

    fn refused(&self) -> ActionError {
        ActionError::Refused(format!(
            "you may not change the workspace's {}",
            self.label.to_lowercase()
        ))
    }

    /// Refuse an action invoking `verb`, or else editing what `need` names, unless the
    /// workspace's grants let the window's person do it: a verb as [`Self::may`] decides it, a
    /// field edit at the field it changes.
    fn admitted(
        &self,
        verb: Option<String>,
        need: &tilo_actions::ActionNeed,
    ) -> Result<(), ActionError> {
        if let Some(verb) = verb {
            return if self.may(&verb) || self.keeps_its_own_housekeeping(&verb) {
                Ok(())
            } else {
                Err(self.refused())
            };
        }
        let node = tilo_actions::resource_node(
            &TreePath::root(),
            self.owner,
            self.kind,
            self.id().route(),
        )
        .field(need.field.as_deref().unwrap_or_default())
        .action(tilo_resource::FIELD_EDIT);
        let allowed = if self.independently_sealed() {
            self.answer(&node, need.kind) == tilo_actions::policy::Answer::Allow
        } else {
            self.permissions.as_ref().is_none_or(|permissions| {
                permissions.check(&PermissionScope::Action {
                    node,
                    kind: need.kind,
                })
            })
        };
        if allowed { Ok(()) } else { Err(self.refused()) }
    }

    /// Whether `verb` is one the record's keeper makes for itself and offers no caller — its
    /// tab's action log, a reading position: the keeper's own window makes it without a grant
    /// on it, as it keeps the record. Every change an app of the workspace asks for is decided
    /// by the grants.
    fn keeps_its_own_housekeeping(&self, verb: &str) -> bool {
        self.owner == self.app_id
            && !(self.declarations)()
                .into_iter()
                .any(|declared| declared.id == verb)
    }

    /// Apply `action` as `invoker` once the grants admitted it, at once or once the record's
    /// store is free.
    fn act_admitted(&self, action: Pending, invoker: &ActionInvoker) -> Applied {
        match self.apply_as(&action, invoker) {
            Applied::Waits => {
                self.wait(action, invoker.clone(), Box::new(|_| {}));
                Applied::Waits
            }
            done @ Applied::Done(_) => done,
        }
    }

    /// Apply History's verb whose encoded form is `payload`, which `action` is as the record's
    /// type applies it.
    fn apply_history(
        &self,
        payload: Vec<u8>,
        action: &dyn tilo_actions::Action,
        item: Option<&str>,
    ) -> Result<(), ActionError> {
        let encoded = tilo_actions::EncodedAction {
            type_id: super::history::EDIT_TYPE.to_owned(),
            payload,
        };
        let invoker = ActionInvoker::member(self.me.clone());
        let actor = invoker.acting.by.actor_bytes();
        let mut context = ActionContext::for_resource(self.kind, self.kind, self.label)
            .with_app(self.owner, self.app_title)
            .with_invoker(invoker)
            .with_change(action.change_kind());
        context.item = item.map(|item| tilo_actions::ActionItem {
            id: item.to_owned(),
            label: item.to_owned(),
        });
        let edited = self.record.try_edit(true, |doc| {
            let writer = ui_core::history_source::authors::device_actor(&actor, doc.get_actor());
            tilo_actions::apply_local_with_context(doc, action, encoded, Some(&writer), context)
                .map(|_| ())
                .map_err(|error| error.to_string())
        });
        match edited {
            Ok(result) => result.map_err(ActionError::Refused),
            Err(unavailable) => Err(ActionError::Refused(unavailable.to_string())),
        }
    }
}

/// The declarations of `P`, as [`Core`] reads them.
fn declarations<P: Resource>() -> Vec<tilo_resource::Declaration> {
    P::declarations()
        .iter()
        .map(|declared| tilo_resource::Declaration::clone(declared))
        .collect()
}

impl<P: Resource> Shared<P> {
    /// Follow the record when it moved. Whether it did; `None` when it could not be read now (its
    /// store in use, or its document not readable yet), and the state is what was last read.
    fn refresh_now(&self) -> Option<bool> {
        let core = &self.core;
        core.apply_waiting();
        let mut moved = match core.record.read(|doc| self.live.sync(doc)) {
            Some(Ok(moved)) => moved,
            Some(Err(error)) => {
                unreadable_workspace_record(P::KIND, &error);
                return None;
            }
            None => return None,
        };
        let _ = self.live.take_change();
        let waiting = core.waiting.borrow().len();
        if waiting > 0 {
            let read = (core.record.heads(), waiting);
            if core.ahead.borrow().as_ref() == Some(&read) {
                return Some(moved);
            }
            if let Some(state) = self.state_with_waiting() {
                self.state.set(state);
                *core.ahead.borrow_mut() = Some(read);
                return Some(true);
            }
        }
        if core.ahead.borrow_mut().take().is_some() || moved {
            self.state.set(self.live.get());
            moved = true;
        }
        Some(moved)
    }

    /// The state the record has once the edits waiting for its store are
    /// made: read from a copy of it they are made in, which is then let go.
    fn state_with_waiting(&self) -> Option<Rc<P::State>> {
        let mut state = None;
        self.core.read_with_waiting(&mut |doc| {
            state = match tilo_resource::Reader::<P>::read(doc) {
                Ok(reader) => Some(Rc::clone(reader.state())),
                Err(error) => {
                    unreadable_workspace_record(P::KIND, &error);
                    None
                }
            };
        });
        state
    }

    /// Follow the record when it moved. Whether it did.
    fn refresh(&self) -> bool {
        self.refresh_now().is_some_and(|moved| moved)
    }
}

impl<P: Resource> WorkspaceResource<P> {
    /// The record `P` that `owner` keeps in the workspace `records` opens,
    /// created with the resource's genesis where nothing holds it yet and
    /// brought to its version: what an app window holds
    /// ([`ResourceEnv::workspace`](super::ResourceEnv::workspace)), and what a
    /// host that is no app window (the session's workspace settings) opens
    /// the same way. Changes are attributed to `app` (its id and title) and,
    /// where the host knows the workspace's grants as `permissions`, decided by
    /// them first; `wake` is woken when another replica's change arrives.
    pub fn open(
        records: &WorkspaceResources,
        owner: &'static str,
        app: (&'static str, &'static str),
        permissions: Option<LivePermissions>,
        wake: Option<WakeHandle>,
    ) -> Option<Self> {
        Self::try_open(records, owner, app, permissions, wake).ok()
    }

    /// [`Self::open`], saying why nothing opened: [`Unopened::Arriving`] for a record the
    /// workspace has not delivered yet, whose store wakes its readers once it does.
    pub(crate) fn try_open(
        records: &WorkspaceResources,
        owner: &'static str,
        app: (&'static str, &'static str),
        permissions: Option<LivePermissions>,
        wake: Option<WakeHandle>,
    ) -> Result<Self, Unopened> {
        declare::<P>(records, owner);
        let opened = records.open_resource(owner, P::KIND, &|doc| {
            P::initialize(doc).map_err(|error| error.to_string())
        });
        Self::hold(opened, owner, app, permissions, wake, records)
    }

    /// [`Self::open`] of the record `id` of `kind` `P` that `owner` keeps:
    /// a record of the owner's that the workspace names otherwise than
    /// [`Self::open`]'s, such as a conversation's, which the two people it
    /// is between name. The workspace's grants decide its changes like any
    /// other.
    pub fn open_at(
        records: &WorkspaceResources,
        owner: &'static str,
        id: &str,
        app: (&'static str, &'static str),
        permissions: Option<LivePermissions>,
        wake: Option<WakeHandle>,
    ) -> Option<Self> {
        Self::try_open_at(records, owner, id, app, permissions, wake).ok()
    }

    /// [`Self::open_at`], saying why nothing opened, as [`Self::try_open`].
    pub(crate) fn try_open_at(
        records: &WorkspaceResources,
        owner: &'static str,
        id: &str,
        app: (&'static str, &'static str),
        permissions: Option<LivePermissions>,
        wake: Option<WakeHandle>,
    ) -> Result<Self, Unopened> {
        declare::<P>(records, owner);
        let opened = records.open_resource_at(owner, id, P::KIND, &|doc| {
            P::initialize(doc).map_err(|error| error.to_string())
        });
        Self::hold(opened, owner, app, permissions, wake, records)
    }

    /// [`Self::open_at`] of the record `owner` keeps as its own primary one of
    /// `P::KIND` in the workspace: the record an app keeps as itself (Chat's
    /// of its chat, `chat.workspace`), which `open_resource` cannot open because
    /// the package's primary id is a sentinel no [`DocumentId::parse`] accepts.
    /// An app's own window opens it this way; so does the session, reading the
    /// conversations its agents answer in.
    pub fn open_primary(
        records: &WorkspaceResources,
        owner: &'static str,
        app: (&'static str, &'static str),
        permissions: Option<LivePermissions>,
        wake: Option<WakeHandle>,
    ) -> Option<Self> {
        declare::<P>(records, owner);
        let opened = records.open_primary(owner, P::KIND, &|doc| {
            P::initialize(doc).map_err(|error| error.to_string())
        });
        Self::hold(opened, owner, app, permissions, wake, records).ok()
    }

    /// [`Self::open`]'s record, opened (or not) by the store it is kept in:
    /// brought to the resource's version, read, and followed change by
    /// change.
    fn hold(
        opened: Result<StoredResource, ui_core::resource::ResourceOpenError>,
        owner: &'static str,
        app: (&'static str, &'static str),
        permissions: Option<LivePermissions>,
        wake: Option<WakeHandle>,
        records: &WorkspaceResources,
    ) -> Result<Self, Unopened> {
        let me = records.author().clone();
        let record = opened_at_version(
            opened,
            (records, owner),
            P::KIND,
            tilo_resource::evolution::needs_migrate::<P>,
            tilo_resource::founding::bring_up_resource::<P>,
        )?;
        let reader = record
            .read(tilo_resource::Reader::<P>::read)
            .ok_or(Unopened::InUse)?
            .map_err(|error| {
                unreadable_workspace_record(P::KIND, &error);
                Unopened::Failed
            })?;
        if let Some(wake) = &wake {
            record.wake_on_arrival(wake.clone());
        }
        let live = Live::new(reader);
        Ok(Self {
            shared: Rc::new(Shared::<P> {
                core: Core {
                    record,
                    owner,
                    app_id: app.0,
                    app_title: app.1,
                    me,
                    permissions,
                    wake,
                    kind: P::KIND,
                    label: P::LABEL,
                    declarations: declarations::<P>,
                    waiting: RefCell::default(),
                    ahead: RefCell::default(),
                },
                state: LocalState::new(live.get()),
                live,
            }),
        })
    }

    /// The record's id, which names it in the workspace.
    #[must_use]
    pub fn id(&self) -> &ui_core::resource::DocumentId {
        self.shared.core.id()
    }

    /// The record's state as the window follows it, for the chain kept
    /// beside it ([`ResourceEnv::chain_beside`](super::ResourceEnv::chain_beside)).
    pub(crate) fn live(&self) -> Live<P> {
        self.shared.live.clone()
    }

    /// Read the record's document as it is now: what a reader needs beyond
    /// the resource's state, such as a text document's file, which a
    /// workspace's tree holds as a document of its own.
    pub fn read<T>(&self, read: impl FnOnce(&ui_core::automerge::AutoCommit) -> T) -> Option<T> {
        self.shared.core.record.read(read)
    }

    /// [`Self::read`] with the edits waiting for the record's store made: what
    /// was asked for a moment ago is read although the store has not taken it
    /// yet.
    pub(crate) fn read_with_waiting<T>(
        &self,
        read: impl FnOnce(&ui_core::automerge::AutoCommit) -> T,
    ) -> Option<T> {
        let mut read = Some(read);
        let mut answer = None;
        self.shared.core.read_with_waiting(&mut |doc| {
            answer = read.take().map(|read| read(doc));
        });
        answer
    }

    /// The current state.
    pub fn get(&self) -> Rc<P::State> {
        self.shared.refresh();
        self.shared.state.with(Rc::clone)
    }

    /// Read the current state.
    pub fn with<T>(&self, read: impl FnOnce(&P::State) -> T) -> T {
        read(&self.get())
    }

    /// The current state, or `None` when the record could not be read now: the state is then what
    /// was last read, not what the record says now. A caller that keeps what it last read asks
    /// this rather than [`Self::get`].
    pub fn try_get(&self) -> Option<Rc<P::State>> {
        self.shared.refresh_now()?;
        Some(self.shared.state.with(Rc::clone))
    }

    /// Increases whenever the state changes.
    pub fn revision(&self) -> u64 {
        self.shared.refresh();
        self.shared.live.revision()
    }

    /// The person who acts here.
    pub fn me(&self) -> &tilo_resource::MemberId {
        &self.shared.core.me
    }

    /// Whether the caller may read this record. A member's own record follows its
    /// authenticated owner; shared records follow the workspace's policy.
    #[must_use]
    pub fn may_read(&self) -> bool {
        self.shared.core.may_read()
    }

    /// Whether the workspace's grants let the person invoke the verb `verb` of
    /// the record, as its owner publishes it. A window that knows nothing
    /// of the workspace's grants (a test host) lets the record decide.
    pub fn may(&self, verb: &str) -> bool {
        self.shared.core.may(verb)
    }

    /// Apply the action `invoke` names: decided by the workspace's grants on the
    /// owner's node, then one change of the record, attributed to the
    /// person, in its history and one undo step of theirs.
    ///
    /// # Errors
    /// The arguments do not build the action, the grants refuse it, or the
    /// record does. Where the record's store is in use by whatever runs now
    /// (a pass of the view), the change waits for the window's next settle and
    /// is made then, in the order it was asked for.
    pub fn invoke(&self, invoke: &Invoke<P>) -> Result<(), ActionError> {
        self.invoke_as(&super::resources::acting(&self.shared.core.me), invoke)
    }

    /// [`Self::invoke`] as `invoker` (an agent, a script), which the change is attributed to —
    /// the agent's own words in Chat are written as the agent, not as the person it acts for.
    /// The grants are the ones this record was opened with, as for the person's own call.
    ///
    /// # Errors
    /// The arguments do not build the action, the grants refuse it, or the record does; a
    /// change asked for while its store is in use waits for the window's next settle.
    pub fn invoke_as(
        &self,
        invoker: &ActionInvoker,
        invoke: &Invoke<P>,
    ) -> Result<(), ActionError> {
        let scope = self
            .shared
            .core
            .independently_sealed()
            .then(|| ui_core::invocation_scope::InvokerScope::enter(invoker.clone()));
        let action = self.admit(invoke)?;
        drop(scope);
        self.applied(Pending::of::<P>(action), invoker)
    }

    /// Apply the typed action attributed to the current caller.
    ///
    /// # Errors
    /// The current grants or the record refuse the change.
    pub fn act(&self, action: &P::Action) -> Result<(), ActionError> {
        self.act_as(&super::resources::acting(&self.shared.core.me), action)
    }

    /// Apply the typed `action` as `invoker`, as [`Self::invoke_as`] applies the one its
    /// arguments build: the grants it was opened with decide it.
    ///
    /// # Errors
    /// The grants refuse it or the document does; a change asked for while its store is in
    /// use waits for the window's next settle.
    pub fn act_as(&self, invoker: &ActionInvoker, action: &P::Action) -> Result<(), ActionError> {
        let scope = self
            .shared
            .core
            .independently_sealed()
            .then(|| ui_core::invocation_scope::InvokerScope::enter(invoker.clone()));
        self.admitted(action)?;
        drop(scope);
        self.applied(Pending::of::<P>(action.clone()), invoker)
    }

    /// Apply the admitted `action` as `invoker`. Where its store is in use by whatever runs now
    /// (a pass of the view), the change lands in the window's next settle, in the order it was
    /// asked for, as an edit asked for in a pass does ([`Self::invoke_then`]).
    fn applied(&self, action: Pending, invoker: &ActionInvoker) -> Result<(), ActionError> {
        match self.shared.core.act_admitted(action, invoker) {
            Applied::Done(result) => {
                self.shared.refresh();
                result
            }
            Applied::Waits => Ok(()),
        }
    }

    /// [`Self::invoke`], and `then` with what became of it: at once, or,
    /// where the record cannot be changed right now (its store in use by
    /// whatever runs as this is asked), once it can, on a refresh of the
    /// window. Actions that wait are applied in the order they were asked
    /// for, after the ones that waited before them.
    pub fn invoke_then(
        &self,
        invoke: &Invoke<P>,
        then: impl FnOnce(Result<(), ActionError>) + 'static,
    ) {
        let action = match self.admit(invoke) {
            Ok(action) => action,
            Err(error) => return then(Err(error)),
        };
        let action = Pending::of::<P>(action);
        let core = &self.shared.core;
        let first = core
            .waiting
            .try_borrow()
            .is_ok_and(|waiting| waiting.is_empty());
        let invoker = super::resources::acting(&core.me);
        if first && let Applied::Done(result) = core.apply_as(&action, &invoker) {
            self.shared.refresh();
            return then(result);
        }
        core.wait(action, invoker, Box::new(then));
    }

    /// Apply History's verb `edit` (undo a step, restore a version) to the record as the
    /// window's person: one change, through the executor every action goes through, naming
    /// `item` as what it acted on. The caller decides first whether the grants let them.
    ///
    /// # Errors
    /// History's verb refused, or the record cannot be changed right now.
    pub fn apply_history(
        &self,
        edit: super::history::HistoryEdit,
        item: Option<&str>,
    ) -> Result<(), ActionError> {
        let payload = super::history::edit_payload(&edit).map_err(ActionError::Refused)?;
        let action = super::history::ResourceHistoryEdit::<P>::new(edit);
        let result = self.shared.core.apply_history(payload, &action, item);
        self.shared.refresh();
        result
    }

    /// Refuse `action` unless the workspace's grants let the window's person do it: a verb as
    /// [`Self::may`] decides it, a field edit at the field it changes. What a caller from
    /// outside asks for passes here before it is applied.
    fn admitted(&self, action: &P::Action) -> Result<(), ActionError> {
        self.shared
            .core
            .admitted(verb_of::<P>(action), &tilo_resource::need_of::<P>(action))
    }

    /// The action `invoke` names, once the workspace's grants admit it.
    fn admit(&self, invoke: &Invoke<P>) -> Result<P::Action, ActionError> {
        let action = invoke
            .build(tilo_resource::origin_now_ms())
            .map_err(ActionError::Refused)?;
        let core = &self.shared.core;
        if core.independently_sealed() {
            self.admitted(&action)?;
        } else if let Some(verb) = verb_of::<P>(&action)
            && !core.may(&verb)
            && !core.keeps_its_own_housekeeping(&verb)
        {
            return Err(core.refused());
        }
        Ok(action)
    }

    /// The record underneath, for a test or host that carries it between
    /// devices itself.
    #[must_use]
    pub fn record(&self) -> &StoredResource {
        &self.shared.core.record
    }
}

/// The stored resource `opened`, the record `owner` keeps in `records`' workspace, brought to the
/// resource of `kind`'s stored version: what every kind does before it reads what it holds, so
/// it is compiled once rather than once per kind.
fn opened_at_version(
    opened: Result<StoredResource, ui_core::resource::ResourceOpenError>,
    (records, owner): (&WorkspaceResources, &str),
    kind: &'static str,
    needs_migrate: fn(&tilo_resource::Doc) -> bool,
    migrate: fn(&str, &str, &mut tilo_resource::Doc) -> Result<bool, tilo_resource::ModelError>,
) -> Result<StoredResource, Unopened> {
    let stored = match opened {
        Ok(stored) => stored,
        // Nothing is wrong: the store wakes the window once it is here.
        Err(ui_core::resource::ResourceOpenError::Arriving) => return Err(Unopened::Arriving),
        // Nor here: whatever has the store now lets go of it.
        Err(ui_core::resource::ResourceOpenError::Busy) => return Err(Unopened::InUse),
        Err(error) => {
            tracing::warn!(kind, %error, "the workspace's resource is unavailable");
            return Err(Unopened::Failed);
        }
    };
    let id = stored.checkout().id.route().to_owned();
    if stored.read(needs_migrate).ok_or(Unopened::InUse)?
        && let Err(error) = records
            .store_of(owner)
            .map(|store| store.workspace())
            .map_err(|error| error.to_string())
            .and_then(|workspace| {
                stored.edit(false, |doc| {
                    migrate(&workspace, &id, doc).map_err(|error| error.to_string())
                })
            })
    {
        tracing::warn!(kind, %error, "the workspace's resource did not migrate");
    }
    Ok(stored)
}

/// What `shared` holds as the keeping app's window offers it to callers from outside, under
/// `id`, beside the app's own resource: its read, and each action once the workspace's grants
/// admit it ([`WorkspaceResource::admitted`]), applied as whoever asked.
fn offered<P: Resource>(shared: &Rc<Shared<P>>, id: String) -> Held<P> {
    let following = Rc::downgrade(shared);
    let applying = Rc::downgrade(shared);
    let reading = Rc::downgrade(shared);
    Held::new(
        id,
        P::LABEL.to_owned(),
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
                .ok_or_else(|| ActionError::Refused("the document is closed".into()))?;
            WorkspaceResource { shared }.act_as(invoker, action)
        },
    )
    .decided_by_grants()
    .reading(move |read| {
        if let Some(shared) = reading.upgrade() {
            let document = WorkspaceResource { shared };
            if !document.shared.core.independently_sealed() || document.may_read() {
                document.read_with_waiting(|doc| read(doc));
            }
        }
    })
}

/// The verb `action` invokes, as its record publishes it and history names
/// it (what a grant on one verb covers); `None` for a field edit.
pub(crate) fn verb_of<P: Resource>(action: &P::Action) -> Option<String> {
    // A record of no workspace's, made only to name what the action does.
    let doc = tilo_resource::founding::new_record::<P>("", "").ok()?;
    match action.describe(&doc)?.what {
        tilo_actions::Did::Verb(call) => Some(call.action.action),
        _ => None,
    }
}

/// A record open in a window: its owner and kind, and which record of them
/// (the workspace's one of the kind, or a pair's).
type OpenKey = (&'static str, &'static str, String);

/// Why a workspace record did not open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unopened {
    /// The workspace has not delivered it yet: its store wakes the window once it has, or once
    /// the workspace answers that it holds none.
    Arriving,
    /// Its store was in use: a pass of the view reads the app's store, which the workspace's
    /// records share, and a record asked for then opens once the pass let go of it.
    InUse,
    /// It cannot be opened, as the warning logged says.
    Failed,
}

/// Asks a window's [`WorkspaceState`] once more for one document that was on its way: whether
/// it opened.
type AskAgain = Rc<dyn Fn(&WorkspaceState) -> bool>;

/// A document on its way ([`WorkspaceState::unopened`]): what had arrived in its stores when it
/// was asked for, or none when whatever held the store then lets go without anything arriving,
/// so it is asked again at the next chance; how to ask for it again, and its type's name.
type OnItsWay = (Option<u64>, AskAgain, fn() -> String);

/// How the keeping app's window offers a record it opened: as its resource under the id it is
/// handed, and whether the caller may read it.
struct Publishing {
    offer: Box<dyn FnOnce(String) -> Rc<dyn ResourceApi>>,
    readable: Box<dyn Fn() -> bool>,
}

struct PublishedWorkspaceResource {
    api: Rc<dyn ResourceApi>,
    readable: Box<dyn Fn() -> bool>,
}

/// The records every app of the workspace opens that a window's views opened:
/// one [`WorkspaceResource`] per kind.
pub(crate) struct WorkspaceState {
    records: WorkspaceResources,
    app_id: &'static str,
    app_title: &'static str,
    permissions: Option<LivePermissions>,
    /// Wakes the window when another replica's change arrives.
    wake: Option<WakeHandle>,
    opened: RefCell<HashMap<OpenKey, Rc<dyn Any>>>,
    /// Resources still on their way from the workspace when a view asked for them, with what had
    /// arrived in their stores then ([`Self::arrivals`]): each is tried again once something
    /// arrives, not on every draw that asks for it. With each, how to ask for it once more:
    /// the window's refresh does when something arrived ([`Self::open_arrived`]), since a
    /// declaration asks once and a view only when it is built. With each, its type's name, as a
    /// window that waits for it too long says ([`Self::end_unanswered`]).
    unopened: RefCell<HashMap<OpenKey, OnItsWay>>,
    owners: RefCell<Vec<HistoryOwner>>,
    refreshers: RefCell<Vec<Box<dyn Fn() -> bool>>>,
    /// What this app keeps for its whole workspace, as its window offers it to callers from
    /// outside ([`offered`]): none in a window of any other app.
    published: RefCell<Vec<PublishedWorkspaceResource>>,
    /// The parts of the window's views being built now, the innermost last
    /// ([`Self::building`]).
    building: RefCell<Vec<Weak<()>>>,
    /// The parts whose build asked for a document that was on its way, each with that
    /// document: what it read then was "not here", so it is built again once the document
    /// opens ([`Self::take_arrived`]).
    asked: RefCell<Vec<(OpenKey, Weak<()>)>>,
    /// The parts to build again: what they asked for has opened since they were built.
    arrived: RefCell<Vec<Weak<()>>>,
    /// What the window's view waits for before it is built: the documents its app declared
    /// that were on its way when the window opened, answered once every one of them opened,
    /// or ended unanswered ([`Self::declared_arriving`]).
    awaited: RefCell<Vec<Awaited>>,
    /// Since when each record asked for has not opened, why not the last time it was asked
    /// for, and whether that was said ([`Self::waiting_since`]).
    waiting: RefCell<HashMap<OpenKey, (ui_core::time::Instant, Unopened, bool)>>,
}

/// How long a record a window asked for goes unopened before the log says so, and a window
/// that waits for it, the workspace's holdings settled, says it has not arrived
/// ([`WorkspaceState::end_unanswered`]).
const LONG_WAIT: ui_core::time::Duration = ui_core::time::Duration::from_secs(15);

/// What a window's view waits for before it is built ([`WorkspaceState::declared_arriving`]).
struct Awaited {
    /// The documents it waits for.
    keys: Vec<OpenKey>,
    /// What the view shows meanwhile: answered once they all opened.
    arriving: Arriving<(), RecordNotHere>,
    /// Since when it waits with the workspace's holdings settled.
    since: ui_core::time::Instant,
    /// Whether it ended unanswered: the person's retry asks again from when it is pending
    /// once more.
    ended: bool,
    /// Whether the person asked again: it then waits its bound out, also for holders that
    /// could not be reached, as the shell dials them again meanwhile.
    asked_again: bool,
}

/// Where the workspace's holdings stand, for a window's declared wait
/// ([`WorkspaceState::end_unanswered`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Holdings {
    /// On their way: what the window waits for may come with them.
    Arriving,
    /// Here: what is still on its way comes from a device that holds it.
    Here,
    /// Not coming: the workspace's holders cannot be reached, or it is unavailable.
    Unreachable,
}

impl Holdings {
    fn now() -> Self {
        use ui_core::arriving::Arrival;
        use ui_core::workspace_sync::WorkspaceSyncState;
        match ui_core::workspace_sync::holdings().get() {
            Arrival::Pending { .. } => Self::Arriving,
            Arrival::Failed(_) => Self::Unreachable,
            Arrival::Ready(()) => match ui_core::workspace_sync::current() {
                Some(sync) if sync.state == WorkspaceSyncState::Unreachable => Self::Unreachable,
                _ => Self::Here,
            },
        }
    }
}

/// A part of a view being built ([`WorkspaceState::building`]), until this is dropped.
pub(crate) struct Building(Rc<WorkspaceState>);

impl Drop for Building {
    fn drop(&mut self) {
        self.0.building.borrow_mut().pop();
    }
}

impl WorkspaceState {
    pub(crate) fn new(
        records: WorkspaceResources,
        app_id: &'static str,
        app_title: &'static str,
        permissions: Option<LivePermissions>,
        wake: Option<WakeHandle>,
    ) -> Self {
        Self {
            records,
            app_id,
            app_title,
            permissions,
            wake,
            opened: RefCell::default(),
            unopened: RefCell::default(),
            owners: RefCell::default(),
            refreshers: RefCell::default(),
            published: RefCell::default(),
            building: RefCell::default(),
            asked: RefCell::default(),
            arrived: RefCell::default(),
            awaited: RefCell::default(),
            waiting: RefCell::default(),
        }
    }

    /// The documents asked for so far that are still on their way, as one arrival that is
    /// answered once every one of them opened: none when all of them are here. A window asks
    /// right after its app's declarations opened what they keep, and shows its view once this
    /// is answered: a view built once reads a declared document once, and one built while the
    /// document was on its way would go on reading that the workspace has none.
    ///
    /// It ends like the workspace's holdings do: once those cannot arrive, or once no device that
    /// holds a document answered within [`LONG_WAIT`], the window says what it waits for and
    /// offers to go on without it, the view reading it as missing, or to ask again
    /// ([`Self::end_unanswered`]).
    pub(crate) fn declared_arriving(&self) -> Option<Arriving<(), RecordNotHere>> {
        let keys: Vec<OpenKey> = self.unopened.borrow().keys().cloned().collect();
        if keys.is_empty() {
            return None;
        }
        let arriving = Arriving::pending(Source::Remote)
            .on_retry(|arriving| {
                // The shell dials the workspace's holders again; the window's next refresh
                // asks for each document once more.
                ui_core::workspace_sync::retry();
                arriving.writer().pending(Source::Remote);
            })
            .on_continue_without(|arriving| arriving.writer().ready(()))
            .failure_as_notice(RecordNotHere::title);
        self.awaited.borrow_mut().push(Awaited {
            keys,
            arriving: arriving.clone(),
            since: ui_core::time::now(),
            ended: false,
            asked_again: false,
        });
        Some(arriving)
    }

    /// When a declared wait ends unanswered unless what it waits for opens first
    /// ([`Self::end_unanswered`]): what wakes a window that nothing else wakes meanwhile.
    pub(crate) fn gives_up_at(&self) -> Option<ui_core::time::Instant> {
        self.awaited
            .borrow()
            .iter()
            .filter(|wait| wait.arriving.with(ui_core::arriving::Arrival::is_pending))
            .filter_map(|wait| wait.since.checked_add(LONG_WAIT))
            .min()
    }

    /// End each declared wait that can no longer be answered: the workspace's holdings cannot
    /// arrive, or no device that holds what it waits for answered within [`LONG_WAIT`] of them
    /// settling. The window then says what it waits for, and offers to go on without it
    /// ([`Self::declared_arriving`]). A wait the person went on from is let go; one they asked
    /// again for waits anew, each of its documents asked for once more.
    fn end_unanswered(&self) {
        if self.awaited.borrow().is_empty() {
            return;
        }
        let now = ui_core::time::now();
        let holdings = Holdings::now();
        let mut asked_again = Vec::new();
        let mut ended = Vec::new();
        {
            let mut awaited = self.awaited.borrow_mut();
            awaited.retain(|wait| wait.arriving.with(|arrival| arrival.ready().is_none()));
            for wait in awaited.iter_mut() {
                if !wait.arriving.with(ui_core::arriving::Arrival::is_pending) {
                    continue;
                }
                if wait.ended {
                    wait.ended = false;
                    wait.asked_again = true;
                    wait.since = now;
                    asked_again.extend(wait.keys.iter().cloned());
                }
                let waited_out = now.saturating_duration_since(wait.since) >= LONG_WAIT;
                let ends = match holdings {
                    Holdings::Arriving => {
                        wait.since = now;
                        false
                    }
                    Holdings::Unreachable => !wait.asked_again || waited_out,
                    Holdings::Here => waited_out,
                };
                if ends {
                    wait.ended = true;
                    ended.push((wait.keys.clone(), wait.arriving.writer()));
                }
            }
        }
        for (keys, writer) in ended {
            writer.fail(self.not_here(&keys));
        }
        let again: Vec<AskAgain> = {
            let unopened = self.unopened.borrow();
            asked_again
                .iter()
                .filter_map(|key| unopened.get(key).map(|(_, again, _)| Rc::clone(again)))
                .collect()
        };
        for again in again {
            again(self);
        }
    }

    /// What a window that waited too long for `keys` says it waits for: the first of them still
    /// on its way, by its type's name, and the app that keeps it: the window's own, or another
    /// app installed here, by the name the session sent for it.
    fn not_here(&self, keys: &[OpenKey]) -> RecordNotHere {
        let unopened = self.unopened.borrow();
        let waited = keys
            .iter()
            .find_map(|key| unopened.get(key).map(|(_, _, name)| (key, name)));
        let (owner, record) = match waited {
            Some((key, name)) => (key.0, name()),
            None => (self.app_id, self.app_title.to_owned()),
        };
        RecordNotHere {
            record,
            keeper: if owner == self.app_id {
                Some(self.app_title.to_owned())
            } else {
                ui_core::history_source::installed_app_name(owner)
            },
        }
    }

    /// The document `key` arrived but could not be read, which is logged: the view waiting for
    /// it is built without it rather than waiting for good.
    fn no_longer_awaited(&self, key: &OpenKey) {
        for wait in self.awaited.borrow_mut().iter_mut() {
            wait.keys.retain(|waited| waited != key);
        }
        self.answer_awaited();
    }

    /// `key` did not open, for `why`.
    fn waiting_since(&self, key: &OpenKey, why: Unopened) {
        let now = ui_core::time::now();
        let mut waiting = self.waiting.borrow_mut();
        let (_, last, _) = waiting.entry(key.clone()).or_insert((now, why, false));
        *last = why;
    }

    /// Say, once each, which records asked for have not opened for [`LONG_WAIT`], and why: a
    /// window that waits for one shows its arriving page all that time.
    fn say_long_waits(&self) {
        let now = ui_core::time::now();
        for (key, (since, why, said)) in self.waiting.borrow_mut().iter_mut() {
            if !*said && now.saturating_duration_since(*since) >= LONG_WAIT {
                *said = true;
                tracing::warn!(
                    owner = key.0,
                    kind = key.1,
                    record = key.2,
                    ?why,
                    "a workspace record a window asked for has not opened"
                );
            }
        }
    }

    /// Answer each wait whose documents all opened ([`Self::declared_arriving`]), also one that
    /// ended unanswered; one the person went on from without them is let go as it is: its view
    /// is built, and builds again the parts that read what opened.
    fn answer_awaited(&self) {
        let unopened = self.unopened.borrow();
        self.awaited.borrow_mut().retain(|wait| {
            let waiting = wait.keys.iter().any(|key| unopened.contains_key(key));
            if !waiting && wait.arriving.with(|arrival| arrival.ready().is_none()) {
                wait.arriving.writer().ready(());
            }
            waiting
        });
    }

    /// The part of a view kept alive by `part` is being built, until what this returns is
    /// dropped. A document it asks for meanwhile that is still on its way from the workspace
    /// is one it reads as not here, so it is built again, it alone, once that document opens
    /// ([`Self::take_arrived`]): nothing else would show the part what arrived, since a part
    /// reads when it is built.
    pub(crate) fn building(self: &Rc<Self>, part: &Rc<()>) -> Building {
        self.building.borrow_mut().push(Rc::downgrade(part));
        Building(Rc::clone(self))
    }

    /// Whether the part kept alive by `part` asked, while it was built, for a document that
    /// was on its way and has opened since: answered once, to the part, which builds itself
    /// again. A window in which nothing arrived answers every part from an empty list.
    pub(crate) fn take_arrived(&self, part: &Rc<()>) -> bool {
        let mut arrived = self.arrived.borrow_mut();
        if arrived.is_empty() {
            return false;
        }
        let this = Rc::downgrade(part);
        let mut found = false;
        // A part that went before it was built again is forgotten here.
        arrived.retain(|waiting| {
            if waiting.ptr_eq(&this) {
                found = true;
                return false;
            }
            waiting.strong_count() > 0
        });
        found
    }

    /// The part being built now asked for the document `key`, which is on its way.
    fn asked_while_building(&self, key: &OpenKey) {
        let Some(part) = self.building.borrow().last().cloned() else {
            return;
        };
        let mut asked = self.asked.borrow_mut();
        asked.retain(|(_, waiting)| waiting.strong_count() > 0);
        if !asked
            .iter()
            .any(|(wanted, waiting)| wanted == key && waiting.ptr_eq(&part))
        {
            asked.push((key.clone(), part));
        }
    }

    /// The document `key` opened: the parts that asked for it while it was on its way are
    /// built again in the window's next frame, which is asked for.
    fn opened_for_parts(&self, key: &OpenKey) {
        let mut asked = self.asked.borrow_mut();
        if asked.is_empty() {
            return;
        }
        let mut arrived = self.arrived.borrow_mut();
        let known = arrived.len();
        asked.retain(|(wanted, waiting)| {
            if wanted != key {
                return true;
            }
            if waiting.strong_count() > 0 && !arrived.iter().any(|due| due.ptr_eq(waiting)) {
                arrived.push(waiting.clone());
            }
            false
        });
        let due = arrived.len() > known;
        drop(arrived);
        drop(asked);
        if due && let Some(window) = &self.wake {
            window.wake();
        }
    }

    /// What wakes the window: for an answer that arrives later, such as a
    /// chooser's.
    pub(crate) fn wake(&self) -> Option<&WakeHandle> {
        self.wake.as_ref()
    }

    /// The window's app.
    pub(crate) fn app_id(&self) -> &'static str {
        self.app_id
    }

    /// The record `P` that `owner` keeps in the workspace, opened on first use
    /// and created with the resource's genesis where it does not exist yet.
    pub(crate) fn open<P: Resource>(&self, owner: &'static str) -> Option<WorkspaceResource<P>> {
        let id = ui_core::resource::workspace::document_id(P::KIND);
        self.open_with::<P>(owner, &id, |records| {
            WorkspaceResource::try_open(
                records,
                owner,
                (self.app_id, self.app_title),
                self.permissions.clone(),
                self.wake.clone(),
            )
        })
    }

    /// A member's own live record, held beside the app's other records in the same store.
    pub(crate) fn open_own<P: Resource>(
        &self,
        owner: &'static str,
    ) -> Option<WorkspaceResource<P>> {
        if !tilo_actions::policy::roles::has_member_reader(owner, P::KIND) {
            return None;
        }
        let person = self.records.author().person_id()?;
        let id = tilo_actions::policy::roles::member_record(P::KIND, person)?;
        self.open_at::<P>(owner, &id)
    }

    /// The record `id` of `P` that `owner` keeps, opened on first use and
    /// created with the resource's genesis where it does not exist yet: a
    /// conversation's record, named by the two people it is between.
    pub(crate) fn open_at<P: Resource>(
        &self,
        owner: &'static str,
        id: &str,
    ) -> Option<WorkspaceResource<P>> {
        self.open_with::<P>(owner, id, |records| {
            WorkspaceResource::try_open_at(
                records,
                owner,
                id,
                (self.app_id, self.app_title),
                self.permissions.clone(),
                self.wake.clone(),
            )
        })
    }

    /// [`Self::open`]'s and [`Self::open_at`]'s record, opened once per
    /// window: held for the views that ask again, and refreshed with them.
    fn open_with<P: Resource>(
        &self,
        owner: &'static str,
        id: &str,
        open: impl FnOnce(&WorkspaceResources) -> Result<WorkspaceResource<P>, Unopened>,
    ) -> Option<WorkspaceResource<P>> {
        let key = (owner, P::KIND, id.to_owned());
        if let Some(open) = self.opened.borrow().get(&key) {
            return Rc::clone(open)
                .downcast::<Shared<P>>()
                .ok()
                .map(|shared| WorkspaceResource { shared });
        }
        // The owner's own window lists the record in its surface: its
        // changes are decided against the owner's surface.
        if owner == self.app_id {
            super::controls::opened_resource::<P>();
        }
        let arrived = self.asks_again(&key)?;
        match open(&self.records) {
            Ok(opened) => {
                self.opened_now(&key);
                Some(self.held(key, opened))
            }
            Err(why) => {
                let again = matches!(why, Unopened::Arriving | Unopened::InUse)
                    .then(|| Self::asking_again::<P>(owner, id));
                self.not_opened(key, why, arrived, again, P::label);
                None
            }
        }
    }

    /// What arrived for the record `key` so far, when the workspace is to be asked for it now:
    /// not when it was on its way the last time and nothing arrived for it since.
    fn asks_again(&self, key: &OpenKey) -> Option<u64> {
        let arrived = self.arrivals(key.0);
        if self
            .unopened
            .borrow()
            .get(key)
            .is_some_and(|(seen, _, _)| *seen == Some(arrived))
        {
            self.asked_while_building(key);
            return None;
        }
        Some(arrived)
    }

    /// The record `key` opened: what waited for it is answered.
    fn opened_now(&self, key: &OpenKey) {
        self.unopened.borrow_mut().remove(key);
        self.waiting.borrow_mut().remove(key);
        self.answer_awaited();
        self.opened_for_parts(key);
    }

    /// The record `key` did not open, for `why`, with `arrived` arrived for it when it was asked
    /// for: one on its way is asked for `again` once it can open, and a window that waits for it
    /// too long names it by `label`.
    fn not_opened(
        &self,
        key: OpenKey,
        why: Unopened,
        arrived: u64,
        again: Option<AskAgain>,
        label: fn() -> String,
    ) {
        match (why, again) {
            (Unopened::Arriving, Some(again)) => {
                // Its store tells the window when something arrives in it, also for what
                // never opened here: nothing else would, and the window would go on showing
                // that the workspace has none until something else woke it.
                if let (Some(window), Ok(store)) = (&self.wake, self.records.store_of(key.0)) {
                    store.wake_on_arrival(window.clone());
                }
                self.asked_while_building(&key);
                self.waiting_since(&key, Unopened::Arriving);
                self.unopened
                    .borrow_mut()
                    .insert(key, (Some(arrived), again, label));
            }
            (Unopened::InUse, Some(again)) => {
                // Asked again in the window's next refresh, which is asked for, and whenever a
                // view asks for it before then: the store lets go when what reads it now is done,
                // and nothing arrives to say so. Marking it with what arrives next instead would
                // pass over the very arrival that answers it (the workspace holding none).
                self.asked_while_building(&key);
                self.waiting_since(&key, Unopened::InUse);
                self.unopened.borrow_mut().insert(key, (None, again, label));
                if let Some(window) = &self.wake {
                    window.wake();
                }
            }
            _ => {
                self.waiting.borrow_mut().remove(&key);
                self.no_longer_awaited(&key);
            }
        }
    }

    /// How to ask once more for the document `id` of `P` that `owner` keeps, which was on its
    /// way when a view or a declaration asked for it.
    fn asking_again<P: Resource>(owner: &'static str, id: &str) -> AskAgain {
        let id = id.to_owned();
        Rc::new(move |state: &Self| {
            if id == ui_core::resource::workspace::document_id(P::KIND) {
                state.open::<P>(owner).is_some()
            } else {
                state.open_at::<P>(owner, &id).is_some()
            }
        })
    }

    /// Whether a document asked for is still on its way ([`Self::open_arrived`] opens it).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn opening(&self) -> bool {
        !self.unopened.borrow().is_empty()
    }

    /// Open what was on its way when it was asked for, once something arrived for it: the
    /// workspace delivered it, or answered that it holds none of it, so it is founded here.
    /// The window is woken for what opened, so that what shows it is built with it. Whether
    /// anything opened. A window does this in its refresh; a view no window shows is never
    /// refreshed, and whoever holds it asks for this when it told the view what arrived.
    pub(crate) fn open_arrived(&self) -> bool {
        self.say_long_waits();
        self.end_unanswered();
        let due: Vec<AskAgain> = {
            let unopened = self.unopened.borrow();
            if unopened.is_empty() {
                return false;
            }
            unopened
                .iter()
                .filter(|((owner, _, _), (seen, _, _))| Some(self.arrivals(owner)) != *seen)
                .map(|(_, (_, again, _))| Rc::clone(again))
                .collect()
        };
        let opened = due.iter().fold(false, |opened, again| again(self) | opened);
        if opened && let Some(window) = &self.wake {
            window.wake();
        }
        opened
    }

    /// What arrived so far in the stores a document of `owner` opens in, the owner's and this
    /// app's: it moves whenever the workspace delivers a record to either, or answers that it
    /// holds none of one asked for ([`ui_core::resource::ResourceStore::kept_revision`]).
    fn arrivals(&self, owner: &str) -> u64 {
        // A store that does not open yet counts as nothing arrived, so that its opening is
        // an arrival too.
        let kept = |owner: &str| {
            self.records
                .store_of(owner)
                .map_or(0, |store| store.kept_revision().wrapping_add(1))
        };
        let own = kept(owner);
        if owner == self.app_id {
            return own;
        }
        own.wrapping_add(kept(self.app_id).rotate_left(32))
    }

    /// Keep an opened record for the window: its route reported to the
    /// runtime, its changes followed, and the record held for the views that
    /// ask for it again.
    fn held<P: Resource>(
        &self,
        key: OpenKey,
        opened: WorkspaceResource<P>,
    ) -> WorkspaceResource<P> {
        let WorkspaceResource { shared } = opened;
        let weak = Rc::downgrade(&shared);
        let refresh = Box::new(move || weak.upgrade().is_some_and(|shared| shared.refresh()));
        // The keeping app's own window offers it to callers from outside, beside its own
        // resource; every other app's window only opens it.
        let publishing = (key.0 == self.app_id).then(|| {
            let offering = Rc::clone(&shared);
            let resource = WorkspaceResource {
                shared: Rc::clone(&shared),
            };
            Publishing {
                offer: Box::new(move |id| Rc::new(offered(&offering, id)) as Rc<dyn ResourceApi>),
                readable: Box::new(move || {
                    !resource.shared.core.independently_sealed() || resource.may_read()
                }),
            }
        });
        self.keep(
            key,
            &shared.core,
            refresh,
            publishing,
            Rc::clone(&shared) as Rc<dyn Any>,
        );
        WorkspaceResource { shared }
    }

    /// [`Self::held`] for a record of any type: `refresh` follows it, and `publishing` offers it
    /// where this window's app keeps it.
    fn keep(
        &self,
        key: OpenKey,
        core: &Core,
        refresh: Box<dyn Fn() -> bool>,
        publishing: Option<Publishing>,
        held: Rc<dyn Any>,
    ) {
        self.owners.borrow_mut().push(core.record.history_owner());
        self.refreshers.borrow_mut().push(refresh);
        // The first resource of a kind is offered as the kind; each further one (a part of a
        // resource kept in several documents, as a workspace's tree is) as its own document, so
        // that no two share an address: two that do withdraw the window's every action.
        if let Some(Publishing { offer, readable }) = publishing {
            let kind_taken = self
                .published
                .borrow()
                .iter()
                .any(|entry| entry.api.id() == key.1);
            let id = if kind_taken {
                core.id().to_string()
            } else {
                key.1.to_owned()
            };
            self.published
                .borrow_mut()
                .push(PublishedWorkspaceResource {
                    api: offer(id),
                    readable,
                });
        }
        self.opened.borrow_mut().insert(key, held);
    }

    /// Read every open record that moved. Whether one did.
    pub(crate) fn refresh(&self) -> bool {
        let arrived = self.open_arrived();
        self.refreshers
            .borrow()
            .iter()
            .fold(arrived, |moved, refresh| refresh() | moved)
    }

    /// What this app keeps for its whole workspace, as the API offers and changes it.
    pub(crate) fn apis(&self) -> Vec<Rc<dyn ResourceApi>> {
        self.published
            .borrow()
            .iter()
            .filter(|resource| (resource.readable)())
            .map(|resource| Rc::clone(&resource.api))
            .collect()
    }

    /// The routes of the open records, which the runtime syncs.
    pub(crate) fn owners(&self) -> Vec<HistoryOwner> {
        self.owners.borrow().clone()
    }
}

/// Log that the workspace's record of `kind` does not read: one event site in
/// the runtime rather than one in every app for each record type.
#[cold]
#[inline(never)]
fn unreadable_workspace_record(kind: &'static str, error: &dyn std::fmt::Display) {
    tracing::warn!(kind, %error, "workspace record unreadable");
}

/// Found `owner`'s records of `P`'s kind by the type this app holds of it, the same on every
/// device. A store that cannot be had is not one a record is founded in either.
fn declare<P: Resource>(records: &WorkspaceResources, owner: &str) {
    if let Ok(store) = records.store_of(owner) {
        store.declare_type(P::KIND, P::surface);
    }
}

#[cfg(test)]
mod tests;
