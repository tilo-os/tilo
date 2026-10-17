//! Asking the workspace's relay where an offloadable action should run.
//!
//! The relay answers placement as an action of its own, `compute.place` on
//! provider `tilo.compute` ([`crate::compute_actions`]), to the relay owner's
//! own devices acting as themselves. [`RelayPlacement`] is the requester's
//! side of that action over the relay-v1 remote-action route: it asks, and
//! reads back the chosen worker or `None` when no device is better placed.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_lock::MutexRecover as _;
use tokio::sync::oneshot;

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
/// The session must stay subscribed to every workspace it is asked about
/// (the relay refuses a remote action from a device that is not), so the
/// room's own traffic — deltas, presence, everything its members send —
/// arrives on it too. A pump started with the placement drains it for as
/// long as the session lives: it hands the one question in flight its
/// answer and drops everything else at once, so a busy room never backs the
/// session up, never pins its queue, and never buries an answer behind
/// traffic placement does not read. Nothing else may receive on the client.
pub struct RelayPlacement {
    client: RelayClient,
    relay_device: DeviceKey,
    /// One question at a time: answers are matched to the one in flight.
    asking: tokio::sync::Mutex<()>,
    answers: Arc<Mutex<Answers>>,
    dropped: Arc<AtomicU64>,
    _pump: AbortOnDrop,
}

/// The question the pump answers, if any, and whether it still runs.
#[derive(Default)]
struct Answers {
    waiting: Option<Waiting>,
    closed: bool,
}

/// A placement question awaiting its receipt.
struct Waiting {
    invocation: InvocationId,
    answer: oneshot::Sender<Result<Placed, String>>,
}

/// Aborts a task when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl RelayPlacement {
    /// Placement asked of the relay whose device key is `relay_device`,
    /// over `client`, whose inbound traffic a pump on the current Tokio
    /// runtime drains from now on.
    #[must_use]
    pub fn new(client: RelayClient, relay_device: DeviceKey) -> Self {
        let answers = Arc::new(Mutex::new(Answers::default()));
        let dropped = Arc::new(AtomicU64::new(0));
        let pump = tokio::spawn(pump(
            Arc::clone(&client.inbound),
            relay_device,
            Arc::clone(&answers),
            Arc::clone(&dropped),
        ));
        Self {
            client,
            relay_device,
            asking: tokio::sync::Mutex::new(()),
            answers,
            dropped,
            _pump: AbortOnDrop(pump),
        }
    }

    /// How many inbound envelopes the session drained without a question
    /// waiting for them: the room's own traffic, and stale answers.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
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
        let asking = self.asking.lock().await;
        let (answer, answered) = oneshot::channel();
        {
            let mut answers = self.answers.lock_recover();
            if answers.closed {
                return Err("the relay session closed".into());
            }
            // A question that timed out is replaced: its answer, should it
            // still come, is dropped as stale.
            answers.waiting = Some(Waiting { invocation, answer });
        }
        self.client
            .send(Envelope::RemoteAction {
                workspace_id,
                version: REMOTE_ACTION_ENVELOPE_VERSION,
                to_device_public_key: self.relay_device,
                // The relay stamps the authenticated sender.
                from_device_public_key: [0; 32],
                from_user_public_key: [0; 32],
                frame,
            })
            .map_err(|_| "the relay session is closed".to_string())?;
        let answer = answered
            .await
            .unwrap_or_else(|_| Err("the relay session closed".into()));
        drop(asking);
        answer
    }
}

/// Drain `inbound` until the session ends: hand the question waiting in
/// `answers` its answer from `relay_device`, count and drop everything
/// else. When the session ends the question waiting fails, and so does
/// every later one.
async fn pump(
    inbound: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Envelope>>>,
    relay_device: DeviceKey,
    answers: Arc<Mutex<Answers>>,
    dropped: Arc<AtomicU64>,
) {
    let mut inbound = inbound.lock().await;
    while let Some(envelope) = inbound.recv().await {
        if !deliver(envelope, relay_device, &answers) {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    drop(inbound);
    let mut answers = answers.lock_recover();
    answers.closed = true;
    // Dropping its sender tells the question waiting the session closed.
    answers.waiting = None;
}

/// Hand `envelope` to the question waiting in `answers` when it is that
/// question's answer from `relay_device`; whether it was.
fn deliver(envelope: Envelope, relay_device: DeviceKey, answers: &Mutex<Answers>) -> bool {
    let (invocation, answer) = match envelope {
        Envelope::RemoteActionUndeliverable {
            to_device_public_key,
            reason,
            ..
        } if to_device_public_key == relay_device => {
            (None, Err(format!("the relay refused placement: {reason}")))
        }
        Envelope::RemoteAction {
            from_device_public_key,
            frame,
            ..
        } if from_device_public_key == relay_device => {
            let Ok(RemoteActionFrame {
                message: RemoteActionMessage::Receipt(receipt),
                ..
            }) = RemoteActionFrame::decode(&frame)
            else {
                return false;
            };
            let answer = match receipt.status {
                ReceiptStatus::Completed { output } => placed(output.value.as_ref()),
                other => Err(format!("placement did not complete: {other:?}")),
            };
            (Some(receipt.invocation), answer)
        }
        _ => return false,
    };
    let mut answers = answers.lock_recover();
    // A receipt answers only its own question; an undeliverable names none,
    // so it fails the question in flight.
    if answers
        .waiting
        .as_ref()
        .is_none_or(|waiting| invocation.is_some_and(|invocation| waiting.invocation != invocation))
    {
        return false;
    }
    let Some(waiting) = answers.waiting.take() else {
        return false;
    };
    drop(answers);
    let _ = waiting.answer.send(answer);
    true
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
