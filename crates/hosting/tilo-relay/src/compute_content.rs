//! Job-scoped content for personal-mesh compute.
//!
//! A job names its inputs by blake3 digest ([`ComputeJobSpec::inputs`]) and
//! its result names its outputs the same way. The bytes never ride in the
//! durable [`ComputeEnvelope`](crate::ComputeEnvelope)s: each digest moves
//! on a stream of its own on the compute connection
//! ([`StreamOpen::ComputeContent`]), and only as far as the job binds it.
//!
//! - **Inputs.** The job's worker asks for a digest the job lists as an
//!   input, while the job is not terminal. The relay asks the job's
//!   *requester* for it ([`WorkerStreamOpen::ComputeContent`]), which
//!   answers from its own workspace content. The worker therefore reads
//!   exactly what the invoker itself can produce: being a worker grants no
//!   read of its own, and the package sandbox only ever sees the staged
//!   bytes.
//! - **Outputs.** The job's requester asks for a digest the completed job
//!   reported. The relay asks the worker, which serves it from the outputs
//!   it retains for that job's requester
//!   ([`ComputeOutputSource`](crate::compute::ComputeOutputSource)).
//!
//! - **Inputs the worker already holds.** Before fetching an input it may
//!   already have a copy of, the worker challenges the requester through
//!   the relay ([`StreamOpen::ComputeContentProof`], under the same input
//!   rule) to key a hash of the bytes with a fresh nonce
//!   ([`prove_job_input`]). A requester that holds the bytes answers
//!   ([`answer_proof`]); the worker uses its own copy only when that copy
//!   hashes to the digest *and* the proof matches it. Knowing a digest is
//!   never authority: what the job runs on is still exactly what its
//!   invoker could produce, it just does not travel.
//!
//! The relay notes which inputs it forwarded and which the requester
//! proved, per live job. When the job completes, an input proven and not
//! forwarded is one the worker held: placement then weighs only what a
//! worker would receive
//! ([`ComputeTransportState::place_weighed`]). That is a routing hint and
//! grants nothing; the next job proves or fetches like any other.
//!
//! Anything else is refused before the other end is asked. Each hop checks
//! the bytes against the digest and its bound: the source before sending,
//! the relay while forwarding, the asker on receipt.

use std::time::Duration;

use iroh::endpoint::{Connection, SendStream};
use thiserror::Error;
use tilo_relay_protocol::environment::{
    ContentProof, ContentReply, ENVIRONMENT_PROTOCOL_VERSION, MemberIdentity, StreamOpen,
    WorkerStreamOpen, bounded_detail,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::auth::AuthenticatedPeer;
use crate::compute::{ComputeJobPhase, ComputeJobSpec, MAX_COMPUTE_INPUT_BYTES};
use crate::compute_transport::{ComputeTransportState, JobScope};
use crate::environment_host::{EnvironmentStreamError, read_message, write_message};
use crate::environment_routing::RoutingContext;
use crate::host_authority::HostOperation;
use crate::mesh::DeviceKey;
use crate::room_access::{self, RoomOperation};

/// Bytes copied per read while forwarding or receiving content.
const CONTENT_CHUNK_BYTES: usize = 64 * 1024;
/// How long the relay waits for the device holding the bytes to answer.
const SOURCE_ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// Why job content could not be fetched.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContentFetchError {
    /// The relay or the device holding the bytes refused; the code says why.
    #[error("compute content refused: {0}")]
    Refused(String),
    /// The bytes exceed the bound the caller allowed.
    #[error("compute content exceeds its bound")]
    TooLarge,
    /// The bytes do not match the digest asked for.
    #[error("compute content does not match its digest")]
    Integrity,
    /// The stream failed before the bytes arrived.
    #[error("compute content stream: {0}")]
    Stream(String),
    /// The bytes arrived but could not be kept.
    #[error("compute content store: {0}")]
    Store(String),
}

impl ContentFetchError {
    /// The stable code for this failure.
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Refused(code) => code,
            Self::TooLarge => "too_large",
            Self::Integrity => "integrity",
            Self::Stream(_) => "stream",
            Self::Store(_) => "store",
        }
    }
}

impl From<EnvironmentStreamError> for ContentFetchError {
    fn from(error: EnvironmentStreamError) -> Self {
        Self::Stream(error.to_string())
    }
}

