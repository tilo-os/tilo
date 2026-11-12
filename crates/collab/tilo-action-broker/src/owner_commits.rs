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

use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::owner_commit::{
    CommitFuture, CommitOperation, LocalDocumentFuture, OwnerCommitRequest, OwnerCommitService,
};

pub use tilo_broker_core::custody::OwnerCustody;

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
    /// An effect's receipt; a document write commits through
    /// [`Self::commit_local_document`].
    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        match &request.operation {
            CommitOperation::EffectReceipt { .. } => {}
            CommitOperation::Document { .. } => {
                let committed = self.commit_local_document(request, None);
                return Box::pin(async move { Ok(committed.await?.1) });
            }
            _ => {
                return Box::pin(std::future::ready(Err(
                    "unsupported native owner commit family".into(),
                )));
            }
        }
        let custody = Arc::clone(&self.custody);
        let writes = self.writes.clone();
        Box::pin(writes.run(move || {
            let owner = custody
                .resolve_owner()
                .map_err(|error| format!("native durable owner unavailable: {error}"))?;
            let room = WorkspaceId::parse(&request.workspace).ok_or("invalid native room")?;
            // A receipt is this device's own, recorded in its person's slice
            // of the workspace and published as its own change.
            custody.check_custody(&owner, room)?;
            let receipt = owner.commit_effect_receipt(&request, std::time::SystemTime::now())?;
            custody.schedule_publication(room);
            Ok(receipt)
        }))
    }
    /// A document write is admitted by the owner as it is, whole record or
    /// change: a record `workspace` does not hold yet is founded by the
    /// write itself, sealed, never by one this device publishes ahead of it.
    /// The owner drafts it again past its record's floor when the device's
    /// several local writers signed out of the order its turns take them,
    /// for this device's key to sign.
    fn commit_local_document(
        &self,
        request: OwnerCommitRequest,
        _record_kind: Option<String>,
    ) -> LocalDocumentFuture {
        let custody = Arc::clone(&self.custody);
        let writes = self.writes.clone();
        let changes = self.changes.clone();
        Box::pin(writes.run(move || {
            let owner = custody
                .resolve_owner()
                .map_err(|error| format!("native durable owner unavailable: {error}"))?;
            let room = WorkspaceId::parse(&request.workspace).ok_or("invalid native room")?;
            let signer = custody.signer(&owner, room)?;
            let (committed, receipt) = futures::executor::block_on(owner.commit_local_document(
                request,
                signer.as_ref(),
                std::time::SystemTime::now(),
            ))?;
            // What search holds of the record is read again from what it now
            // holds.
            if let (Some(changes), CommitOperation::Document { app, record, .. }) =
                (&changes, &committed.operation)
            {
                changes.changed(&committed.workspace, app, record);
            }
            // What the owner committed leaves for the workspace's peers from
            // its custody, the one copy every write goes through.
            custody.schedule_publication(room);
            Ok((committed, receipt))
        }))
    }
}
