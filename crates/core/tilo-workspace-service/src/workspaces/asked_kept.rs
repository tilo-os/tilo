//! The asked records that wait, kept in each workspace as its `tilo.invocations` record
//! ([`RECORD`]), a resource of the workspace's store of the kind [`KIND`]: every call that waits
//! for its person's yes, by its invocation, with what admitting it again needs. The record syncs
//! as every resource of the workspace does, so a call that waits stays suspended in it across the
//! host's restarts, and the host reads the calls back from it when it starts.
//!
//! A call only the person's root signs is kept there by the device that keeps no root, for one
//! that does ([`tilo_actions::remote::asked::AT_ROOT`]): every device follows the record
//! ([`Workspaces::follow_asked_at_root`]), so a root-holding device takes the call up when it
//! comes online, and the device that asked reads how it went once it ran.

use std::sync::{Arc, Weak};
use std::time::Duration;

use automerge::transaction::Transactable as _;
use automerge::{ObjType, ROOT, ReadDoc as _, Value};
use tilo_action_services::workspace_invocations::PROVIDER;
use tilo_actions::remote::asked::{
    AT_ROOT, AskedDispatch, AskedFacts, AskedKeeping, KeptAsk, RecordRef,
};
use tilo_actions::remote::{Caller, InvokeRequest};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;

use super::{WeakWorkspaces, Workspaces};

/// The record of a workspace's invocations that keeps those waiting for their person's yes:
/// a map from each one's id to the call as it was asked ([`KeptAsk`]).
pub(crate) const RECORD: &str = "asked";
/// The kind the record is, as [`surface`] declares it.
pub(crate) const KIND: &str = "tilo.invocations.asked";
/// Where the record keeps the calls: each one's id to the call as it was asked, as JSON.
const CALLS: &str = "calls";
/// What [`PROVIDER`] declares its records are, so every holder's store keeps them: the
/// surface's compiled form.
pub(crate) fn surface() -> String {
    collab_types::surface::canonical(&serde_json::json!({
        "format": 2,
        "resources": [{
            "kind": KIND,
            "label": "Asked calls",
            "version": 1,
            "fields": [{ "path": CALLS, "type": { "map": "text" } }],
            "verbs": [{ "id": "type", "kind": "undoable" }],
        }],
    }))
}

/// The code a call ends with whose question could not be kept ([`UNKEPT`]).
pub const UNKEPT_CODE: &str = "unkept";

/// What a call whose question could not be kept for the person ends with.
pub(crate) const UNKEPT: &str = "The question could not be kept where the person's device that \
     keeps their identity reads it, so nothing waits for their answer";

/// Why a call kept for a device that keeps the person's root did not end there, for the device
/// that kept it and waits on it ([`Workspaces::ask_at_root`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotAtRoot {
    /// No device that keeps the root took it in within the time given: none was reached.
    Unreached(Duration),
    /// It reached one, and the person did not answer there within the time given.
    Unanswered(Duration),
    /// Its record could not be written here, for the reason given.
    Unkept(String),
}

/// What keeps nothing beside a call this device kept for the root, and admits nothing: it runs
/// there.
struct RunsThere;

impl AskedDispatch for RunsThere {
    fn context(&self, _: InvocationId) -> serde_json::Value {
        serde_json::Value::Null
    }

    fn readmit(
        &self,
        _: &Caller,
        _: InvokeRequest,
        _: &serde_json::Value,
    ) -> Result<(), tilo_actions::remote::ActionCallError> {
        Err(tilo_actions::remote::ActionCallError::Refused {
            reason: "it is answered on a device that keeps your identity".to_owned(),
        })
    }

    fn declined(
        &self,
        _: &Caller,
        _: InvocationId,
    ) -> Result<(), tilo_actions::remote::ActionCallError> {
        Ok(())
    }
}

/// Forgets, when dropped, that this device waits on the call it kept for the root.
struct Awaited<'a>(&'a Workspaces, InvocationId);

