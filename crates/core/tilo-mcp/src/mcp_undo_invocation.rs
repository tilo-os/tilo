//! Taking back one invocation, as the action on its record: `<record>#undo`, answered by the
//! host that keeps the invocation. The host takes back exactly the changes its calls made, in
//! every record it changed, through those records' own history; nothing is looked for here.
//!
//! This is the one take-back a declared run has, whatever asked for it: `/v1`'s
//! `undoInvocation` and an agent's `tilo_status {undo: true}` reach the record's action here,
//! and a decline takes a waiting run back through the same take-back.

use tilo_action_services::workspace_invocations;
use tilo_actions::ActionAddress;
use tilo_actions::remote::{InvokeRequest, ReceiptStatus, RemoteActionTarget};

use super::{ActionProvider, Failure, McpServer, Reply, invocation_id, json};

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// Take back what the invocation `invocation` changed, through the record's own `undo`.
    pub(super) async fn undo_invocation(
        &self,
        invocation: &str,
        structured: bool,
    ) -> Result<Reply, Failure> {
        let id = invocation_id(invocation)?;
        let target = RemoteActionTarget {
            provider: workspace_invocations::PROVIDER.to_owned(),
            address: ActionAddress::new(id.0.to_string(), workspace_invocations::UNDO),
        };
        let request = InvokeRequest::plain(target, serde_json::Map::new());
        let receipt = self
            .host()
            .invoke(&self.caller(), request)
            .await
            .ok_or("the host dropped the request")??;
        match receipt.status {
            ReceiptStatus::Completed { .. } => {}
            ReceiptStatus::Rejected { error } => return Err(error.into()),
            ReceiptStatus::Failed { error } => return Err(error.message.into()),
            ReceiptStatus::Cancelled { .. } => return Err("taking it back was stopped".into()),
            ReceiptStatus::Accepted => return Err("taking it back is still running".into()),
        }
        let mut reply = Reply::ok(format!(
            "{invocation} is taken back: what it changed is gone"
        ));
        if structured {
            reply.structured = Some(json!({
                "status": "ok",
                "invocation_id": id.0.to_string(),
            }));
        }
        Ok(reply)
    }
}
