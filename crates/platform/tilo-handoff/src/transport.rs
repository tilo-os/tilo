//! Bounded peer transport for the other-computer handoff. The existing
//! `tilo-net-p2p` endpoint carries frames; the package keeps its existing
//! password envelope and only the generated password is sealed to the
//! destination's ephemeral HPKE key.
//!
//! The transport proves the destination's certified device and ephemeral key
//! together, bound to this operation, descriptor, package and person identity.
//! It releases the password only after a person approves that exact request.
//! The package receipt names the exact package digest and an explicit
//! `prepared-package-imported` stage: receiving the package is not a claim that
//! Home or application migration has finished, and nothing here deletes an
//! original.

use super::{
    ALPN, CHUNK, HandoffIdentity, MAX_FRAME, MAX_PACKAGE, PublicHandoff, SourceState, bootstrap,
    digest, handoff_aad, invalid, valid_operation, valid_ticket, verification_code,
};
use ed25519_dalek::{Signer as _, Verifier as _};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{CString, OsStr},
    fs::File,
    io::{self, Read as _},
    os::{
        fd::{AsRawFd as _, FromRawFd as _},
        unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver as Channel, Sender, TryRecvError},
    },
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};
use tilo_net_p2p::{Endpoint, Peer, Sent};
use tilo_seal::{Label, PublicKey, SecretKey, hpke};
use tilo_user_identity::{CertificationRequest, DeviceCert};
use zeroize::Zeroizing;

const OPERATION_WINDOW: Duration = Duration::from_secs(6 * 60 * 60);
const RESEND: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(40);
const QUEUE: usize = 64;
const PACKAGE: &str = "prepared-setup.sealed";
const RECEIPT: &str = "receipt.json";
const BINDING: &str = "handoff.json";

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| {
            u64::try_from(value.as_nanos()).unwrap_or(u64::MAX)
        })
}

fn current_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn safe_failure(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::Interrupted => "This transfer was stopped".into(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
            "The other computer stopped responding".into()
        }
        _ => "The other computer could not complete the transfer".into(),
    }
}

struct Serve {
    ciphertext: Arc<Vec<u8>>,
    password: Zeroizing<String>,
    digest: [u8; 32],
}

struct Pending {
    label: String,
    reply_ticket: String,
    ephemeral: [u8; 32],
    device_key: [u8; 32],
    approval: String,
}

struct PendingBootstrap {
    label: String,
    reply_ticket: String,
    ephemeral: [u8; 32],
    device_key: [u8; 32],
    request_digest: [u8; 32],
    request: CertificationRequest,
    approval: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ReceiptStage {
    PreparedPackageImported,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum Frame {
    BootstrapRequest {
        schema: u32,
        operation_id: String,
        identity_public_key: [u8; 32],
        package_sha256: String,
        expires_at_ns: u64,
        reply_ticket: String,
        request: Box<CertificationRequest>,
        ephemeral_public_key: [u8; 32],
        signature: Vec<u8>,
    },
    BootstrapApproved {
        operation_id: String,
        package_sha256: String,
        identity_public_key: [u8; 32],
        device_public_key: [u8; 32],
        ephemeral_public_key: [u8; 32],
        request_digest: [u8; 32],
        sealed_projection: String,
        receipt: Vec<u8>,
    },
    Request {
        schema: u32,
        operation_id: String,
        reply_ticket: String,
        identity_public_key: [u8; 32],
        package_sha256: String,
        expires_at_ns: u64,
        device_cert: Box<DeviceCert>,
        ephemeral_public_key: [u8; 32],
        signature: Vec<u8>,
    },
    Approved {
        operation_id: String,
        total: u32,
        package_bytes: u64,
        package_sha256: String,
        sealed_secret: String,
    },
    Chunk {
        operation_id: String,
        index: u32,
        data: String,
        sha256: String,
    },
    ChunkAck {
        operation_id: String,
        index: u32,
    },
    End {
        operation_id: String,
        package_sha256: String,
    },
    Receipt {
        operation_id: String,
        package_sha256: String,
        stage: ReceiptStage,
        device_public_key: [u8; 32],
        signature: Vec<u8>,
    },
    Rejected {
        operation_id: String,
        reason: String,
    },
}

#[derive(Serialize)]
struct DeviceProof<'a> {
    tag: &'static str,
    operation_id: &'a str,
    reply_ticket: &'a str,
    identity_public_key: [u8; 32],
    package_sha256: &'a str,
    expires_at_ns: u64,
    device_public_key: [u8; 32],
    ephemeral_public_key: [u8; 32],
}

pub(crate) fn proof_bytes(
    operation_id: &str,
    reply_ticket: &str,
    identity_public_key: [u8; 32],
    package_sha256: &str,
    expires_at_ns: u64,
    device_public_key: [u8; 32],
    ephemeral_public_key: [u8; 32],
) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&DeviceProof {
        tag: "tilo-migration-handoff-device-v2",
        operation_id,
        reply_ticket,
        identity_public_key,
        package_sha256,
        expires_at_ns,
        device_public_key,
        ephemeral_public_key,
    })
    .map_err(|_| invalid("Device proof is invalid"))
}

impl Frame {
    fn encode(&self) -> io::Result<Vec<u8>> {
        let bytes =
            serde_json::to_vec(self).map_err(|_| invalid("Other-computer frame is invalid"))?;
        if bytes.len() > MAX_FRAME {
            return Err(invalid("Other-computer frame is too large"));
        }
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_FRAME {
            return None;
        }
        serde_json::from_slice(bytes).ok()
    }
}

fn send(peer: &Peer, frame: &Frame, cancel: &AtomicBool) -> io::Result<()> {
    let bytes = frame.encode()?;
    for _ in 0..2_000 {
        if cancel.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Handoff cancelled",
            ));
        }
        match peer.try_send(bytes.clone()) {
            Sent::Queued => return Ok(()),
            Sent::Closed => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Receiving computer disconnected",
                ));
            }
            Sent::Full => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "Receiving computer is not keeping up",
    ))
}

fn send_final(peer: &Peer, frame: &Frame) -> io::Result<()> {
    let bytes = frame.encode()?;
    for _ in 0..2_000 {
        match peer.try_send(bytes.clone()) {
            Sent::Queued => return Ok(()),
            Sent::Closed => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Receiving computer disconnected",
                ));
            }
            Sent::Full => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "Receiving computer is not keeping up",
    ))
}

fn encode_hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

fn decode_hex<const N: usize>(value: &str) -> io::Result<[u8; N]> {
    let bytes = hex::decode(value).map_err(|_| invalid("Invalid handoff digest"))?;
    bytes
        .try_into()
        .map_err(|_| invalid("Invalid handoff digest"))
}

pub(crate) fn receipt_matches(
    operation_id: &str,
    package_digest: &[u8; 32],
    frame_operation: &str,
    frame_package: &str,
    stage: ReceiptStage,
) -> bool {
    stage == ReceiptStage::PreparedPackageImported
        && frame_operation == operation_id
        && frame_package == hex::encode(package_digest)
}

pub(crate) fn receipt_bytes(
    operation_id: &str,
    package_sha256: &str,
    stage: ReceiptStage,
) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "tag": "tilo-migration-handoff-receipt-v2",
        "operation_id": operation_id,
        "package_sha256": package_sha256,
        "stage": stage,
    }))
    .map_err(|_| invalid("Import receipt is invalid"))
}

