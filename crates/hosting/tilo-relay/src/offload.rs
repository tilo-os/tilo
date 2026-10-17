//! Offloadable actions: invoking an action that declares itself heavy
//! ([`tilo_actions::Offload`]) runs it on the invoker's best-placed device
//! and returns what a local run returns.
//!
//! [`Offloader::invoke`] is the invoke path. For an offloadable action it
//!
//! 1. binds the action's inputs — workspace files, by the manifest hash the
//!    invoker's own view resolved — in this device's workspace content
//!    ([`WorkspaceContent`]), which is all a worker can ever read;
//! 2. asks the workspace's relay where the job should run
//!    ([`ComputePlacement`], the relay's `compute.place`), naming the
//!    inputs' digests and bytes and how long this device last took for the
//!    action, so the relay keeps the job here when moving the inputs to a
//!    worker would take longer than that ([`WallTime`]) — counting only
//!    the inputs that worker was not seen to hold already;
//! 3. when another device is better placed, binds a compute job to it —
//!    the declared package, entrypoint and ceilings, the inputs by digest,
//!    no arguments — submits it on this device's long-lived compute session,
//!    serves the worker exactly those inputs, follows the result and
//!    fetches the outputs back into the workspace content;
//! 4. otherwise — no better device, the relay unreachable, the package not
//!    installed here, or the remote run failing in any way, including its
//!    worker being lost or revoked mid-job, which the relay reports at once
//!    — runs the same transform locally.
//!
//! Both paths end the same way: the outputs, stored as workspace content,
//! returned as content resources in order. An offloadable action is a pure
//! transform, so the bytes, and therefore the [`ActionOutput`], are the
//! same whichever device produced them. Where it ran is reported beside the
//! output ([`OffloadRun::ran_on`]), never inside it.
//!
//! Offloading changes where an admitted invocation runs, never whether it
//! may: the action door decided that before this path is reached, for a
//! person, a script and an agent alike. Placement stays the relay owner's;
//! the relay admits the job again against the worker's live advertisement;
//! the worker runs it as the signed package's catalogue entrypoint under its
//! launcher sandbox and reads nothing the job does not list.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use iroh::endpoint::Connection;
use thiserror::Error;
use tilo_actions::{ActionDescriptor, ActionOutput, Offload};
use tilo_clock::unix_now_ms;
use tokio::sync::{mpsc, oneshot};

use crate::compute::{
    COMPUTE_JOB_SCHEMA_VERSION, ComputeJobPhase, ComputeJobSpec, ComputePackageCatalogue,
    ComputeResourceLimits, MAX_COMPUTE_JOB_LIFETIME_MS,
};
use crate::compute_content::fetch_job_content;
use crate::compute_session::{ComputeClient, ComputeSessionError};
use crate::compute_transport::{ComputeEnvelope, ComputeJobBinding, ComputeResultUpdate};
use crate::mesh::DeviceKey;
pub use crate::offload_content::{BindError, ReplicaContent, WorkspaceContent};
pub use crate::offload_placement::{
    ComputePlacement, PLACEMENT_TIMEOUT, Placed, PlacementAnswer, PlacementQuery, RelayPlacement,
};
pub use crate::offload_route::OffloadRoute;

/// How long past a job's own lifetime its requester waits for the result
/// before it gives up on the worker and runs the action itself.
const RESULT_GRACE: Duration = Duration::from_secs(5);
/// Commands queued to the compute session at once.
const COMMAND_CAPACITY: usize = 16;

/// The local run of an offloadable action: its inputs' bytes in, its
/// outputs' bytes out, in order. The same transform the declared package
/// entrypoint performs on a worker.
pub type LocalTransform = dyn Fn(&[Vec<u8>]) -> Result<Vec<Vec<u8>>, String> + Send + Sync;

/// One invocation of an offloadable action.
pub struct OffloadJob<'a> {
    /// The workspace it was invoked in.
    pub workspace_id: u64,
    /// The invoked action; its contract declares the offload.
    pub action: &'a ActionDescriptor,
    /// Its inputs: workspace file versions, by manifest hash, as the
    /// invoker's view of the workspace resolved them.
    pub inputs: Vec<[u8; 32]>,
}

/// Where an invocation ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RanOn {
    /// On this other device.
    Worker(DeviceKey),
    /// Here, and why it was not offloaded.
    Local {
        /// Why: no better-placed device, or what failed remotely.
        reason: String,
    },
}

