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
use tilo_server_protocol::agents::{
    AGENT_RESOURCE_PREFIX, AGENTS_PROVIDER, GRANT_STATUS_ACTION, GrantState,
    REQUEST_ASSISTANT_WORK_GRANT_ACTION, REQUEST_CHAT_GRANT_ACTION, REVOKE_ACTION,
};
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
    let resource = format!("{AGENT_RESOURCE_PREFIX}{}", entry.id.0);
    let row = |action: &str, label: &str, description: &str, kind: ActionKind| {
        let offer = Offer {
            resource: &resource,
            resource_kind: "agent.grant",
            action,
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
            AGENTS_PROVIDER,
            AGENTS_PROVIDER,
            offered.descriptor(ExecutionLocus::ServerPreferred),
        )
    };
    let mut rows = vec![row(
        GRANT_STATUS_ACTION,
        "Read assistant access",
        "Read the Chat signing session's readiness, approved apps and expiry.",
        ActionKind::Read,
    )];
    match state {
        GrantState::Approved | GrantState::Pending => {
            rows.push(row(
                REVOKE_ACTION,
                "Revoke assistant access",
                "Withdraw this assistant's signing grant in this workspace.",
                ActionKind::Outside,
            ));
        }
        GrantState::Missing | GrantState::Denied | GrantState::Expired | GrantState::Revoked => {
            rows.push(row(REQUEST_CHAT_GRANT_ACTION, "Request Chat access", "Ask your approving device to allow this assistant to reply in Chat in this workspace.", ActionKind::Outside));
            rows.push(row(REQUEST_ASSISTANT_WORK_GRANT_ACTION, "Request assistant work access", "Ask your approving device to allow Chat, Tasks, Assistant and Shortcuts in this workspace.", ActionKind::Outside));
        }
    }
    let revision = blake3::hash(&serde_json::to_vec(&rows).unwrap_or_default())
        .to_hex()
        .to_string();
    CataloguePage::from_rows(revision, rows, query)
}

/// The return value of each published grant action, available to clients browsing its contract.
fn grant_result(action: &str) -> serde_json::Value {
    match action {
        GRANT_STATUS_ACTION => json!({
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
        REVOKE_ACTION => json!({"type": "object", "maxProperties": 0}),
        _ => json!({
            "type": "object",
            "properties": {"grant": {"type": "string"}, "pairingCode": {"type": "string"}},
            "required": ["grant", "pairingCode"]
        }),
    }
}
