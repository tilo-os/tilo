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

use std::sync::Arc;

use automerge::transaction::Transactable as _;
use automerge::{ObjType, ROOT, ReadDoc as _, Value};
use tilo_actions::remote::asked::{AT_ROOT, AskedKeeping, KeptAsk, RecordRef};
use tilo_actions::remote::workspace_invocations::PROVIDER;

use super::{WeakWorkspaces, Workspaces};

/// The record of a workspace's invocations that keeps those waiting for their person's yes:
/// a map from each one's id to the call as it was asked ([`KeptAsk`]).
const RECORD: &str = "asked";
/// The kind the record is, as [`SURFACE`] declares it.
const KIND: &str = "tilo.invocations.asked";
/// Where the record keeps the calls: each one's id to the call as it was asked, as JSON.
const CALLS: &str = "calls";
/// What [`PROVIDER`] declares its records are, so every holder's store keeps them.
pub(crate) const SURFACE: &str = r#"format = 2

[[resources]]
kind = "tilo.invocations.asked"
label = "Asked calls"
version = 1
fields = [
  { path = "calls", type = { map = "text" } },
]

[[resources.verbs]]
id = "type"
kind = "undoable"
"#;

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

/// Write `change` into its workspace's record, as this host.
async fn write(workspaces: &Workspaces, change: Change) -> Result<(), String> {
    let workspace = change.workspace().to_owned();
    workspaces
        .author_record(&workspace, (PROVIDER, KIND, RECORD), |doc| {
            let calls = match doc.get(ROOT, CALLS).map_err(|error| error.to_string())? {
                Some((Value::Object(ObjType::Map), calls)) => calls,
                _ => doc
                    .put_object(ROOT, CALLS, ObjType::Map)
                    .map_err(|error| error.to_string())?,
            };
            match &change {
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
                if let Err(why) = write(&workspaces, change).await {
                    tracing::error!(%why, %record, "a workspace's record of its asked calls was not written");
                }
            }
        });
        Arc::new(KeptInRecords { changes })
    }

    /// Every asked call that waits in the records of the workspaces this host governs, as
    /// they were kept before it started.
    pub async fn asked_kept(&self) -> Vec<KeptAsk> {
        let mut kept = Vec::new();
        for workspace in self.governed_ids().await {
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
    /// again on each write to one of those records, and again whenever presence moves (a device
    /// of the person's came online), from a task of the runtime this is called on.
    pub fn follow_asked_at_root(&self, seen: impl Fn(Vec<KeptAsk>) + Send + 'static) {
        let workspaces = self.downgrade();
        let mut writes = self.document_writes();
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
                let written = async {
                    loop {
                        match writes.recv().await {
                            Ok(write) if write.app == PROVIDER && write.record == RECORD => {
                                return true;
                            }
                            Ok(_) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                return true;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return false,
                        }
                    }
                };
                let moved = tokio::select! {
                    open = written => {
                        if !open {
                            return;
                        }
                        false
                    }
                    _ = moved => true,
                };
                if moved {
                    presence = None;
                }
            }
        });
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
    async fn asked_at_root(&self) -> Vec<KeptAsk> {
        let mut kept = Vec::new();
        for workspace in self.ids().await {
            if let Ok(Some(bytes)) = self
                .resource_document(&workspace.to_string(), PROVIDER, RECORD)
                .await
                && let Ok(calls) = read(&bytes)
            {
                kept.extend(calls.into_iter().filter(|call| call.dispatch == AT_ROOT));
            }
        }
        kept
    }
}
