//! One node of a workspace's tree, as an app asks about it: sharing a record or an item of it with
//! the workspace's people (Chat showing a window, a note shared into a channel), ending that share,
//! or asking what the person may do there.
//!
//! A share is not a promise kept by the app that shares: it is grants on the node, written into the
//! workspace's own governance with this device's key, which the workspace decides as it decides
//! every grant and every device of it receives as it receives every change its people make
//! ([`tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Share`]). What a person
//! may do at a node is the workspace's policy, read where this device holds it.

use std::sync::Arc;

use serde_json::json;
use tilo_action_policy::share::{ShareAccess, is_of_share};
use tilo_action_policy::{ActionKind, Authority, Context, TreePath, WorkspacePolicy};
use tilo_broker_core::services::{WorkspaceNodeActions, WorkspaceNodeAsk, WorkspaceNodeOutcome};
use tilo_desktop_model::WorkspaceId;

use crate::engine::Engine;
use crate::membership::Membership;

/// An app's asks about the nodes of a workspace this device holds, answered here as the person at
/// this device: the context the broker keeps nothing of, because the workspace decides every one.
pub(crate) struct WorkspaceNodes {
    engine: Arc<Engine>,
    /// Who sits here, as the workspaces' governance names them.
    person: String,
}

impl WorkspaceNodes {
    /// The service over `engine`'s workspaces; `None` for a session that holds none, which has no
    /// governance to ask and no grant to give.
    pub(crate) fn service(engine: Option<&Arc<Engine>>) -> Option<Arc<dyn WorkspaceNodeActions>> {
        let engine = Arc::clone(engine?);
        let person = hex::encode(engine.identity().device_cert.user_public_key);
        Some(Arc::new(Self { engine, person }) as Arc<dyn WorkspaceNodeActions>)
    }

    /// The governance of `workspace` as this device holds it.
    fn policy(&self, workspace: &str) -> Result<WorkspacePolicy, String> {
        self.engine.policy(workspace)
    }

    /// What `ask` of `node` in `workspace` comes to: `grants` a share holds, `revoked` by its end,
    /// or `read` and `undoable` at the node.
    fn answer(&self, workspace: &str, node: &str, ask: WorkspaceNodeAsk) -> WorkspaceNodeOutcome {
        let id = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        if !self.engine.holds(id) {
            return Err("this device does not hold the workspace".into());
        }
        let node: TreePath = node
            .parse()
            .map_err(|_| "`node` is not a node of the workspace's tree".to_owned())?;
        if node.workspace_id() != Some(workspace) {
            return Err("the node is not in this workspace".into());
        }
        let held = |share: &str| {
            self.policy(workspace).map(|policy| {
                policy
                    .grants
                    .iter()
                    .filter(|grant| is_of_share(grant, share))
                    .count()
            })
        };
        let membership = Membership::of(&self.engine, id);
        Ok(match ask {
            WorkspaceNodeAsk::Share { share, access } => {
                let access: ShareAccess = access.parse().map_err(
                    |refusal: tilo_action_policy::share::ShareRefusal| refusal.to_string(),
                )?;
                // What the share gives, as the workspace writes it: the same grants every device
                // reads, bounded by what this person holds there when the workspace admits them.
                membership.share(&share, node, access)?;
                json!({ "grants": held(&share)? })
            }
            WorkspaceNodeAsk::Unshare { share } => {
                let before = held(&share)?;
                membership.unshare(&share)?;
                json!({ "revoked": before.saturating_sub(held(&share)?) })
            }
            WorkspaceNodeAsk::Permits => {
                let policy = self.policy(workspace)?;
                let authority = Authority::person(policy.party(&self.person));
                let now = Context::at_time(tilo_clock::wall());
                let may = |kind| {
                    authority.decide(&node, kind, &now).answer == tilo_action_policy::Answer::Allow
                };
                json!({ "read": may(ActionKind::Read), "undoable": may(ActionKind::Undoable) })
            }
        })
    }
}

impl WorkspaceNodeActions for WorkspaceNodes {
    fn ask(
        &self,
        workspace: &str,
        node: &str,
        ask: WorkspaceNodeAsk,
    ) -> Result<tilo_actions::remote::Pending<WorkspaceNodeOutcome>, String> {
        let (complete, answer) = tilo_actions::remote::pending();
        let workspace = workspace.to_owned();
        let node = node.to_owned();
        let this = Self {
            engine: Arc::clone(&self.engine),
            person: self.person.clone(),
        };
        let answer_it = move || this.answer(&workspace, &node, ask);
        // A share is a commit into the workspace's record, and what a person may do there waits on
        // that record's open: both wait on the disk, off the loop the ask arrived on.
        #[cfg(target_os = "linux")]
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(move || complete.complete(answer_it()));
            return Ok(answer);
        }
        complete.complete(answer_it());
        Ok(answer)
    }
}