pub(crate) fn verify_receipt_signature(
    operation_id: &str,
    package_sha256: &str,
    stage: ReceiptStage,
    device_public_key: &[u8; 32],
    signature: &[u8],
) -> bool {
    if signature.len() != 64 {
        return false;
    }
    let Ok(verifying) = ed25519_dalek::VerifyingKey::from_bytes(device_public_key) else {
        return false;
    };
    let Ok(signature) = <[u8; 64]>::try_from(signature) else {
        return false;
    };
    let signature = ed25519_dalek::Signature::from_bytes(&signature);
    receipt_bytes(operation_id, package_sha256, stage)
        .is_ok_and(|message| verifying.verify(&message, &signature).is_ok())
}

pub(crate) fn verify_device(
    frame: &Frame,
    now: u64,
    identity: &HandoffIdentity,
    package_sha256: &str,
) -> Option<(String, [u8; 32], [u8; 32], DeviceCert)> {
    let Frame::Request {
        schema,
        operation_id,
        reply_ticket,
        identity_public_key,
        package_sha256: requested_package,
        expires_at_ns,
        device_cert,
        ephemeral_public_key,
        signature,
    } = frame
    else {
        return None;
    };
    if *schema != super::SCHEMA
        || operation_id != &identity.operation_id
        || identity_public_key != &identity.identity_public_key
        || requested_package.as_str() != package_sha256
        || *expires_at_ns != identity.expires_at_ns
        || !valid_operation(operation_id)
        || !valid_ticket(reply_ticket)
        || device_cert.device_label.is_empty()
        || device_cert.device_label.len() > 64
        || device_cert.device_label.chars().any(char::is_control)
        || *ephemeral_public_key == [0; 32]
        || device_cert.user_public_key != identity.identity_public_key
        || signature.len() != 64
        || signature.iter().all(|byte| *byte == 0)
    {
        return None;
    }
    tilo_user_identity::verify_device_cert_at(device_cert, now).ok()?;
    let message = proof_bytes(
        operation_id,
        reply_ticket,
        *identity_public_key,
        requested_package,
        *expires_at_ns,
        device_cert.device_public_key,
        *ephemeral_public_key,
    )
    .ok()?;
    let verifying = ed25519_dalek::VerifyingKey::from_bytes(&device_cert.device_public_key).ok()?;
    let sig = ed25519_dalek::Signature::from_bytes(signature.as_slice().try_into().ok()?);
    verifying.verify(&message, &sig).ok()?;
    Some((
        device_cert.device_label.clone(),
        device_cert.device_public_key,
        *ephemeral_public_key,
        device_cert.as_ref().clone(),
    ))
}

/// The preparation computer's live source. It can be bound before the medium
/// is written, so the published descriptor names the endpoint that will serve
/// the package.
pub struct Source {
    identity: HandoffIdentity,
    state: Channel<SourceState>,
    serve: Sender<Serve>,
    approve: Sender<String>,
    cancel: Arc<AtomicBool>,
    joined: Option<JoinHandle<()>>,
}

impl Source {
    /// Bind the source endpoint. The returned identity is written to the
    /// medium by the package producer; no private source data is exposed.
    ///
    /// # Errors
    ///
    /// Refuses an invalid operation id or identity key, or an endpoint that
    /// cannot bind.
    pub fn bind(operation_id: String, identity_public_key: [u8; 32]) -> io::Result<Self> {
        Self::bind_inner(operation_id, identity_public_key, None)
    }

    /// Bind a source that can also admit a fresh destination into this exact
    /// person before it receives the package. The signer stays in the
    /// prepared-identity vault; only the root signature leaves it.
    ///
    /// # Errors
    ///
    /// As [`Self::bind`], plus an invalid identity export.
    pub fn bind_with_identity(
        operation_id: String,
        identity: super::SourceIdentity,
    ) -> io::Result<Self> {
        identity.valid()?;
        let identity_public_key = identity.identity_public_key;
        Self::bind_inner(operation_id, identity_public_key, Some(identity))
    }

    fn bind_inner(
        operation_id: String,
        identity_public_key: [u8; 32],
        authority: Option<super::SourceIdentity>,
    ) -> io::Result<Self> {
        if !valid_operation(&operation_id) || identity_public_key == [0; 32] {
            return Err(invalid("Invalid other-computer handoff"));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (identity_tx, identity_rx) = mpsc::channel();
        let (state_tx, state) = mpsc::channel();
        let (serve_tx, serve_rx) = mpsc::channel();
        let (approve_tx, approve_rx) = mpsc::channel();
        let worker_cancel = cancel.clone();
        let joined = std::thread::spawn(move || {
            let result = source_worker(
                &operation_id,
                identity_public_key,
                authority.as_ref(),
                &identity_tx,
                &state_tx,
                &serve_rx,
                &approve_rx,
                &worker_cancel,
            );
            if result.is_err() {
                let _ = identity_tx.send(Err("The source endpoint stopped".into()));
                let _ = state_tx.send(SourceState::Failed);
            }
        });
        let identity = match identity_rx.recv() {
            Ok(Ok(identity)) => identity,
            Ok(Err(message)) => return Err(io::Error::other(message)),
            Err(_) => return Err(io::Error::other("Source endpoint stopped")),
        };
        Ok(Self {
            identity,
            state,
            serve: serve_tx,
            approve: approve_tx,
            cancel,
            joined: Some(joined),
        })
    }

    pub fn identity(&self) -> &HandoffIdentity {
        &self.identity
    }

    /// Make the exact ciphertext available. The password stays in this
    /// process and is only sealed to the approved receiver.
    ///
    /// # Errors
    ///
    /// Refuses an empty or oversized package, a digest that does not match the
    /// bytes, or a stopped worker.
    pub fn serve(
        &self,
        ciphertext: Vec<u8>,
        password: Zeroizing<String>,
        digest: [u8; 32],
    ) -> io::Result<()> {
        if ciphertext.len() as u64 > MAX_PACKAGE || ciphertext.is_empty() {
            return Err(invalid("Prepared package is too large"));
        }
        if super::digest(&ciphertext) != digest {
            return Err(invalid("Prepared package digest differs"));
        }
        self.serve
            .send(Serve {
                ciphertext: Arc::new(ciphertext),
                password,
                digest,
            })
            .map_err(|_| io::Error::other("Source handoff stopped"))
    }

    /// Approve the exact request a person saw. The token comes from
    /// [`SourceState::Requested`]; a later request never inherits it.
    ///
    /// # Errors
    ///
    /// Refuses a malformed approval token or a stopped worker.
    pub fn approve(&self, approval: &str) -> io::Result<()> {
        if !valid_operation(approval) {
            return Err(invalid("Invalid handoff approval"));
        }
        self.approve
            .send(approval.to_owned())
            .map_err(|_| io::Error::other("Source handoff stopped"))
    }

    pub fn poll(&self) -> Option<SourceState> {
        self.state.try_recv().ok()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(joined) = self.joined.take() {
            let _ = joined.join();
        }
    }
}

/// Approvals source workers took that named another request.
#[cfg(any(test, feature = "test-support"))]
static REFUSED_APPROVALS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Join requests source workers refused: another operation, person or
/// package, or a request that does not verify.
#[cfg(any(test, feature = "test-support"))]
static REFUSED_JOIN_REQUESTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// How many approvals naming another request source workers in this process
/// have refused, so a test can wait for a refusal instead of sleeping.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn refused_approvals() -> usize {
    REFUSED_APPROVALS.load(Ordering::SeqCst)
}

/// How many join requests source workers in this process have refused.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn refused_join_requests() -> usize {
    REFUSED_JOIN_REQUESTS.load(Ordering::SeqCst)
}

/// Take the next approval, if one came, and whether it names `expected`.
fn take_approval(approve: &Channel<String>, expected: &str) -> bool {
    let Ok(received) = approve.try_recv() else {
        return false;
    };
    let matches = received == expected;
    #[cfg(any(test, feature = "test-support"))]
    if !matches {
        REFUSED_APPROVALS.fetch_add(1, Ordering::SeqCst);
    }
    matches
}

