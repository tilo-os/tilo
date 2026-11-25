//! Delegated agents as members of the workspaces this server hosts.
//!
//! An agent of a person this server serves is a device of that person, with
//! its own key, its own certificate and a root-signed delegation naming the
//! workspaces and apps it may act in, until when. To act in a workspace this
//! server hosts it needs what every member needs: a place on the workspace's
//! signed member list, which every member reads, which the server admits to the
//! workspace, and from which the workspace's key holders admit devices to each
//! record's channel.
//!
//! So when the agent's grant is approved (or restored after a restart), each
//! workspace this server hosts asks the one admission decision,
//! [`WorkspaceOperation::DelegateMembership`], with the agent's certificate: only a
//! live delegated agent whose delegation names its person, in a workspace the
//! delegation names and governance owns here. If admitted, the agent writes its
//! own row bound to its delegation ([`tilo_peer_state::PeerGovernanceMut::admit_agent`]); the
//! server then admits it to the workspace and a key holder to the channels it asks
//! for, like any member. Every member admits its changes as its own: signed
//! by its key, at its person's role but never above Editor, and only inside
//! its delegation.
//!
//! A grant that ends, is narrowed or is withdrawn is carried onto the same
//! member lists ([`Workspaces::reconcile_agent`]): the host withdraws it from
//! each workspace whose decision no longer admits it, which removes it from the
//! workspace and every channel of it, and members refuse whatever it writes from
//! then on. Its delegation's expiry needs no host at all: members check it on
//! every change.

use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::delegation::Delegation;
use tilo_user_identity::DeviceCert;

use super::{HostedWorkspace, Workspaces, system_time};
use crate::auth::AuthenticatedPeer;
use crate::workspace_access::{self, Decision, DenyReason, ServerTrust, WorkspaceOperation};

