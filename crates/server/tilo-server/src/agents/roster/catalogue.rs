//! The hosted assistant's grant controls in its keeper's workspace catalogue.

use std::sync::{Arc, Weak};

use serde_json::json;
use tilo_actions::published::{Offer, PublishedAction};
use tilo_actions::remote::{
    ActionCallError, Caller, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueRow,
    Pending, RemoteActionMessage, pending,
};
use tilo_actions::{ActionKind, ExecutionLocus, OperationKind, ResultSpec, RetryPolicy};
use tilo_agent_session::AgentSupervisor;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::agents::{AGENTS_PROVIDER, AgentGrantAction, GrantState};
use tilo_user_identity::DeviceCert;

use super::{RosterProvisioning, personal_entry};

impl RosterProvisioning {
    /// Advertise the provider beside the workspace apps. A query addressed to the provider
    /// resolves the keeper and the current grant afresh, including for an exact action lookup.
    pub(in crate::agents) fn catalogue(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        cert: &DeviceCert,
        supervisor: &Arc<AgentSupervisor>,
        message: &RemoteActionMessage,
        apps: impl FnOnce() -> Option<Pending<RemoteActionMessage>>,
    ) -> Option<Pending<RemoteActionMessage>> {
        let RemoteActionMessage::CatalogueRequest { correlation, query } = message else {
            return None;
        };
        let apps = match query.provider.as_deref() {
            Some(AGENTS_PROVIDER) => None,
            None if query.canonical_route.is_none()
                && query.view_resource.is_none()
                && query.action.is_none()
                && query.target.is_none() =>
            {
                Some(apps()?)
            }
            _ => return None,
        };
        let link = self.link.get().and_then(Weak::upgrade)?;
        let (caller, cert, supervisor, query, correlation) = (
            caller.clone(),
            cert.clone(),
            supervisor.clone(),
            query.clone(),
            *correlation,
        );
        let (done, answer) = pending();
        link.runtime.clone().spawn(async move {
            let entry = personal_entry(&link, workspace, &caller, &cert).await;
            if let Some(apps) = apps {
                let Some(mut answer) = apps.await else { return };
                if let RemoteActionMessage::CatalogueResponse {
                    result: Ok(page), ..
                } = &mut answer
                    && matches!(entry, Ok(Some(_)))
                {
                    page.providers.push(CatalogueProvider {
                        provider: AGENTS_PROVIDER.into(),
                        app_id: AGENTS_PROVIDER.into(),
                        workspace: workspace.to_string(),
                        browsable: true,
                        ..CatalogueProvider::default()
                    });
                }
                done.complete(answer);
                return;
            }
            let result = match entry {
                Ok(Some(entry)) => {
                    let status = supervisor.agent_grant_status(
                        workspace,
                        &entry.id.0,
                        &cert.user_public_key,
                        entry.created_at,
                    );
                    Ok(grant_page(&query, &entry, status.state))
                }
                Ok(None) | Err(ActionCallError::Refused { .. }) => {
                    Ok(CataloguePage::from_rows("0".into(), Vec::new(), &query))
                }
                Err(error) => Err(error),
            };
            done.complete(RemoteActionMessage::CatalogueResponse {
                correlation,
                result,
            });
        });
        Some(answer)
    }
}

fn grant_page(
    query: &CatalogueQuery,
    entry: &tilo_workspace_agent::Agent,
    state: GrantState,
) -> CataloguePage {
    let row = |action: AgentGrantAction, label: &str, description: &str, kind: ActionKind| {
        let target = action.target(&entry.id.0);
        let offer = Offer {
            resource: &target.address.resource,
            resource_kind: "agent.grant",
            action: &target.address.action,
            label,
            description: Some(description),
            kind,
            operation: OperationKind::Effect,
            confirm: false,
            asks: None,
            icon: None,
            keywords: Vec::new(),
        };
        let mut offered = PublishedAction::offered(&offer, &entry.name);
        offered.contract.retry = if kind == ActionKind::Read {
            RetryPolicy::Idempotent
        } else {
            RetryPolicy::RetryWithInvocationId
        };
        offered.contract.result = ResultSpec::Json {
            schema: grant_result(action),
        };
        CatalogueRow::for_action(
            &target.provider,
            AGENTS_PROVIDER,
            offered.descriptor(ExecutionLocus::ServerPreferred),
        )
    };
    let mut rows = vec![row(
        AgentGrantAction::Status,
        "Read assistant access",
        "Read the Chat signing session's readiness, approved apps and expiry.",
        ActionKind::Read,
    )];
    match state {
        GrantState::Approved | GrantState::Pending => {
            rows.push(row(
                AgentGrantAction::Revoke,
                "Revoke assistant access",
                "Withdraw this assistant's signing grant in this workspace.",
                ActionKind::Outside,
            ));
        }
        GrantState::Missing | GrantState::Denied | GrantState::Expired | GrantState::Revoked => {
            rows.push(row(AgentGrantAction::RequestChat, "Request Chat access", "Ask your approving device to allow this assistant to reply in Chat in this workspace.", ActionKind::Outside));
            rows.push(row(AgentGrantAction::RequestWork, "Request assistant work access", "Ask your approving device to allow Chat, Tasks, Assistant and Shortcuts in this workspace.", ActionKind::Outside));
        }
    }
    let revision = blake3::hash(&serde_json::to_vec(&rows).unwrap_or_default())
        .to_hex()
        .to_string();
    CataloguePage::from_rows(revision, rows, query)
}

/// The return value of each published grant action, available to clients browsing its contract.
fn grant_result(action: AgentGrantAction) -> serde_json::Value {
    match action {
        AgentGrantAction::Status => json!({
            "type": "object",
            "properties": {
                "state": {"enum": [GrantState::Missing, GrantState::Pending, GrantState::Approved,
                    GrantState::Denied, GrantState::Expired, GrantState::Revoked]},
                "ready": {"type": "boolean"},
                "apps": {"type": "array", "items": {"type": "string"}},
                "expiresAtMs": {"type": ["integer", "null"]}
            },
            "required": ["state", "ready", "apps", "expiresAtMs"]
        }),
        AgentGrantAction::Revoke => json!({"type": "object", "maxProperties": 0}),
        AgentGrantAction::RequestChat | AgentGrantAction::RequestWork => json!({
            "type": "object",
            "properties": {"grant": {"type": "string"}, "pairingCode": {"type": "string"}},
            "required": ["grant", "pairingCode"]
        }),
    }
}