#[expect(
    clippy::too_many_lines,
    reason = "one state machine for the source endpoint: request, approval, chunks and receipt"
)]
fn source_worker(
    operation_id: &str,
    identity_public_key: [u8; 32],
    authority: Option<&super::SourceIdentity>,
    identity_tx: &Sender<Result<HandoffIdentity, String>>,
    state: &Sender<SourceState>,
    serve: &Channel<Serve>,
    approve: &Channel<String>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| io::Error::other("Source endpoint is unavailable"))?;
    let mut endpoint = runtime
        .block_on(Endpoint::<Frame>::bind(
            ALPN,
            MAX_FRAME,
            QUEUE,
            Frame::decode,
        ))
        .map_err(|_| io::Error::other("Source endpoint is unavailable"))?;
    let expires_at_ns = now_ns()
        .checked_add(u64::try_from(OPERATION_WINDOW.as_nanos()).unwrap_or(u64::MAX))
        .ok_or_else(|| io::Error::other("Handoff expiry overflowed"))?;
    let identity = HandoffIdentity {
        operation_id: operation_id.to_owned(),
        ticket: endpoint.ticket().to_owned(),
        identity_public_key,
        expires_at_ns,
    };
    let _ = identity_tx.send(Ok(identity.clone()));
    let _ = state.send(SourceState::Waiting);

    let mut ready: Option<Serve> = None;
    let mut pending: Option<Pending> = None;
    let mut pending_bootstrap: Option<PendingBootstrap> = None;
    let mut bootstrap_result: Option<(String, Frame)> = None;
    let mut bootstrap_peer: Option<Peer> = None;
    let mut bootstrap_joined: Option<([u8; 32], [u8; 32])> = None;
    let mut approved = false;
    let mut peer: Option<Peer> = None;
    let mut next = 0_usize;
    let mut awaiting: Option<usize> = None;
    let mut last_send = std::time::Instant::now();
    let mut end_sent = false;

    loop {
        if cancel.load(Ordering::Acquire) {
            let _ = state.send(SourceState::Cancelled);
            return Ok(());
        }
        if now_ns() >= expires_at_ns {
            let _ = state.send(SourceState::Expired);
            return Ok(());
        }
        if ready.is_none()
            && let Ok(value) = serve.try_recv()
        {
            ready = Some(value);
        }
        while let Some(frame) = endpoint.try_recv() {
            match &frame {
                Frame::ChunkAck {
                    operation_id,
                    index,
                } => {
                    if operation_id == &identity.operation_id && awaiting == Some(*index as usize) {
                        awaiting = None;
                        next = next.saturating_add(1);
                        if let Some(request) = pending.as_ref() {
                            let total = ready.as_ref().map_or(0, |ready| ready.ciphertext.len());
                            let _ = state.send(SourceState::Sending {
                                label: request.label.clone(),
                                completed: (next.saturating_mul(CHUNK).min(total)) as u64,
                                total: total as u64,
                            });
                        }
                    }
                }
                Frame::Receipt {
                    operation_id,
                    package_sha256,
                    stage,
                    device_public_key,
                    signature,
                } => {
                    let digest = ready.as_ref().map_or([0; 32], |ready| ready.digest);
                    if receipt_matches(
                        &identity.operation_id,
                        &digest,
                        operation_id,
                        package_sha256,
                        *stage,
                    ) && end_sent
                        && pending.as_ref().is_some_and(|request| {
                            *device_public_key == request.device_key
                                && verify_receipt_signature(
                                    operation_id,
                                    package_sha256,
                                    *stage,
                                    device_public_key,
                                    signature,
                                )
                        })
                    {
                        if let Some(request) = pending.as_ref() {
                            let _ = state.send(SourceState::PackageImported {
                                label: request.label.clone(),
                            });
                        }
                        return Ok(());
                    }
                }
                Frame::Rejected {
                    operation_id,
                    reason: _,
                } => {
                    if operation_id == &identity.operation_id {
                        let _ = state.send(SourceState::Rejected);
                        return Ok(());
                    }
                }
                Frame::BootstrapRequest {
                    schema,
                    operation_id,
                    identity_public_key,
                    package_sha256,
                    expires_at_ns,
                    reply_ticket,
                    request,
                    ephemeral_public_key,
                    signature,
                } => {
                    if let Some((ticket, response)) = bootstrap_result.as_ref()
                        && ticket == reply_ticket
                        && matches!(
                            response,
                            Frame::BootstrapApproved { device_public_key, .. }
                                if device_public_key == &request.device_public_key
                        )
                    {
                        // The response was lost or the destination restarted
                        // its wait: resend exactly the same signed answer.
                        if let Some(destination) = bootstrap_peer.as_ref() {
                            send(destination, response, cancel)?;
                        } else if let Ok(destination) = endpoint.connect(reply_ticket, QUEUE) {
                            send(&destination, response, cancel)?;
                            bootstrap_peer = Some(destination);
                        }
                        continue;
                    }
                    // A person approves one join for one exact request. Do not
                    // replace it while an approval or the admission is in flight.
                    if approved
                        || pending.is_some()
                        || pending_bootstrap.is_some()
                        || bootstrap_result.is_some()
                    {
                        continue;
                    }
                    if authority.is_none() {
                        continue;
                    }
                    let Some(ready) = ready.as_ref() else {
                        continue;
                    };
                    let digest = encode_hex(&ready.digest);
                    if *schema != super::SCHEMA
                        || operation_id != &identity.operation_id
                        || identity_public_key != &identity.identity_public_key
                        || package_sha256 != &digest
                        || *expires_at_ns != identity.expires_at_ns
                        || !valid_ticket(reply_ticket)
                        || request.user_public_key != identity.identity_public_key
                        || request.device_public_key == [0; 32]
                        || *ephemeral_public_key == [0; 32]
                        || request.verify_at(now_ns()).is_err()
                        || signature.len() != 64
                        || signature.iter().all(|byte| *byte == 0)
                    {
                        #[cfg(any(test, feature = "test-support"))]
                        REFUSED_JOIN_REQUESTS.fetch_add(1, Ordering::SeqCst);
                        continue;
                    }
                    let Ok(request_digest) = request.digest() else {
                        continue;
                    };
                    let Ok(message) = bootstrap::proof_bytes(
                        operation_id,
                        reply_ticket,
                        *identity_public_key,
                        package_sha256,
                        *expires_at_ns,
                        request_digest,
                        request.device_public_key,
                        *ephemeral_public_key,
                    ) else {
                        continue;
                    };
                    let Ok(verifying) =
                        ed25519_dalek::VerifyingKey::from_bytes(&request.device_public_key)
                    else {
                        continue;
                    };
                    let Ok(signature) = <[u8; 64]>::try_from(signature.as_slice()) else {
                        continue;
                    };
                    if verifying
                        .verify(&message, &ed25519_dalek::Signature::from_bytes(&signature))
                        .is_err()
                    {
                        continue;
                    }
                    let code = verification_code(
                        &identity.operation_id,
                        &request.device_public_key,
                        ephemeral_public_key,
                        &ready.digest,
                        &identity.identity_public_key,
                    );
                    let approval = uuid::Uuid::new_v4().to_string();
                    pending_bootstrap = Some(PendingBootstrap {
                        label: request.device_label.clone(),
                        reply_ticket: reply_ticket.clone(),
                        ephemeral: *ephemeral_public_key,
                        device_key: request.device_public_key,
                        request_digest,
                        request: request.as_ref().clone(),
                        approval: approval.clone(),
                    });
                    let _ = state.send(SourceState::JoinRequested {
                        label: request.device_label.clone(),
                        code,
                        approval,
                    });
                }
                Frame::Request { reply_ticket, .. } => {
                    // Once a person approved one request, no later request may
                    // replace it and inherit that approval.
                    if approved {
                        continue;
                    }
                    let Some(ready) = ready.as_ref() else {
                        // The package is not prepared yet. Do not show an
                        // approval prompt whose exact package cannot be named.
                        continue;
                    };
                    let digest = encode_hex(&ready.digest);
                    let Some((label, device_key, ephemeral, _cert)) =
                        verify_device(&frame, now_ns(), &identity, &digest)
                    else {
                        continue;
                    };
                    // A destination whose join this person just approved for
                    // this exact package already proved this device key. The
                    // typed enrollment receipt bound that key to this
                    // operation and package, so no second person prompt can
                    // release a different package or device.
                    let joined = bootstrap_joined.is_some_and(|(device, package)| {
                        device == device_key && package == ready.digest
                    });
                    if pending_bootstrap.is_some() && !joined {
                        continue;
                    }
                    // The destination repeats its request until the source
                    // has the package ready. A repeat of the exact same
                    // device and ephemeral key must not mint a new approval
                    // token behind the person's back.
                    if pending.as_ref().is_some_and(|request| {
                        request.reply_ticket == *reply_ticket && request.ephemeral == ephemeral
                    }) {
                        continue;
                    }
                    let approval = if joined {
                        String::new()
                    } else {
                        uuid::Uuid::new_v4().to_string()
                    };
                    pending = Some(Pending {
                        label: label.clone(),
                        reply_ticket: reply_ticket.clone(),
                        ephemeral,
                        device_key,
                        approval: approval.clone(),
                    });
                    if joined {
                        approved = true;
                    } else {
                        let code = verification_code(
                            &identity.operation_id,
                            &device_key,
                            &ephemeral,
                            &ready.digest,
                            &identity.identity_public_key,
                        );
                        let _ = state.send(SourceState::Requested {
                            label,
                            code,
                            approval,
                        });
                    }
                }
                _ => {}
            }
        }
        if pending_bootstrap.is_some()
            && let Some(request) = pending_bootstrap.as_ref()
            && take_approval(approve, &request.approval)
        {
            let Some(authority) = authority else {
                return Err(io::Error::other("Identity bootstrap is unavailable"));
            };
            let Some(ready) = ready.as_ref() else {
                return Err(io::Error::other("Prepared package is unavailable"));
            };
            let (_cert, projection) = bootstrap::issue(authority, &request.request, now_ns())?;
            let projection_bytes = projection.encode()?;
            let projection_sha256 = bootstrap::projection_digest(&projection_bytes);
            let digest = encode_hex(&ready.digest);
            let aad = bootstrap::aad(
                &identity.operation_id,
                &ready.digest,
                &identity.identity_public_key,
                &request.device_key,
                &request.ephemeral,
                &request.request_digest,
            );
            let sealed_projection =
                bootstrap::seal_projection(&projection, &request.ephemeral, &aad)?;
            let receipt = bootstrap::receipt_bytes(
                &identity.operation_id,
                &digest,
                identity.identity_public_key,
                request.device_key,
                request.ephemeral,
                request.request_digest,
                projection_sha256,
            )?;
            let signature = (authority.signer)(&receipt)?;
            let response = Frame::BootstrapApproved {
                operation_id: identity.operation_id.clone(),
                package_sha256: digest,
                identity_public_key: identity.identity_public_key,
                device_public_key: request.device_key,
                ephemeral_public_key: request.ephemeral,
                request_digest: request.request_digest,
                sealed_projection,
                receipt: signature.to_vec(),
            };
            let destination = endpoint
                .connect(&request.reply_ticket, QUEUE)
                .map_err(|_| io::Error::other("Joining computer is unavailable"))?;
            send(&destination, &response, cancel)?;
            bootstrap_peer = Some(destination);
            bootstrap_result = Some((request.reply_ticket.clone(), response));
            bootstrap_joined = Some((request.device_key, ready.digest));
            let label = request.label.clone();
            pending_bootstrap = None;
            let _ = state.send(SourceState::Joining { label });
        } else if !approved
            && ready.is_some()
            && let Some(request) = pending.as_ref()
            && take_approval(approve, &request.approval)
        {
            approved = true;
        }
        if approved
            && peer.is_none()
            && let (Some(request), Some(ready)) = (pending.as_ref(), ready.as_ref())
        {
            let destination = endpoint
                .connect(&request.reply_ticket, QUEUE)
                .map_err(|_| io::Error::other("Receiving computer is unavailable"))?;
            let sealed = hpke::seal_to_bytes(
                &PublicKey::from_bytes(request.ephemeral),
                Label::MigrationPackage,
                &handoff_aad(
                    &identity.operation_id,
                    &ready.digest,
                    &identity.identity_public_key,
                ),
                ready.password.as_bytes(),
            )
            .map_err(|_| io::Error::other("Could not seal the prepared package key"))?;
            let total = ready.ciphertext.len().div_ceil(CHUNK);
            send(
                &destination,
                &Frame::Approved {
                    operation_id: identity.operation_id.clone(),
                    total: u32::try_from(total).map_err(|_| invalid("Package is too large"))?,
                    package_bytes: ready.ciphertext.len() as u64,
                    package_sha256: encode_hex(&ready.digest),
                    sealed_secret: base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        sealed,
                    ),
                },
                cancel,
            )?;
            peer = Some(destination);
            next = 0;
            awaiting = None;
            last_send = std::time::Instant::now();
            let _ = state.send(SourceState::Sending {
                label: request.label.clone(),
                completed: 0,
                total: ready.ciphertext.len() as u64,
            });
        }
        if approved && let (Some(ready), Some(peer)) = (ready.as_ref(), peer.as_ref()) {
            let total = ready.ciphertext.len().div_ceil(CHUNK);
            if next >= total {
                if !end_sent && awaiting.is_none() {
                    send(
                        peer,
                        &Frame::End {
                            operation_id: identity.operation_id.clone(),
                            package_sha256: encode_hex(&ready.digest),
                        },
                        cancel,
                    )?;
                    end_sent = true;
                    if let Some(request) = pending.as_ref() {
                        let _ = state.send(SourceState::AwaitingReceipt {
                            label: request.label.clone(),
                        });
                    }
                }
            } else if awaiting.is_none() || last_send.elapsed() > RESEND {
                let index = awaiting.unwrap_or(next);
                let start = index.saturating_mul(CHUNK);
                let end = start.saturating_add(CHUNK).min(ready.ciphertext.len());
                let data = ready
                    .ciphertext
                    .get(start..end)
                    .ok_or_else(|| invalid("Prepared package changed while sending"))?;
                send(
                    peer,
                    &Frame::Chunk {
                        operation_id: identity.operation_id.clone(),
                        index: u32::try_from(index).map_err(|_| invalid("Invalid chunk"))?,
                        data: base64::Engine::encode(
                            &base64::engine::general_purpose::STANDARD,
                            data,
                        ),
                        sha256: encode_hex(&digest(data)),
                    },
                    cancel,
                )?;
                awaiting = Some(index);
                last_send = std::time::Instant::now();
            }
        }
        std::thread::sleep(POLL);
    }
}

