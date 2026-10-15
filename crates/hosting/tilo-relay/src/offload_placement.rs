//! Asking the workspace's relay where an offloadable action should run.
//!
//! The relay answers placement as an action of its own, `compute.place` on
//! provider `tilo.compute` ([`crate::compute_actions`]), to the relay owner's
//! own devices acting as themselves. [`RelayPlacement`] is the requester's
//! side of that action over the relay-v1 remote-action route: it asks, and
//! reads back the chosen worker or `None` when no device is better placed.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};

use crate::RelayClient;
use crate::compute_actions::{COMPUTE_PROVIDER, PLACE_JOB, compute_resource};
use crate::mesh::DeviceKey;
use crate::transport::{Envelope, REMOTE_ACTION_ENVELOPE_VERSION};

/// How long the relay has to answer a placement question. Placement reads
/// the workspace's live advertisements and runs nothing, so a slow answer
/// means an unreachable relay: the action then runs locally.
pub const PLACEMENT_TIMEOUT: Duration = Duration::from_secs(10);

/// A boxed placement answer.
pub type PlacementAnswer<'a> =
    Pin<Box<dyn Future<Output = Result<Option<DeviceKey>, String>> + Send + 'a>>;

/// Where a compute job of `app` in a workspace should run.
pub trait ComputePlacement: Send + Sync {
    /// The device that should run a job of `app` (needing a GPU when `gpu`)
    /// in `workspace_id` now, `Ok(None)` when none can, or why nobody could
    /// be asked.
    fn place<'a>(&'a self, workspace_id: u64, app: &'a str, gpu: bool) -> PlacementAnswer<'a>;
}

/// Placement asked of the workspace's relay over its relay-v1 session.
///
/// The client is this placement's own: answers are read off it, so nothing
/// else should receive on it. It must be subscribed to every workspace it is
/// asked about.
pub struct RelayPlacement {
    client: tokio::sync::Mutex<RelayClient>,
    relay_device: DeviceKey,
}

impl RelayPlacement {
    /// Placement asked of the relay whose device key is `relay_device`,
    /// over `client`.
    #[must_use]
    pub fn new(client: RelayClient, relay_device: DeviceKey) -> Self {
        Self {
            client: tokio::sync::Mutex::new(client),
            relay_device,
        }
    }

    async fn ask(
        &self,
        workspace_id: u64,
        app: &str,
        gpu: bool,
    ) -> Result<Option<DeviceKey>, String> {
        let invocation = InvocationId::new();
        let mut arguments = Map::new();
        arguments.insert("app".into(), Value::String(app.into()));
        arguments.insert("gpu".into(), Value::Bool(gpu));
        let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(InvokeRequest {
            invocation,
            target: RemoteActionTarget {
                provider: COMPUTE_PROVIDER.into(),
                address: ActionAddress::new(compute_resource(workspace_id), PLACE_JOB),
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        }))
        .encode()
        .map_err(|error| error.to_string())?;
        // One question at a time: answers are read off the shared session.
        let client = self.client.lock().await;
        let answer = exchange(&client, self.relay_device, workspace_id, invocation, frame).await;
        drop(client);
        answer
    }
}

/// Send placement question `frame` (invocation `invocation`) to
/// `relay_device` over `client` and read its answer.
async fn exchange(
    client: &RelayClient,
    relay_device: DeviceKey,
    workspace_id: u64,
    invocation: InvocationId,
    frame: Vec<u8>,
) -> Result<Option<DeviceKey>, String> {
    client
        .send(Envelope::RemoteAction {
            workspace_id,
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: relay_device,
            // The relay stamps the authenticated sender.
            from_device_public_key: [0; 32],
            from_user_public_key: [0; 32],
            frame,
        })
        .map_err(|_| "the relay session is closed".to_string())?;
    loop {
        let envelope = client
            .recv()
            .await
            .ok_or_else(|| "the relay session closed".to_string())?;
        match envelope {
            Envelope::RemoteActionUndeliverable {
                to_device_public_key,
                reason,
                ..
            } if to_device_public_key == relay_device => {
                return Err(format!("the relay refused placement: {reason}"));
            }
            Envelope::RemoteAction {
                from_device_public_key,
                frame,
                ..
            } if from_device_public_key == relay_device => {
                let Ok(decoded) = RemoteActionFrame::decode(&frame) else {
                    continue;
                };
                let status = match decoded.message {
                    RemoteActionMessage::Receipt(receipt) if receipt.invocation == invocation => {
                        receipt.status
                    }
                    _ => continue,
                };
                return match status {
                    ReceiptStatus::Completed { output } => worker_of(output.value.as_ref()),
                    other => Err(format!("placement did not complete: {other:?}")),
                };
            }
            _ => {}
        }
    }
}

/// The worker a `compute.place` output names: hex, or `null` for none.
fn worker_of(output: Option<&Value>) -> Result<Option<DeviceKey>, String> {
    match output.and_then(|output| output.get("worker")) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(worker)) => {
            let mut key = [0; 32];
            hex::decode_to_slice(worker, &mut key)
                .map_err(|_| "placement named a malformed worker".to_string())?;
            Ok(Some(key))
        }
        _ => Err("placement answered without a worker".into()),
    }
}

impl ComputePlacement for RelayPlacement {
    fn place<'a>(&'a self, workspace_id: u64, app: &'a str, gpu: bool) -> PlacementAnswer<'a> {
        Box::pin(async move {
            tokio::time::timeout(PLACEMENT_TIMEOUT, self.ask(workspace_id, app, gpu))
                .await
                .unwrap_or_else(|_| Err("the relay did not answer placement".into()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_placement_output_names_a_worker_or_none() {
        let worker = [9_u8; 32];
        assert_eq!(
            worker_of(Some(
                &serde_json::json!({"worker": hex::encode(worker), "reason": "x"})
            )),
            Ok(Some(worker))
        );
        assert_eq!(
            worker_of(Some(&serde_json::json!({"worker": null, "reason": "none"}))),
            Ok(None)
        );
        assert!(worker_of(Some(&serde_json::json!({"worker": "zz"}))).is_err());
        assert!(worker_of(None).is_err());
    }
}
