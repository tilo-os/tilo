//! Approve a roster entry's signing key, and admit it, through device certification.

use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_agent_session::grants::RosterAdmissionDone;
use tilo_agent_session::{AgentIdentity, ApprovalDone};
use tilo_server_protocol::delegation::DelegateProposal;
use tilo_user_identity::DeviceCert;

use super::ServerAgentLink;

impl ServerAgentLink {
    pub(super) fn approve_roster(
        &self,
        key: &SigningKey,
        proposal: &DelegateProposal,
        created_at: i64,
        requester: &DeviceCert,
        done: ApprovalDone,
    ) -> Result<String, String> {
        let [workspace] = proposal.scope.workspaces.as_slice() else {
            return Err("a roster grant names exactly one workspace".into());
        };
        let workspace = *workspace;
        let workspaces = self
            .workspaces
            .clone()
            .ok_or("this server does not govern the workspace")?;
        let agent = proposal
            .delegate
            .agent_id()
            .ok_or("the proposal names no agent")?
            .to_owned();
        let label = proposal.label.clone();
        let keeper = requester.user_public_key;
        let (digest, code) = self.enqueue_approval(keeper, key, proposal)?;
        let certification = self.certification.clone();
        let requester = requester.clone();
        self.runtime.spawn(async move {
            let mut outcome = certification.delegate_answer(&digest).await;
            if outcome.is_ok()
                && !workspaces
                    .hosted_roster_agent(workspace, &agent, &keeper, Some(&requester))
                    .await
                    .is_some_and(|entry| {
                        entry.created_at == created_at && entry.name.trim() == label
                    })
            {
                outcome = Err("the roster entry or keeper changed before approval".into());
            }
            done(outcome);
        });
        Ok(code)
    }

    pub(super) fn join_roster(
        &self,
        identity: &AgentIdentity,
        created_at: i64,
        lifetime: tokio::sync::oneshot::Receiver<()>,
        done: RosterAdmissionDone,
    ) {
        let link = self.clone();
        let identity = identity.clone();
        self.runtime.spawn(async move {
            let outcome = link.admit_signer(&identity, created_at, lifetime).await;
            done(outcome);
        });
    }

    async fn admit_signer(
        &self,
        identity: &AgentIdentity,
        created_at: i64,
        mut lifetime: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<(), String> {
        let workspaces = self
            .workspaces
            .as_ref()
            .ok_or("this server does not govern the workspace")?;
        let delegation = &identity.delegation;
        let &[workspace] = delegation.scope.workspaces.as_slice() else {
            return Err("a roster grant names exactly one workspace".into());
        };
        let agent = delegation
            .delegate
            .agent_id()
            .ok_or("the approval names no agent")?;
        let device = identity.cert.device_public_key;
        let keeper = identity.cert.user_public_key;
        let roster_document = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let mut documents = workspaces.document_writes();
        let mut governance = workspaces.governance_changes();
        let current = || async {
            workspaces
                .hosted_roster_agent(workspace, agent, &keeper, None)
                .await
                .is_some_and(|entry| entry.created_at == created_at)
        };
        if !current().await {
            return Err("the approved roster entry is no longer active".into());
        }
        let now = tilo_clock::unix_now_ns();
        tilo_user_identity::verify_device_cert_at(&identity.cert, now)
            .map_err(|error| error.to_string())?;
        if self.certification.revocations().refuses(&identity.cert) {
            return Err("the roster agent's certificate is no longer admitted".into());
        }
        crate::daemon::register_agent_in(&self.agents, &identity.cert, delegation)?;
        let admitted = workspaces
            .admit_roster_agent(workspace, agent, &identity.cert)
            .await;
        let active = workspaces
            .approved_agent_device(workspace, agent, &keeper)
            .await
            == Some(device);
        if admitted.is_err() || !active || !current().await {
            self.withdraw_device(device);
            return admitted
                .and_then(|()| Err("the roster signing device was not admitted".into()));
        }

        let link = self.clone();
        let agent = agent.to_owned();
        let workspaces = workspaces.clone();
        let valid_until = identity
            .cert
            .expires_at_ns
            .unwrap_or(u64::MAX)
            .min(delegation.expires_at_ns);
        self.runtime.spawn(async move {
            let expires = tokio::time::sleep(Duration::from_nanos(
                valid_until.saturating_sub(tilo_clock::unix_now_ns()),
            ));
            tokio::pin!(expires);
            let workspace_name = workspace.to_string();
            loop {
                let recheck = tokio::select! {
                    () = &mut expires => break,
                    _ = &mut lifetime => return,
                    change = documents.recv() => match change {
                        Ok(change) => change.workspace == workspace_name
                            && change.app == tilo_workspace_agent::OWNER
                            && change.record == roster_document,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                    change = governance.recv() => match change {
                        Ok(change) => change == workspace_name,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                };
                if recheck
                    && (workspaces
                        .approved_agent_device(workspace, &agent, &keeper)
                        .await
                        != Some(device)
                        || workspaces
                            .hosted_roster_agent(workspace, &agent, &keeper, None)
                            .await
                            .is_none_or(|entry| entry.created_at != created_at))
                {
                    break;
                }
            }
            link.withdraw_device(device);
        });
        Ok(())
    }
}