impl Drop for Awaited<'_> {
    fn drop(&mut self) {
        self.0.inner.awaited_at_root.lock_recover().remove(&self.1);
    }
}

/// One change to a workspace's record of the calls that wait.
enum Change {
    Keep(Box<KeptAsk>),
    Forget(RecordRef),
}

impl Change {
    fn workspace(&self) -> &str {
        match self {
            Self::Keep(ask) => &ask.at.workspace,
            Self::Forget(at) => &at.workspace,
        }
    }
}

/// The calls that wait, written into their workspaces' records in the order they were asked
/// and answered.
struct KeptInRecords {
    changes: tokio::sync::mpsc::UnboundedSender<Change>,
}

impl AskedKeeping for KeptInRecords {
    fn keep(&self, ask: &KeptAsk) {
        if self
            .changes
            .send(Change::Keep(Box::new(ask.clone())))
            .is_err()
        {
            tracing::error!(record = %ask.at.record, "the host's workspaces stopped: an asked call is not kept across a restart");
        }
    }

    fn forget(&self, at: &RecordRef) {
        if self.changes.send(Change::Forget(at.clone())).is_err() {
            tracing::error!(record = %at.record, "the host's workspaces stopped: an answered call stays kept");
        }
    }
}

/// The document that holds a workspace's record of the calls that wait: the one record a site
/// that keeps the workspace for another operator's person is placed for
/// ([`crate::workspaces::asks_placed`]).
pub(crate) fn asked_document() -> String {
    tilo_resource_store::content_id(collab_types::EVERY_MEMBER, KIND, RECORD)
}