impl Workspaces {
    /// The server-wide facts the admission decision reads.
    fn trust(&self) -> ServerTrust<'_> {
        let host = &self.inner.host;
        ServerTrust {
            revocations: &host.revocations,
            policy: &host.trust_policy,
            blocked: &host.blocked,
            own_user: &host.cert.user_public_key,
            agents: &host.agents,
            environments: &host.environments,
        }
    }

    /// Whether [`WorkspaceOperation::DelegateMembership`] admits `peer` to `workspace`
    /// now.
    async fn agent_admitted(&self, hosted: &HostedWorkspace, peer: &AuthenticatedPeer) -> bool {
        let Some(workspace) = self
            .inner
            .host
            .workspaces
            .lock()
            .await
            .get(&hosted.workspace_id())
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
            tracing::debug!(workspace = %hosted.workspace(), device = %hex::encode(peer.device_public_key),
                reason = reason.reason(), "an agent is not admitted to the workspace");
        }
        decision == Decision::Admit
    }

    /// Read the current hosted agent entry for `agent`, kept for the person whose root
    /// `acts_for` names. Where a requester is supplied, that exact certified person
    /// device must still be admitted to the workspace.
    pub async fn hosted_roster_agent(
        &self,
        workspace: WorkspaceId,
        agent: &str,
        acts_for: &[u8; 32],
        requester: Option<&DeviceCert>,
    ) -> Option<tilo_workspace_agent::Agent> {
        let id = workspace.to_string();
        let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let agents = self
            .read_resource::<tilo_workspace_agent::WorkspaceAgents>(
                &id,
                (tilo_workspace_agent::OWNER, &list),
            )
            .await
            .ok()??;
        let entry = roster_entry(&agents, agent, acts_for)?;
        if let Some(cert) = requester {
            if !self.acts_for_device(workspace, acts_for, cert).await {
                return None;
            }
        } else {
            self.person_device(workspace, *acts_for).await?;
        }
        Some(entry.clone())
    }

    /// Check the requesting person's certificate against current governance.
    pub async fn acts_for_device(
        &self,
        workspace: WorkspaceId,
        acts_for: &[u8; 32],
        cert: &DeviceCert,
    ) -> bool {
        if &cert.user_public_key != acts_for {
            return false;
        }
        let Ok(hosted) = self.workspace(&workspace.to_string()).await else {
            return false;
        };
        let hosted = hosted.lock().await;
        let now = tilo_clock::unix_now_ns();
        let peer = AuthenticatedPeer {
            user_public_key: cert.user_public_key,
            device_public_key: cert.device_public_key,
            cert: cert.clone(),
        };
        let trust = self.trust();
        workspace_access::person_device(cert)
            && !trust.is_agent(&cert.device_public_key)
            && trust
                .environments
                .of(&cert.device_public_key, now)
                .is_none()
            && trust.cert(&peer, now).is_ok()
            && hosted
                .peer()
                .permissions(cert.device_public_key, system_time(now))
                .is_ok()
    }

    /// The certified device of an exact, currently approved agent on this
    /// workspace's member list. The host register and the member list must keep
    /// the same delegation. Two live assignments of the same agent id are
    /// ambiguous.
    pub async fn approved_agent_device(
        &self,
        workspace: WorkspaceId,
        agent: &str,
        acts_for: &[u8; 32],
    ) -> Option<[u8; 32]> {
        let reading = self.reading(workspace).await?;
        let peer = reading.replica.lock();
        self.approved_agent(&peer, workspace, agent, acts_for)
            .map(|(cert, _)| cert.device_public_key)
    }

    pub(super) fn approved_agent(
        &self,
        peer: &super::StatePeer,
        workspace: WorkspaceId,
        agent: &str,
        acts_for: &[u8; 32],
    ) -> Option<(DeviceCert, Delegation)> {
        let now_ns = tilo_clock::unix_now_ns();
        let trust = self.trust();
        let mut matching = peer
            .governance()
            .delegated_agents(system_time(now_ns))
            .ok()?
            .into_iter()
            .filter(|entry| {
                entry.acting && entry.agent_id == agent && &entry.user_public_key == acts_for
            })
            .filter_map(|entry| {
                let device = entry.device_public_key;
                let (cert, delegation, None) = peer.governance().listed_delegate(&device)? else {
                    return None;
                };
                let peer = AuthenticatedPeer {
                    user_public_key: *acts_for,
                    device_public_key: device,
                    cert,
                };
                (trust.cert(&peer, now_ns).is_ok()
                    && trust.delegation(&device, workspace, now_ns).as_ref() == Some(&delegation))
                .then_some((peer.cert, delegation))
            });
        matching.next().filter(|_| matching.next().is_none())
    }

    /// Bring agent `device`'s place in every workspace this server hosts in line
    /// with its delegation now: after its grant was approved, restored,
    /// renewed, narrowed, withdrawn or ended. `agent` is its certificate and
    /// what signs as it, when the agent is here to sign: it writes its own row,
    /// bound to its newest delegation, in each workspace the decision admits it
    /// to, and no other device writes that row in its place. Without it only
    /// the workspaces whose member list already has it are looked at. A workspace
    /// whose member list has it and whose decision no longer admits it withdraws
    /// it, which takes it out of the workspace and every channel of it.
    pub async fn reconcile_agent(
        &self,
        device: [u8; 32],
        agent: Option<(&DeviceCert, &dyn collab_types::signer::DeviceSigner)>,
    ) {
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
            if let Err(error) = self.reconcile_agent_in(&mut workspace, device, agent).await {
                tracing::warn!(workspace = %workspace.workspace(), device = %hex::encode(device), %error,
                    "an agent's place in the workspace was not brought up to date");
            }
        }
    }

    async fn reconcile_agent_in(
        &self,
        workspace: &mut HostedWorkspace,
        device: [u8; 32],
        agent: Option<(&DeviceCert, &dyn collab_types::signer::DeviceSigner)>,
    ) -> Result<(), String> {
        let recorded = workspace.peer().governance().listed_delegate(&device);
        let offered = agent.map(|(cert, _)| cert);
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
            // The agent writes its own row: without it here to sign, its
            // place waits for it.
            (Some(delegation), recorded) => match agent {
                Some((_, signer)) if !recorded.is_some_and(|(_, known, _)| known == delegation) => {
                    candidate
                        .governance_mut()
                        .admit_agent(peer.cert, delegation, signer, now)?
                }
                _ => false,
            },
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
        workspace.keep(candidate, &self.inner.host.key, now, |_, _| Ok(state))?;
        self.settle(workspace).await?;
        tracing::info!(workspace = %workspace.workspace(), device = %hex::encode(device),
            "an agent's place on the workspace's roster changed with its grant");
        Ok(())
    }
}

fn roster_entry<'a>(
    agents: &'a tilo_workspace_agent::WorkspaceAgents,
    agent: &str,
    acts_for: &[u8; 32],
) -> Option<&'a tilo_workspace_agent::Agent> {
    let entry = agents.get(&tilo_resource::Id::from_raw(agent.to_owned()))?;
    if !entry.is_active()
        || tilo_action_policy::PersonId::parse(&entry.acts_for)
            .ok()?
            .root()
            .as_ref()
            != Some(acts_for)
    {
        return None;
    }
    Some(entry)
}