/// What an offloadable invocation came to.
#[derive(Clone, Debug, PartialEq)]
pub struct OffloadRun {
    /// The action's output, identical wherever it ran.
    pub output: ActionOutput,
    /// Its outputs' byte digests, in order. Each is in the workspace content
    /// ([`WorkspaceContent::manifest_of`] names its file version).
    pub outputs: Vec<[u8; 32]>,
    /// Where it ran.
    pub ran_on: RanOn,
    /// Wall time from invocation to output.
    pub elapsed: Duration,
}

/// Why an offloadable invocation produced no output.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum OffloadError {
    /// The action declares no offload, or an invalid one.
    #[error("the action is not offloadable: {0}")]
    NotOffloadable(String),
    /// An input could not be bound from the workspace.
    #[error("input {index}: {error}")]
    Input {
        /// Which input, in invocation order.
        index: usize,
        /// Why.
        error: BindError,
    },
    /// The action itself failed (it fails the same way on any device).
    #[error("{0}")]
    Action(String),
    /// The outputs could not be kept in the workspace content.
    #[error("output store: {0}")]
    Store(String),
}

/// The wall time last observed for an action on a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WallTime {
    /// The offloaded transform, as `package:entrypoint`.
    pub action: String,
    /// The device that ran it.
    pub device: DeviceKey,
    /// How long it took: on another device the whole remote run, transfers
    /// included; here the local run alone.
    pub elapsed: Duration,
    /// The bytes of the inputs it ran over.
    pub input_bytes: u64,
}

/// One recorded run: how long, over how many input bytes.
#[derive(Clone, Copy, Debug)]
struct Observed {
    elapsed: Duration,
    input_bytes: u64,
}

impl Observed {
    /// This run's wall time scaled to `input_bytes` of input, in whole
    /// milliseconds: the transforms offloaded are batch work whose cost
    /// grows with what they read.
    fn scaled_ms(self, input_bytes: u64) -> u64 {
        let millis = self.elapsed.as_millis();
        let scaled = if self.input_bytes == 0 {
            millis
        } else {
            millis
                .saturating_mul(u128::from(input_bytes))
                .div_ceil(u128::from(self.input_bytes))
        };
        u64::try_from(scaled).unwrap_or(u64::MAX)
    }
}

/// One invocation's inputs as bound in this device's workspace content.
struct BoundInputs {
    /// Their byte digests, in invocation order.
    digests: Vec<[u8; 32]>,
    /// Each distinct digest once, with its bytes.
    distinct: Vec<([u8; 32], u64)>,
    /// The bytes of all of them, in invocation order.
    bytes: u64,
}

/// What the compute session is asked to do.
enum Command {
    Submit {
        spec: Box<ComputeJobSpec>,
        result: oneshot::Sender<ComputeResultUpdate>,
    },
    Cancel(ComputeJobBinding),
}

/// Aborts a task when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What the offloader runs on.
pub struct OffloaderParts {
    /// This device's authenticated compute session with the relay.
    pub compute: ComputeClient,
    /// The workspaces it offloads in; the session attaches to each.
    pub workspaces: Vec<u64>,
    /// This device's workspace content.
    pub content: WorkspaceContent,
    /// Where jobs should run.
    pub placement: Arc<dyn ComputePlacement>,
    /// This device's installed packages: the signed package a job names.
    pub packages: Arc<dyn ComputePackageCatalogue>,
}

/// The invoke path of offloadable actions on one device.
pub struct Offloader {
    device: DeviceKey,
    content: WorkspaceContent,
    placement: Arc<dyn ComputePlacement>,
    packages: Arc<dyn ComputePackageCatalogue>,
    connection: Connection,
    commands: mpsc::Sender<Command>,
    _session: AbortOnDrop,
    wall_times: Mutex<HashMap<(String, DeviceKey), Observed>>,
}