/// The destination side of the handoff. It binds its own endpoint, proves its
/// certified device identity, receives the package, writes the ordinary
/// prepared-setup transport, and releases the source only after a verified
/// import receipt.
pub struct Receiver {
    state: Channel<ReceiverState>,
    acknowledge: Sender<[u8; 32]>,
    cancel: Arc<AtomicBool>,
    joined: Option<JoinHandle<()>>,
    expected: [u8; 32],
    code: Arc<Mutex<Option<String>>>,
}

enum ReceiverState {
    Received(Box<Received>),
    Rejected(String),
    Complete,
}

/// A package that has arrived and verified, before the canonical import has
/// run. The password exists only in this struct and the caller's worker.
pub struct Received {
    pub password: Zeroizing<String>,
    pub package_sha256: [u8; 32],
    pub sealed_bytes: u64,
    pub package_path: PathBuf,
}

/// The installed destination's real identity. The identity directory holds
/// the person's record; the system root holds the certified device key.
pub struct ReceiverIdentity {
    pub identity_root: PathBuf,
    pub identity_dir: PathBuf,
}

impl Receiver {
    /// Connect to the source named by `handoff` and wait for approval. The
    /// descriptor is validated against this computer's clock and the active
    /// person identity before any frame is sent.
    ///
    /// # Errors
    ///
    /// Refuses an expired descriptor, a missing or non-private identity, and a
    /// descriptor that names another person's prepared package.
    pub fn open(
        handoff: PublicHandoff,
        identity: ReceiverIdentity,
        package_directory: PathBuf,
    ) -> io::Result<Self> {
        let now = now_ns();
        if !handoff.valid_at(now) {
            return Err(invalid(
                "This other-computer handoff is no longer available",
            ));
        }
        preflight_identity(&handoff, &identity)?;
        let expected = handoff.digest()?;
        let cancel = Arc::new(AtomicBool::new(false));
        let (state_tx, state) = mpsc::channel();
        let (acknowledge_tx, acknowledge_rx) = mpsc::channel();
        let code = Arc::new(Mutex::new(None));
        let worker_cancel = cancel.clone();
        let worker_code = code.clone();
        let joined = std::thread::spawn(move || {
            let result = receiver_worker(
                &handoff,
                &identity,
                &package_directory,
                &state_tx,
                &acknowledge_rx,
                &worker_code,
                &worker_cancel,
            );
            if let Err(error) = result {
                let _ = state_tx.send(ReceiverState::Rejected(safe_failure(&error)));
            }
        });
        Ok(Self {
            state,
            acknowledge: acknowledge_tx,
            cancel,
            joined: Some(joined),
            expected,
            code,
        })
    }

