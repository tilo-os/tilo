//! The grant a person gives with an approve of an asked record ("approve and grant"): an
//! ordinary grant of the workspace's policy, a change of its member list that the device they
//! answered on signs, shown and taken back like every other grant.
//!
//! The workspace's policy decides it as every grant its person gives: a grant never reaches
//! beyond the one who gives it.

use std::sync::Arc;

use tilo_action_policy::{Grant, Grantee};
use tilo_actions::remote::asked::{AskedGrants, Giving};
use tilo_actions::remote::{ActionCallError, Pending, pending};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

use super::weak::WeakWorkspaces;
use super::{SignedAsk, Workspaces};

impl Workspaces {
    /// Write `grant` in `workspace`, as `giving` gives it: the change the device its person
    /// answered on signs, refused unless the grant names that person as its giver.
    ///
    /// # Errors
    /// Why it was not written, or [`ActionCallError::SignatureRequired`] until that device
    /// signed it.
    pub async fn give_asked_grant(
        &self,
        workspace: &str,
        giving: &Giving,
        grant: Grant,
    ) -> Result<(), ActionCallError> {
        if grant.granted_by != Grantee::person(giving.person.clone()) {
            return Err(ActionCallError::refused(
                "a grant is written only by the person who gives it",
            ));
        }
        let device = match collab_types::WorkspaceId::parse(workspace) {
            Some(workspace_id) => {
                self.callers_certificate(Some(workspace_id), &giving.answering)
                    .await
            }
            None => None,
        }
        .ok_or_else(|| ActionCallError::refused("the answering device is not in the workspace"))?;
        // Nothing signed yet asks the device to sign it.
        let signed = (!giving.signed.is_empty()).then(|| {
            (
                tilo_actions::remote::SIGNED_ARGUMENT.to_owned(),
                giving.signed.clone().into(),
            )
        });
        let signed = super::actions::signed_in(&signed.into_iter().collect())?;
        let ask = SignedAsk {
            device: &device,
            request_id: rand::random(),
            signed,
        };
        Self::signed_change(
            self.govern(workspace, ask, &WorkspaceMembershipCommand::Grant { grant })
                .await,
        )
        .map(drop)
    }

    /// What writes the grants this host's people give with an approve, on `runtime`.
    #[must_use]
    pub fn asked_grants(&self, runtime: tokio::runtime::Handle) -> Arc<dyn AskedGrants> {
        Arc::new(HostGrants {
            workspaces: self.downgrade(),
            runtime,
        })
    }
}

struct HostGrants {
    /// Held without keeping them alive: the store these grants answer through lives in the
    /// workspaces themselves, and a strong handle here would keep the host's workspaces, and
    /// the endpoint they hold, from ever being let go.
    workspaces: WeakWorkspaces,
    runtime: tokio::runtime::Handle,
}

impl AskedGrants for HostGrants {
    fn grant(
        &self,
        workspace: &str,
        giving: &Giving,
        grant: Grant,
    ) -> Pending<Result<(), ActionCallError>> {
        let (done, answer) = pending();
        let (workspaces, workspace, giving) = (
            self.workspaces.clone(),
            workspace.to_owned(),
            giving.clone(),
        );
        self.runtime.spawn(async move {
            let Some(workspaces) = workspaces.upgrade() else {
                done.complete(Err(ActionCallError::unavailable(
                    "this server no longer keeps its workspaces",
                )));
                return;
            };
            done.complete(
                workspaces
                    .give_asked_grant(&workspace, &giving, grant)
                    .await,
            );
        });
        answer
    }
}
