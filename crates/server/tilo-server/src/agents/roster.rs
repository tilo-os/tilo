//! Provision a hosted roster entry's signing peer through device certification.

use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, Pending, RemoteActionMessage, pending,
    refuse,
};
use tilo_agent_session::grants::RosterAdmissionDone;
use tilo_agent_session::{AgentIdentity, AgentSessions, AgentSupervisor, ApprovalDone};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::agents::{
    AGENT_RESOURCE_PREFIX, AGENTS_PROVIDER, GrantRequest, REQUEST_GRANT_ACTION, REVOKE_ACTION,
    RosterGrantRequest, SUPERVISOR_RESOURCE,
};
use tilo_server_protocol::delegation::DelegateProposal;
use tilo_user_identity::DeviceCert;

use super::ServerAgentLink;

pub(super) struct RosterProvisioning {
    link: OnceLock<Weak<ServerAgentLink>>,
    sessions: AgentSessions,
}

impl RosterProvisioning {
    pub(super) fn new(sessions: AgentSessions) -> Self {
        Self {
            link: OnceLock::new(),
            sessions,
        }
    }

    pub(super) fn start(&self, link: &Arc<ServerAgentLink>) {
        let _ = self.link.set(Arc::downgrade(link));
    }

    pub(super) fn is_signer(&self, caller: &Caller) -> bool {
        caller
            .device_public_key()
            .is_some_and(|device| self.sessions.is_roster_signer(device))
    }

    pub(super) fn serve(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        cert: &DeviceCert,
        message: &RemoteActionMessage,
        supervisor: &Arc<AgentSupervisor>,
    ) -> Option<Pending<RemoteActionMessage>> {
        if self.is_signer(caller) {
            return refuse(
                message,
                ActionCallError::Refused {
                    reason: "a roster signing session does not execute workspace actions".into(),
                },
            )
            .map(Pending::ready);
        }
        let RemoteActionMessage::Invoke(request) = message else {
            return None;
        };
        if request.target.provider != AGENTS_PROVIDER {
            return None;
        }
        let address = &request.target.address;
        let revoke = if address.action == REVOKE_ACTION {
            Some(
                address
                    .resource
                    .strip_prefix(AGENT_RESOURCE_PREFIX)
                    .filter(|id| supervisor.has_roster_grant(workspace, id))?
                    .to_owned(),
            )
        } else if address.resource == SUPERVISOR_RESOURCE
            && address.action == REQUEST_GRANT_ACTION
            && matches!(&request.argument, InvokeArgument::Json(arguments) if arguments.contains_key("agentId"))
        {
            None
        } else {
            return None;
        };
        let Some(link) = self.link.get().and_then(Weak::upgrade) else {
            return refuse(
                message,
                ActionCallError::Unavailable {
                    reason: "the roster supervisor is starting".into(),
                },
            )
            .map(Pending::ready);
        };
        let (done, answer) = pending();
        let (caller, cert, request, supervisor) = (
            caller.clone(),
            cert.clone(),
            request.clone(),
            supervisor.clone(),
        );
        link.runtime.clone().spawn(async move {
            let result = match revoke {
                Some(id) => {
                    revoke_grant(&link, workspace, &caller, &cert, &request, &supervisor, &id).await
                }
                None => provision(&link, workspace, &caller, &cert, &request, &supervisor).await,
            };
            let receipt = result.unwrap_or_else(|error| tilo_actions::remote::InvocationReceipt {
                invocation: request.invocation,
                status: tilo_actions::remote::ReceiptStatus::Rejected { error },
            });
            done.complete(RemoteActionMessage::Receipt(receipt));
        });
        Some(answer)
    }
}

fn acts_for_required() -> ActionCallError {
    ActionCallError::Refused {
        reason: "only the active person of this hosted roster agent can manage its grant here"
            .into(),
    }
}

fn check_the_persons_session(caller: &Caller, cert: &DeviceCert) -> Result<(), ActionCallError> {
    if !caller.acting().is_person()
        || caller.user_public_key() != Some(&cert.user_public_key)
        || caller.device_public_key() != Some(&cert.device_public_key)
    {
        return Err(acts_for_required());
    }
    Ok(())
}

async fn revoke_grant(
    link: &ServerAgentLink,
    workspace: WorkspaceId,
    caller: &Caller,
    cert: &DeviceCert,
    request: &InvokeRequest,
    supervisor: &Arc<AgentSupervisor>,
    id: &str,
) -> Result<tilo_actions::remote::InvocationReceipt, ActionCallError> {
    let workspaces = link.workspaces.as_ref().ok_or_else(acts_for_required)?;
    check_the_persons_session(caller, cert)?;
    if !workspaces
        .acts_for_device(workspace, &cert.user_public_key, cert)
        .await
    {
        return Err(acts_for_required());
    }
    supervisor.invoke_roster_revoke(caller, request, workspace, id)
}

async fn provision(
    link: &ServerAgentLink,
    workspace: WorkspaceId,
    caller: &Caller,
    cert: &DeviceCert,
    request: &InvokeRequest,
    supervisor: &Arc<AgentSupervisor>,
) -> Result<tilo_actions::remote::InvocationReceipt, ActionCallError> {
    let InvokeArgument::Json(arguments) = &request.argument else {
        return Err(acts_for_required());
    };
    let asked: RosterGrantRequest =
        serde_json::from_value(serde_json::Value::Object(arguments.clone())).map_err(|error| {
            ActionCallError::InvalidRequest {
                reason: error.to_string(),
            }
        })?;
    check_the_persons_session(caller, cert)?;
    if asked.workspaces.as_slice() != [workspace.to_string()]
        || asked.agent_id.is_empty()
        || asked.agent_id.len() > tilo_server_protocol::delegation::MAX_DELEGATE_TEXT_BYTES
    {
        return Err(acts_for_required());
    }
    let workspaces = link.workspaces.as_ref().ok_or_else(acts_for_required)?;
    let acts_for = cert.user_public_key;
    let entry = workspaces
        .hosted_roster_agent(workspace, &asked.agent_id, &acts_for, Some(cert))
        .await
        .ok_or_else(acts_for_required)?;
    let grant = GrantRequest {
        label: entry.name.trim().to_owned(),
        color: None,
        workspaces: asked.workspaces,
        apps: asked.apps,
        expires_in_secs: asked.expires_in_secs,
    };
    supervisor.invoke_roster_grant(
        caller,
        request,
        &grant,
        &asked.agent_id,
        entry.created_at,
        cert,
    )
}

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
        let acts_for = requester.user_public_key;
        let (digest, code) = self.enqueue_approval(acts_for, key, proposal)?;
        let certification = self.certification.clone();
        let requester = requester.clone();
        self.runtime.spawn(async move {
            let mut outcome = certification.delegate_answer(&digest).await;
            if outcome.is_ok()
                && !workspaces
                    .hosted_roster_agent(workspace, &agent, &acts_for, Some(&requester))
                    .await
                    .is_some_and(|entry| {
                        entry.created_at == created_at && entry.name.trim() == label
                    })
            {
                outcome = Err("the roster entry or person changed before approval".into());
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
        let acts_for = identity.cert.user_public_key;
        let roster_document = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let mut documents = workspaces.document_writes();
        let mut governance = workspaces.governance_changes();
        let current = || async {
            workspaces
                .hosted_roster_agent(workspace, agent, &acts_for, None)
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
            .approved_agent_device(workspace, agent, &acts_for)
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
                        .approved_agent_device(workspace, &agent, &acts_for)
                        .await
                        != Some(device)
                        || workspaces
                            .hosted_roster_agent(workspace, &agent, &acts_for, None)
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
