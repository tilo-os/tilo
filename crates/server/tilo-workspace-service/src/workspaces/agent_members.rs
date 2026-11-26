//! Delegated agents as governed members of the workspaces this server governs.
//!
//! An agent of a person this server serves is a device of that person, with
//! its own key, its own certificate and a root-signed delegation naming the
//! workspaces and apps it may act in, until when. To act in a governed
//! workspace it needs what every member needs: a place on the workspace's
//! governed roster, which every member reads, which the server admits to the
//! workspace, and from which the workspace's key holders admit devices to each
//! record's channel.
//!
//! So when the agent's grant is approved (or restored after a restart), each
//! workspace this server governs asks the one admission decision,
//! [`WorkspaceOperation::DelegateMembership`], with the agent's certificate: only a
//! live delegated agent whose delegation names its person, in a workspace the
//! delegation names and governance owns here. If admitted, the agent goes on the
//! roster bound to its delegation ([`tilo_peer_state::PeerGovernanceMut::admit_agent`]); the
//! server then admits it to the workspace and a key holder to the channels it asks
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

use tilo_actions::remote::Caller;
use tilo_agent_session::workspace_actions::ViewReadPolicy;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::delegation::Delegation;
use tilo_user_identity::DeviceCert;
use tilo_workspace_agent::{Agent, WorkspaceAgents};

use super::{GovernedWorkspace, Workspaces, system_time};
use crate::auth::AuthenticatedPeer;
use crate::workspace_access::{self, Decision, DenyReason, ServerTrust, WorkspaceOperation};

