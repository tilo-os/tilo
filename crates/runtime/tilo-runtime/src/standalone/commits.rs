//! The broker's owner commits, through the profile's workspace service.
//!
//! Every workspace this runtime's apps write in is held by the device's
//! workspace service: the profile's own workspace, which the service governs,
//! and the member copies of the workspaces other devices serve. A document
//! write is admitted here, in the writing device's name, and answered with the
//! receipt that binds it; the service keeps it in the workspace's ordered
//! persistence and carries it to the workspace's other devices. The
//! workspaces the desktop session governs are refused, as they always were.

use std::{collections::BTreeSet, sync::Arc, time::SystemTime};

use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::owner_commit::{
    CommitFuture, CommitOperation, LocalDocumentFuture, OwnerCommitReceipt, OwnerCommitRequest,
    OwnerCommitService,
};
use tilo_workspace_service::service::WorkspaceService;
use tilo_workspace_service::workspaces::Workspaces;

use super::service::ServiceWorkspaces;

/// The resource kinds an installed app declares its records hold; `None`
/// for an app this profile has not installed.
pub(in crate::standalone) type DeclaredKinds =
    Box<dyn Fn(&str) -> Option<Vec<String>> + Send + Sync>;

/// In which kinds an app may write a record the workspace does not hold yet.
pub(in crate::standalone) struct RecordCreation {
    pub(in crate::standalone) declared_kinds: DeclaredKinds,
    /// Started with `--dev`: an app that is not installed is a build under
    /// development, and may create records in the kinds it names.
    pub(in crate::standalone) dev: bool,
}

impl RecordCreation {
    fn allows(&self, app: &str, kind: &str) -> bool {
        match (self.declared_kinds)(app) {
            Some(declared) => declared.iter().any(|declared| declared == kind),
            None => self.dev,
        }
    }
}

/// The broker's commits into the workspaces this device's service holds.
pub(super) struct ServiceCommits {
    service: Arc<WorkspaceService>,
    device: [u8; 32],
    creation: Arc<RecordCreation>,
    /// Workspaces whose writes are refused here: those the desktop session
    /// governs.
    refused: BTreeSet<WorkspaceId>,
}

impl ServiceCommits {
    pub(super) fn new(
        workspaces: &Arc<ServiceWorkspaces>,
        creation: Arc<RecordCreation>,
    ) -> Arc<Self> {
        Arc::new(Self {
            service: Arc::clone(workspaces.service()),
            device: workspaces.device_cert().device_public_key,
            creation,
            refused: workspaces.refused_workspaces(),
        })
    }

    fn copies(&self) -> &Workspaces {
        self.service.workspaces()
    }
}

impl OwnerCommitService for ServiceCommits {
    /// The service keeps each device's own effect receipts in its copy of the
    /// workspace: a member copy's are this device's.
    fn records_effects(&self, workspace: &str) -> bool {
        if WorkspaceId::parse(workspace).is_none_or(|workspace| self.refused.contains(&workspace)) {
            return false;
        }
        let copies = self.copies().clone();
        let workspace = workspace.to_owned();
        crate::standalone::service::block_on_owned(async move {
            copies.is_member(&workspace).await.unwrap_or(false)
        })
    }

    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        let copies = self.copies().clone();
        Box::pin(async move {
            copies
                .commit_member_effect_receipt(&request.workspace, &request)
                .await
                .map_err(|error| format!("owner commit failed: {error}"))
        })
    }

    fn commit_with_resource_type(
        &self,
        request: OwnerCommitRequest,
        resource_type: Option<String>,
    ) -> CommitFuture {
        let parts = CommitParts {
            service: Arc::clone(&self.service),
            device: self.device,
            creation: Arc::clone(&self.creation),
            refused: self.refused.clone(),
        };
        Box::pin(async move { parts.commit(request, resource_type).await })
    }

    fn commit_local_document(
        &self,
        request: OwnerCommitRequest,
        resource_type: Option<String>,
    ) -> LocalDocumentFuture {
        let parts = CommitParts {
            service: Arc::clone(&self.service),
            device: self.device,
            creation: Arc::clone(&self.creation),
            refused: self.refused.clone(),
        };
        Box::pin(async move {
            let receipt = parts.commit(request.clone(), resource_type).await?;
            Ok((request, receipt))
        })
    }

    fn health(&self) -> Result<(), String> {
        Ok(())
    }
}

/// The service a write is committed through, held without borrowing the
/// broker's own value: the futures it answers are `'static`.
struct CommitParts {
    service: Arc<WorkspaceService>,
    device: [u8; 32],
    creation: Arc<RecordCreation>,
    refused: BTreeSet<WorkspaceId>,
}

impl CommitParts {
    async fn commit(
        &self,
        request: OwnerCommitRequest,
        resource_type: Option<String>,
    ) -> Result<OwnerCommitReceipt, String> {
        let Some(workspace) = WorkspaceId::parse(&request.workspace) else {
            return Err("invalid exact workspace".to_owned());
        };
        if self.refused.contains(&workspace) {
            return Err("the desktop session's workspace is read only in the runtime".to_owned());
        }
        let copies = self.service.workspaces();
        let peer = match copies.member_copy(workspace).await {
            Some((_, peer)) => peer,
            None => copies
                .governed_copy(workspace)
                .await
                .map(|(_, peer)| peer)
                .ok_or_else(|| "this device holds no such workspace".to_owned())?,
        };
        let (app, record, envelope, own, readers) = match &request.operation {
            CommitOperation::Document {
                app,
                document,
                envelope,
                own,
                readers,
            } => (
                app.clone(),
                document.clone(),
                envelope.clone(),
                own.clone(),
                readers.clone(),
            ),
            _ => return Err("an app writes documents to its workspace".into()),
        };
        if envelope.author.as_bytes() != &self.device {
            return Err("the document's signer differs from this device".into());
        }
        let now = SystemTime::now();
        let held = peer.holds_document(&app, &record);
        let changes = peer.permissions(self.device, now).is_ok_and(|member| {
            member.allows(
                &member.policy.node().app(&app),
                tilo_action_policy::ActionKind::Undoable,
                now,
            )
        });
        if !held {
            let kind = resource_type
                .as_deref()
                .ok_or_else(|| format!("{app} named no kind for a new record"))?;
            if !self.creation.allows(&app, kind) {
                return Err(format!("{app} does not declare the resource kind `{kind}`"));
            }
        }
        if !changes {
            return Err(format!(
                "the person at this device may not change {app} in this workspace"
            ));
        }
        let message = neighbour_wire::NeighbourMsg::AppDocument {
            peer: neighbour_wire::peer_handle(&self.device),
            app_id: app,
            cluster_id: workspace.to_string(),
            document_id: record,
            envelope,
            own,
            readers,
        };
        let body = postcard::to_stdvec(&message).map_err(|error| error.to_string())?;
        let binding = request.binding()?;
        let aggregate_hash = *blake3::hash(&body).as_bytes();
        if copies.is_member(&request.workspace).await.unwrap_or(false) {
            let Some((channel, retention)) = neighbour_wire::retention(&message, false) else {
                return Err("the write carries no retention route".into());
            };
            let order = copies
                .commit_member_document(
                    &request.workspace,
                    tilo_workspace_service::workspace_documents::HostCommit {
                        body,
                        channel,
                        checkpoint: retention == neighbour_wire::Retention::Checkpoint,
                    },
                )
                .await?;
            return Ok(OwnerCommitReceipt {
                binding,
                generation: order.max(1),
                aggregate_hash,
            });
        }
        let name = request.workspace.clone();
        copies.commit_own_document(&name, request).await
    }
}