/// Fetch the bytes of `digest`, which job `job_id` in `workspace_id` binds
/// to this device, over its authenticated compute `connection` to the
/// relay: an input when this device is the job's worker, an output when it
/// is the job's requester.
///
/// # Errors
///
/// [`ContentFetchError::Refused`] with the relay's or the source's code,
/// [`ContentFetchError::TooLarge`] past `max_bytes`,
/// [`ContentFetchError::Integrity`] when the bytes do not hash to `digest`,
/// or [`ContentFetchError::Stream`] when the stream fails.
pub async fn fetch_job_content(
    connection: &Connection,
    workspace_id: u64,
    job_id: [u8; 16],
    digest: [u8; 32],
    max_bytes: u64,
) -> Result<Vec<u8>, ContentFetchError> {
    let (mut send, mut receive) = connection
        .open_bi()
        .await
        .map_err(|error| ContentFetchError::Stream(error.to_string()))?;
    write_message(
        &mut send,
        &StreamOpen::ComputeContent {
            version: ENVIRONMENT_PROTOCOL_VERSION,
            workspace_id,
            job_id,
            digest,
        },
    )
    .await?;
    let _ = send.finish();
    receive_content(&mut receive, digest, max_bytes).await
}

/// Read one [`ContentReply`] and the bytes it announces, checked against
/// `digest` and `max_bytes`.
async fn receive_content<R>(
    receive: &mut R,
    digest: [u8; 32],
    max_bytes: u64,
) -> Result<Vec<u8>, ContentFetchError>
where
    R: AsyncRead + Unpin + Send,
{
    let len = match read_message::<_, ContentReply>(receive).await? {
        Some(ContentReply::Found { len }) => len,
        Some(ContentReply::Refused { code }) => return Err(ContentFetchError::Refused(code)),
        None => return Err(ContentFetchError::Stream("stream ended unanswered".into())),
    };
    if len > max_bytes {
        return Err(ContentFetchError::TooLarge);
    }
    let expected = usize::try_from(len).map_err(|_| ContentFetchError::TooLarge)?;
    let mut bytes = Vec::with_capacity(expected.min(CONTENT_CHUNK_BYTES));
    (&mut *receive)
        .take(len)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| ContentFetchError::Stream(error.to_string()))?;
    if bytes.len() < expected {
        return Err(ContentFetchError::Stream("content ended early".into()));
    }
    if blake3::hash(&bytes).as_bytes() == &digest {
        Ok(bytes)
    } else {
        Err(ContentFetchError::Integrity)
    }
}

/// The proof that whoever computed it holds `bytes`, under challenge
/// `nonce`.
#[must_use]
pub fn content_proof(nonce: &[u8; 32], bytes: &[u8]) -> [u8; 32] {
    *blake3::keyed_hash(nonce, bytes).as_bytes()
}

/// Ask, over this worker's authenticated compute `connection`, that the
/// requester of job `job_id` in `workspace_id` prove it holds input
/// `digest` under challenge `nonce` ([`StreamOpen::ComputeContentProof`]).
/// Returns the proof and the requester's locator hint; the caller checks
/// the proof against its own copy ([`content_proof`]).
///
/// # Errors
///
/// [`ContentFetchError::Refused`] with the relay's or the requester's code,
/// or [`ContentFetchError::Stream`] when the stream fails.
pub async fn prove_job_input(
    connection: &Connection,
    (workspace_id, job_id): (u64, [u8; 16]),
    digest: [u8; 32],
    nonce: [u8; 32],
) -> Result<([u8; 32], Option<[u8; 32]>), ContentFetchError> {
    let (mut send, mut receive) = connection
        .open_bi()
        .await
        .map_err(|error| ContentFetchError::Stream(error.to_string()))?;
    write_message(
        &mut send,
        &StreamOpen::ComputeContentProof {
            version: ENVIRONMENT_PROTOCOL_VERSION,
            workspace_id,
            job_id,
            digest,
            nonce,
        },
    )
    .await?;
    let _ = send.finish();
    match read_message::<_, ContentProof>(&mut receive).await? {
        Some(ContentProof::Proven { proof, locator }) => Ok((proof, locator)),
        Some(ContentProof::Refused { code }) => Err(ContentFetchError::Refused(code)),
        None => Err(ContentFetchError::Stream("stream ended unanswered".into())),
    }
}

