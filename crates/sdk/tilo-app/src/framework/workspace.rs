//! The records every app of the window's workspace opens beside its own
//! ([`ui_core::record::workspace`]): the workspace's emoji among them,
//! which any view of any app draws for `:name:`, and a conversation, which
//! the two people it is between open from either side.
//!
//! Such a record is its owner's resource: the workspace's grants on the owner's
//! node decide every change, here at the door as on the relay, whichever
//! app's window makes it. The window opens it on first use, follows it
//! change by change, and reports its route so the runtime syncs it; only
//! the owner's own window publishes it (and lists it in its surface).

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_actions::{Action as _, ActionContext, ActionError, ActionInvoker, ChangeKind};
use tilo_resource::{Invoke, Resource};
use ui_core::LocalState;
use ui_core::history_mode::HistoryOwner;
use ui_core::permissions::{LivePermissions, PermissionScope, TreePath};
use ui_core::record::StoredRecord;
use ui_core::record::workspace::WorkspaceRecords;
use ui_core::wake::WakeHandle;

use super::live::Live;

use super::chain::{Chain, Chained};

/// A record every app of the window's workspace opens, held for the window
/// ([`RecordEnv::workspace`](super::RecordEnv::workspace)).
pub struct WorkspaceRecord<P: Resource> {
    shared: Rc<Shared<P>>,
}

impl<P: Resource> Clone for WorkspaceRecord<P> {
    fn clone(&self) -> Self {
        Self {
            shared: Rc::clone(&self.shared),
        }
    }
}

struct Shared<P: Resource> {
    record: StoredRecord,
    /// The app that keeps it: whose node the workspace's grants decide on.
    owner: &'static str,
    app_id: &'static str,
    app_title: &'static str,
    me: String,
    permissions: Option<LivePermissions>,
    /// The state, followed change by change.
    live: Live<P>,
    /// The same state as view state: a view that read it is built again
    /// when another app's or device's change arrives.
    state: LocalState<Rc<P::State>>,
    /// Wakes the window: for an action that waits for the record.
    wake: Option<WakeHandle>,
    /// Actions asked for while the record could not be changed (its store
    /// in use by whatever runs now), applied in order once it can.
    waiting: RefCell<Vec<Waiting<P>>>,
}

/// An action that waits for its record ([`WorkspaceRecord::invoke_then`]),
/// with what to do once it was applied or refused.
struct Waiting<P: Resource> {
    action: P::Action,
    then: Box<dyn FnOnce(Result<(), ActionError>)>,
    /// The window's refreshes it asked for: a few, so a record held
    /// throughout them waits for the window's next refresh of its own
    /// instead of keeping the window busy.
    woken: u8,
}

/// How many refreshes an action that waits for its record asks for.
const WAITING_WAKES: u8 = 4;

/// What became of an action applied to the record.
enum Applied {
    Done(Result<(), ActionError>),
    /// The record cannot be changed right now; nothing ran.
    Waits,
}