impl Offloader {
    /// Attach `parts.compute` to every workspace, serve job inputs from
    /// `parts.content`, and start following results.
    ///
    /// # Errors
    ///
    /// The session's error, or [`ComputeSessionError::Protocol`] when the
    /// relay refuses a workspace.
    pub async fn start(parts: OffloaderParts) -> Result<Self, ComputeSessionError> {
        let OffloaderParts {
            mut compute,
            workspaces,
            content,
            placement,
            packages,
        } = parts;
        for workspace_id in workspaces {
            compute.attach(workspace_id).await?;
            loop {
                let delivery = compute.receive().await?;
                match delivery.envelope {
                    ComputeEnvelope::Ready {
                        workspace_id: ready,
                        ..
                    } if ready == workspace_id => {
                        break;
                    }
                    ComputeEnvelope::NotAuthorised { reason, .. } => {
                        return Err(ComputeSessionError::Protocol(format!(
                            "workspace {workspace_id} refused compute: {reason}"
                        )));
                    }
                    // A result for an earlier run of this device is only
                    // acknowledged: nobody waits for it now.
                    _ => compute.acknowledge(&delivery).await?,
                }
            }
        }
        compute.serve_inputs(Arc::new(content.clone()));
        let device = compute.own_device();
        let connection = compute.connection();
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let session = AbortOnDrop(tokio::spawn(drive(compute, receiver)));
        Ok(Self {
            device,
            content,
            placement,
            packages,
            connection,
            commands,
            _session: session,
            wall_times: Mutex::default(),
        })
    }