/// Answer a [`WorkerStreamOpen::ComputeContentProof`] from what the local
/// store returned for `digest`: the keyed hash of the bytes under `nonce`
/// when they match `digest`, otherwise a refusal coded as
/// [`answer_content`] codes it. `locator` is passed on as the hint.
pub async fn answer_proof<W>(
    send: &mut W,
    (digest, nonce): ([u8; 32], [u8; 32]),
    found: Result<Option<Vec<u8>>, String>,
    locator: Option<[u8; 32]>,
) -> Result<(), EnvironmentStreamError>
where
    W: AsyncWrite + Unpin + Send,
{
    let code = match found {
        Ok(Some(bytes)) if blake3::hash(&bytes).as_bytes() == &digest => {
            let proof = content_proof(&nonce, &bytes);
            return write_message(send, &ContentProof::Proven { proof, locator }).await;
        }
        Ok(Some(_)) => "integrity",
        Ok(None) => "missing",
        Err(error) => {
            tracing::warn!(%error, "compute content could not be read for a proof");
            "unreadable"
        }
    };
    refuse_proof(send, code).await
}

pub(crate) async fn refuse_proof<W>(send: &mut W, code: &str) -> Result<(), EnvironmentStreamError>
where
    W: AsyncWrite + Unpin + Send,
{
    write_message(send, &ContentProof::Refused { code: code.into() }).await
}