impl<P: Resource> Shared<P> {
    /// Follow the record when it moved. Whether it did.
    fn refresh(&self) -> bool {
        self.apply_waiting();
        let moved = match self.record.read(|doc| self.live.sync(doc)) {
            Some(Ok(moved)) => moved,
            Some(Err(error)) => {
                tracing::warn!(kind = P::KIND, %error, "workspace record unreadable");
                false
            }
            None => false,
        };
        let _ = self.live.take_change();
        if moved {
            self.state.set(self.live.get());
        }
        moved
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
            match self.apply(&entry.action) {
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

    fn apply(&self, action: &P::Action) -> Applied {
        self.apply_as(action, None)
    }

    /// [`Self::apply`] as `invoker` (an agent, a script), which the change is
    /// attributed to and which [`ActionCx::author`](tilo_resource::action)
    /// answers inside the action; the person seated here when `None`.
    fn apply_as(&self, action: &P::Action, invoker: Option<&ActionInvoker>) -> Applied {
        let encoded = match tilo_resource::encode::<P>(action) {
            Ok(encoded) => encoded,
            Err(error) => return Applied::Done(Err(error)),
        };
        let change = action.change_kind();
        // Who the change is: the invoker's own actor when the host names one (an agent acting in
        // a turn, attributed as it is at an app's record origin), else the person seated here.
        let me = invoker
            .and_then(|invoker| (!invoker.actor.is_empty()).then_some(invoker.actor.clone()));
        let actor = me.as_deref().unwrap_or(&self.me);
        let context = ActionContext::for_resource(P::KIND, P::KIND, P::LABEL)
            .with_app(self.app_id, self.app_title)
            .with_invoker(
                invoker
                    .cloned()
                    .unwrap_or_else(|| ActionInvoker::actor(actor)),
            )
            .with_change(change);
        let edited = self.record.try_edit(change == ChangeKind::Content, |doc| {
            let writer = ui_core::history_source::authors::device_actor(actor, doc.get_actor());
            tilo_actions::apply_local_with_context(doc, action, encoded, Some(&writer), context)
                .map(|_| ())
                .map_err(|error| error.to_string())
        });
        match edited {
            Ok(result) => Applied::Done(result.map_err(ActionError::Refused)),
            Err(unavailable) if unavailable.is_momentary() => Applied::Waits,
            Err(unavailable) => Applied::Done(Err(ActionError::Refused(unavailable.to_string()))),
        }
    }
}

impl<P: Resource> WorkspaceRecord<P> {
    /// The record `P` that `owner` keeps in the workspace `records` opens,
    /// created with the resource's genesis where nothing holds it yet and
    /// brought to its version: what an app window holds
    /// ([`RecordEnv::workspace`](super::RecordEnv::workspace)), and what a
    /// host that is no app window (the session's workspace settings) opens
    /// the same way. Changes are attributed to `app` (its id and title) and,
    /// where the host knows the workspace's grants as `permissions`, decided by
    /// them first; `wake` is woken when another replica's change arrives.
    pub fn open(
        records: &WorkspaceRecords,
        owner: &'static str,
        app: (&'static str, &'static str),
        permissions: Option<LivePermissions>,
        wake: Option<WakeHandle>,
    ) -> Option<Self> {
        let opened = records.open_record(owner, P::KIND, &|doc| {
            P::initialize(doc).map_err(|error| error.to_string())
        });
        Self::hold(
            opened,
            owner,
            app,
            permissions,
            wake,
            records.author().0.clone(),
        )
    }

    /// [`Self::open`] of the record `id` of `kind` `P` that `owner` keeps:
    /// a record of the owner's that the workspace names otherwise than
    /// [`Self::open`]'s, such as a conversation's, which the two people it
    /// is between name. The workspace's grants decide its changes like any
    /// other.
    pub fn open_at(
        records: &WorkspaceRecords,
        owner: &'static str,
        id: &str,
        app: (&'static str, &'static str),
        permissions: Option<LivePermissions>,
        wake: Option<WakeHandle>,
    ) -> Option<Self> {
        let opened = records.open_record_at(owner, id, P::KIND, &|doc| {
            P::initialize(doc).map_err(|error| error.to_string())
        });
        Self::hold(
            opened,
            owner,
            app,
            permissions,
            wake,
            records.author().0.clone(),
        )
    }

    /// [`Self::open_at`] of the record `owner` keeps as its own primary one of
    /// `P::KIND` in the workspace: the record an app keeps as itself (Chat's
    /// of its chat, `chat.workspace`), which `open_record` cannot open because
    /// the package's primary id is a sentinel no [`RecordId::parse`] accepts.
    /// An app's own window opens it this way; so does the session, reading the
    /// conversations its agents answer in.
    pub fn open_primary(
        records: &WorkspaceRecords,
        owner: &'static str,
        app: (&'static str, &'static str),
        permissions: Option<LivePermissions>,
        wake: Option<WakeHandle>,
    ) -> Option<Self> {
        let opened = records.open_primary(owner, P::KIND, &|doc| {
            P::initialize(doc).map_err(|error| error.to_string())
        });
        Self::hold(
            opened,
            owner,
            app,
            permissions,
            wake,
            records.author().0.clone(),
        )
    }

    /// [`Self::open`]'s record, opened (or not) by the store it is kept in:
    /// brought to the resource's version, read, and followed change by
    /// change.
    fn hold(
        opened: Result<StoredRecord, ui_core::record::RecordOpenError>,
        owner: &'static str,
        app: (&'static str, &'static str),
        permissions: Option<LivePermissions>,
        wake: Option<WakeHandle>,
        me: String,
    ) -> Option<Self> {
        let record = opened
            .map_err(|error| tracing::warn!(kind = P::KIND, %error, "workspace record unavailable"))
            .ok()?;
        if record.read(tilo_resource::evolution::needs_migrate::<P>)?
            && let Err(error) = record.edit(false, |doc| {
                tilo_resource::evolution::migrate::<P>(doc).map_err(|error| error.to_string())
            })
        {
            tracing::warn!(kind = P::KIND, %error, "workspace record not migrated");
        }
        let reader = record
            .read(tilo_resource::Reader::<P>::read)?
            .map_err(|error| tracing::warn!(kind = P::KIND, %error, "workspace record unreadable"))
            .ok()?;
        if let Some(wake) = &wake {
            record.wake_on_arrival(wake.clone());
        }
        let live = Live::new(reader);
        Some(Self {
            shared: Rc::new(Shared::<P> {
                record,
                owner,
                app_id: app.0,
                app_title: app.1,
                me,
                permissions,
                state: LocalState::new(live.get()),
                live,
                wake,
                waiting: RefCell::default(),
            }),
        })
    }

    /// The record's id, which names it in the workspace.
    #[must_use]
    pub fn id(&self) -> &ui_core::record::RecordId {
        &self.shared.record.checkout().id
    }

    /// The record's state as the window follows it, for the chain kept
    /// beside it ([`RecordEnv::chain_beside`](super::RecordEnv::chain_beside)).
    pub(crate) fn live(&self) -> Live<P> {
        self.shared.live.clone()
    }

    /// The current state.
    pub fn get(&self) -> Rc<P::State> {
        self.shared.refresh();
        self.shared.state.with(Rc::clone)
    }

    /// Increases whenever the state changes.
    pub fn revision(&self) -> u64 {
        self.shared.refresh();
        self.shared.live.revision()
    }

    /// The person who acts here.
    pub fn me(&self) -> &str {
        &self.shared.me
    }

    /// Whether the workspace's grants let the person invoke the verb `verb` of
    /// the record, as its owner publishes it. A window that knows nothing
    /// of the workspace's grants (a test host) lets the record decide.
    pub fn may(&self, verb: &str) -> bool {
        let Some(permissions) = &self.shared.permissions else {
            return true;
        };
        let Some(declared) = P::declarations().into_iter().find(|decl| decl.id == verb) else {
            return false;
        };
        permissions.check(&PermissionScope::Action {
            node: tilo_actions::action_node(
                &TreePath::root(),
                self.shared.owner,
                P::KIND,
                self.shared.record.checkout().id.as_str(),
                declared.field,
                verb,
            ),
            kind: declared.kind,
        })
    }

    /// Apply the action `invoke` names: decided by the workspace's grants on the
    /// owner's node, then one change of the record, attributed to the
    /// person, in its history and one undo step of theirs.
    ///
    /// # Errors
    /// The arguments do not build the action, the grants refuse it, the
    /// record does, or the record cannot be changed right now (for that,
    /// [`Self::invoke_then`] waits).
    pub fn invoke(&self, invoke: &Invoke<P>) -> Result<(), ActionError> {
        let action = self.admit(invoke)?;
        match self.shared.apply(&action) {
            Applied::Done(result) => {
                self.shared.refresh();
                result
            }
            Applied::Waits => Err(ActionError::Refused(
                ui_core::record::HistoryUnavailable::StoreInUse.to_string(),
            )),
        }
    }

    /// [`Self::invoke`] as `invoker` (an agent, a script), which the change is attributed to —
    /// the agent's own words in Chat are written as the agent, not as the person it acts for.
    /// The grants are the ones this record was opened with, as for the person's own call.
    ///
    /// # Errors
    /// The arguments do not build the action, the grants refuse it, or the record does.
    pub fn invoke_as(
        &self,
        invoker: &ActionInvoker,
        invoke: &Invoke<P>,
    ) -> Result<(), ActionError> {
        let action = self.admit(invoke)?;
        match self.shared.apply_as(&action, Some(invoker)) {
            Applied::Done(result) => {
                self.shared.refresh();
                result
            }
            Applied::Waits => Err(ActionError::Refused(
                ui_core::record::HistoryUnavailable::StoreInUse.to_string(),
            )),
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
        let shared = &self.shared;
        let first = shared
            .waiting
            .try_borrow()
            .is_ok_and(|waiting| waiting.is_empty());
        if first && let Applied::Done(result) = shared.apply(&action) {
            shared.refresh();
            return then(result);
        }
        shared.waiting.borrow_mut().push(Waiting {
            action,
            then: Box::new(then),
            woken: 0,
        });
        if let Some(window) = &shared.wake {
            window.wake();
        }
    }

    /// The action `invoke` names, once the workspace's grants admit it.
    fn admit(&self, invoke: &Invoke<P>) -> Result<P::Action, ActionError> {
        let action = invoke
            .build(tilo_resource::origin_now_ms())
            .map_err(ActionError::Refused)?;
        if let Some(verb) = verb_of::<P>(&action)
            && !self.may(&verb)
        {
            return Err(ActionError::Refused(format!(
                "you may not change the workspace's {}",
                P::LABEL.to_lowercase()
            )));
        }
        Ok(action)
    }

    /// The record underneath, for a test or host that carries it between
    /// devices itself.
    #[must_use]
    pub fn record(&self) -> &StoredRecord {
        &self.shared.record
    }
}

/// The verb `action` invokes, as its record publishes it and history names
/// it (what a grant on one verb covers); `None` for a field edit.
pub(crate) fn verb_of<P: Resource>(action: &P::Action) -> Option<String> {
    let mut doc = ui_core::automerge::AutoCommit::new();
    P::initialize(&mut doc).ok()?;
    match action.describe(&doc)?.what {
        tilo_actions::Did::Verb(call) => Some(call.action.action),
        _ => None,
    }
}

/// The records of the window's workspace, for what settles after the view
/// that asked for them: a reply, or a message another device sent
/// ([`RecordEnv::workspace_handle`](super::RecordEnv::workspace_handle)).
#[derive(Clone)]
pub struct WorkspaceHandle {
    state: Rc<WorkspaceState>,
}

impl WorkspaceHandle {
    /// The record `id` of `P` that `owner` keeps: opened, created where
    /// nothing holds it yet, and followed change by change. `None` where it
    /// cannot be opened.
    pub fn record<P: Resource>(&self, owner: &'static str, id: &str) -> Option<WorkspaceRecord<P>> {
        self.state.open_at::<P>(owner, id)
    }

    /// Whether the record `id` of `P` that `owner` keeps is held already,
    /// without opening or creating it.
    #[must_use]
    pub fn holds_record<P: Resource>(&self, owner: &'static str, id: &str) -> bool {
        self.state.holds_record(owner, id, P::KIND)
    }

    /// The chain of `P` kept beside one of the workspace's records
    /// ([`Chained::chain_beside`]): the messages of one direct conversation,
    /// in parts of the record the two people share, opened in the store that
    /// keeps that record. `None` where it cannot be opened.
    pub fn chain_beside<P: Chained>(
        &self,
        owner: &'static str,
        base: &WorkspaceRecord<P::Base>,
    ) -> Option<Chain<P>> {
        self.state.chain_beside::<P>(owner, base)
    }

    /// What wakes the window: for an answer that arrives later.
    #[must_use]
    pub fn wake(&self) -> Option<&WakeHandle> {
        self.state.wake()
    }
}

/// A record open in a window: its owner and kind, and which record of them
/// (the workspace's one of the kind, or a pair's).
type OpenKey = (&'static str, &'static str, String);

/// The records every app of the workspace opens that a window's views opened:
/// one [`WorkspaceRecord`] per kind.
pub(crate) struct WorkspaceState {
    records: WorkspaceRecords,
    app_id: &'static str,
    app_title: &'static str,
    permissions: Option<LivePermissions>,
    /// Wakes the window when another replica's change arrives.
    wake: Option<WakeHandle>,
    opened: RefCell<HashMap<OpenKey, Rc<dyn Any>>>,
    /// The chains kept beside the opened records (a chat's message parts,
    /// [`WorkspaceHandle::chain_beside`]).
    chains: super::chain::Chains,
    owners: RefCell<Vec<HistoryOwner>>,
    refreshers: RefCell<Vec<Box<dyn Fn() -> bool>>>,
}

impl WorkspaceState {
    pub(crate) fn new(
        records: WorkspaceRecords,
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
            chains: super::chain::Chains::default(),
            owners: RefCell::default(),
            refreshers: RefCell::default(),
        }
    }

    /// What wakes the window: for an answer that arrives later, such as a
    /// chooser's.
    pub(crate) fn wake(&self) -> Option<&WakeHandle> {
        self.wake.as_ref()
    }

    /// This state as the window's views hold it on
    /// ([`RecordEnv::workspace_handle`](super::RecordEnv::workspace_handle)).
    pub(crate) fn handle(self: &Rc<Self>) -> WorkspaceHandle {
        WorkspaceHandle {
            state: Rc::clone(self),
        }
    }

    /// The window's app.
    pub(crate) fn app_id(&self) -> &'static str {
        self.app_id
    }

    /// Whether the record `id` of `kind` that `owner` keeps is held already,
    /// without opening or creating it: what a list asks.
    pub(crate) fn holds_record(&self, owner: &str, id: &str, kind: &str) -> bool {
        self.records
            .holds_record(owner, id, kind)
            .is_ok_and(|held| held)
    }

    /// The record `P` that `owner` keeps in the workspace, opened on first use
    /// and created with the resource's genesis where it does not exist yet.
    pub(crate) fn open<P: Resource>(&self, owner: &'static str) -> Option<WorkspaceRecord<P>> {
        let id = ui_core::record::workspace::record_id(P::KIND);
        self.open_with::<P>(owner, &id, |records| {
            WorkspaceRecord::open(
                records,
                owner,
                (self.app_id, self.app_title),
                self.permissions.clone(),
                self.wake.clone(),
            )
        })
    }

    /// The record `id` of `P` that `owner` keeps, opened on first use and
    /// created with the resource's genesis where it does not exist yet: a
    /// conversation's record, named by the two people it is between.
    pub(crate) fn open_at<P: Resource>(
        &self,
        owner: &'static str,
        id: &str,
    ) -> Option<WorkspaceRecord<P>> {
        self.open_with::<P>(owner, id, |records| {
            WorkspaceRecord::open_at(
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
        open: impl FnOnce(&WorkspaceRecords) -> Option<WorkspaceRecord<P>>,
    ) -> Option<WorkspaceRecord<P>> {
        let key = (owner, P::KIND, id.to_owned());
        if let Some(open) = self.opened.borrow().get(&key) {
            return Rc::clone(open)
                .downcast::<Shared<P>>()
                .ok()
                .map(|shared| WorkspaceRecord { shared });
        }
        // The owner's own window lists the record in its surface: its
        // changes are decided against the owner's surface.
        if owner == self.app_id {
            super::controls::opened_record::<P>();
        }
        let opened = open(&self.records);
        self.held(key, opened)
    }

    /// The chain of `P` kept beside one of the workspace's records
    /// ([`Chained::chain_beside`]), opened on first use in the store that keeps
    /// that record: a conversation's messages, decided by the workspace's
    /// grants on the app like the record they are kept beside. `None` where it
    /// cannot be opened.
    pub(crate) fn chain_beside<P: Chained>(
        &self,
        owner: &'static str,
        base: &WorkspaceRecord<P::Base>,
    ) -> Option<Chain<P>> {
        super::controls::opened_record::<P>();
        let workspace = super::chain::WorkspaceRecords {
            store: self.records.store_of(owner).ok()?,
            me: self.records.author().0.clone(),
            permissions: self.permissions.clone(),
            app_id: self.app_id,
            app_title: self.app_title,
        };
        let live = base.live();
        let chain = P::chain_beside(base.id());
        let state = self
            .chains
            .open_at::<P>(&workspace, &chain, move || live.get())?;
        Some(Chain::applying(&state, self.app_id, base.me()))
    }

    /// Keep an opened record for the window: its route reported to the
    /// runtime, its changes followed, and the record held for the views that
    /// ask for it again.
    fn held<P: Resource>(
        &self,
        key: OpenKey,
        opened: Option<WorkspaceRecord<P>>,
    ) -> Option<WorkspaceRecord<P>> {
        let WorkspaceRecord { shared } = opened?;
        self.owners.borrow_mut().push(shared.record.history_owner());
        let weak = Rc::downgrade(&shared);
        self.refreshers.borrow_mut().push(Box::new(move || {
            weak.upgrade().is_some_and(|shared| shared.refresh())
        }));
        self.opened
            .borrow_mut()
            .insert(key, Rc::clone(&shared) as Rc<dyn Any>);
        Some(WorkspaceRecord { shared })
    }

    /// Read every open record that moved. Whether one did.
    pub(crate) fn refresh(&self) -> bool {
        let moved = self
            .refreshers
            .borrow()
            .iter()
            .fold(false, |moved, refresh| refresh() | moved);
        // A part another device wrote into a chain kept beside a record is
        // read here, so the app's views show it.
        for chain in self.chains.doors() {
            let _ = chain.revision();
        }
        moved
    }

    /// The routes of the open records, which the runtime syncs.
    pub(crate) fn owners(&self) -> Vec<HistoryOwner> {
        self.owners.borrow().clone()
    }
}