    /// Join the person named by `handoff` as a new member device before
    /// receiving the package. This is the path for a fresh computer that has
    /// no person yet. The source signs a canonical member certificate for
    /// this computer's own new device key after a person approves the displayed
    /// code; the typed enrollment receipt binds that key to this exact
    /// operation and package. Nothing is written over an existing identity.
    pub fn join(
        handoff: PublicHandoff,
        identity: ReceiverIdentity,
        package_directory: PathBuf,
        device_label: String,
    ) -> io::Result<Self> {
        let now = now_ns();
        if !handoff.valid_at(now) {
            return Err(invalid(
                "This other-computer handoff is no longer available",
            ));
        }
        preflight_join_identity(&identity)?;
        if device_label.is_empty()
            || device_label.len() > 64
            || device_label.chars().any(char::is_control)
        {
            return Err(invalid("Invalid device label"));
        }
        let expected = handoff.digest()?;
        let cancel = Arc::new(AtomicBool::new(false));
        let (state_tx, state) = mpsc::channel();
        let (acknowledge_tx, acknowledge_rx) = mpsc::channel();
        let code = Arc::new(Mutex::new(None));
        let worker_cancel = cancel.clone();
        let worker_code = code.clone();
        let joined = std::thread::spawn(move || {
            let result = join_worker(
                &handoff,
                &identity,
                &package_directory,
                &device_label,
                &state_tx,
                &acknowledge_rx,
                &worker_code,
                &worker_cancel,
            );
            if let Err(error) = result {
                let _ = state_tx.send(ReceiverState::Rejected(safe_failure(&error)));
            }
        });
        Ok(Self {
            state,
            acknowledge: acknowledge_tx,
            cancel,
            joined: Some(joined),
            expected,
            code,
        })
    }

    /// The code both screens must show before the person approves this
    /// computer on the source. It appears once this receiver has bound its
    /// endpoint and ephemeral key.
    pub fn verification_code(&self) -> Option<String> {
        self.code
            .lock()
            .ok()
            .and_then(|value| value.as_ref().cloned())
    }

    /// Block until the package has arrived or the peer ends. This is used by
    /// first boot, which runs the handoff on its own worker.
    ///
    /// # Errors
    ///
    /// Refuses a source rejection, a completed handoff without a package, or a
    /// stopped worker.
    pub fn wait(&self) -> io::Result<Received> {
        match self.state.recv() {
            Ok(ReceiverState::Received(received)) => Ok(*received),
            Ok(ReceiverState::Rejected(reason)) => Err(io::Error::other(reason)),
            Ok(ReceiverState::Complete) => {
                Err(io::Error::other("Handoff ended before a package arrived"))
            }
            Err(_) => Err(io::Error::other("Handoff stopped before a package arrived")),
        }
    }

