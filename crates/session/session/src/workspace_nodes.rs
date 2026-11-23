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
use std::time::Duration;

use serde_json::json;
use tilo_action_policy::share::{ShareAccess, is_of_share};
use tilo_action_policy::{ActionKind, Authority, Context, TreePath, WorkspacePolicy};
use tilo_broker_core::services::{WorkspaceNodeActions, WorkspaceNodeAsk, WorkspaceNodeOutcome};
use tilo_desktop_model::WorkspaceId;

use crate::membership::Membership;
use crate::workspace_view::WorkspaceView;

/// How long after the daemon took a share its standing is waited for to say it: it says it on its
/// next reading, which follows the change at once.
const STANDING_SAYS_WITHIN: Duration = Duration::from_secs(10);

/// An app's asks about the nodes of a workspace this device holds, answered here as the person at
/// this device: the context the broker keeps nothing of, because the workspace decides every one.
pub(crate) struct WorkspaceNodes {
    view: Arc<WorkspaceView>,
    /// Who sits here, as the workspaces' governance names them.
    person: String,
}

impl WorkspaceNodes {
    /// The service over the workspaces `view` reads; `None` for a session that reads none, which has no
    /// governance to ask and no grant to give.
    pub(crate) fn service(
        view: Option<&Arc<WorkspaceView>>,
    ) -> Option<Arc<dyn WorkspaceNodeActions>> {
        let view = Arc::clone(view?);
        let person = hex::encode(view.person());
        Some(Arc::new(Self { view, person }) as Arc<dyn WorkspaceNodeActions>)
    }

    /// The governance of `workspace` as this device holds it.
    fn policy(&self, workspace: &str) -> Result<WorkspacePolicy, String> {
        self.view.policy(workspace)
    }