    /// The wall times observed so far, one per action and device.
    #[must_use]
    pub fn wall_times(&self) -> Vec<WallTime> {
        let mut times: Vec<_> = self
            .wall_times
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|((action, device), observed)| WallTime {
                action: action.clone(),
                device: *device,
                elapsed: observed.elapsed,
                input_bytes: observed.input_bytes,
            })
            .collect();
        times.sort_by(|a, b| (&a.action, a.device).cmp(&(&b.action, b.device)));
        times
    }

    /// Invoke `job`'s offloadable action: on the best-placed device when
    /// that is another one, otherwise through `local`.
    ///
    /// # Errors
    ///
    /// [`OffloadError`] when the action is not offloadable, an input cannot
    /// be bound, or the action itself fails.
    pub async fn invoke(
        &self,
        job: OffloadJob<'_>,
        local: Arc<LocalTransform>,
    ) -> Result<OffloadRun, OffloadError> {
        let started = Instant::now();
        let offload = job
            .action
            .contract
            .offload
            .clone()
            .ok_or_else(|| OffloadError::NotOffloadable("no offload declared".into()))?;
        let invalid = job.action.contract.validate_declaration();
        if !invalid.is_empty() {
            return Err(OffloadError::NotOffloadable(invalid.join("; ")));
        }
        let mut bound = BoundInputs {
            digests: Vec::with_capacity(job.inputs.len()),
            distinct: Vec::with_capacity(job.inputs.len()),
            bytes: 0,
        };
        for (index, manifest) in job.inputs.iter().enumerate() {
            let (digest, len) = self
                .content
                .bind_input(*manifest)
                .await
                .map_err(|error| OffloadError::Input { index, error })?;
            bound.digests.push(digest);
            if !bound.distinct.iter().any(|(listed, _)| *listed == digest) {
                bound.distinct.push((digest, len));
            }
            bound.bytes = bound.bytes.saturating_add(len);
        }
        match self
            .remote_run(job.workspace_id, &offload, &bound, started)
            .await
        {
            Ok(run) => Ok(run),
            Err(reason) => {
                tracing::info!(
                    app = %offload.app,
                    entrypoint = %offload.entrypoint,
                    %reason,
                    "offloadable action runs locally"
                );
                // The local run alone: time lost to a worker that failed is
                // not what running here costs. A worker stages each distinct
                // input once, so the batch here is of distinct inputs too.
                let local_started = Instant::now();
                let distinct: Vec<[u8; 32]> =
                    bound.distinct.iter().map(|(digest, _)| *digest).collect();
                let outputs = self.local(&distinct, local).await?;
                self.record_local(&offload, local_started.elapsed(), bound.bytes);
                Ok(OffloadRun {
                    output: content_output(&outputs),
                    outputs,
                    ran_on: RanOn::Local { reason },
                    elapsed: started.elapsed(),
                })
            }
        }
    }

    /// Run `offload` over `inputs` — content this device holds, by the
    /// digest of its bytes, as the invoker's room reaches it — on the
    /// best-placed other device, or say why it should run here. The caller
    /// runs it here itself then, through the app's own handler, and reports
    /// how long that took ([`Self::record_local`]).
    ///
    /// # Errors
    ///
    /// Why it was not offloaded: an input not wholly on this device, no
    /// better-placed device, or what failed remotely.
    pub async fn offload(
        &self,
        workspace_id: u64,
        offload: &Offload,
        inputs: &[[u8; 32]],
    ) -> Result<OffloadRun, String> {
        let started = Instant::now();
        let invalid = offload.validate();
        if !invalid.is_empty() {
            return Err(format!(
                "the action is not offloadable: {}",
                invalid.join("; ")
            ));
        }
        let mut bound = BoundInputs {
            digests: Vec::with_capacity(inputs.len()),
            distinct: Vec::with_capacity(inputs.len()),
            bytes: 0,
        };
        for (index, digest) in inputs.iter().enumerate() {
            if bound.digests.contains(digest) {
                continue;
            }
            let len = self
                .content
                .bind_blob(*digest)
                .await
                .map_err(|error| format!("input {index}: {error}"))?;
            bound.digests.push(*digest);
            bound.distinct.push((*digest, len));
            bound.bytes = bound.bytes.saturating_add(len);
        }
        self.remote_run(workspace_id, offload, &bound, started)
            .await
    }

    /// The bytes of `inputs` [`Self::offload`] would bind, once each.
    ///
    /// # Errors
    ///
    /// Names an input that is not wholly on this device.
    pub async fn input_bytes(&self, inputs: &[[u8; 32]]) -> Result<u64, String> {
        let mut seen = Vec::with_capacity(inputs.len());
        let mut total = 0_u64;
        for (index, digest) in inputs.iter().enumerate() {
            if seen.contains(digest) {
                continue;
            }
            seen.push(*digest);
            let len = self
                .content
                .blob_len(*digest)
                .await
                .map_err(|error| format!("input {index}: {error}"))?;
            total = total.saturating_add(len);
        }
        Ok(total)
    }

    /// `offload` ran here in `elapsed` over `input_bytes` of input: what
    /// placement weighs moving its inputs against next time.
    pub fn record_local(&self, offload: &Offload, elapsed: Duration, input_bytes: u64) {
        self.record(offload, self.device, elapsed, input_bytes);
    }

    fn record(&self, offload: &Offload, device: DeviceKey, elapsed: Duration, input_bytes: u64) {
        let action = format!("{}:{}", offload.app, offload.entrypoint);
        self.wall_times
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                (action, device),
                Observed {
                    elapsed,
                    input_bytes,
                },
            );
    }

    /// [`Self::remote`] over bound `inputs`, recorded and answered as an
    /// [`OffloadRun`] timed from `started`.
    async fn remote_run(
        &self,
        workspace_id: u64,
        offload: &Offload,
        inputs: &BoundInputs,
        started: Instant,
    ) -> Result<OffloadRun, String> {
        let remote_started = Instant::now();
        let (worker, outputs) = self.remote(workspace_id, offload, inputs).await?;
        self.record(offload, worker, remote_started.elapsed(), inputs.bytes);
        Ok(OffloadRun {
            output: content_output(&outputs),
            outputs,
            ran_on: RanOn::Worker(worker),
            elapsed: started.elapsed(),
        })
    }

    /// The bytes of `digest`, an input bound or an output kept here.
    ///
    /// # Errors
    ///
    /// A diagnostic when the content can no longer be read as those bytes.
    pub async fn content(&self, digest: [u8; 32]) -> Result<Option<Vec<u8>>, String> {
        self.content.read(digest).await
    }

    /// Run the transform here over the bound `inputs` and keep its outputs.
    async fn local(
        &self,
        inputs: &[[u8; 32]],
        local: Arc<LocalTransform>,
    ) -> Result<Vec<[u8; 32]>, OffloadError> {
        let mut bytes = Vec::with_capacity(inputs.len());
        for (index, digest) in inputs.iter().enumerate() {
            let input = self
                .content
                .read(*digest)
                .await
                .map_err(|_| OffloadError::Input {
                    index,
                    error: BindError::Integrity,
                })?
                .ok_or(OffloadError::Input {
                    index,
                    error: BindError::UnknownManifest,
                })?;
            bytes.push(input);
        }
        let produced = tokio::task::spawn_blocking(move || local(&bytes))
            .await
            .map_err(|_| OffloadError::Action("the local run panicked".into()))?
            .map_err(OffloadError::Action)?;
        let mut outputs = Vec::with_capacity(produced.len());
        for output in produced {
            let digest = *blake3::hash(&output).as_bytes();
            self.content
                .store(digest, &output)
                .await
                .map_err(OffloadError::Store)?;
            outputs.push(digest);
        }
        Ok(outputs)
    }

    /// How long this device last took for `action`, scaled to
    /// `input_bytes`; `None` before it has run it here.
    fn local_wall_ms(&self, action: &str, input_bytes: u64) -> Option<u64> {
        self.wall_times
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(action.to_owned(), self.device))
            .map(|observed| observed.scaled_ms(input_bytes))
    }

    /// Run the job over `inputs` on the best-placed other device, or say
    /// why not.
    async fn remote(
        &self,
        workspace_id: u64,
        offload: &Offload,
        inputs: &BoundInputs,
    ) -> Result<(DeviceKey, Vec<[u8; 32]>), String> {
        let action = format!("{}:{}", offload.app, offload.entrypoint);
        let placed = self
            .placement
            .place(PlacementQuery {
                workspace_id,
                app: &offload.app,
                gpu: offload.gpu,
                // A worker stages each distinct input once.
                input_bytes: inputs
                    .distinct
                    .iter()
                    .fold(0_u64, |sum, (_, bytes)| sum.saturating_add(*bytes)),
                inputs: &inputs.distinct,
                local_wall_ms: self.local_wall_ms(&action, inputs.bytes),
            })
            .await
            .map_err(|error| format!("placement unavailable: {error}"))?;
        let worker = placed
            .worker
            .ok_or_else(|| format!("no other device is better placed: {}", placed.reason))?;
        if worker == self.device {
            return Err("this device is the best placed".into());
        }
        let package_digest = self.package_digest(&offload.app)?;
        let spec = job_spec(
            self.device,
            worker,
            (workspace_id, package_digest),
            offload,
            &inputs.digests,
        )?;
        let binding = ComputeJobBinding {
            workspace_id,
            job_id: spec.job_id,
            requester: spec.requester,
            worker,
        };
        let deadline = Duration::from_millis(spec.expires_at_ms.saturating_sub(unix_now_ms()))
            .saturating_add(RESULT_GRACE);
        let (result, answer) = oneshot::channel();
        self.commands
            .send(Command::Submit {
                spec: Box::new(spec),
                result,
            })
            .await
            .map_err(|_| "the compute session is closed".to_string())?;
        let result = match tokio::time::timeout(deadline, answer).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => return Err("the compute session closed before the result".into()),
            Err(_) => {
                let _ = self.commands.send(Command::Cancel(binding)).await;
                return Err("the worker did not answer within the job's lifetime".into());
            }
        };
        let outputs = match result.phase {
            ComputeJobPhase::Completed { outputs } => outputs,
            ComputeJobPhase::Failed { code } => {
                return Err(format!(
                    "the job failed remotely ({code}): {}",
                    String::from_utf8_lossy(&result.log_tail).trim()
                ));
            }
            other => return Err(format!("the job ended {other:?}")),
        };
        let mut remaining = offload.output_bytes;
        for digest in &outputs {
            let bytes = fetch_job_content(
                &self.connection,
                workspace_id,
                binding.job_id,
                *digest,
                remaining,
            )
            .await
            .map_err(|error| format!("output {}: {error}", hex::encode(digest)))?;
            remaining = remaining.saturating_sub(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            self.content.store(*digest, &bytes).await?;
        }
        Ok((worker, outputs))
    }

    /// The signed root of `app` as this device has it installed: the exact
    /// package a worker must run.
    fn package_digest(&self, app: &str) -> Result<[u8; 32], String> {
        let package = self
            .packages
            .resolve(app)
            .map_err(|error| format!("package catalogue: {error}"))?
            .ok_or_else(|| format!("{app} is not installed here"))?;
        let mut digest = [0; 32];
        hex::decode_to_slice(&package.package_id_hex, &mut digest)
            .map_err(|_| format!("{app} has a malformed package id"))?;
        Ok(digest)
    }
}

