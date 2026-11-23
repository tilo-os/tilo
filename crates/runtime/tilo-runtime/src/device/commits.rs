//! The broker's writes, through the device's workspace service.
//!
//! Every workspace this daemon's apps write in is kept by the device's
//! workspace service: a workspace another device serves, or the workspace
//! founded here, which the service governs. A document write is admitted here,
//! in the writing device's name, and answered with the signed write that binds
//! it; the service keeps it in the workspace's ordered persistence and carries
//! it to the workspace's other devices.

use std::{sync::Arc, time::SystemTime};

use tilo_desktop_model::WorkspaceId;
use tilo_workspace_service::apps::workspace_commits::{
    DocumentFuture, EffectFuture, WorkspaceCommits,
};
use tilo_workspace_service::service::WorkspaceService;
use tilo_workspace_service::workspaces::Workspaces;

use super::workspaces::ServiceWorkspaces;

/// The resource kinds an installed app declares its records hold; `None`
/// for an app this profile has not installed.
pub(super) type DeclaredKinds = Box<dyn Fn(&str) -> Option<Vec<String>> + Send + Sync>;

/// In which kinds an app may write a resource this device's own workspace does not hold yet.
pub(super) struct RecordCreation {
    pub(super) declared_kinds: DeclaredKinds,
    /// Started with `--dev`: an app that is not installed is a build under
    /// development, and may create records in the kinds it names.
    pub(super) dev: bool,
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
        })
    }

    fn copies(&self) -> &Workspaces {
        self.service.workspaces()
    }
}

impl WorkspaceCommits for ServiceCommits {
    /// The service keeps each device's own effect receipts in its copy of the
    /// workspace: a member copy's are this device's.
    fn projects_workspace(&self, workspace: WorkspaceId) -> bool {
        let copies = self.copies().clone();
        super::workspaces::block_on_owned(async move {
            copies
                .is_member(&workspace.to_string())
                .await
                .unwrap_or(false)
        })
    }

    fn effect(&self, workspace: String, redacted: Vec<u8>) -> EffectFuture {
        let copies = self.copies().clone();
        Box::pin(async move {
            copies
                .commit_member_effect_receipt(&workspace, &redacted)
                .await
                .map_err(|error| format!("the effect receipt was not recorded: {error}"))
        })
    }

    fn document(
        &self,
        workspace: String,
        app: String,
        document: String,
        readers: Option<String>,
        envelope: collab_types::SignedOp,
        resource_type: Option<String>,
    ) -> DocumentFuture {
        let parts = CommitParts {
            service: Arc::clone(&self.service),
            device: self.device,
            creation: Arc::clone(&self.creation),
        };
        Box::pin(async move {
            parts
                .commit(workspace, app, document, readers, envelope, resource_type)
                .await
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
}

impl CommitParts {
    async fn commit(
        &self,
        workspace: String,
        app: String,
        document: String,
        readers: Option<String>,
        envelope: collab_types::SignedOp,
        resource_type: Option<String>,
    ) -> Result<collab_types::SignedOp, String> {
        let Some(workspace_id) = WorkspaceId::parse(&workspace) else {
            return Err("invalid exact workspace".to_owned());
        };
        let copies = self.service.workspaces();
        // What type a new resource may be is this device's rule only in the workspace it
        // governs, where the apps installed here say it; a member copy's writes are the
        // governing device's to admit.
        let (peer, governs) = match copies.member_copy(workspace_id).await {
            Some((_, peer)) => (peer, false),
            None => (
                copies
                    .governed_copy(workspace_id)
                    .await
                    .map(|(_, peer)| peer)
                    .ok_or_else(|| "this device holds no such workspace".to_owned())?,
                true,
            ),
        };
        if envelope.author.as_bytes() != &self.device {
            return Err("the document's signer differs from this device".into());
        }
        let now = SystemTime::now();
        let held = peer.holds_document(&app, &document);
        let changes = peer.permissions(self.device, now).is_ok_and(|member| {
            member.allows(
                &member.policy.node().app(&app),
                tilo_action_policy::ActionKind::Undoable,
                now,
            )
        });
        if !held && governs {
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
        drop(peer);
        copies
            .commit_local_document(
                &workspace,
                &app,
                &document,
                readers.as_deref(),
                envelope.clone(),
            )
            .await?;
        Ok(envelope)
    }
}