impl Workspaces {
    /// The server-wide facts the admission decision reads.
    fn trust(&self) -> ServerTrust<'_> {
        let host = &self.inner.host;
        ServerTrust {
            revocations: &host.revocations,
            policy: &host.trust_policy,
            own_user: &host.cert.user_public_key,
            agents: &host.agents,
            environments: &host.environments,
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
                reason = reason.reason(), "an agent is not admitted to the workspace");
        }
        decision == Decision::Admit
    }

    /// The workspace's current agents, folded from their one native resource.
    pub(crate) async fn kept_agents(&self, workspace: &str) -> Option<WorkspaceAgents> {
        let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        self.read_resource::<WorkspaceAgents>(workspace, (tilo_workspace_agent::OWNER, &list))
            .await
            .ok()?
    }

    /// Read the current hosted agent entry for `keeper`, the person who
    /// approved it. Where a requester is supplied, that exact certified person
    /// device must still be admitted to the workspace.
    pub async fn hosted_roster_agent(
        &self,
        workspace: WorkspaceId,
        agent: &str,
        keeper: &[u8; 32],
        requester: Option<&DeviceCert>,
    ) -> Option<tilo_workspace_agent::Agent> {
        let agents = self.kept_agents(&workspace.to_string()).await?;
        let entry = roster_entry(&agents, agent, keeper)?;
        if let Some(cert) = requester {
            if !self.roster_keeper_device(workspace, keeper, cert).await {
                return None;
            }
        } else {
            self.person_device(workspace, *keeper).await?;
        }
        Some(entry.clone())
    }

    /// Check the requesting keeper's certificate against current governance.
    pub async fn roster_keeper_device(
        &self,
        workspace: WorkspaceId,
        keeper: &[u8; 32],
        cert: &DeviceCert,
    ) -> bool {
        if &cert.user_public_key != keeper {
            return false;
        }
        let Ok(governed) = self.workspace(&workspace.to_string()).await else {
            return false;
        };
        let governed = governed.lock().await;
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
            && governed
                .peer()
                .permissions(cert.device_public_key, system_time(now))
                .is_ok()
    }

    /// Admit this signing device to its one workspace, refusing a different
    /// certified device that still holds the same roster assignment.
    pub async fn admit_roster_agent(
        &self,
        workspace: WorkspaceId,
        agent: &str,
        cert: &DeviceCert,
    ) -> Result<(), String> {
        let governed = self
            .workspace(&workspace.to_string())
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = governed.lock().await;
        let device = cert.device_public_key;
        if governed
            .peer()
            .governance()
            .governed_agents(tilo_clock::wall())?
            .iter()
            .any(|entry| {
                entry.acting && entry.agent_id == agent && entry.device_public_key != device
            })
        {
            return Err("another device already holds this roster assignment".into());
        }
        self.reconcile_agent_in(&mut governed, device, Some(cert))
            .await
    }

    /// The certified device of an exact, currently approved agent on this workspace's
    /// governed roster. The host register and the roster must hold the same delegation.
    /// Two live assignments of the same roster id are ambiguous.
    pub async fn approved_agent_device(
        &self,
        workspace: WorkspaceId,
        agent: &str,
        keeper: &[u8; 32],
    ) -> Option<[u8; 32]> {
        let reading = self.reading(workspace).await?;
        let peer = reading.replica.lock();
        self.approved_agent(&peer, workspace, agent, keeper)
            .map(|(cert, _)| cert.device_public_key)
    }

    pub(super) fn approved_agent(
        &self,
        peer: &super::StatePeer,
        workspace: WorkspaceId,
        agent: &str,
        keeper: &[u8; 32],
    ) -> Option<(DeviceCert, Delegation)> {
        let now_ns = tilo_clock::unix_now_ns();
        let trust = self.trust();
        let mut matching = peer
            .governance()
            .governed_agents(system_time(now_ns))
            .ok()?
            .into_iter()
            .filter(|entry| {
                entry.acting && entry.agent_id == agent && &entry.user_public_key == keeper
            })
            .filter_map(|entry| {
                let device = entry.device_public_key;
                let (cert, delegation, None) = peer.governance().governed_agent(&device)? else {
                    return None;
                };
                let peer = AuthenticatedPeer {
                    user_public_key: *keeper,
                    device_public_key: device,
                    cert,
                };
                (trust.cert(&peer, now_ns).is_ok()
                    && trust.delegation(&device, workspace, now_ns).as_ref() == Some(&delegation))
                .then_some((peer.cert, delegation))
            });
        matching.next().filter(|_| matching.next().is_none())
    }

    /// Current authority of the exact roster device serving this turn. The
    /// live session supplies its certificate and root-signed delegation.
    pub async fn roster_action_policy(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        session: (&DeviceCert, &Delegation),
        inside: Option<(&str, &str)>,
    ) -> Option<ViewReadPolicy> {
        let agents = self.kept_agents(&workspace.to_string()).await?;
        // Read without the workspace's lock, which is held across a commit's
        // writes: every call an agent makes reads this before it saves.
        let reading = self.reading(workspace).await?;
        if reading.member {
            return None;
        }
        self.roster_policy_for(
            (&reading.replica.lock(), workspace, &reading.workspace),
            &agents,
            caller,
            session,
            inside,
        )
    }

    pub(super) async fn roster_policy_in(
        &self,
        governed: &GovernedWorkspace,
        caller: &Caller,
        session: (&DeviceCert, &Delegation),
        inside: Option<(&str, &str)>,
    ) -> Option<ViewReadPolicy> {
        use tilo_workspace_agent::{KIND, OWNER};

        if governed.member.is_some() {
            return None;
        }
        let list = collab_types::workspace_document_id(KIND);
        let bytes = governed.readable_document(OWNER, &list).await.ok()??;
        let agents = super::actions::agents::read_resource_bytes::<WorkspaceAgents>(&bytes).ok()?;
        self.roster_policy_for(
            (
                &governed.peer(),
                governed.workspace_id(),
                governed.workspace(),
            ),
            &agents,
            caller,
            session,
            inside,
        )
    }

    fn roster_policy_for(
        &self,
        (peer, workspace, name): (&super::StatePeer, WorkspaceId, &str),
        agents: &WorkspaceAgents,
        caller: &Caller,
        session: (&DeviceCert, &Delegation),
        inside: Option<(&str, &str)>,
    ) -> Option<ViewReadPolicy> {
        let agent = caller.approved_agent()?;
        let keeper = caller.user_public_key()?;
        let actor = caller.acting().agent_id()?;
        let entry = roster_entry(agents, agent, keeper)?;
        if caller.workspace() != Some(name) || caller.roster_created_at() != Some(entry.created_at)
        {
            return None;
        }
        let now_ns = tilo_clock::unix_now_ns();
        super::hosted_use::person_device_in(peer, keeper, |device| {
            self.inner.host.environments.of(device, now_ns).is_some()
        })?;
        let (cert, delegation) = self.approved_agent(peer, workspace, agent, keeper)?;
        if caller.device_public_key() != Some(&cert.device_public_key)
            || session != (&cert, &delegation)
        {
            return None;
        }
        let now = tilo_clock::wall();
        if let Some((app, document)) = inside {
            if !delegation.permits_app(app) {
                return None;
            }
            peer.check_document_read(cert.device_public_key, app, document, now)
                .ok()?;
        }
        let person = caller.acting_for_person()?;
        let mut grants =
            tilo_action_policy::roles::agent_bound(agent, person, &entry.turn_rules(name));
        if actor != agent {
            let child = child_for_signer(
                agents,
                actor,
                keeper,
                agent,
                caller.roster_created_at(),
                caller.child_created_at(),
            )?;
            grants.extend(tilo_action_policy::roles::agent_bound(
                actor,
                person,
                &child.turn_rules(name),
            ));
        }
        let access = peer
            .permissions(cert.device_public_key, now)
            .ok()?
            .granted(grants);
        let sandbox = peer.governance().governed_sandbox().ok()?;
        Some(ViewReadPolicy {
            workspace: name.to_owned(),
            access,
            sandbox,
            unix_time: now
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .ok()?
                .as_secs(),
            agent_bound: Vec::new(),
        })
    }

    /// Bring agent `device`'s place in every workspace this server governs in line
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
                    "an agent's place in the workspace was not brought up to date");
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
            "an agent's place on the workspace's roster changed with its grant");
        Ok(())
    }
}

