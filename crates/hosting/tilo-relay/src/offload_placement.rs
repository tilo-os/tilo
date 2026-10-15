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

use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};

use crate::RelayClient;
use crate::compute::MAX_COMPUTE_INPUTS;
use crate::compute_actions::{COMPUTE_PROVIDER, PLACE_JOB, compute_resource};
use crate::mesh::DeviceKey;
use crate::transport::{Envelope, REMOTE_ACTION_ENVELOPE_VERSION};

/// How long the relay has to answer a placement question. Placement reads
/// the workspace's live advertisements and runs nothing, so a slow answer
/// means an unreachable relay: the action then runs locally.
pub const PLACEMENT_TIMEOUT: Duration = Duration::from_secs(10);

/// A boxed placement answer.
pub type PlacementAnswer<'a> = Pin<Box<dyn Future<Output = Result<Placed, String>> + Send + 'a>>;

/// What a requester asks placement about one job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacementQuery<'a> {
    /// The workspace the job runs in.
    pub workspace_id: u64,
    /// The installed app whose entrypoint it runs.
    pub app: &'a str,
    /// Whether it needs a GPU.
    pub gpu: bool,
    /// The bytes of its inputs.
    pub input_bytes: u64,
    /// Its distinct inputs by digest, each with its bytes, so a device
    /// already holding some is not weighed as receiving them. Named only
    /// up to [`MAX_COMPUTE_INPUTS`]; past that placement weighs
    /// `input_bytes` alone.
    pub inputs: &'a [([u8; 32], u64)],
    /// How long the asking device last took for this job, scaled to inputs
    /// this size; `None` before it has run it. Placement names a worker only
    /// when moving the inputs there is faster.
    pub local_wall_ms: Option<u64>,
}

/// Placement's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    /// The device that should run the job, or `None` to run it here.
    pub worker: Option<DeviceKey>,
    /// Why, as placement put it.
    pub reason: String,
}

/// Where a compute job in a workspace should run.
pub trait ComputePlacement: Send + Sync {
    /// Where `query`'s job should run now, or why nobody could be asked.
    fn place<'a>(&'a self, query: PlacementQuery<'a>) -> PlacementAnswer<'a>;
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

    async fn ask(&self, query: PlacementQuery<'_>) -> Result<Placed, String> {
        let PlacementQuery {
            workspace_id,
            app,
            gpu,
            input_bytes,
            inputs,
            local_wall_ms,
        } = query;
        let invocation = InvocationId::new();
        let mut arguments = Map::new();
        arguments.insert("app".into(), Value::String(app.into()));
        arguments.insert("gpu".into(), Value::Bool(gpu));
        arguments.insert("input_bytes".into(), Value::from(input_bytes));
        if !inputs.is_empty() && inputs.len() <= MAX_COMPUTE_INPUTS {
            let inputs = inputs
                .iter()
                .map(|(digest, bytes)| json!({ "digest": hex::encode(digest), "bytes": bytes }))
                .collect();
            arguments.insert("inputs".into(), Value::Array(inputs));
        }
        if let Some(local_wall_ms) = local_wall_ms {
            arguments.insert("local_wall_ms".into(), Value::from(local_wall_ms));
        }
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
) -> Result<Placed, String> {
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
                    ReceiptStatus::Completed { output } => placed(output.value.as_ref()),
                    other => Err(format!("placement did not complete: {other:?}")),
                };
            }
            _ => {}
        }
    }
}

/// What a `compute.place` output says: the worker it names (hex, or `null`
/// for none) and its reason.
fn placed(output: Option<&Value>) -> Result<Placed, String> {
    let reason = output
        .and_then(|output| output.get("reason"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let worker = match output.and_then(|output| output.get("worker")) {
        Some(Value::Null) => None,
        Some(Value::String(worker)) => {
            let mut key = [0; 32];
            hex::decode_to_slice(worker, &mut key)
                .map_err(|_| "placement named a malformed worker".to_string())?;
            Some(key)
        }
        _ => return Err("placement answered without a worker".into()),
    };
    Ok(Placed { worker, reason })
}

impl ComputePlacement for RelayPlacement {
    fn place<'a>(&'a self, query: PlacementQuery<'a>) -> PlacementAnswer<'a> {
        Box::pin(async move {
            tokio::time::timeout(PLACEMENT_TIMEOUT, self.ask(query))
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
            placed(Some(
                &serde_json::json!({"worker": hex::encode(worker), "reason": "x"})
            )),
            Ok(Placed {
                worker: Some(worker),
                reason: "x".into()
            })
        );
        assert_eq!(
            placed(Some(&serde_json::json!({"worker": null, "reason": "none"}))),
            Ok(Placed {
                worker: None,
                reason: "none".into()
            })
        );
        assert!(placed(Some(&serde_json::json!({"worker": "zz"}))).is_err());
        assert!(placed(None).is_err());
    }
}
