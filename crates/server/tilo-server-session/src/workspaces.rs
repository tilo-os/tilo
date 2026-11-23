//! Workspace administration asked of a host over a device's session: the
//! host's own operations, as actions addressed to the device the host serves
//! them by.
//!
//! The requests, answers and action names live in
//! [`tilo_server_protocol::workspaces`], where the host and every client read
//! them; what stays here is the asking, which needs a session.

pub use tilo_server_protocol::workspaces::*;

use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};

use crate::client::ServerClient;
use crate::transport::Envelope;

/// Ask `client`'s host its own operation `invoke`, directly over the
/// session (a device of the host's person asks; no workspace is named), and
/// answer its receipt.
///
/// # Errors
///
/// The session closed before the host answered, or the invocation could not
/// be encoded.
pub async fn ask_host(
    client: &ServerClient,
    invoke: InvokeRequest,
) -> Result<ReceiptStatus, String> {
    let invocation = invoke.invocation;
    let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(invoke))
        .encode()
        .map_err(|error| error.to_string())?;
    client
        .send(Envelope::HostAction { frame })
        .map_err(|_| "the host's session closed".to_string())?;
    loop {
        match client.recv().await {
            Some(Envelope::HostAction { frame }) => {
                let message = RemoteActionFrame::decode(&frame)
                    .map_err(|error| error.to_string())?
                    .message;
                if let RemoteActionMessage::Receipt(receipt) = message
                    && receipt.invocation == invocation
                {
                    return Ok(receipt.status);
                }
            }
            Some(_) => {}
            None => return Err("the host's session closed".into()),
        }
    }
}

/// Ask `client`'s server to enter this device into `workspace`, and answer the
/// invitation text the device redeems to hold it: [`ENTER_WORKSPACE`]'s ask, and
/// the invitation it answers with, in the shape every join redeems.
///
/// # Errors
///
/// The host did not answer, refused, or answered no invitation.
pub async fn enter_invitation(
    client: &ServerClient,
    workspace: &str,
    expires_in_seconds: u64,
) -> Result<String, String> {
    let arguments = serde_json::Map::from_iter([
        ("workspace".to_owned(), workspace.into()),
        ("expires_in_seconds".to_owned(), expires_in_seconds.into()),
    ]);
    let entered: EnteredWorkspace =
        match ask_host(client, host_invoke(ENTER_WORKSPACE, arguments)).await? {
            ReceiptStatus::Completed { output } => {
                serde_json::from_value(output.value.unwrap_or_default())
                    .map_err(|error| format!("the server answered no invitation: {error}"))?
            }
            ReceiptStatus::Rejected { error } => {
                return Err(format!("the server refused to enter it: {error}"));
            }
            ReceiptStatus::Failed { error } => return Err(error.message),
            ReceiptStatus::Accepted | ReceiptStatus::Cancelled { .. } => {
                return Err("the server did not answer the entry".into());
            }
        };
    let signed = entered.invitation.decode()?.signed_invitation;
    Ok(tilo_workspace_join::encode_invitation_text(&signed))
}

/// The host operation `action` with `arguments`, under a new invocation.
fn host_invoke(
    action: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.to_owned(),
            address: ActionAddress::new(HOST_WORKSPACES_RESOURCE.to_owned(), action),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}