/// Answer a [`WorkerStreamOpen::ComputeContent`] from what the local store
/// returned for it: the bytes when they match `digest`, otherwise a refusal
/// (`missing` when the store has nothing, `unreadable` when it failed).
pub async fn answer_content<W>(
    send: &mut W,
    digest: [u8; 32],
    found: Result<Option<Vec<u8>>, String>,
) -> Result<(), EnvironmentStreamError>
where
    W: AsyncWrite + Unpin + Send,
{
    let bytes = match found {
        Ok(Some(bytes)) if blake3::hash(&bytes).as_bytes() == &digest => bytes,
        Ok(Some(_)) => return refuse(send, "integrity").await,
        Ok(None) => return refuse(send, "missing").await,
        Err(error) => {
            tracing::warn!(%error, "compute content could not be read");
            return refuse(send, "unreadable").await;
        }
    };
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    write_message(send, &ContentReply::Found { len }).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

pub(crate) async fn refuse<W>(send: &mut W, code: &str) -> Result<(), EnvironmentStreamError>
where
    W: AsyncWrite + Unpin + Send,
{
    write_message(send, &ContentReply::Refused { code: code.into() }).await
}

/// Which end of a job holds a digest the other end asked for.
struct ContentRoute {
    source: DeviceKey,
    connection: Connection,
    max_bytes: u64,
    /// The job, and the requester and worker the bytes move between.
    scope: JobScope,
}

/// The relay's side of [`StreamOpen::ComputeContent`]: decide from the
/// job's durable record whether `peer` may have `digest`, ask the end that
/// holds it, and forward its answer, checked against the digest.
pub(crate) async fn route_content(
    context: &RoutingContext,
    peer: &AuthenticatedPeer,
    version: u16,
    (workspace_id, job_id, digest): (u64, [u8; 16], [u8; 32]),
    send: &mut SendStream,
) -> Result<(), EnvironmentStreamError> {
    if version != ENVIRONMENT_PROTOCOL_VERSION {
        return refuse(send, "unsupported_version").await;
    }
    let route = match choose_source(context, peer, (workspace_id, job_id, digest), source_for).await
    {
        Ok(route) => route,
        Err(code) => return refuse(send, code).await,
    };
    let Ok((mut source_send, mut source_receive)) = route.connection.open_bi().await else {
        return refuse(send, "source_offline").await;
    };
    write_message(
        &mut source_send,
        &WorkerStreamOpen::ComputeContent {
            version: ENVIRONMENT_PROTOCOL_VERSION,
            workspace_id,
            asker: MemberIdentity {
                user: peer.user_public_key,
                device: peer.device_public_key,
            },
            job_id,
            digest,
        },
    )
    .await?;
    let _ = source_send.finish();
    let answer = tokio::time::timeout(
        SOURCE_ANSWER_TIMEOUT,
        read_message::<_, ContentReply>(&mut source_receive),
    )
    .await;
    let len = match answer {
        Ok(Ok(Some(ContentReply::Found { len }))) => len,
        Ok(Ok(Some(ContentReply::Refused { code }))) => {
            return refuse(send, &bounded_detail(code)).await;
        }
        Ok(Ok(None) | Err(_)) => return refuse(send, "source_failed").await,
        Err(_) => return refuse(send, "source_unresponsive").await,
    };
    if len > route.max_bytes {
        let _ = source_receive.stop(0u32.into());
        return refuse(send, "too_large").await;
    }
    // Inputs are what placement weighs. One the relay forwards is one the
    // worker did not hold: noted before the first byte moves, so the job
    // cannot complete ahead of it.
    let input = route.source == route.scope.pair.0;
    if input {
        observe_input(context, workspace_id, route.scope, |compute, now_ms| {
            compute.observe_forwarded(route.scope, digest, now_ms);
        })
        .await;
    }
    write_message(send, &ContentReply::Found { len }).await?;
    let started = std::time::Instant::now();
    let forwarded = forward(&mut source_receive, send, len).await?;
    if forwarded.as_bytes() == &digest {
        let elapsed = started.elapsed();
        tracing::debug!(
            source = %hex::encode(route.source),
            job = %hex::encode(job_id),
            bytes = len,
            elapsed_ms = elapsed.as_millis(),
            "compute content forwarded"
        );
        // Only inputs sample the rate, and only transfers large enough to
        // say something about it.
        if input && len >= crate::compute_transport::MIN_THROUGHPUT_SAMPLE_BYTES {
            observe_input(context, workspace_id, route.scope, |compute, _| {
                compute.observe_transfer(route.scope.pair, len, elapsed);
            })
            .await;
        }
    } else {
        // The asker checks too; ending the stream abnormally tells it now.
        let _ = send.reset(1u32.into());
        tracing::warn!(
            source = %hex::encode(route.source),
            job = %hex::encode(job_id),
            "compute content did not match its digest; not forwarded"
        );
    }
    Ok(())
}

/// The relay's side of [`StreamOpen::ComputeContentProof`]: under the input
/// rule ([`proof_source_for`]), ask the job's requester to prove it holds
/// `digest` under the worker's `nonce`, and forward its answer. The relay
/// cannot check the proof (it never sees the bytes); the worker does.
pub(crate) async fn route_proof(
    context: &RoutingContext,
    peer: &AuthenticatedPeer,
    version: u16,
    (workspace_id, job_id, digest, nonce): (u64, [u8; 16], [u8; 32], [u8; 32]),
    send: &mut SendStream,
) -> Result<(), EnvironmentStreamError> {
    if version != ENVIRONMENT_PROTOCOL_VERSION {
        return refuse_proof(send, "unsupported_version").await;
    }
    let route = match choose_source(
        context,
        peer,
        (workspace_id, job_id, digest),
        proof_source_for,
    )
    .await
    {
        Ok(route) => route,
        Err(code) => return refuse_proof(send, code).await,
    };
    let Ok((mut source_send, mut source_receive)) = route.connection.open_bi().await else {
        return refuse_proof(send, "source_offline").await;
    };
    write_message(
        &mut source_send,
        &WorkerStreamOpen::ComputeContentProof {
            version: ENVIRONMENT_PROTOCOL_VERSION,
            workspace_id,
            asker: MemberIdentity {
                user: peer.user_public_key,
                device: peer.device_public_key,
            },
            job_id,
            digest,
            nonce,
        },
    )
    .await?;
    let _ = source_send.finish();
    let answer = tokio::time::timeout(
        SOURCE_ANSWER_TIMEOUT,
        read_message::<_, ContentProof>(&mut source_receive),
    )
    .await;
    match answer {
        Ok(Ok(Some(proven @ ContentProof::Proven { .. }))) => {
            // Whether the worker's copy matched shows when the job
            // completes without the relay forwarding this input.
            observe_input(context, workspace_id, route.scope, |compute, now_ms| {
                compute.observe_proof(route.scope, digest, now_ms);
            })
            .await;
            write_message(send, &proven).await
        }
        Ok(Ok(Some(ContentProof::Refused { code }))) => {
            refuse_proof(send, &bounded_detail(code)).await
        }
        Ok(Ok(None) | Err(_)) => refuse_proof(send, "source_failed").await,
        Err(_) => refuse_proof(send, "source_unresponsive").await,
    }
}

/// Copy exactly `len` bytes from `from` to `to`, hashing them.
async fn forward<R, W>(
    from: &mut R,
    to: &mut W,
    len: u64,
) -> Result<blake3::Hash, EnvironmentStreamError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let mut hasher = blake3::Hasher::new();
    let mut chunk = vec![0; CONTENT_CHUNK_BYTES];
    let mut limited = (&mut *from).take(len);
    let mut copied = 0_u64;
    loop {
        let read = limited.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let piece = chunk.get(..read).unwrap_or_default();
        hasher.update(piece);
        to.write_all(piece).await?;
        copied = copied.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
    }
    if copied < len {
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
    }
    Ok(hasher.finalize())
}