/// The active child and the active signing lead of its keeper.
pub(super) fn child_and_lead<'a>(
    agents: &'a WorkspaceAgents,
    actor: &str,
    keeper: &str,
) -> Option<(&'a Agent, &'a Agent)> {
    let child = agents.get(&tilo_resource::Id::from_raw(actor.to_owned()))?;
    if !child.is_active() || !child.is_sub() || !child.keeper.eq_ignore_ascii_case(keeper) {
        return None;
    }
    let lead = agents.root_of(child);
    (lead.is_active() && !lead.is_sub() && lead.keeper.eq_ignore_ascii_case(keeper))
        .then_some((child, lead))
}

/// Check both entries against the original turn, including when the workspace is already locked.
pub(super) fn child_for_signer<'a>(
    agents: &'a WorkspaceAgents,
    actor: &str,
    keeper: &[u8; 32],
    signer: &str,
    lead_created_at: Option<i64>,
    child_created_at: Option<i64>,
) -> Option<&'a Agent> {
    let (child, lead) = child_and_lead(agents, actor, &hex::encode(keeper))?;
    (Some(child.created_at) == child_created_at
        && lead.id.0 == signer
        && Some(lead.created_at) == lead_created_at)
        .then_some(child)
}

fn roster_entry<'a>(
    agents: &'a tilo_workspace_agent::WorkspaceAgents,
    agent: &str,
    keeper: &[u8; 32],
) -> Option<&'a tilo_workspace_agent::Agent> {
    let entry = agents.get(&tilo_resource::Id::from_raw(agent.to_owned()))?;
    if !entry.is_active()
        || tilo_action_policy::PersonId::parse(&entry.keeper)
            .ok()?
            .root()
            .as_ref()
            != Some(keeper)
    {
        return None;
    }
    Some(entry)
}
