//! The workspaces a device's service keeps, as the app broker asks about them and writes to them.
//!
//! Every device that runs apps over its workspace service answers the broker the same way: a
//! workspace is described from what the service keeps of it ([`describe`]), and a change an
//! app makes is committed through the service, in the name of the device that ran the app's
//! code ([`ServiceCommits`]). What differs between devices is passed in: which
//! resource types its installed apps may create.

use std::sync::Arc;

use tilo_desktop_model::{Workspace, WorkspaceId};
use tilo_peer_state::StatePeer;
use tilo_peer_state::owner_commit::{
    CommitFuture, CommitOperation, LocalDocumentFuture, OwnerCommitReceipt, OwnerCommitRequest,
    OwnerCommitService,
};

use crate::workspaces::Workspaces;

/// A workspace as what its service keeps of it says: its name, its people and the sandbox its
/// apps are granted. `None` while the service has not admitted the workspace.
#[must_use]
pub fn describe(workspace: WorkspaceId, name: &str, peer: &StatePeer) -> Option<Workspace> {
    let document = peer.admitted_workspace()?;
    let mut described = Workspace::new(workspace, name.to_owned());
    tilo_peer_state::workspace::adopt_workspace(&mut described, document, &[]);
    described.sandbox = peer.governance().governed_sandbox().ok()?;
    // The names the workspace keeps for its people: what every view showing someone who is not
    // here reads, from the same list the agent worker's members come from.
    if let Ok(people) = peer.governance().governed_members(tilo_clock::wall()) {
        described.named_people(
            people
                .members
                .iter()
                .filter(|member| !member.removed)
                .filter_map(|member| Some((member.id.person_id()?, member.name.as_str()))),
        );
    }
    Some(described)
}

/// Whether an app may create a resource of a resource type in a workspace this device's
/// service admits writes for: the types its installed package declares.
pub type MayCreate = Box<dyn Fn(&str, &str) -> bool + Send + Sync>;

/// The broker's commits into the workspaces a device's service keeps.
pub struct ServiceCommits {
    workspaces: Workspaces,
    device: [u8; 32],
    may_create: Arc<MayCreate>,
    /// The runtime the service runs on, which the broker's synchronous questions are answered on.
    runtime: tokio::runtime::Handle,
}

impl ServiceCommits {
    /// Commits through `workspaces`, signed by `device`, creating only what `may_create`
    /// allows; `runtime` is the one the service runs on.
    #[must_use]
    pub fn new(
        workspaces: Workspaces,
        device: [u8; 32],
        may_create: MayCreate,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        Arc::new(Self {
            workspaces,
            device,
            may_create: Arc::new(may_create),
            runtime,
        })
    }

    fn parts(&self) -> CommitParts {
        CommitParts {
            workspaces: self.workspaces.clone(),
            device: self.device,
            may_create: Arc::clone(&self.may_create),
        }
    }
}

impl OwnerCommitService for ServiceCommits {
    /// The service keeps each device's own effect receipts in its copy of a workspace another
    /// device serves.
    fn records_effects(&self, workspace: &str) -> bool {
        if WorkspaceId::parse(workspace).is_none() {
            return false;
        }
        let workspaces = self.workspaces.clone();
        let workspace = workspace.to_owned();
        block_on(&self.runtime, async move {
            workspaces.is_member(&workspace).await.unwrap_or(false)
        })
    }

    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        let workspaces = self.workspaces.clone();
        Box::pin(async move {
            workspaces
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
        let parts = self.parts();
        Box::pin(async move { parts.commit(request, resource_type).await })
    }

    fn commit_local_document(
        &self,
        request: OwnerCommitRequest,
        resource_type: Option<String>,
    ) -> LocalDocumentFuture {
        let parts = self.parts();
        Box::pin(async move {
            let receipt = parts.commit(request.clone(), resource_type).await?;
            Ok((request, receipt))
        })
    }

    fn health(&self) -> Result<(), String> {
        Ok(())
    }
}

/// What a write is committed through, kept without borrowing the broker's own value: the
/// futures it answers are `'static`.
struct CommitParts {
    workspaces: Workspaces,
    device: [u8; 32],
    may_create: Arc<MayCreate>,
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
        // What type a new resource may be is this device's rule only in a workspace whose writes
        // it admits itself; a write to one another device serves is that device's to admit.
        let (peer, admits) = match self.workspaces.member_copy(workspace).await {
            Some((_, peer)) => (peer, false),
            None => (
                self.workspaces
                    .governed_copy(workspace)
                    .await
                    .map(|(_, peer)| peer)
                    .ok_or_else(|| "this device keeps no such workspace".to_owned())?,
                true,
            ),
        };
        let CommitOperation::Document {
            app,
            document,
            envelope,
            ..
        } = &request.operation
        else {
            return Err("an app changes resources in its workspace".into());
        };
        let (app, document) = (app.clone(), document.clone());
        if envelope.author.as_bytes() != &self.device {
            return Err("the change's signer differs from this device".into());
        }
        let now = tilo_clock::wall();
        let kept = peer.holds_document(&app, &document);
        let changes = peer.permissions(self.device, now).is_ok_and(|member| {
            member.allows(
                &member.policy.node().app(&app),
                tilo_action_policy::ActionKind::Undoable,
                now,
            )
        });
        if !kept && admits {
            let resource_type = resource_type
                .as_deref()
                .ok_or_else(|| format!("{app} named no resource type for a new resource"))?;
            if !(self.may_create)(&app, resource_type) {
                return Err(format!(
                    "{app} does not declare the resource type `{resource_type}`"
                ));
            }
        }
        if !changes {
            return Err(format!(
                "this device may not change {app} in this workspace"
            ));
        }
        drop(peer);
        let workspace = request.workspace.clone();
        self.workspaces
            .commit_local_document(&workspace, request)
            .await
            .map_err(String::from)
    }
}

/// Run `future` on `runtime` from a synchronous call: from a thread of its own when this one
/// already drives a runtime, where a second `block_on` would panic.
pub fn block_on<F>(runtime: &tokio::runtime::Handle, future: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_err() {
        return runtime.block_on(future);
    }
    std::thread::scope(
        |scope| match scope.spawn(|| runtime.block_on(future)).join() {
            Ok(output) => output,
            Err(panic) => std::panic::resume_unwind(panic),
        },
    )
}
