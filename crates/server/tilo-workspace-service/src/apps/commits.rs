//! The broker's writes for every session that holds this device's workspaces.
//!
//! What an app writes in a workspace is committed through this device's
//! workspace service, which holds the one copy of every workspace the device
//! takes part in and carries the write to the workspace's other devices. The
//! receipt of every effect an app's action had is recorded in the workspace's
//! own document.

use std::sync::Arc;

use tilo_broker_core::workspace_commits::{DocumentFuture, EffectFuture, WorkspaceCommits};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::owner::WriteRefusal;

/// The broker's writes: documents through the workspace service, effects in the workspace's own
/// document.
#[derive(Clone)]
pub struct ServiceCommits {
    /// Where documents and receipts are committed: this device's workspace service.
    service: crate::service::ServiceSlot,
    writes: OwnerWrites,
    /// What hears of a record committed here: search reads it again.
    changes: Option<Arc<dyn crate::apps::services::DocumentIndexChanges>>,
}

impl ServiceCommits {
    /// Effect receipts kept in the workspace `service` holds, and documents through it.
    #[must_use]
    pub fn new(
        service: crate::service::ServiceSlot,
        changes: Option<Arc<dyn crate::apps::services::DocumentIndexChanges>>,
    ) -> Self {
        Self {
            service,
            writes: OwnerWrites::default(),
            changes,
        }
    }
}

/// Where a commit runs. A commit waits for the disk (the state file's data and directory syncs),
/// which on a loaded disk takes seconds, and its future is awaited on the broker's single-threaded
/// loop, which answers every peer action, catalogue and liveness probe. So a commit runs on a
/// blocking thread instead, and the loop goes on answering while it waits. Commits still run one at
/// a time, in the order they were asked for.
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
        .map_err(|_| "a workspace write ended without an outcome".to_string())?
    }
}

impl WorkspaceCommits for ServiceCommits {
    fn projects_workspace(&self, _workspace: WorkspaceId) -> bool {
        // The service holds every workspace this device takes part in: its effects are its own
        // to record, whichever copy it keeps.
        self.service.service().is_some()
    }

    fn health(&self) -> Result<(), String> {
        self.service
            .service()
            .map(|_| ())
            .ok_or_else(|| "this device's workspace service is not open".to_owned())
    }

    fn document(
        &self,
        workspace: String,
        app: String,
        document: String,
        readers: Option<String>,
        envelope: collab_types::SignedOp,
        _resource_type: Option<String>,
    ) -> DocumentFuture {
        let (copies, runtime) = self.service.service().map_or((None, None), |service| {
            (Some(service.workspaces().clone()), service.writes_runtime())
        });
        let changes = self.changes.clone();
        Box::pin(async move {
            let copies = copies.ok_or("this device's workspace service is not open")?;
            let committing = workspace.clone();
            super::service_resources::on_the_service(
                runtime,
                Box::pin(async move {
                    copies
                        .commit_local_document(
                            &committing,
                            &app,
                            &document,
                            readers.as_deref(),
                            envelope.clone(),
                        )
                        .await
                        .map_err(|refusal: WriteRefusal| refusal.to_string())
                }),
            )
            .await?;
            // What search holds of the record is read again from what it now holds.
            if let Some(changes) = &changes {
                changes.changed(&workspace, &app, &document);
            }
            Ok(envelope)
        })
    }

    fn effect(&self, workspace: String, redacted: Vec<u8>) -> EffectFuture {
        let (copies, runtime) = self.service.service().map_or((None, None), |service| {
            (Some(service.workspaces().clone()), service.writes_runtime())
        });
        Box::pin(async move {
            let copies = copies.ok_or("this device's workspace service is not open")?;
            super::service_resources::on_the_service(
                runtime,
                Box::pin(async move {
                    copies
                        .commit_member_effect_receipt(&workspace, &redacted)
                        .await
                }),
            )
            .await
        })
    }
}