/// Found `workspace`'s record of the calls that wait, empty, where it has none: written by a
/// device of the person's, so a site placed for that record alone has a record to write into.
///
/// # Errors
/// The workspace is not held here, or its store refused the record.
pub(crate) async fn found_record(workspaces: &Workspaces, workspace: &str) -> Result<(), String> {
    workspaces
        .author_record(workspace, (PROVIDER, KIND, RECORD), |doc| {
            if doc
                .get(ROOT, CALLS)
                .map_err(|error| error.to_string())?
                .is_none()
            {
                doc.put_object(ROOT, CALLS, ObjType::Map)
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })
        .await
}

/// Write `change` into its workspace's record, as this host, or as the device it was placed
/// for that record with where the workspace is another operator's person's.
async fn write(workspaces: &Workspaces, change: Change) -> Result<(), String> {
    let workspace = change.workspace().to_owned();
    let Some(placed) = workspaces.asks_device(&workspace).await else {
        return write_as(workspaces, workspaces.signing_device(), &change).await;
    };
    let device = placed?;
    // The person's device founds the record as it hands the workspace over, and its content comes
    // here with the workspace's first sync: an ask that comes before it waits for it, as long as
    // a sync takes, and no longer.
    let mut writes = workspaces.document_writes();
    let mut within = std::pin::pin!(tokio::time::sleep(CONTENT_WITHIN));
    loop {
        match write_as(workspaces, Arc::clone(&device), &change).await {
            Err(why) if why.ends_with(super::resources::NOT_HELD_YET) => {
                let arrived = async {
                    loop {
                        match writes.recv().await {
                            Ok(write) if write.workspace != workspace => {}
                            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                return true;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return false,
                        }
                    }
                };
                let arrived = tokio::select! {
                    arrived = arrived => arrived,
                    () = within.as_mut() => false,
                };
                if !arrived {
                    return Err(why);
                }
            }
            written => return written,
        }
    }
}

/// How long a site waits for the record of asked calls the person's device founded to reach it.
const CONTENT_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

/// [`write`] as `device`.
async fn write_as(
    workspaces: &Workspaces,
    device: Arc<super::MemberDevice>,
    change: &Change,
) -> Result<(), String> {
    let workspace = change.workspace().to_owned();
    workspaces
        .author_record_as(device, &workspace, (PROVIDER, KIND, RECORD), |doc| {
            let calls = match doc.get(ROOT, CALLS).map_err(|error| error.to_string())? {
                Some((Value::Object(ObjType::Map), calls)) => calls,
                _ => doc
                    .put_object(ROOT, CALLS, ObjType::Map)
                    .map_err(|error| error.to_string())?,
            };
            match change {
                Change::Keep(ask) => {
                    let text = serde_json::to_string(ask).map_err(|error| error.to_string())?;
                    doc.put(&calls, ask.at.record.as_str(), text)
                }
                Change::Forget(at) => doc.delete(&calls, at.record.as_str()),
            }
            .map_err(|error| error.to_string())
        })
        .await
}

/// The calls `bytes`, a workspace's record of them, keeps.
fn read(bytes: &[u8]) -> Result<Vec<KeptAsk>, String> {
    let doc = automerge::AutoCommit::load(bytes).map_err(|error| error.to_string())?;
    let Some((Value::Object(ObjType::Map), calls)) =
        doc.get(ROOT, CALLS).map_err(|error| error.to_string())?
    else {
        return Ok(Vec::new());
    };
    let mut kept = Vec::new();
    for key in doc.keys(&calls) {
        let Some((Value::Scalar(value), _)) = doc
            .get(&calls, key.as_str())
            .map_err(|error| error.to_string())?
        else {
            return Err(format!("{key} is no kept call"));
        };
        let automerge::ScalarValue::Str(text) = value.as_ref() else {
            return Err(format!("{key} is no kept call"));
        };
        kept.push(serde_json::from_str(text).map_err(|error| format!("{key}: {error}"))?);
    }
    Ok(kept)
}

impl Workspaces {
    /// Where this host keeps the asked calls that wait: each workspace's own record of them,
    /// written in order by a task of the runtime this is called on.
    #[must_use]
    pub fn asked_keeping(&self) -> Arc<dyn AskedKeeping> {
        let (changes, mut written) = tokio::sync::mpsc::unbounded_channel::<Change>();
        let workspaces: WeakWorkspaces = self.downgrade();
        tokio::spawn(async move {
            while let Some(change) = written.recv().await {
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
                let record = match &change {
                    Change::Keep(ask) => ask.at.record.clone(),
                    Change::Forget(at) => at.record.clone(),
                };
                let kept = match &change {
                    Change::Keep(ask) => Some(ask.clone()),
                    Change::Forget(_) => None,
                };
                if let Err(why) = write(&workspaces, change).await {
                    tracing::error!(%why, %record, "a workspace's record of its asked calls was not written");
                    // A call nobody can be asked about ends now, telling whoever follows it
                    // why, rather than waiting for an answer that cannot come.
                    if let Some(ask) = kept {
                        workspaces.unkept(*ask, &format!("{UNKEPT}: {why}"));
                    }
                }
            }
        });
        Arc::new(KeptInRecords { changes })
    }

    /// Every asked call that waits in the records of the workspaces this host keeps, as
    /// they were kept before it started.
    pub async fn asked_kept(&self) -> Vec<KeptAsk> {
        let mut kept = Vec::new();
        for workspace in self.administered_ids().await {
            match self.resource_document(&workspace, PROVIDER, RECORD).await {
                Ok(None) => {}
                Ok(Some(bytes)) => match read(&bytes) {
                    Ok(calls) => kept.extend(calls),
                    Err(why) => {
                        tracing::error!(%why, %workspace, "a workspace's asked calls could not be read: nobody can answer them");
                    }
                },
                Err(why) => {
                    tracing::error!(%why, %workspace, "a workspace's asked calls could not be read: nobody can answer them");
                }
            }
        }
        kept
    }

    /// Hand `seen` the calls kept in the records of every workspace kept here to be answered at
    /// a device that keeps the person's root ([`tilo_actions::remote::asked::AT_ROOT`]): now,
    /// again on each write to one of those records, whenever a workspace's governance moves or
    /// one is held here (the keys that open a record kept in it may have come), and whenever
    /// presence moves (a device of the person's came online), from a task of the runtime this
    /// is called on.
    pub fn follow_asked_at_root(&self, seen: impl Fn(Vec<KeptAsk>) + Send + 'static) {
        use tokio::sync::broadcast::error::RecvError;

        let workspaces = self.downgrade();
        // Subscribed before the first read, so a change between the two is read again.
        let (mut writes, mut governance) = (self.document_writes(), self.governance_changes());
        let mut holds = self.holds_changed();
        tokio::spawn(async move {
            // One presence wake at a time: made again only once presence moved.
            let mut presence = None;
            loop {
                let moved = presence.get_or_insert_with(|| {
                    let (wake, moved) = tilo_actions::remote::ResourceWake::new();
                    super::on_presence_change(&wake);
                    moved
                });
                let Some(held) = workspaces.upgrade() else {
                    return;
                };
                let asked = held.asked_at_root().await;
                seen(asked);
                drop(held);
                // Whether presence moved: its wake is made again for the next read.
                let present = loop {
                    tokio::select! {
                        write = writes.recv() => match write {
                            // The record's id on the wire is the store's, not its name: any
                            // write to the provider's records reads them again.
                            Ok(write) if write.app == PROVIDER => break false,
                            Ok(_) => {}
                            Err(RecvError::Lagged(_)) => break false,
                            Err(RecvError::Closed) => return,
                        },
                        changed = governance.recv() => match changed {
                            Ok(_) | Err(RecvError::Lagged(_)) => break false,
                            Err(RecvError::Closed) => return,
                        },
                        held = holds.changed() => {
                            if held.is_err() {
                                return;
                            }
                            break false;
                        }
                        _ = &mut *moved => break true,
                    }
                };
                if present {
                    presence = None;
                }
            }
        });
    }

    /// Keep `request`, which `caller` asks in `workspace` and only a device that keeps the
    /// person's root runs, as its record there, and wait until that device ran it: how it ended.
    /// A device that keeps the root marks the record once it takes it in, within
    /// `reached_within`, and the person answers there within `answered_within`; past either the
    /// call ends here, as the record then says, and runs nowhere.
    ///
    /// # Errors
    /// No device that keeps the root took it in in time, the person did not answer in time, or
    /// its record could not be written here.
    pub async fn ask_at_root(
        &self,
        workspace: &str,
        (caller, request): (&Caller, &InvokeRequest),
        facts: &AskedFacts,
        (reached_within, answered_within): (Duration, Duration),
    ) -> Result<InvocationUpdate, NotAtRoot> {
        use tokio::sync::broadcast::error::RecvError;

        let store = self
            .asked_store()
            .ok_or_else(|| NotAtRoot::Unkept("this device keeps no asked calls".to_owned()))?;
        let invocation = request.invocation;
        // Subscribed before it is written, so no write of its record is missed.
        let mut writes = self.document_writes();
        self.inner.awaited_at_root.lock_recover().insert(invocation);
        let _awaited = Awaited(self, invocation);
        let runs_there: Weak<dyn AskedDispatch> = Weak::<RunsThere>::new();
        let at = store
            .writes_for(AT_ROOT, runs_there)
            .write_asked(workspace, caller, request, facts)
            .map_err(NotAtRoot::Unkept)?;
        let mut reach = std::pin::pin!(tokio::time::sleep(reached_within));
        let mut answer = std::pin::pin!(tokio::time::sleep(answered_within));
        let mut reached = false;
        let ended = loop {
            if let Some(kept) = self.asked_record(workspace, invocation).await {
                if let Some(outcome) = kept.outcome.filter(InvocationUpdate::is_terminal) {
                    store.settled(&at);
                    return Ok(outcome);
                }
                reached |= kept.seen.is_some() || kept.taken.is_some();
            }
            tokio::select! {
                write = writes.recv() => match write {
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => {
                        break NotAtRoot::Unkept("this device stopped".to_owned());
                    }
                },
                () = reach.as_mut(), if !reached => break NotAtRoot::Unreached(reached_within),
                () = answer.as_mut() => break NotAtRoot::Unanswered(answered_within),
            }
        };
        // Ended here: the record says so, so no device that keeps the root takes it up after.
        store.settle_for(
            invocation,
            InvocationUpdate::Cancelled {
                invocation,
                message: Some(format!("{ended:?}")),
            },
        );
        Err(ended)
    }

    /// Whether a device that keeps the person's root took up the call `invocation` kept in
    /// `workspace`'s record for it within `within`: it marked the record seen or taken, or wrote
    /// how it ended.
    pub async fn reached_at_root(
        &self,
        workspace: &str,
        invocation: tilo_actions::InvocationId,
        within: Duration,
    ) -> bool {
        use tokio::sync::broadcast::error::RecvError;

        let mut writes = self.document_writes();
        let mut within = std::pin::pin!(tokio::time::sleep(within));
        loop {
            if self
                .asked_record(workspace, invocation)
                .await
                .is_some_and(|kept| {
                    kept.seen.is_some() || kept.taken.is_some() || kept.outcome.is_some()
                })
            {
                return true;
            }
            tokio::select! {
                write = writes.recv() => match write {
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return false,
                },
                () = within.as_mut() => return false,
            }
        }
    }

    /// Whether this device waits itself on the call `invocation` it kept for a device that keeps
    /// the person's root ([`Self::ask_at_root`]).
    #[must_use]
    pub fn awaits_at_root(&self, invocation: InvocationId) -> bool {
        self.inner
            .awaited_at_root
            .lock_recover()
            .contains(&invocation)
    }

    /// The record `workspace` keeps of the call `invocation` while it waits, as it stands there.
    pub async fn asked_record(
        &self,
        workspace: &str,
        invocation: tilo_actions::InvocationId,
    ) -> Option<KeptAsk> {
        let bytes = self
            .resource_document(workspace, PROVIDER, RECORD)
            .await
            .ok()??;
        read(&bytes)
            .ok()?
            .into_iter()
            .find(|call| call.request.invocation == invocation)
    }

    /// The calls kept to be answered at a device that keeps the person's root, in the records
    /// of every workspace kept here.
    /// End `ask`, a call this host kept for a device that keeps the person's root whose record
    /// could not be written, with `why`: it waits no more here, and whoever follows it reads it
    /// ended so ([`Self::follow_asked_at_root`]).
    fn unkept(&self, mut ask: KeptAsk, why: &str) {
        if let Some(store) = self.asked_store() {
            store.withdraw(&ask.at, why);
        }
        ask.outcome = Some(tilo_actions::InvocationUpdate::Failed {
            invocation: ask.request.invocation,
            error: tilo_actions::ActionFailure {
                code: UNKEPT_CODE.to_owned(),
                message: why.to_owned(),
                details: None,
                retryable: false,
            },
        });
        let workspace = ask.at.workspace.clone();
        self.inner.unkept_asks.lock_recover().push(ask);
        let _ = self.inner.document_writes.send(super::ResourceWrite {
            workspace,
            app: PROVIDER.to_owned(),
            record: RECORD.to_owned(),
            together: Arc::default(),
        });
    }

    async fn asked_at_root(&self) -> Vec<KeptAsk> {
        let mut kept = std::mem::take(&mut *self.inner.unkept_asks.lock_recover());
        for workspace in self.ids().await {
            if let Ok(Some(bytes)) = self
                .resource_document(&workspace.to_string(), PROVIDER, RECORD)
                .await
                && let Ok(calls) = read(&bytes)
            {
                // What this device waits on itself is its own to read and forget.
                kept.extend(calls.into_iter().filter(|call| {
                    call.dispatch == AT_ROOT && !self.awaits_at_root(call.request.invocation)
                }));
            }
        }
        kept
    }
}