    /// Non-blocking progress for a first-boot loop that also draws.
    pub fn poll(&self) -> Option<io::Result<Received>> {
        match self.state.try_recv() {
            Ok(ReceiverState::Received(received)) => Some(Ok(*received)),
            Ok(ReceiverState::Rejected(reason)) => Some(Err(io::Error::other(reason))),
            Ok(ReceiverState::Complete) => Some(Err(io::Error::other(
                "Handoff ended before a package arrived",
            ))),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    /// Confirm that the canonical importer verified and acknowledged this
    /// exact package. The source reports completion only after this frame and
    /// only for a receipt naming this package digest.
    ///
    /// # Errors
    ///
    /// Refuses a receipt for another package or a stopped worker.
    pub fn acknowledge(&self, received: &Received) -> io::Result<()> {
        if received.package_sha256 != self.expected || received.package_sha256 == [0; 32] {
            return Err(invalid("Import receipt is for another package"));
        }
        self.acknowledge
            .send(self.expected)
            .map_err(|_| io::Error::other("Handoff stopped before its receipt"))
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        // The compositor drops this on its own loop after the canonical
        // importer has acknowledged. Setting cancellation wakes the worker,
        // but joining here would freeze a frame on the network; the worker
        // still processes an already queued acknowledgement, flushes its
        // receipt and then releases the endpoint on its own thread.
        self.cancel.store(true, Ordering::Release);
        drop(self.joined.take());
    }
}

fn preflight_identity(handoff: &PublicHandoff, identity: &ReceiverIdentity) -> io::Result<()> {
    configured_identity_root(&identity.identity_root)?;
    let active = runtime_identity::resolve_active_identity(&identity.identity_dir)
        .ok_or_else(|| invalid("The receiving computer has no real identity yet"))?;
    let metadata = runtime_identity::load_metadata_from_path(&active)
        .map_err(|_| invalid("The receiving computer has no real identity yet"))?;
    if metadata.group_public_key != handoff.identity_public_key {
        return Err(invalid("This transfer belongs to another identity"));
    }
    Ok(())
}

fn preflight_join_identity(identity: &ReceiverIdentity) -> io::Result<()> {
    if !identity.identity_root.is_absolute() || !identity.identity_dir.is_absolute() {
        return Err(invalid("The receiving identity location is invalid"));
    }
    configured_identity_root(&identity.identity_root)?;
    if runtime_identity::resolve_active_identity(&identity.identity_dir).is_some()
        || !identity_dir_empty(&identity.identity_dir)?
    {
        return Err(invalid("This computer already has an identity"));
    }
    let paths = tilo_user_identity::IdentityPaths::under(&identity.identity_root);
    if paths.user_privkey.exists() || paths.user_pubkey.exists() || paths.device_cert.exists() {
        return Err(invalid("This computer already has an identity"));
    }
    Ok(())
}

fn identity_dir_empty(directory: &Path) -> io::Result<bool> {
    match runtime_identity::list_identities(directory) {
        Ok(identities) => Ok(identities.is_empty()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

fn configured_identity_root(root: &Path) -> io::Result<()> {
    let configured = tilo_identity_host::provisioning::collaboration_identity_paths();
    let configured_root = configured
        .device_cert
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid("The receiving identity root is unavailable"))?;
    if root == configured_root
        || root
            .canonicalize()
            .ok()
            .zip(configured_root.canonicalize().ok())
            .is_some_and(|(left, right)| left == right)
    {
        Ok(())
    } else {
        Err(invalid(
            "The receiving identity root is not this device's configured root",
        ))
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one ordered join: prove the new key, wait for approval, verify the receipt, then receive"
)]
fn join_worker(
    handoff: &PublicHandoff,
    identity: &ReceiverIdentity,
    package_directory: &Path,
    device_label: &str,
    state: &Sender<ReceiverState>,
    acknowledge: &Channel<[u8; 32]>,
    code: &Mutex<Option<String>>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let expected = handoff.digest()?;
    let paths = tilo_user_identity::IdentityPaths::under(&identity.identity_root);
    paths.ensure_directories()?;
    let (device, _) = tilo_user_identity::persist::load_or_create_device_key(&paths)
        .map_err(|_| invalid("The destination device key is unavailable"))?;
    let device_public_key = device.verifying_key().to_bytes();
    let now = now_ns();
    let issued_at_ns = now.saturating_sub(60 * 1_000_000_000);
    let expires_at_ns = now
        .saturating_add(8 * 60 * 1_000_000_000)
        .min(handoff.expires_at_ns);
    if expires_at_ns <= issued_at_ns {
        return Err(invalid("This other-computer handoff expired"));
    }
    let nonce_uuid = uuid::Uuid::new_v4().into_bytes();
    let mut nonce = [0_u8; 16];
    nonce.copy_from_slice(&nonce_uuid[..16]);
    let request = CertificationRequest {
        version: tilo_user_identity::CERTIFICATION_SCHEMA_VERSION,
        user_public_key: handoff.identity_public_key,
        device_public_key,
        device_label: device_label.to_owned(),
        requested: tilo_user_identity::DeviceCapabilities::member(),
        nonce,
        invite: [0; 16],
        issued_at_ns,
        expires_at_ns,
        signature: [0; 64],
    }
    .sign(&device)
    .map_err(|_| invalid("Could not sign the joining request"))?;
    let request_digest = request
        .digest()
        .map_err(|_| invalid("Could not sign the joining request"))?;
    let ephemeral =
        SecretKey::generate().map_err(|_| io::Error::other("No randomness for the handoff"))?;
    let ephemeral_public = ephemeral
        .public_key()
        .map_err(|_| io::Error::other("Could not derive the handoff key"))?;
    if let Ok(mut slot) = code.lock() {
        *slot = Some(verification_code(
            &handoff.operation_id,
            &device_public_key,
            &ephemeral_public.to_bytes(),
            &expected,
            &handoff.identity_public_key,
        ));
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| io::Error::other("The joining endpoint is unavailable"))?;
    let mut endpoint = runtime
        .block_on(Endpoint::<Frame>::bind(
            ALPN,
            MAX_FRAME,
            QUEUE,
            Frame::decode,
        ))
        .map_err(|_| io::Error::other("The joining endpoint is unavailable"))?;
    let reply_ticket = endpoint.ticket().to_owned();
    let sender_endpoint = runtime
        .block_on(Endpoint::<Frame>::bind(
            ALPN,
            MAX_FRAME,
            QUEUE,
            Frame::decode,
        ))
        .map_err(|_| io::Error::other("The joining endpoint is unavailable"))?;
    let source = sender_endpoint
        .connect(&handoff.ticket, QUEUE)
        .map_err(|_| io::Error::other("The preparing computer is unavailable"))?;
    let message = bootstrap::proof_bytes(
        &handoff.operation_id,
        &reply_ticket,
        handoff.identity_public_key,
        &handoff.package_sha256,
        handoff.expires_at_ns,
        request_digest,
        device_public_key,
        ephemeral_public.to_bytes(),
    )?;
    let signature = device.sign(&message).to_bytes();
    let request_frame = Frame::BootstrapRequest {
        schema: super::SCHEMA,
        operation_id: handoff.operation_id.clone(),
        identity_public_key: handoff.identity_public_key,
        package_sha256: handoff.package_sha256.clone(),
        expires_at_ns: handoff.expires_at_ns,
        reply_ticket,
        request: Box::new(request.clone()),
        ephemeral_public_key: ephemeral_public.to_bytes(),
        signature: signature.to_vec(),
    };
    send(&source, &request_frame, cancel)?;
    let mut last_request = std::time::Instant::now();
    let mut admitted = false;
    while !admitted {
        if cancel.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Handoff cancelled",
            ));
        }
        if now_ns() >= handoff.expires_at_ns {
            return Err(invalid("This other-computer handoff expired"));
        }
        if last_request.elapsed() > RESEND {
            send(&source, &request_frame, cancel)?;
            last_request = std::time::Instant::now();
        }
        while let Some(frame) = endpoint.try_recv() {
            match frame {
                Frame::BootstrapApproved {
                    operation_id,
                    package_sha256,
                    identity_public_key,
                    device_public_key: admitted_device,
                    ephemeral_public_key,
                    request_digest: admitted_request,
                    sealed_projection,
                    receipt,
                } => {
                    if operation_id != handoff.operation_id
                        || package_sha256 != handoff.package_sha256
                        || identity_public_key != handoff.identity_public_key
                        || admitted_device != device_public_key
                        || ephemeral_public_key != ephemeral_public.to_bytes()
                        || admitted_request != request_digest
                    {
                        continue;
                    }
                    let aad = bootstrap::aad(
                        &handoff.operation_id,
                        &expected,
                        &handoff.identity_public_key,
                        &device_public_key,
                        &ephemeral_public.to_bytes(),
                        &request_digest,
                    );
                    let projection =
                        bootstrap::open_projection(&sealed_projection, &ephemeral, &aad)?;
                    let projection_bytes = projection.encode()?;
                    if !bootstrap::verify_receipt(
                        &receipt,
                        &handoff.identity_public_key,
                        &handoff.operation_id,
                        &handoff.package_sha256,
                        &device_public_key,
                        &ephemeral_public.to_bytes(),
                        &request_digest,
                        &bootstrap::projection_digest(&projection_bytes),
                    ) {
                        return Err(invalid("The enrollment receipt did not verify"));
                    }
                    bootstrap::install(
                        &identity.identity_root,
                        &identity.identity_dir,
                        &request,
                        &projection,
                        now_ns(),
                    )?;
                    admitted = true;
                    break;
                }
                Frame::Rejected { operation_id, .. } if operation_id == handoff.operation_id => {
                    return Err(invalid("The preparing computer refused the join"));
                }
                _ => {}
            }
        }
        if !admitted {
            std::thread::sleep(POLL);
        }
    }
    drop(source);
    drop(sender_endpoint);
    drop(endpoint);
    drop(runtime);
    receiver_worker(
        handoff,
        identity,
        package_directory,
        state,
        acknowledge,
        code,
        cancel,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "one state machine for the destination endpoint: request, chunks, package and receipt"
)]
fn receiver_worker(
    handoff: &PublicHandoff,
    identity: &ReceiverIdentity,
    package_directory: &Path,
    state: &Sender<ReceiverState>,
    acknowledge: &Channel<[u8; 32]>,
    code: &Mutex<Option<String>>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let (device_cert, signing, person) = load_identity(identity)?;
    if person != handoff.identity_public_key {
        return Err(invalid("This transfer belongs to another identity"));
    }
    let ephemeral =
        SecretKey::generate().map_err(|_| io::Error::other("No randomness for the handoff"))?;
    let ephemeral_public = ephemeral
        .public_key()
        .map_err(|_| io::Error::other("Could not derive the handoff key"))?;
    let expected = handoff.digest()?;
    if let Ok(mut slot) = code.lock() {
        *slot = Some(verification_code(
            &handoff.operation_id,
            &device_cert.device_public_key,
            &ephemeral_public.to_bytes(),
            &expected,
            &handoff.identity_public_key,
        ));
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| io::Error::other("The receiving endpoint is unavailable"))?;
    let mut endpoint = runtime
        .block_on(Endpoint::<Frame>::bind(
            ALPN,
            MAX_FRAME,
            QUEUE,
            Frame::decode,
        ))
        .map_err(|_| io::Error::other("The receiving endpoint is unavailable"))?;
    let reply_ticket = endpoint.ticket().to_owned();
    let source = endpoint
        .connect(&handoff.ticket, QUEUE)
        .map_err(|_| io::Error::other("The preparing computer is unavailable"))?;
    let device_public_key = device_cert.device_public_key;
    let message = proof_bytes(
        &handoff.operation_id,
        &reply_ticket,
        handoff.identity_public_key,
        &handoff.package_sha256,
        handoff.expires_at_ns,
        device_cert.device_public_key,
        ephemeral_public.to_bytes(),
    )?;
    let signature = signing.sign(&message).to_bytes();
    let request = Frame::Request {
        schema: super::SCHEMA,
        operation_id: handoff.operation_id.clone(),
        reply_ticket,
        identity_public_key: handoff.identity_public_key,
        package_sha256: handoff.package_sha256.clone(),
        expires_at_ns: handoff.expires_at_ns,
        device_cert: Box::new(device_cert),
        ephemeral_public_key: ephemeral_public.to_bytes(),
        signature: signature.to_vec(),
    };
    send(&source, &request, cancel)?;
    let mut last_request = std::time::Instant::now();
    let mut password: Option<Zeroizing<String>> = None;
    let mut buffer = Vec::new();
    let mut next = 0_usize;
    let mut total = 0_usize;
    let mut end_seen = false;
    let mut delivered = false;
    loop {
        if cancel.load(Ordering::Acquire) {
            // A dropped receiver can still hold the one acknowledgement the
            // canonical importer just produced. Send its receipt before
            // leaving; without it the source would wait forever for a stage
            // the destination actually verified.
            if delivered && let Ok(imported) = acknowledge.try_recv() {
                if imported != expected {
                    return Err(invalid("Import receipt is for another package"));
                }
                let package_sha256 = hex::encode(expected);
                let message = receipt_bytes(
                    &handoff.operation_id,
                    &package_sha256,
                    ReceiptStage::PreparedPackageImported,
                )?;
                let signature = signing.sign(&message).to_bytes().to_vec();
                send_final(
                    &source,
                    &Frame::Receipt {
                        operation_id: handoff.operation_id.clone(),
                        package_sha256,
                        stage: ReceiptStage::PreparedPackageImported,
                        device_public_key,
                        signature,
                    },
                )?;
                std::thread::sleep(Duration::from_millis(500));
                let _ = state.send(ReceiverState::Complete);
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Handoff cancelled",
            ));
        }
        if now_ns() >= handoff.expires_at_ns {
            return Err(invalid("This other-computer handoff expired"));
        }
        while let Some(frame) = endpoint.try_recv() {
            if delivered {
                continue;
            }
            match frame {
                Frame::Approved {
                    operation_id,
                    total: announced,
                    package_bytes,
                    package_sha256,
                    sealed_secret,
                } => {
                    if password.is_some() {
                        continue;
                    }
                    if operation_id != handoff.operation_id
                        || package_sha256 != handoff.package_sha256
                        || package_bytes != handoff.package_bytes
                        || announced == 0
                        || u64::from(announced)
                            > MAX_PACKAGE.div_ceil(CHUNK as u64).saturating_add(1)
                    {
                        return Err(invalid("The source approved a different package"));
                    }
                    let sealed = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        sealed_secret,
                    )
                    .map_err(|_| invalid("The source sent an invalid key"))?;
                    let opened = hpke::open_bytes(
                        &ephemeral,
                        Label::MigrationPackage,
                        &handoff_aad(
                            &handoff.operation_id,
                            &expected,
                            &handoff.identity_public_key,
                        ),
                        &sealed,
                    )
                    .map_err(|_| invalid("The source key did not open"))?;
                    let text = std::str::from_utf8(&opened)
                        .map_err(|_| invalid("The source sent an invalid key"))?;
                    if text.is_empty() || text.len() > 4096 {
                        return Err(invalid("The source sent an invalid key"));
                    }
                    password = Some(Zeroizing::new(text.to_owned()));
                    total = announced as usize;
                }
                Frame::Chunk {
                    operation_id,
                    index,
                    data,
                    sha256,
                } => {
                    if operation_id != handoff.operation_id || password.is_none() {
                        continue;
                    }
                    if (index as usize) < next {
                        // The acknowledgement was lost; repeat it instead of
                        // failing a transfer whose bytes already arrived.
                        send(
                            &source,
                            &Frame::ChunkAck {
                                operation_id: handoff.operation_id.clone(),
                                index,
                            },
                            cancel,
                        )?;
                        continue;
                    }
                    if index as usize != next {
                        return Err(invalid("The source sent chunks out of order"));
                    }
                    let data =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
                            .map_err(|_| invalid("The source sent an invalid chunk"))?;
                    if data.is_empty()
                        || data.len() > CHUNK
                        || digest(&data) != decode_hex::<32>(&sha256)?
                        || buffer.len().saturating_add(data.len()) as u64 > handoff.package_bytes
                    {
                        return Err(invalid("The source sent an invalid chunk"));
                    }
                    buffer.extend_from_slice(&data);
                    send(
                        &source,
                        &Frame::ChunkAck {
                            operation_id: handoff.operation_id.clone(),
                            index,
                        },
                        cancel,
                    )?;
                    next = next.saturating_add(1);
                }
                Frame::End {
                    operation_id,
                    package_sha256,
                } if operation_id == handoff.operation_id
                    && package_sha256 == handoff.package_sha256 =>
                {
                    end_seen = true;
                }
                Frame::Rejected { operation_id, .. } if operation_id == handoff.operation_id => {
                    return Err(invalid("The preparing computer refused the transfer"));
                }
                _ => {}
            }
        }
        if password.is_none() && last_request.elapsed() > RESEND {
            // The source cannot show an approval request until its package is
            // ready. Repeat the same signed request rather than waiting on a
            // single frame the source may have seen too early.
            send(&source, &request, cancel)?;
            last_request = std::time::Instant::now();
        }
        if !delivered && end_seen {
            if next != total {
                return Err(invalid("The source ended before every chunk arrived"));
            }
            let bytes = std::mem::take(&mut buffer);
            if bytes.len() as u64 != handoff.package_bytes || digest(&bytes) != expected {
                return Err(invalid("The prepared package did not verify"));
            }
            let password = password
                .take()
                .ok_or_else(|| invalid("The prepared package key is missing"))?;
            let path = write_package(package_directory, handoff, &bytes, &expected)?;
            let _ = state.send(ReceiverState::Received(Box::new(Received {
                password,
                package_sha256: expected,
                sealed_bytes: bytes.len() as u64,
                package_path: path,
            })));
            delivered = true;
        }
        if delivered {
            match acknowledge.try_recv() {
                Ok(imported) if imported == expected => {
                    let package_sha256 = hex::encode(expected);
                    let message = receipt_bytes(
                        &handoff.operation_id,
                        &package_sha256,
                        ReceiptStage::PreparedPackageImported,
                    )?;
                    let signature = signing.sign(&message).to_bytes().to_vec();
                    send_final(
                        &source,
                        &Frame::Receipt {
                            operation_id: handoff.operation_id.clone(),
                            package_sha256,
                            stage: ReceiptStage::PreparedPackageImported,
                            device_public_key,
                            signature,
                        },
                    )?;
                    // `try_send` only queues the receipt. Keep the peer and
                    // its runtime alive long enough for the writer to flush
                    // it before the endpoint is dropped.
                    std::thread::sleep(Duration::from_millis(500));
                    let _ = state.send(ReceiverState::Complete);
                    return Ok(());
                }
                Ok(_) => return Err(invalid("Import receipt is for another package")),
                Err(_) => {}
            }
        }
        std::thread::sleep(POLL);
    }
}

