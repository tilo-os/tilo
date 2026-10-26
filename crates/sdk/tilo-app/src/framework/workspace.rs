//! The records every app of the window's room opens beside its own
//! ([`ui_core::record::workspace`]): the workspace's emoji among them,
//! which any view of any app draws for `:name:`.
//!
//! Such a record is its owner's resource: the room's grants on the owner's
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
use ui_core::record::workspace::{WorkspaceRecord as StoredRecord, WorkspaceRecords};
use ui_core::wake::WakeHandle;

use super::live::Live;

/// A record every app of the window's room opens, held for the window
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
    /// The app that keeps it: whose node the room's grants decide on.
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
}

impl<P: Resource> Shared<P> {
    /// Follow the record when it moved. Whether it did.
    fn refresh(&self) -> bool {
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
}

impl<P: Resource> WorkspaceRecord<P> {
    /// The record `P` that `owner` keeps in the room `records` opens,
    /// created with the resource's genesis where nothing holds it yet and
    /// brought to its version: what an app window holds
    /// ([`RecordEnv::workspace`](super::RecordEnv::workspace)), and what a
    /// host that is no app window (the session's workspace settings) opens
    /// the same way. Changes are attributed to `app` (its id and title) and,
    /// where the host knows the room's grants as `permissions`, decided by
    /// them first; `wake` is woken when another replica's change arrives.
    pub fn open(
        records: &WorkspaceRecords,
        owner: &'static str,
        app: (&'static str, &'static str),
        permissions: Option<LivePermissions>,
        wake: Option<WakeHandle>,
    ) -> Option<Self> {
        let record = records
            .open_record(owner, P::KIND, &|doc| {
                P::initialize(doc).map_err(|error| error.to_string())
            })
            .map_err(|error| tracing::warn!(kind = P::KIND, %error, "workspace record unavailable"))
            .ok()?;
        if record.read(tilo_resource::evolution::stored_version)? < P::VERSION
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
        if let Some(wake) = wake {
            record.wake_on_arrival(wake);
        }
        let live = Live::new(reader);
        Some(Self {
            shared: Rc::new(Shared::<P> {
                record,
                owner,
                app_id: app.0,
                app_title: app.1,
                me: records.author().0.clone(),
                permissions,
                state: LocalState::new(live.get()),
                live,
            }),
        })
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

    /// Whether the room's grants let the person invoke the verb `verb` of
    /// the record, as its owner publishes it. A window that knows nothing
    /// of the room's grants (a test host) lets the record decide.
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

    /// Apply the action `invoke` names: decided by the room's grants on the
    /// owner's node, then one change of the record, attributed to the
    /// person, in its history and one undo step of theirs.
    ///
    /// # Errors
    /// The arguments do not build the action, the grants refuse it, or the
    /// record does.
    pub fn invoke(&self, invoke: &Invoke<P>) -> Result<(), ActionError> {
        let action = invoke
            .build(tilo_resource::origin_now_ms())
            .map_err(ActionError::Refused)?;
        self.apply(&action)
    }

    fn apply(&self, action: &P::Action) -> Result<(), ActionError> {
        let shared = &self.shared;
        if let Some(verb) = verb_of::<P>(action)
            && !self.may(&verb)
        {
            return Err(ActionError::Refused(format!(
                "you may not change the workspace's {}",
                P::LABEL.to_lowercase()
            )));
        }
        let encoded = tilo_resource::encode::<P>(action)?;
        let change = action.change_kind();
        let context = ActionContext::for_resource(P::KIND, P::KIND, P::LABEL)
            .with_app(shared.app_id, shared.app_title)
            .with_invoker(ActionInvoker::actor(&shared.me))
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

    /// The record underneath, for a test or host that carries it between
    /// devices itself.
    #[must_use]
    pub fn record(&self) -> &StoredRecord {
        &self.shared.record
    }
}

/// The verb `action` invokes, as its record publishes it; `None` for a
/// field edit.
fn verb_of<P: Resource>(action: &P::Action) -> Option<String> {
    let mut doc = ui_core::automerge::AutoCommit::new();
    P::initialize(&mut doc).ok()?;
    match action.describe(&doc)?.what {
        tilo_actions::Did::Verb(call) => Some(call.action.action),
        _ => None,
    }
}

/// A record open in a window: its owner and kind.
type OpenKey = (&'static str, &'static str);

/// The records every app of the room opens that a window's views opened:
/// one [`WorkspaceRecord`] per kind.
pub(crate) struct WorkspaceState {
    records: WorkspaceRecords,
    app_id: &'static str,
    app_title: &'static str,
    permissions: Option<LivePermissions>,
    /// Wakes the window when another replica's change arrives.
    wake: Option<WakeHandle>,
    opened: RefCell<HashMap<OpenKey, Rc<dyn Any>>>,
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
            owners: RefCell::default(),
            refreshers: RefCell::default(),
        }
    }

    /// The window's app.
    pub(crate) fn app_id(&self) -> &'static str {
        self.app_id
    }

    /// The record `P` that `owner` keeps in the room, opened on first use
    /// and created with the resource's genesis where it does not exist yet.
    pub(crate) fn open<P: Resource>(&self, owner: &'static str) -> Option<WorkspaceRecord<P>> {
        if let Some(open) = self.opened.borrow().get(&(owner, P::KIND)) {
            return Rc::clone(open)
                .downcast::<Shared<P>>()
                .ok()
                .map(|shared| WorkspaceRecord { shared });
        }
        // The owner's own window lists the record in its surface: its
        // changes are decided against the owner's surface.
        if owner == self.app_id {
            super::controls::opened_in_room::<P>();
        }
        let WorkspaceRecord { shared } = WorkspaceRecord::<P>::open(
            &self.records,
            owner,
            (self.app_id, self.app_title),
            self.permissions.clone(),
            self.wake.clone(),
        )?;
        self.owners.borrow_mut().push(shared.record.history_owner());
        let weak = Rc::downgrade(&shared);
        self.refreshers.borrow_mut().push(Box::new(move || {
            weak.upgrade().is_some_and(|shared| shared.refresh())
        }));
        self.opened
            .borrow_mut()
            .insert((owner, P::KIND), Rc::clone(&shared) as Rc<dyn Any>);
        Some(WorkspaceRecord { shared })
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