/// A job's rule for who may have a digest: the device holding it and its
/// bound, or the code refusing it.
type JobRule = fn(
    &ComputeJobSpec,
    &ComputeJobPhase,
    DeviceKey,
    [u8; 32],
) -> Result<(DeviceKey, u64), &'static str>;

/// Decide under `rule` who holds `digest` for `peer`, or the code refusing
/// it.
async fn choose_source(
    context: &RoutingContext,
    peer: &AuthenticatedPeer,
    (workspace_id, job_id, digest): (u64, [u8; 16], [u8; 32]),
    rule: JobRule,
) -> Result<ContentRoute, &'static str> {
    // Job content is compute control: the host's own person's devices, and
    // never an agent of theirs.
    if !HostOperation::ComputeControl.permits(&context.own_user_public_key, &peer.user_public_key)
        || context.trust().is_agent(&peer.device_public_key)
    {
        return Err("not_authorised");
    }
    let Some(workspace) = context.workspace(workspace_id).await else {
        return Err("not_authorised");
    };
    let (source, max_bytes, connection, scope) = job_source(
        &*workspace.lock().await,
        &context.trust(),
        peer,
        (job_id, digest),
        rule,
    )?;
    Ok(ContentRoute {
        source,
        connection: connection.ok_or("source_offline")?,
        max_bytes,
        scope,
    })
}

/// Record what the relay carried for one input of the job `scope` names
/// (`observe`, given the time): what placement weighs inputs by.
async fn observe_input(
    context: &RoutingContext,
    workspace_id: u64,
    scope: JobScope,
    observe: impl FnOnce(&mut ComputeTransportState, u64) + Send,
) {
    let Some(workspace) = context.workspace(workspace_id).await else {
        return;
    };
    let now_ms = tilo_clock::unix_now_ms();
    if scope.expires_at_ms > now_ms {
        observe(&mut workspace.lock().await.compute, now_ms);
    }
}

/// The end holding the bytes, their bound, its live connection, and the
/// job they move for.
type JobSource = (DeviceKey, u64, Option<Connection>, JobScope);

/// [`choose_source`] under the workspace lock: the peer's admission, the
/// job's record, and the live connection of the end holding the bytes.
fn job_source(
    workspace: &crate::workspace::WorkspaceRuntime,
    trust: &room_access::RelayTrust<'_>,
    peer: &AuthenticatedPeer,
    (job_id, digest): ([u8; 16], [u8; 32]),
    rule: JobRule,
) -> Result<JobSource, &'static str> {
    if !room_access::decide(workspace, trust, peer, RoomOperation::Compute).admitted() {
        return Err("not_authorised");
    }
    let record = workspace
        .store
        .load_compute_job(&job_id)
        .map_err(|_| "unavailable")?
        .ok_or("not_authorised")?;
    let (source, max_bytes) = rule(
        &record.spec,
        &record.lifecycle.phase,
        peer.device_public_key,
        digest,
    )?;
    Ok((
        source,
        max_bytes,
        workspace.compute.worker_connection(&source),
        JobScope {
            job_id,
            pair: (record.spec.requester, record.spec.worker),
            expires_at_ms: record.spec.expires_at_ms,
        },
    ))
}