fn load_identity(
    identity: &ReceiverIdentity,
) -> io::Result<(DeviceCert, ed25519_dalek::SigningKey, [u8; 32])> {
    if !identity.identity_root.is_absolute() || !identity.identity_dir.is_absolute() {
        return Err(invalid("The receiving identity location is invalid"));
    }
    let configured = tilo_identity_host::provisioning::collaboration_identity_paths();
    let configured_root = configured
        .device_cert
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid("The receiving identity root is unavailable"))?;
    if identity.identity_root.canonicalize()? != configured_root.canonicalize()? {
        return Err(invalid(
            "The receiving identity root is not this device's configured root",
        ));
    }
    let directory = std::fs::symlink_metadata(&identity.identity_dir)?;
    if !directory.is_dir() || directory.uid() != current_uid() || directory.mode() & 0o077 != 0 {
        return Err(invalid("The receiving identity directory is not private"));
    }
    let active = runtime_identity::resolve_active_identity(&identity.identity_dir)
        .unwrap_or_else(|| identity.identity_dir.join("primary.json"));
    let signer = runtime_identity::load_from_path(active)
        .map_err(|_| invalid("The receiving computer has no real identity yet"))?;
    let person = signer.group_public_key();
    let paths = tilo_user_identity::persist::IdentityPaths::under(&identity.identity_root);
    let signing = tilo_user_identity::persist::load_device_key(&paths)
        .map_err(|_| invalid("The receiving computer has no device key"))?;
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
        .or_else(|_| {
            tilo_user_identity::persist::read_device_cert(&paths.device_cert_for_user(&person))
        })
        .map_err(|_| invalid("The receiving computer has no device certificate"))?;
    tilo_user_identity::verify_device_cert_at(&cert, now_ns())
        .map_err(|_| invalid("The receiving computer's device certificate is not valid"))?;
    if cert.user_public_key != person
        || cert.device_public_key != signing.verifying_key().to_bytes()
    {
        return Err(invalid("The device certificate names another key"));
    }
    Ok((cert, signing, person))
}