    /// What `ask` of `node` in `workspace` comes to: `grants` a share holds, `revoked` by its end,
    /// or `read` and `undoable` at the node.
    async fn answer(
        &self,
        workspace: &str,
        node: &str,
        ask: WorkspaceNodeAsk,
    ) -> WorkspaceNodeOutcome {
        let id = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        if !self.view.holds(id) {
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
        let membership = Membership::of(&self.view, id);
        Ok(match ask {
            WorkspaceNodeAsk::Share { share, access } => {
                let access: ShareAccess = access.parse().map_err(
                    |refusal: tilo_action_policy::share::ShareRefusal| refusal.to_string(),
                )?;
                // What the share gives, as the workspace writes it: the same grants every device
                // reads, bounded by what this person holds there when the workspace admits them.
                membership.share(&share, node, access).await?;
                self.settled(|| held(&share).is_ok_and(|grants| grants > 0))
                    .await;
                json!({ "grants": held(&share)? })
            }
            WorkspaceNodeAsk::Unshare { share } => {
                let before = held(&share)?;
                membership.unshare(&share).await?;
                self.settled(|| held(&share).is_ok_and(|grants| grants == 0))
                    .await;
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

impl WorkspaceNodes {
    /// Wait until the daemon's standing says what it just took, as `said` reads it, or until it
    /// had time to: what the share answers is what every device reads, not what was asked.
    async fn settled(&self, mut said: impl FnMut() -> bool) {
        let settled = self.view.until(|_| said());
        if tokio::time::timeout(STANDING_SAYS_WITHIN, settled)
            .await
            .is_err()
        {
            tracing::warn!("the daemon took a share its standing does not say yet");
        }
    }
}

impl WorkspaceNodeActions for WorkspaceNodes {
    fn ask(
        &self,
        workspace: &str,
        node: &str,
        ask: WorkspaceNodeAsk,
    ) -> Result<tilo_actions::remote::Pending<WorkspaceNodeOutcome>, String> {
        let workspace = workspace.to_owned();
        let node = node.to_owned();
        let this = Self {
            view: Arc::clone(&self.view),
            person: self.person.clone(),
        };
        // A share is a commit into the workspace's record, signed by this device's signer: it
        // answers once signed, off the loop the ask arrived on.
        Ok(crate::landing::off_loop(async move {
            this.answer(&workspace, &node, ask).await
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use tilo_action_policy::share::share_grants;
    use tilo_actions::remote::{InvokeRequest, PendingPoll};
    use tilo_peer_state::workspace_view::{WorkspaceReading, resource};
    use tilo_session_client::engine::ServiceAnswer;
    use tilo_session_client::workspace_view::ShellAsks;

    use super::*;

    /// A daemon busy with other work: each change it is asked is answered once the test lets it,
    /// through the sender it hands the test for it.
    struct Late(std::sync::Mutex<mpsc::Sender<tokio::sync::oneshot::Sender<()>>>);

    impl ShellAsks for Late {
        fn account(
            &self,
            _: &str,
            _: &str,
            _: serde_json::Map<String, serde_json::Value>,
        ) -> ServiceAnswer {
            Box::pin(std::future::ready(Err(
                "nothing founds or joins here".to_owned()
            )))
        }

        fn invoke(&self, _: InvokeRequest) -> ServiceAnswer {
            let (answer, answered) = tokio::sync::oneshot::channel();
            self.0
                .lock()
                .expect("the test hears its asks")
                .send(answer)
                .expect("the test hears its asks");
            Box::pin(async move {
                answered
                    .await
                    .map(|()| serde_json::Value::Null)
                    .map_err(|_| "the daemon stopped".to_owned())
            })
        }
    }

    /// A share the daemon answers late hands its asker a pending answer at once and stops
    /// nothing: the loop that asked goes on, and the share lands once the daemon answered and
    /// its standing says the share's grant.
    #[test]
    fn a_share_the_daemon_answers_late_never_stops_the_loop_that_asked() {
        let workspace = WorkspaceId::new();
        let id = workspace.to_string();
        let person = [3; 32];
        let owner = hex::encode(person);
        let (asked, asks) = mpsc::channel();
        let view = Arc::new(WorkspaceView::new(
            person,
            [4; 32],
            Arc::new(Late(std::sync::Mutex::new(asked))),
        ));
        let standing = |policy: WorkspacePolicy| {
            serde_json::to_string(&WorkspaceReading {
                id: id.clone(),
                owner: Some(owner.clone()),
                policy: Some(policy),
                ..WorkspaceReading::default()
            })
            .expect("a reading encodes")
        };
        let mut policy = WorkspacePolicy::new(id.clone(), owner.clone());
        view.apply(&resource(&id), Some(&standing(policy.clone())))
            .expect("the daemon's reading applies");
        let nodes = WorkspaceNodes::service(Some(&view)).expect("the session reads its workspaces");
        let node = TreePath::workspace(id.clone())
            .app("so.tilo.notes")
            .kind("notes.notebook")
            .resource("plan");

        let mut share = nodes
            .ask(
                &id,
                &node.to_string(),
                WorkspaceNodeAsk::Share {
                    share: "m1".into(),
                    access: "view".into(),
                },
            )
            .expect("a node of this workspace");
        assert!(
            matches!(share.poll_ready(), PendingPoll::Waiting),
            "nothing is shared before the daemon answered"
        );
        let answer = asks
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("the share is asked of the daemon");
        assert!(
            matches!(share.poll_ready(), PendingPoll::Waiting),
            "the asker waits on the daemon without waiting for it"
        );

        answer.send(()).expect("the share still waits");
        policy.grants.extend(
            share_grants(&id, &owner, "m1", &node, ShareAccess::View).expect("a share of a node"),
        );
        view.apply(&resource(&id), Some(&standing(policy)))
            .expect("the daemon's reading applies");
        let within =
            tilo_test_env::hang_deadline().saturating_duration_since(std::time::Instant::now());
        let shared = share
            .wait_timeout(within)
            .unwrap_or_else(|_| panic!("the share did not land"))
            .expect("the share was answered")
            .expect("the workspace's owner shares what they hold");
        assert_eq!(
            shared["grants"], 1,
            "the share answers what the daemon's standing says of it"
        );
    }
}
