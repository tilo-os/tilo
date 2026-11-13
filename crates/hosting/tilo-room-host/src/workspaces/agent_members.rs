//! Delegated agents as governed members of the workspaces this relay governs.
//!
//! An agent of this relay's user is a device of that person, with its own
//! key, its own certificate and a root-signed delegation naming the workspaces
//! and apps it may act in, until when. To act in a governed workspace it needs
//! what every collaborator needs: a place on the workspace's governed roster,
//! which every member reads, which the relay admits to the workspace, and from
//! which the workspace's participants admit devices to each record's channel.
//!
//! So when the agent's grant is approved (or restored after a restart), each
//! workspace this relay governs asks the one admission decision,
//! [`WorkspaceOperation::DelegateMembership`], with the agent's certificate: only a
//! live delegated agent of this relay's own user, in a workspace the delegation
//! names and governance owns here. If admitted, the agent goes on the roster
//! bound to its delegation ([`tilo_peer_state::PeerGovernanceMut::admit_agent`]); the
//! relay then admits it to the workspace and a participant to the channels it asks
//! for, like any member. Every member admits its changes as its own: signed
//! by its key, at its person's role but never above Editor, and only inside
//! its delegation.
//!
//! A grant that ends, is narrowed or is withdrawn is carried onto the same
//! rosters ([`Workspaces::reconcile_agent`]): the governor withdraws it from each
//! workspace whose decision no longer admits it, which removes it from the workspace
//! and every channel of it, and members refuse whatever it writes from then
//! on. Its delegation's expiry needs no governor at all: members check it on
//! every change.

use tilo_user_identity::DeviceCert;

use super::{GovernedWorkspace, Workspaces, system_time};
use crate::auth::AuthenticatedPeer;
use crate::workspace_access::{self, Decision, DenyReason, RelayTrust, WorkspaceOperation};

impl Workspaces {
    /// The relay-wide facts the admission decision reads.
    fn trust(&self) -> RelayTrust<'_> {
        let host = &self.inner.host;
        RelayTrust {
            revocations: &host.revocations,
            policy: &host.trust_policy,
            own_user: &host.cert.user_public_key,
            agents: &host.agents,
            // No plane here asks about a browser environment's device: one
            // asks to join as a delegated agent, which an environment is not.
            environments: crate::environment::none(),
        }
    }

    /// Whether [`WorkspaceOperation::DelegateMembership`] admits `peer` to `workspace`
    /// now.
    async fn agent_admitted(&self, governed: &GovernedWorkspace, peer: &AuthenticatedPeer) -> bool {
        let Some(workspace) = self
            .inner
            .host
            .workspaces
            .lock()
            .await
            .get(&governed.workspace_id())
            .cloned()
        else {
            return false;
        };
        let decision = workspace_access::decide(
            &*workspace.lock().await,
            &self.trust(),
            peer,
            WorkspaceOperation::DelegateMembership,
        );
        if let Decision::Deny(reason) = decision
            && reason != DenyReason::NotADelegatedAgent
        {
            tracing::debug!(workspace = %governed.workspace(), device = %hex::encode(peer.device_public_key),
                reason = reason.reason(), "an agent is not admitted to the room");
        }
        decision == Decision::Admit
    }

    /// Bring agent `device`'s place in every workspace this relay governs in line
    /// with its delegation now: after its grant was approved, restored,
    /// renewed, narrowed, withdrawn or ended. `cert` is its certificate when
    /// known; without it only the workspaces whose roster already lists it are
    /// looked at. A workspace the decision admits it to records its newest
    /// delegation; a workspace whose roster lists it and whose decision no longer
    /// admits it withdraws it, which takes it out of the workspace and every
    /// channel of it.
    pub async fn reconcile_agent(&self, device: [u8; 32], cert: Option<&DeviceCert>) {
        let workspaces = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for workspace in workspaces {
            let mut workspace = workspace.lock().await;
            if let Err(error) = self.reconcile_agent_in(&mut workspace, device, cert).await {
                tracing::warn!(workspace = %workspace.workspace(), device = %hex::encode(device), %error,
                    "an agent's place in the room was not brought up to date");
            }
        }
    }

    async fn reconcile_agent_in(
        &self,
        workspace: &mut GovernedWorkspace,
        device: [u8; 32],
        offered: Option<&DeviceCert>,
    ) -> Result<(), String> {
        let recorded = workspace.peer().governance().governed_agent(&device);
        let cert = match (&recorded, offered) {
            // Withdrawal is final.
            (Some((_, _, Some(_))), _) | (None, None) => return Ok(()),
            (Some((cert, _, None)), _) => cert.clone(),
            (None, Some(cert)) if cert.device_public_key == device => cert.clone(),
            (None, Some(_)) => return Err("the certificate names another device".into()),
        };
        let peer = AuthenticatedPeer {
            user_public_key: cert.user_public_key,
            device_public_key: cert.device_public_key,
            cert,
        };
        let now_ns = tilo_clock::unix_now_ns();
        let now = system_time(now_ns);
        let live = if self.agent_admitted(workspace, &peer).await {
            self.trust()
                .delegation(&device, workspace.workspace_id(), now_ns)
        } else {
            None
        };
        let mut candidate = workspace.peer().clone();
        let changed = match (live, recorded) {
            (Some(delegation), recorded) => {
                let known = recorded.is_some_and(|(_, known, _)| known == delegation);
                !known
                    && candidate.governance_mut().admit_agent(
                        peer.cert,
                        delegation,
                        &self.inner.host.key,
                        now,
                    )?
            }
            (None, Some(_)) => {
                candidate
                    .governance_mut()
                    .withdraw_agent(device, &self.inner.host.key, now)?
            }
            (None, None) => false,
        };
        if !changed {
            return Ok(());
        }
        let state = workspace.collaboration().clone();
        workspace.commit(candidate, &self.inner.host.key, now, |_, _| Ok(state))?;
        self.settle(workspace).await?;
        tracing::info!(workspace = %workspace.workspace(), device = %hex::encode(device),
            "an agent's place on the room's roster changed with its grant");
        Ok(())
    }
}