fn open_child(directory: &File, name: &str, flags: i32) -> io::Result<File> {
    let name = CString::new(name).map_err(|_| invalid("Prepared setup directory is invalid"))?;
    // SAFETY: the name is terminated and `directory` is a live directory
    // descriptor; openat returns a newly owned descriptor on success.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful openat returned a newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn create_child(directory: &File, name: &str) -> io::Result<File> {
    let encoded = CString::new(name).map_err(|_| invalid("Prepared setup directory is invalid"))?;
    // SAFETY: the name is terminated and `directory` is a live directory
    // descriptor; mkdirat creates exactly one child.
    if unsafe { libc::mkdirat(directory.as_raw_fd(), encoded.as_ptr(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    directory.sync_all()?;
    open_child(directory, name, libc::O_RDONLY | libc::O_DIRECTORY)
}

fn owned_directory(file: &File, private: bool) -> io::Result<()> {
    let metadata = file.metadata()?;
    let forbidden = if private { 0o077 } else { 0o022 };
    if !metadata.is_dir() || metadata.uid() != current_uid() || metadata.mode() & forbidden != 0 {
        return Err(invalid("The prepared setup directory is not private"));
    }
    Ok(())
}

fn open_state_directory(directory: &Path) -> io::Result<StorageDirectory> {
    if !directory.is_absolute() || !directory.ends_with(".local/state/tilo/prepared-setup") {
        return Err(invalid("The prepared setup directory is invalid"));
    }
    let home = directory
        .ancestors()
        .nth(4)
        .ok_or_else(|| invalid("The prepared setup directory is invalid"))?;
    let home = home.canonicalize()?;
    let mut pin = File::options()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&home)?;
    owned_directory(&pin, false)?;
    for (index, segment) in [".local", "state", "tilo", "prepared-setup"]
        .into_iter()
        .enumerate()
    {
        pin = match open_child(&pin, segment, libc::O_RDONLY | libc::O_DIRECTORY) {
            Ok(next) => next,
            Err(error) if error.kind() == io::ErrorKind::NotFound => create_child(&pin, segment)?,
            Err(error) => return Err(error),
        };
        owned_directory(&pin, index == 3)?;
    }
    StorageDirectory::from_file(pin)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HandoffBinding {
    schema: u32,
    operation_id: String,
    identity_public_key: [u8; 32],
    package_sha256: String,
    sealed_bytes: u64,
}

impl HandoffBinding {
    fn matches(&self, handoff: &PublicHandoff) -> bool {
        self.schema == 1
            && self.operation_id == handoff.operation_id
            && self.identity_public_key == handoff.identity_public_key
            && self.package_sha256 == handoff.package_sha256
            && self.sealed_bytes == handoff.package_bytes
    }
}

fn write_package(
    directory: &Path,
    handoff: &PublicHandoff,
    bytes: &[u8],
    sha256: &[u8; 32],
) -> io::Result<PathBuf> {
    let path = directory.to_owned();
    let directory = open_state_directory(directory)?;
    let binding = HandoffBinding {
        schema: 1,
        operation_id: handoff.operation_id.clone(),
        identity_public_key: handoff.identity_public_key,
        package_sha256: handoff.package_sha256.clone(),
        sealed_bytes: handoff.package_bytes,
    };
    let binding =
        serde_json::to_vec(&binding).map_err(|_| invalid("Handoff binding is invalid"))?;
    match directory.open_read(OsStr::new(BINDING), Privacy::OwnerOnly) {
        Ok(file) => {
            let mut existing = Vec::new();
            file.take(4097).read_to_end(&mut existing)?;
            let existing: HandoffBinding = serde_json::from_slice(&existing)
                .map_err(|_| invalid("Another prepared setup is already waiting"))?;
            if !existing.matches(handoff) {
                return Err(invalid("Another prepared setup is already waiting"));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            for name in [PACKAGE, RECEIPT] {
                match directory.open_read(OsStr::new(name), Privacy::OwnerOnly) {
                    Ok(_) => return Err(invalid("Another prepared setup is already waiting")),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            directory
                .publish(
                    OsStr::new(BINDING),
                    &binding,
                    PublishMode::IfAbsent,
                    Privacy::OwnerOnly,
                )
                .map_err(tilo_files::native::atomic::PublicationError::into_io_error)?;
        }
        Err(error) => return Err(error),
    }
    directory
        .publish(
            OsStr::new(PACKAGE),
            bytes,
            PublishMode::Replace,
            Privacy::OwnerOnly,
        )
        .map_err(tilo_files::native::atomic::PublicationError::into_io_error)?;
    let receipt = serde_json::json!({
        "schema": 1,
        "sealed_bytes": bytes.len() as u64,
        "ciphertext_sha256": hex::encode(sha256),
    });
    directory
        .publish(
            OsStr::new(RECEIPT),
            &serde_json::to_vec(&receipt).map_err(io::Error::other)?,
            PublishMode::Replace,
            Privacy::OwnerOnly,
        )
        .map_err(tilo_files::native::atomic::PublicationError::into_io_error)?;
    Ok(path.join(PACKAGE))
}
