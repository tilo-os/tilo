//! The asked records that wait, kept in each workspace as its `tilo.invocations` record
//! ([`RECORD`]): every call that waits for its person's yes, by its invocation, with what
//! admitting it again needs. The record syncs and is sealed as every record of the workspace
//! is, so a call that waits stays suspended in it across the host's restarts, and the host
//! reads the calls back from it when it starts.

use std::sync::Arc;

use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use tilo_actions::remote::asked::{AskedKeeping, KeptAsk, RecordRef};
use tilo_actions::remote::workspace_invocations::PROVIDER;

use super::{WeakWorkspaces, Workspaces};

/// The record of a workspace's invocations that keeps those waiting for their person's yes:
/// a map from each one's id to the call as it was asked ([`KeptAsk`]).
const RECORD: &str = "asked";

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
    let host = workspaces.serving_certificate().clone();
    let workspace = change.workspace().to_owned();
    workspaces
        .author_workspace_document(&workspace, &host, (PROVIDER, RECORD), |doc| {
            match &change {
                Change::Keep(ask) => {
                    let text = serde_json::to_string(ask).map_err(|error| error.to_string())?;
                    doc.put(automerge::ROOT, ask.at.record.as_str(), text)
                }
                Change::Forget(at) => doc.delete(automerge::ROOT, at.record.as_str()),
            }
            .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| error.to_string())
}

/// The calls `bytes`, a workspace's record of them, keeps.
fn read(bytes: &[u8]) -> Result<Vec<KeptAsk>, String> {
    let doc = automerge::AutoCommit::load(bytes).map_err(|error| error.to_string())?;
    let mut kept = Vec::new();
    for key in doc.keys(automerge::ROOT) {
        let Some((automerge::Value::Scalar(value), _)) = doc
            .get(automerge::ROOT, key.as_str())
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
}