/// The job's rule: the worker may have a listed input while the job runs,
/// the requester a reported output once it completed. Returns the device
/// holding the bytes and their bound.
fn source_for(
    spec: &ComputeJobSpec,
    phase: &ComputeJobPhase,
    asker: DeviceKey,
    digest: [u8; 32],
) -> Result<(DeviceKey, u64), &'static str> {
    let running = matches!(phase, ComputeJobPhase::Pending | ComputeJobPhase::Running);
    if asker == spec.worker && running && spec.inputs.contains(&digest) {
        return Ok((spec.requester, MAX_COMPUTE_INPUT_BYTES));
    }
    if asker == spec.requester
        && matches!(phase, ComputeJobPhase::Completed { outputs } if outputs.contains(&digest))
    {
        return Ok((spec.worker, spec.limits.output_bytes));
    }
    Err("not_authorised")
}

/// The input half of [`source_for`]: only the job's worker may ask its
/// requester to prove it holds a listed input, and only while the job runs.
fn proof_source_for(
    spec: &ComputeJobSpec,
    phase: &ComputeJobPhase,
    asker: DeviceKey,
    digest: [u8; 32],
) -> Result<(DeviceKey, u64), &'static str> {
    match source_for(spec, phase, asker, digest)? {
        (source, bound) if source == spec.requester && asker == spec.worker => Ok((source, bound)),
        _ => Err("not_authorised"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute::{COMPUTE_JOB_SCHEMA_VERSION, ComputeResourceLimits};

    fn spec() -> ComputeJobSpec {
        ComputeJobSpec {
            version: COMPUTE_JOB_SCHEMA_VERSION,
            job_id: [9; 16],
            requester: [1; 32],
            worker: [2; 32],
            workspace_id: 7,
            app_id: "org.tilo.render".into(),
            package_digest: [3; 32],
            entrypoint: "render".into(),
            arguments: Vec::new(),
            inputs: vec![[4; 32]],
            limits: ComputeResourceLimits {
                cpu_time_ms: 5_000,
                memory_bytes: 32 * 1024 * 1024,
                output_bytes: 1024,
                gpu: false,
            },
            issued_at_ms: 100,
            expires_at_ms: 10_000,
        }
    }

    #[test]
    fn job_content_goes_only_where_the_job_binds_it() {
        let job = spec();
        let worker = job.worker;
        let requester = job.requester;
        let stranger = [5; 32];
        let done = ComputeJobPhase::Completed {
            outputs: vec![[6; 32]],
        };

        // A listed input, to the worker while the job runs, from the
        // requester; never once it ended, never an unlisted digest.
        assert_eq!(
            source_for(&job, &ComputeJobPhase::Running, worker, [4; 32]),
            Ok((job.requester, MAX_COMPUTE_INPUT_BYTES))
        );
        assert_eq!(
            source_for(&job, &done, worker, [4; 32]),
            Err("not_authorised")
        );
        assert_eq!(
            source_for(&job, &ComputeJobPhase::Running, worker, [8; 32]),
            Err("not_authorised")
        );
        // The requester cannot pull its own input back through the worker,
        // nor an output before the job reported it.
        assert_eq!(
            source_for(&job, &ComputeJobPhase::Running, requester, [4; 32]),
            Err("not_authorised")
        );
        assert_eq!(
            source_for(&job, &ComputeJobPhase::Running, requester, [6; 32]),
            Err("not_authorised")
        );
        assert_eq!(
            source_for(&job, &done, requester, [6; 32]),
            Ok((job.worker, 1024))
        );
        assert_eq!(
            source_for(&job, &done, worker, [6; 32]),
            Err("not_authorised")
        );
        for phase in [ComputeJobPhase::Running, done.clone()] {
            for digest in [[4; 32], [6; 32]] {
                assert_eq!(
                    source_for(&job, &phase, stranger, digest),
                    Err("not_authorised")
                );
            }
        }

        // A proof of possession follows the input rule alone: the worker,
        // of a listed input, while the job runs, from the requester. Nobody
        // gets one of an output.
        assert_eq!(
            proof_source_for(&job, &ComputeJobPhase::Running, worker, [4; 32]),
            Ok((job.requester, MAX_COMPUTE_INPUT_BYTES))
        );
        for (phase, asker, digest) in [
            (&done, worker, [4; 32]),
            (&ComputeJobPhase::Running, worker, [8; 32]),
            (&ComputeJobPhase::Running, requester, [4; 32]),
            (&ComputeJobPhase::Running, stranger, [4; 32]),
            (&done, requester, [6; 32]),
        ] {
            assert_eq!(
                proof_source_for(&job, phase, asker, digest),
                Err("not_authorised"),
                "{phase:?} {digest:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_proof_binds_the_bytes_to_the_challenge() {
        let bytes = b"a photograph".to_vec();
        let digest = *blake3::hash(&bytes).as_bytes();
        let nonce = [3; 32];
        let manifest = Some([5; 32]);

        let mut wire = Vec::new();
        answer_proof(
            &mut wire,
            (digest, nonce),
            Ok(Some(bytes.clone())),
            manifest,
        )
        .await
        .unwrap();
        let answer = read_message::<_, ContentProof>(&mut wire.as_slice())
            .await
            .unwrap();
        assert_eq!(
            answer,
            Some(ContentProof::Proven {
                proof: content_proof(&nonce, &bytes),
                locator: manifest,
            })
        );
        // The proof is keyed: neither the digest, another challenge, nor
        // other bytes reproduce it.
        assert_ne!(content_proof(&nonce, &bytes), digest);
        assert_ne!(
            content_proof(&nonce, &bytes),
            content_proof(&[4; 32], &bytes)
        );
        assert_ne!(
            content_proof(&nonce, &bytes),
            content_proof(&nonce, b"other")
        );

        // No proof without the bytes, or of bytes under the wrong digest.
        for (found, code) in [
            (Ok(None), "missing"),
            (Ok(Some(b"other".to_vec())), "integrity"),
            (Err("disk".to_string()), "unreadable"),
        ] {
            let mut wire = Vec::new();
            answer_proof(&mut wire, (digest, nonce), found, manifest)
                .await
                .unwrap();
            assert_eq!(
                read_message::<_, ContentProof>(&mut wire.as_slice())
                    .await
                    .unwrap(),
                Some(ContentProof::Refused { code: code.into() })
            );
        }
    }

    #[tokio::test]
    async fn content_is_checked_against_its_digest_and_bound_on_receipt() {
        let bytes = b"rendered frame".to_vec();
        let digest = *blake3::hash(&bytes).as_bytes();

        let mut wire = Vec::new();
        answer_content(&mut wire, digest, Ok(Some(bytes.clone())))
            .await
            .unwrap();
        assert_eq!(
            receive_content(&mut wire.as_slice(), digest, 1024).await,
            Ok(bytes.clone())
        );
        assert_eq!(
            receive_content(&mut wire.as_slice(), digest, 4).await,
            Err(ContentFetchError::TooLarge)
        );
        assert_eq!(
            receive_content(&mut wire.as_slice(), [0; 32], 1024).await,
            Err(ContentFetchError::Integrity)
        );
        // A truncated body is an error, not a short read.
        let cut = &wire[..wire.len().saturating_sub(1)];
        assert!(matches!(
            receive_content(&mut &cut[..], digest, 1024).await,
            Err(ContentFetchError::Stream(_))
        ));

        // A source never sends bytes under the wrong digest, nor pretends
        // to have what its store lacks.
        let mut wrong = Vec::new();
        answer_content(&mut wrong, [0; 32], Ok(Some(bytes)))
            .await
            .unwrap();
        assert_eq!(
            receive_content(&mut wrong.as_slice(), [0; 32], 1024).await,
            Err(ContentFetchError::Refused("integrity".into()))
        );
        let mut missing = Vec::new();
        answer_content(&mut missing, digest, Ok(None))
            .await
            .unwrap();
        assert_eq!(
            receive_content(&mut missing.as_slice(), digest, 1024).await,
            Err(ContentFetchError::Refused("missing".into()))
        );

        // The relay forwards exactly the announced length and reports the
        // hash of what it forwarded.
        let payload = vec![7_u8; CONTENT_CHUNK_BYTES * 2 + 3];
        let mut forwarded = Vec::new();
        let hash = forward(
            &mut payload.as_slice(),
            &mut forwarded,
            u64::try_from(payload.len()).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(forwarded, payload);
        assert_eq!(hash, blake3::hash(&payload));
    }
}
