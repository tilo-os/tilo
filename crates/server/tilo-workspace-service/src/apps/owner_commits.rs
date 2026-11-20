//! The broker's commits into this device's durable owner, for every session
//! that holds one.
//!
//! A document write is committed where the workspace is kept: the service a
//! session's own writes reach, where it holds the workspace, so an app's
//! window writes into the same copy the session reads, and the service keeps
//! it and carries it to the workspace's peers. A workspace the service holds
//! none of, and the receipt of every effect an app's action had, are
//! committed to the owner before anyone else hears of it: the owner is then
//! the canonical copy here, and what it committed it publishes to the
//! workspace's other devices as its outgoing ownership. The questions only the
//! session that holds the owner can answer — which workspaces it commits,
//! where the owner is, whether this device still owns a
//! workspace, and how a commit reaches the workspace's peers — are its
//! [`OwnerService`].

use std::sync::Arc;

use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::owner_commit::{
    CommitFuture, CommitOperation, LocalDocumentFuture, OwnerCommitRequest, OwnerCommitService,
};

use crate::service::ServiceSlot;

pub use tilo_broker_core::owner_service::OwnerService;

/// The broker's owner commits, through `ownership`.
#[derive(Clone)]
pub struct NativeOwnerCommits {
    ownership: Arc<dyn OwnerService>,
    writes: OwnerWrites,
    /// What hears of a record committed here: search reads it again.
    changes: Option<Arc<dyn crate::apps::services::DocumentIndexChanges>>,
    /// This device's workspace service, where it holds the workspace a write is for: the store a
    /// session's own writes reach and its reads answer from. A document write there is committed
    /// by the service's one commit path, so an app's window writes where the session reads, and
    /// the owner's copy of the workspace is left to follow.
    service: Option<ServiceSlot>,
}

impl NativeOwnerCommits {
    /// Commits into the owner `ownership` names.
    #[must_use]
    pub fn new(
        ownership: Arc<dyn OwnerService>,
        changes: Option<Arc<dyn crate::apps::services::DocumentIndexChanges>>,
    ) -> Self {
        Self {
            ownership,
            writes: OwnerWrites::default(),
            changes,
            service: None,
        }
    }

    /// [`Self::new`], with the document writes of the workspaces `service` holds committed by the
    /// service: the one store the embedder's own writes and reads go through.
    #[must_use]
    pub fn through_service(
        service: ServiceSlot,
        ownership: Arc<dyn OwnerService>,
        changes: Option<Arc<dyn crate::apps::services::DocumentIndexChanges>>,
    ) -> Self {
        Self {
            service: Some(service),
            ..Self::new(ownership, changes)
        }
    }

    /// The workspaces this device's service holds, once it opened one.
    /// The workspaces this device's service holds, and the runtime it makes its writes on.
    fn held(
        &self,
    ) -> (
        Option<crate::workspaces::Workspaces>,
        Option<tokio::runtime::Handle>,
    ) {
        self.service
            .as_ref()
            .and_then(ServiceSlot::service)
            .map_or((None, None), |service| {
                (Some(service.workspaces().clone()), service.writes_runtime())
            })
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
    /// A document write is admitted by the owner as it is, whole record or
    /// change: a record `workspace` does not hold yet is founded by the
    /// write itself, sealed, never by one this device publishes ahead of it.
    /// The owner drafts it again past its record's floor when the device's
    /// several local writers signed out of the order its turns take them,
    /// for this device's key to sign.
    ///
    /// A workspace this device's service holds is written there instead: the
    /// service keeps that workspace's copy and carries the write to its peers,
    /// and it is the copy the session's own writes reach and its reads answer
    /// from. The owner keeps only the workspaces the service holds none of.
    fn commit_local_document(
        &self,
        request: OwnerCommitRequest,
        _resource_type: Option<String>,
    ) -> LocalDocumentFuture {
        let ownership = Arc::clone(&self.ownership);
        let writes = self.writes.clone();
        let changes = self.changes.clone();
        let (held, runtime) = self.held();
        Box::pin(async move {
            if let Some(workspaces) = held
                && workspaces.holds_workspace(&request.workspace).await
            {
                let committing = request.clone();
                let receipt = super::service_resources::on_the_service(
                    runtime,
                    Box::pin(async move {
                        workspaces
                            .commit_local_document(&committing.workspace, committing.clone())
                            .await
                    }),
                )
                .await?;
                if let (Some(changes), CommitOperation::Document { app, document, .. }) =
                    (&changes, &request.operation)
                {
                    changes.changed(&request.workspace, app, document);
                }
                return Ok((request, receipt));
            }
            writes
                .run(move || {
                    let owner = ownership
                        .resolve_owner()
                        .map_err(|error| format!("native durable owner unavailable: {error}"))?;
                    let id =
                        WorkspaceId::parse(&request.workspace).ok_or("invalid native workspace")?;
                    let signer = ownership.signer(&owner, id)?;
                    // The commit runs whole on this blocking thread, waiting here for the
                    // ownership's signatures.
                    let (committed, receipt) = tokio::runtime::Handle::current().block_on(
                        owner.commit_local_document(request, signer.as_ref(), tilo_clock::wall()),
                    )?;
                    // What search holds of the record is read again from what it now
                    // holds.
                    if let (
                        Some(changes),
                        CommitOperation::Document {
                            app,
                            document: record,
                            ..
                        },
                    ) = (&changes, &committed.operation)
                    {
                        changes.changed(&committed.workspace, app, record);
                    }
                    // What the owner committed leaves for the workspace's peers from
                    // its ownership, the one copy every write goes through.
                    ownership.schedule_publication(id);
                    Ok((committed, receipt))
                })
                .await
        })
    }
}
