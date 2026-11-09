//! The broker's commits into this device's native durable owner, for every
//! session that holds one.
//!
//! What an app writes in a workspace whose records the owner commits, and
//! the receipt of every effect an app's action had, is committed to the
//! owner before anyone else hears of it: the owner is the canonical copy of
//! the workspace on this device, and what it committed it publishes to the
//! workspace's other devices as its outgoing custody. The questions only the
//! session that holds the owner can answer — which workspaces it commits,
//! where the owner is, whether this device still holds custody of a
//! workspace, and how a commit reaches the workspace's peers — are its
//! [`OwnerCustody`].

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::owner::NativeWorkspaceOwner;
use tilo_peer_state::owner_commit::{
    CommitFuture, CommitOperation, OwnerCommitRequest, OwnerCommitService,
};

/// What the session holding the native owner answers for its commits.
pub trait OwnerCustody: Send + Sync + 'static {
    /// Whether `workspace`'s app records are documents the owner commits:
    /// a workspace held here whose host takes part in no channels.
    fn commits_documents(&self, workspace: WorkspaceId) -> bool;
    /// Whether `workspace`'s replica is projected here, its records committed
    /// documents or sealed channels: a workspace held here, with its
    /// governance.
    fn projects_room(&self, workspace: WorkspaceId) -> bool;
    /// The owner if it is open, never waiting: `unavailable` when there is
    /// none.
    fn opened_owner(&self, unavailable: &str) -> Result<NativeWorkspaceOwner, String>;
    /// The owner, waiting for its open: for a commit's blocking thread.
    fn resolve_owner(&self) -> Result<NativeWorkspaceOwner, String>;
    /// Whether this device still holds custody of `workspace` in `owner`, as
    /// its current identity for the workspace says.
    ///
    /// # Errors
    /// Why this device may not commit to `workspace` now.
    fn check_custody(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
    ) -> Result<(), String>;
    /// The key this device signs its own changes of `workspace` with.
    ///
    /// # Errors
    /// This device holds no custody of `workspace`, or has no key.
    fn signing_key(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
    ) -> Result<SigningKey, String>;
    /// Send the owner's outgoing custody of `workspace` to its peers soon:
    /// what was just committed leaves this device. Queued, never awaited.
    fn schedule_publication(&self, workspace: WorkspaceId);
}

/// The broker's owner commits, through `custody`.
#[derive(Clone)]
pub struct NativeOwnerCommits {
    custody: Arc<dyn OwnerCustody>,
    writes: OwnerWrites,
    /// What hears of a record committed here: search reads it again.
    changes: Option<Arc<dyn crate::services::RecordIndexChanges>>,
}

impl NativeOwnerCommits {
    /// Commits into the owner `custody` names.
    #[must_use]
    pub fn new(
        custody: Arc<dyn OwnerCustody>,
        changes: Option<Arc<dyn crate::services::RecordIndexChanges>>,
    ) -> Self {
        Self {
            custody,
            writes: OwnerWrites::default(),
            changes,
        }
    }
}

/// Where the owner's commits run. A commit waits for the disk (opening the
/// owner, the state file's data and directory syncs), which on a loaded disk
/// takes seconds, and its future is awaited on the broker's single-threaded
/// loop, which answers every peer action, catalogue and liveness probe. So a
/// commit runs on a blocking thread instead, and the loop goes on answering
/// while it waits. Commits still run one at a time, in the order they were
/// asked for: the turn is held until the write is done, also when the future
/// awaiting it is dropped (a dropped wait never cancels a started commit).
#[derive(Clone, Default)]
struct OwnerWrites(Arc<tokio::sync::Mutex<()>>);

impl OwnerWrites {
    async fn run<T: Send + 'static>(
        self,
        write: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let turn = self.0.lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let written = write();
            drop(turn);
            written
        })
        .await
        .map_err(|_| "native owner commit ended without an outcome".to_string())?
    }
}

impl OwnerCommitService for NativeOwnerCommits {
    fn requires_workspace(&self, workspace: &str) -> bool {
        WorkspaceId::parse(workspace).is_some_and(|room| self.custody.commits_documents(room))
    }
    /// A held workspace's effect receipts are this device's own to record in
    /// it, whichever model its records follow.
    fn records_effects(&self, workspace: &str) -> bool {
        WorkspaceId::parse(workspace).is_some_and(|room| self.custody.projects_room(room))
    }
    fn health(&self) -> Result<(), String> {
        self.custody
            .opened_owner("native durable owner unavailable")?
            .health()
    }
    /// A document write is admitted by the owner as it is, whole record or
    /// change: a record `workspace` does not hold yet is founded by the
    /// write itself, sealed, never by one this device publishes ahead of it.
    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        let custody = Arc::clone(&self.custody);
        let writes = self.writes.clone();
        let changes = self.changes.clone();
        Box::pin(writes.run(move || {
            let owner = custody
                .resolve_owner()
                .map_err(|error| format!("native durable owner unavailable: {error}"))?;
            let room = WorkspaceId::parse(&request.workspace).ok_or("invalid native room")?;
            let now = std::time::SystemTime::now();
            if !matches!(request.operation, CommitOperation::EffectReceipt { .. }) {
                custody.check_custody(&owner, room)?;
                let record = match &request.operation {
                    CommitOperation::Document { app, record, .. } => {
                        Some((request.workspace.clone(), app.clone(), record.clone()))
                    }
                    _ => None,
                };
                let receipt = owner.commit_document(request, now)?;
                // What search holds of the record is read again from what it
                // now holds.
                if let (Some(changes), Some((workspace, app, record))) = (&changes, record) {
                    changes.changed(&workspace, &app, &record);
                }
                // What the owner committed leaves for the workspace's peers
                // from its custody, the one copy every write goes through.
                custody.schedule_publication(room);
                return Ok(receipt);
            }
            // A receipt is this device's own, recorded in its person's slice
            // of the workspace with its own key and published as its own
            // change.
            let key = custody.signing_key(&owner, room)?;
            let receipt = owner.commit_effect_receipt(&request, &key, now)?;
            custody.schedule_publication(room);
            Ok(receipt)
        }))
    }
}
