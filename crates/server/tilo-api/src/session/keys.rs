//! The keys a workspace admitted, as the path `workspace/keys` of every
//! workspace a key acts in.
//!
//! - `tilo_read workspace/keys` answers the grants the workspace's governance
//!   records of the key's person: each key it admitted, what it is called, the
//!   workspace it reaches and until when;
//! - `workspace/keys#revoke {grant}` records the key's revocation there, and
//!   every holder and API server that reads the workspace refuses it from then
//!   on.
//!
//! The record is the workspace's own (`key_grants` in its governance), written
//! by the host that admitted the key and read by every holder, so a key is
//! listed and revoked the same wherever the workspace is held and whichever
//! API server the caller reaches.

use crate::account::{Outcome, failed};
use crate::server::Connection;
use collab_types::workspace_grant::WorkspaceGrant;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};

/// Listing the workspace's grants.
pub const LIST: &str = "workspace/keys";
/// Revoking one of them.
pub const REVOKE: &str = "workspace/keys#revoke";
/// The governance verb the workspace host answers the listing by.
const LIST_ACTION: &str = "key.list";
/// The governance verb it answers the revocation by.
const REVOKE_ACTION: &str = "key.revoke";

/// The grants `workspace` records of the calling person's, as its host
/// answers them.
///
/// # Errors
/// The workspace did not answer, or would not list them.
pub(crate) async fn grants_in(
    connection: &Connection,
    workspace: &str,
) -> Result<Vec<WorkspaceGrant>, Outcome> {
    let output = call(connection, workspace, LIST_ACTION, Map::new()).await?;
    serde_json::from_value::<Grants>(output)
        .map(|grants| grants.grants)
        .map_err(|error| {
            failed(
                "unavailable",
                format!("the workspace answered no keys: {error}"),
                "try again shortly",
            )
        })
}

/// Record `grant` revoked in `workspace`: whether it changed the workspace.
///
/// # Errors
/// The workspace did not answer, would not revoke it, or does not let this key.
pub(crate) async fn revoke_in(
    connection: &Connection,
    workspace: &str,
    grant: &str,
) -> Result<bool, Outcome> {
    let mut arguments = Map::new();
    arguments.insert("grant".to_owned(), json!(grant));
    match call(connection, workspace, REVOKE_ACTION, arguments).await {
        Ok(_) => Ok(true),
        // A workspace that holds no such grant, or already refuses the key as
        // revoked, has nothing left to record.
        Err(outcome) if settled(&outcome) => Ok(false),
        Err(outcome) => Err(outcome),
    }
}

#[derive(serde::Deserialize)]
struct Grants {
    grants: Vec<WorkspaceGrant>,
}

/// Run the governance verb `action` in `workspace` as the key's session, and
/// answer its output.
async fn call(
    connection: &Connection,
    workspace: &str,
    action: &str,
    arguments: Map<String, Value>,
) -> Result<Value, Outcome> {
    let id = connection
        .server()
        .enter(connection, workspace)
        .await
        .map_err(|error| crate::account::refused(&error))?;
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, action),
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    let unanswered = || {
        failed(
            "unavailable",
            "the workspace did not answer",
            "try again shortly",
        )
    };
    let receipt = connection
        .device
        .actions(id)
        .invoke(&Caller::local_person("tilo api"), request)
        .await
        .ok_or_else(unanswered)?
        .map_err(|error| refused(&error))?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        ReceiptStatus::Rejected { error } => Err(refused(&error)),
        ReceiptStatus::Failed { error } => {
            Err(failed("unavailable", error.message, "try again shortly"))
        }
        ReceiptStatus::Accepted | ReceiptStatus::Cancelled { .. } => Err(unanswered()),
    }
}

/// The workspace already holds nothing left to record for this key: it never
/// admitted the grant, or it refuses the key as revoked.
fn settled(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::Failed { code, .. } if *code == "not_found" || *code == "key_revoked")
}

/// What the workspace's refusal, `error`, tells the caller.
fn refused(error: &ActionCallError) -> Outcome {
    match error {
        // The workspace records its keys' revocations: one it refuses as
        // revoked asks its person for another.
        ActionCallError::Refused { reason } if reason.contains("key_revoked") => failed(
            "key_revoked",
            "this key was revoked",
            "ask the person for a new key",
        ),
        ActionCallError::UnknownAction => failed(
            "not_found",
            "the workspace holds no such key, or it is not this key's to revoke",
            "GET /v1/keys lists the keys this one holds and the keys it minted",
        ),
        ActionCallError::NotPermitted => failed(
            "not_permitted",
            "the workspace does not let this key revoke that one",
            "only the key itself and the key that minted it revoke it",
        ),
        other => failed("unavailable", other.to_string(), "try again shortly"),
    }
}