/// The action output of `outputs`: each a content resource, in order.
fn content_output(outputs: &[[u8; 32]]) -> ActionOutput {
    ActionOutput {
        value: None,
        resources: outputs
            .iter()
            .filter_map(|digest| tilo_actions::content_resource(&hex::encode(digest)))
            .collect(),
    }
}

/// A job binding `offload` over `inputs` from `requester` to `worker`.
fn job_spec(
    requester: DeviceKey,
    worker: DeviceKey,
    (workspace_id, package_digest): (u64, [u8; 32]),
    offload: &Offload,
    inputs: &[[u8; 32]],
) -> Result<ComputeJobSpec, String> {
    let mut job_id = [0; 16];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut job_id);
    let issued_at_ms = unix_now_ms();
    // Room to fetch the inputs and run the declared CPU time, never past a
    // job's ceiling.
    let lifetime = offload
        .cpu_time_ms
        .saturating_mul(2)
        .saturating_add(30_000)
        .min(MAX_COMPUTE_JOB_LIFETIME_MS);
    let spec = ComputeJobSpec {
        version: COMPUTE_JOB_SCHEMA_VERSION,
        job_id,
        requester,
        worker,
        workspace_id,
        app_id: offload.app.clone(),
        package_digest,
        entrypoint: offload.entrypoint.clone(),
        arguments: Vec::new(),
        inputs: inputs.to_vec(),
        limits: ComputeResourceLimits {
            cpu_time_ms: offload.cpu_time_ms,
            memory_bytes: offload.memory_bytes,
            output_bytes: offload.output_bytes,
            gpu: offload.gpu,
        },
        issued_at_ms,
        expires_at_ms: issued_at_ms.saturating_add(lifetime),
    };
    spec.validate_for(worker, issued_at_ms)
        .map_err(|error| format!("the job cannot be bound: {error}"))?;
    Ok(spec)
}

