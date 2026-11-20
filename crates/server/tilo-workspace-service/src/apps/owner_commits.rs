//! The broker's commits for every session that holds this device's native
//! durable owner.
//!
//! What an app writes in a workspace is committed through this device's
//! workspace service, which holds the one copy of every workspace the device
//! takes part in and carries the write to the workspace's other devices. The
//! receipt of every effect an app's action had is committed to the owner,
//! which publishes it as its outgoing ownership. The questions only the
//! session that holds the owner can answer — which workspaces it commits,
//! where the owner is, whether this device still owns a workspace, and how a
//! commit reaches the workspace's peers — are its [`OwnerService`].

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::owner_commit::{
    CommitFuture, CommitOperation, LocalDocumentFuture, OwnerCommitRequest, OwnerCommitService,
};

pub use tilo_broker_core::owner_service::OwnerService;

/// The broker's commits: documents through the workspace service, receipts through `ownership`.
#[derive(Clone)]
pub struct NativeOwnerCommits {
    ownership: Arc<dyn OwnerService>,
    /// Where documents are committed: this device's workspace service.
    service: crate::service::ServiceSlot,
    writes: OwnerWrites,
    /// What hears of a record committed here: search reads it again.
    changes: Option<Arc<dyn crate::apps::services::DocumentIndexChanges>>,
}

impl NativeOwnerCommits {
    /// Effect receipts committed into the owner `ownership` names, and documents through the
    /// workspace service `service` holds.
    #[must_use]
    pub fn new(
        ownership: Arc<dyn OwnerService>,
        service: crate::service::ServiceSlot,
        changes: Option<Arc<dyn crate::apps::services::DocumentIndexChanges>>,
    ) -> Self {
        Self {
            ownership,
            service,
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
    /// A held workspace's effect receipts are this device's own to record in
    /// it.
    fn records_effects(&self, workspace: &str) -> bool {
        WorkspaceId::parse(workspace)
            .is_some_and(|workspace| self.ownership.projects_workspace(workspace))
    }
    fn health(&self) -> Result<(), String> {
        self.ownership
            .opened_owner("native durable owner unavailable")?
            .health()
    }
    /// An effect's receipt. A record's write is committed through
    /// [`Self::commit_local_document`], which the broker calls directly.
    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        match &request.operation {
            CommitOperation::EffectReceipt { .. } => {}
            _ => {
                return Box::pin(std::future::ready(Err(
                    "unsupported native owner commit family".into(),
                )));
            }
        }
        let ownership = Arc::clone(&self.ownership);
        let writes = self.writes.clone();
        Box::pin(writes.run(move || {
            let owner = ownership
                .resolve_owner()
                .map_err(|error| format!("native durable owner unavailable: {error}"))?;
            let id = WorkspaceId::parse(&request.workspace).ok_or("invalid native workspace")?;
            // A receipt is this device's own, recorded in its person's slice
            // of the workspace and published as its own change.
            ownership.check_ownership(&owner, id)?;
            let receipt = owner.commit_effect_receipt(&request, tilo_clock::wall())?;
            ownership.schedule_publication(id);
            Ok(receipt)
        }))
    }
    /// A document write is committed through this device's workspace service, which holds every
    /// workspace the device takes part in: admitted into the workspace it governs, or taken into
    /// the member copy of one another device serves and carried there. Every read of the
    /// workspace's resources answers from that same copy.
    fn commit_local_document(
        &self,
        request: OwnerCommitRequest,
        _resource_type: Option<String>,
    ) -> LocalDocumentFuture {
        let (copies, runtime) = self.service.service().map_or((None, None), |service| {
            (Some(service.workspaces().clone()), service.writes_runtime())
        });
        let changes = self.changes.clone();
        Box::pin(async move {
            let copies = copies.ok_or("this device's workspace service is not open")?;
            let committing = request.clone();
            let receipt = super::service_resources::on_the_service(
                runtime,
                Box::pin(async move {
                    copies
                        .commit_local_document(&committing.workspace, committing.clone())
                        .await
                }),
            )
            .await?;
            // What search holds of the record is read again from what it now holds.
            if let (Some(changes), CommitOperation::Document { app, document, .. }) =
                (&changes, &request.operation)
            {
                changes.changed(&request.workspace, app, document);
            }
            Ok((request, receipt))
        })
    }
}