/// Drive the compute session: submit and cancel what is asked, and hand
/// each job's result to whoever waits for it.
async fn drive(mut compute: ComputeClient, mut commands: mpsc::Receiver<Command>) {
    let mut waiting: HashMap<[u8; 16], oneshot::Sender<ComputeResultUpdate>> = HashMap::new();
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None => break,
                Some(Command::Submit { spec, result }) => {
                    if let Err(error) = compute.submit(&spec).await {
                        tracing::warn!(%error, "submitting an offloaded job failed");
                        break;
                    }
                    waiting.insert(spec.job_id, result);
                }
                Some(Command::Cancel(binding)) => {
                    waiting.remove(&binding.job_id);
                    if let Err(error) = compute.cancel(&binding).await {
                        tracing::warn!(%error, "cancelling an offloaded job failed");
                        break;
                    }
                }
            },
            delivery = compute.receive() => {
                let delivery = match delivery {
                    Ok(delivery) => delivery,
                    Err(error) => {
                        tracing::info!(%error, "the offload compute session ended");
                        break;
                    }
                };
                if let Err(error) = compute.acknowledge(&delivery).await {
                    tracing::info!(%error, "the offload compute session ended");
                    break;
                }
                if let ComputeEnvelope::Result { result } = delivery.envelope
                    && let Some(waiter) = waiting.remove(&result.binding.job_id)
                {
                    let _ = waiter.send(result);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offload() -> Offload {
        Offload {
            app: "so.tilo.thumbnails".into(),
            entrypoint: "thumbnail-batch".into(),
            gpu: false,
            cpu_time_ms: 60_000,
            memory_bytes: 512 * 1024 * 1024,
            output_bytes: 8 * 1024 * 1024,
        }
    }

    #[test]
    fn an_offloaded_job_carries_the_declaration_and_nothing_else() {
        let spec = job_spec([1; 32], [2; 32], (7, [3; 32]), &offload(), &[[4; 32]]).unwrap();
        assert_eq!(spec.app_id, "so.tilo.thumbnails");
        assert_eq!(spec.entrypoint, "thumbnail-batch");
        assert!(spec.arguments.is_empty(), "no requester argv, ever");
        assert_eq!(spec.inputs, vec![[4; 32]]);
        assert_eq!(spec.limits.cpu_time_ms, 60_000);
        assert!(
            spec.expires_at_ms.saturating_sub(spec.issued_at_ms) <= MAX_COMPUTE_JOB_LIFETIME_MS
        );
        assert_ne!(spec.job_id, [0; 16]);
        // A job never targets its own requester.
        assert!(job_spec([1; 32], [1; 32], (7, [3; 32]), &offload(), &[]).is_err());
    }
}
