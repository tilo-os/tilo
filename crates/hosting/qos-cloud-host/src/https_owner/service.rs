//! Installed Cloud HTTPS delivery: one serialized actor over the actual Cloud
//! Store. Every request is admitted with its original proof inside a fresh,
//! reserved Store turn; no registry or cache substitutes for that owner.
use super::{
    CloudDeliveryError, CloudStoreSource, CloudStoreTurn, Current, DeliveryOperation,
    live::InstalledLive, media::CloudMediaService,
};
use anyhow::{Context as _, Result, anyhow, bail, ensure};
use context::Pins;
use ed25519_dalek::{Signature, Signer, VerifyingKey};
use qos_relay::delivery_v3::{
    auth::Authority,
    capabilities::{PRODUCT_DATA_V1, validate},
    descriptor,
    http_proof::{Request, RequestProof},
    media_wire::{self, BootstrapRequest, BootstrapResponse, ControlReply},
};
use qos_session_peer::ingress::{
    BufferedRequest, CarrierRegistry, HostBytes, HostWorkInventory, Route,
};
use reqwest::StatusCode;
use std::sync::Arc;
use tokio::time::{Duration, Instant};

mod context;
mod data;
mod onboarding;
mod private;
pub(crate) use private::agent_rejection_code;
mod registries;
mod snapshots;
#[cfg(test)]
mod tests;
mod upload;

#[cfg(test)]
pub(in crate::https_owner) use private::{ObserveInvocation, Watched, observe_transitions};
pub(crate) use private::{PrivateLane, spawn_private_broker};
use registries::Registries;

const BOOTSTRAP_NS: u64 = 30_000_000_000;
const IDLE_TICK: Duration = Duration::from_millis(250);
const BUSY_TICK: Duration = Duration::from_millis(20);
/// Bounded wait for a contended Store lock held by another Cloud component.
const BUSY_RETRY: Duration = Duration::from_millis(2);
/// Installation may wait this many retries (about 2 s) for the lock.
const BUSY_LIMIT: u32 = 1000;
/// Serving waits only this many retries in line: the service loop is one
/// serialized actor, so a longer wait would stall every other request. A
/// request that still finds the Store busy is deferred (`Deferred`) and
/// retried on the short tick without blocking the loop.
const BUSY_INLINE: u32 = 3;
/// Longest a deferred request waits for the Store lock before its retryable
/// 503, and the most requests deferred at once.
const DEFER_LIMIT: Duration = Duration::from_secs(4);
const DEFER_MARGIN: Duration = Duration::from_millis(250);
const MAX_DEFERRED: usize = 64;

/// Rate-limited operational log lines (fix plan B7): at most one line per
/// reason every `LOG_INTERVAL`, with the count of suppressed repeats. Only a
/// fixed reason, route or error category is logged, never request content.
pub(super) mod ops_log {
    use std::{sync::Mutex, time::Instant};
    const LOG_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
    static SEEN: Mutex<Vec<(&'static str, Instant, u64)>> = Mutex::new(Vec::new());
    /// Returns the suppressed count when this reason may log now.
    pub(in crate::https_owner) fn admit(reason: &'static str) -> Option<u64> {
        let mut seen = SEEN.lock().ok()?;
        let now = Instant::now();
        match seen.iter_mut().find(|(key, ..)| *key == reason) {
            Some((_, last, suppressed)) if now.duration_since(*last) < LOG_INTERVAL => {
                *suppressed += 1;
                None
            }
            Some((_, last, suppressed)) => {
                *last = now;
                Some(std::mem::take(suppressed))
            }
            None => {
                seen.push((reason, now, 0));
                Some(0)
            }
        }
    }
}
/// How often maintenance checks delivery retention budgets.
const RETENTION_INTERVAL: Duration = Duration::from_secs(30);

/// Test-only: stores whose service loop gets no idle tick (60 s instead of
/// 250 ms), so a completion is answered only through its wake-up event.
#[cfg(test)]
pub(super) mod quiet_ticks {
    use std::{collections::BTreeSet, path::PathBuf, sync::Mutex};
    static QUIET: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
    pub(in crate::https_owner) fn quiet(root: &std::path::Path) {
        QUIET.lock().unwrap().insert(root.to_owned());
    }
    pub(in crate::https_owner) fn is_quiet(root: &std::path::Path) -> bool {
        QUIET.lock().unwrap().contains(root)
    }
    /// Service loop wake-ups (each runs `maintain`) and their total time.
    static WAKES: Mutex<Vec<(PathBuf, u64, std::time::Duration)>> = Mutex::new(Vec::new());
    pub(in crate::https_owner) fn count(root: &std::path::Path, spent: std::time::Duration) {
        if !is_quiet(root) {
            return;
        }
        let mut wakes = WAKES.lock().unwrap();
        match wakes.iter_mut().find(|(path, ..)| path == root) {
            Some((_, count, total)) => {
                *count += 1;
                *total += spent;
            }
            None => wakes.push((root.to_owned(), 1, spent)),
        }
    }
    pub(in crate::https_owner) fn wakes(root: &std::path::Path) -> (u64, std::time::Duration) {
        WAKES
            .lock()
            .unwrap()
            .iter()
            .find(|(path, ..)| path == root)
            .map_or((0, std::time::Duration::ZERO), |(_, count, total)| {
                (*count, *total)
            })
    }
}

/// Test-only retention trigger keyed by origin (see `forced_renewal`).
#[cfg(test)]
pub(super) mod forced_rotation {
    use std::{collections::BTreeSet, sync::Mutex};
    static FORCED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    pub(in crate::https_owner) fn request(origin: &str) {
        FORCED.lock().unwrap().insert(origin.to_owned());
    }
    pub(in crate::https_owner) fn take(origin: &str) -> bool {
        FORCED.lock().unwrap().remove(origin)
    }
}
fn rotation_forced(_origin: &str) -> bool {
    #[cfg(test)]
    return forced_rotation::take(_origin);
    #[cfg(not(test))]
    false
}

/// Which data routes are actually composed into this installed service. The
/// descriptor advertises exactly the capability bits these routes cover.
#[derive(Clone, Copy)]
pub(crate) struct Installed {
    pub(crate) state: bool,
    pub(crate) resumable: bool,
    pub(crate) snapshots: bool,
    pub(crate) private_commands: bool,
    pub(crate) private_reads: bool,
}
impl Installed {
    pub(crate) fn capabilities(self) -> u64 {
        use qos_relay::delivery_v3::capabilities::*;
        // Every data bit depends on the durable replay/consumer lane.
        if !self.state {
            return 0;
        }
        let mut mask = STATE_REPLAY_V1;
        if self.private_commands {
            mask |= PRIVATE_COMMANDS_V1;
        }
        if self.private_reads {
            mask |= PRIVATE_READS_V1;
        }
        if self.snapshots {
            mask |= SNAPSHOT_V1;
        }
        if self.resumable {
            mask |= RESUMABLE_STATE_V1;
        }
        mask
    }
}

/// The HTTPS carrier charges and bounds a response by its allocation
/// capacity (at most 1 MiB), not its length. Encoders and page builders grow
/// their vectors by doubling, so a body above ~512 KiB (a membership
/// bootstrap of a grown workspace, a full data page) could carry up to twice
/// its length in capacity, be refused by the carrier and reach the client as
/// a retryable 503 on every attempt. Every OK body is sent at exact capacity.
pub(super) fn exact_body(body: Vec<u8>) -> Vec<u8> {
    if body.capacity() == body.len() {
        return body;
    }
    body.into_boxed_slice().into_vec()
}
fn refuse(request: BufferedRequest) {
    let _ = request.respond(StatusCode::FORBIDDEN, b"permission-denied".to_vec());
}
/// The Store lock stayed held by another Cloud component for the whole
/// bounded wait. Retryable, never an authorization answer.
#[derive(Debug)]
pub(super) struct OwnerBusy;
impl std::fmt::Display for OwnerBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cloud owner busy")
    }
}
impl std::error::Error for OwnerBusy {}
/// The Store was busy after part of a request already took effect: still a
/// retryable 503, but the request is never re-dispatched as a whole.
#[derive(Debug)]
pub(super) struct OwnerBusyAfterEffect;
impl std::fmt::Display for OwnerBusyAfterEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cloud owner busy after a committed part")
    }
}
impl std::error::Error for OwnerBusyAfterEffect {}
/// Whether `error` is exactly the in-line busy refusal of a turn whose action
/// never ran (so the unchanged request may be dispatched again).
pub(super) fn deferrable(error: &anyhow::Error) -> bool {
    !after_effect(error)
        && error
            .chain()
            .any(|cause| cause.downcast_ref::<OwnerBusy>().is_some())
}
/// `OwnerBusyAfterEffect` is attached as context (anyhow resolves it only
/// through its own downcast, not through the source chain).
fn after_effect(error: &anyhow::Error) -> bool {
    error.downcast_ref::<OwnerBusyAfterEffect>().is_some()
}
/// Capacity and contention refusals are retryable 503s; everything else
/// (authentication, authorization, invalid input) stays a closed 403.
pub(super) fn transient(error: &anyhow::Error) -> bool {
    after_effect(error)
        || error.chain().any(|cause| {
            cause.downcast_ref::<OwnerBusy>().is_some()
                || matches!(
                    cause.downcast_ref::<CloudDeliveryError>(),
                    Some(CloudDeliveryError::Capacity | CloudDeliveryError::Unavailable)
                )
        })
}
pub(super) fn refuse_error(request: BufferedRequest, error: &anyhow::Error) {
    if transient(error) {
        let reason = if after_effect(error)
            || error
                .chain()
                .any(|cause| cause.downcast_ref::<OwnerBusy>().is_some())
        {
            "owner_busy"
        } else {
            "capacity"
        };
        if let Some(suppressed) = ops_log::admit(reason) {
            tracing::warn!(
                event = "cloud_request_unavailable",
                reason,
                route = ?request.route(),
                suppressed,
                "Cloud request answered 503"
            );
        }
        let _ = request.respond(
            StatusCode::SERVICE_UNAVAILABLE,
            b"temporarily-unavailable".to_vec(),
        );
    } else {
        refuse(request);
    }
}
fn delivery(error: CloudDeliveryError) -> anyhow::Error {
    error.into()
}

/// Run one reserved turn, waiting only briefly (`BUSY_INLINE`) while another
/// Cloud component holds the Store lock; then `OwnerBusy`, with the action not
/// run. A denied source never resumes.
fn with_turn<R>(
    source: &CloudStoreSource,
    pool: &HostBytes,
    operation: DeliveryOperation,
    action: impl FnOnce(&mut CloudStoreTurn) -> Result<R>,
) -> Result<R> {
    with_turn_waiting(source, pool, operation, BUSY_INLINE, action)
}
/// `with_turn` with an explicit bounded number of lock retries.
fn with_turn_waiting<R>(
    source: &CloudStoreSource,
    pool: &HostBytes,
    operation: DeliveryOperation,
    retries: u32,
    action: impl FnOnce(&mut CloudStoreTurn) -> Result<R>,
) -> Result<R> {
    let mut action = Some(action);
    for attempt in 0..=retries {
        if attempt > 0 {
            std::thread::sleep(BUSY_RETRY);
        }
        let result = source.with_delivery_turn(pool, operation, |turn| {
            let action = action.take().expect("invoked once");
            Ok(action(turn))
        });
        match result {
            Current::Current(Ok(result)) => return result,
            Current::Current(Err(error)) => return Err(delivery(error)),
            Current::Denied => bail!("Cloud owner denied"),
            Current::Busy => {}
        }
    }
    Err(OwnerBusy.into())
}
fn with_state_turn<R>(
    source: &CloudStoreSource,
    pool: &HostBytes,
    input_len: usize,
    action: impl FnOnce(&mut CloudStoreTurn) -> Result<R>,
) -> Result<R> {
    let mut action = Some(action);
    for attempt in 0..=BUSY_INLINE {
        if attempt > 0 {
            std::thread::sleep(BUSY_RETRY);
        }
        let result = source.with_state_turn(pool, input_len, |turn| {
            let action = action.take().expect("invoked once");
            Ok(action(turn))
        });
        match result {
            Current::Current(Ok(result)) => return result,
            Current::Current(Err(error)) => return Err(delivery(error)),
            Current::Denied => bail!("Cloud owner denied"),
            Current::Busy => {}
        }
    }
    Err(OwnerBusy.into())
}

/// A request that found the Store busy before any effect, held for retry.
struct Deferred {
    request: BufferedRequest,
    since: Instant,
}

pub(crate) struct CloudService {
    source: CloudStoreSource,
    pool: HostBytes,
    work: HostWorkInventory,
    origin: String,
    installed: Installed,
    media_mask: u64,
    registry: Registries,
    pins: Pins,
    descriptor_expires_ns: u64,
    // Exact signed public hint of the installed descriptor, as persisted.
    descriptor_hint: Vec<u8>,
    polls: data::Polls,
    uploads: upload::Uploads,
    snapshots: snapshots::Snapshots,
    onboarding: onboarding::Onboarding,
    private: Option<PrivateLane>,
    media: Option<Arc<CloudMediaService>>,
    /// LIVE_V1 authority and forwarder; `Some` only when the relay is
    /// installed on this host's QUIC endpoint (then the live bit is signed).
    live: Option<InstalledLive>,
    probe: Option<super::transaction::AggregateProbe>,
    retention_due: Instant,
    failed: bool,
    /// Raised by completed upload work and by the private broker; the serve
    /// loop waits on it besides requests and the (idle/deadline) timer.
    wake: Arc<tokio::sync::Notify>,
    /// Requests that found the Store busy, in arrival order (fix plan B3).
    deferred: std::collections::VecDeque<Deferred>,
}

impl CloudService {
    /// Install the signed descriptor through the actual Store barrier and bind
    /// a fresh registry to it. Only composed routes contribute capability bits.
    pub(crate) fn install(
        source: CloudStoreSource,
        origin: &str,
        pool: HostBytes,
        work: HostWorkInventory,
        private: Option<PrivateLane>,
        media: Option<Arc<CloudMediaService>>,
        live: Option<InstalledLive>,
    ) -> Result<Self> {
        ensure!(
            work.same_pool(&pool),
            "host work inventory uses another pool"
        );
        let installed = Installed {
            state: true,
            resumable: true,
            snapshots: true,
            private_commands: private.is_some(),
            private_reads: private.is_some(),
        };
        let capabilities = installed.capabilities();
        ensure!(validate(capabilities), "unknown data capability");
        let media_mask = if media.is_some() {
            descriptor::MEDIA_VOICE_V1
        } else {
            0
        } | if live.is_some() {
            qos_relay::delivery_v3::live_wire::MEDIA_LIVE_V1
        } else {
            0
        };
        let (registry, pins, generation, expires, hint) =
            Self::bind_registry(&source, &pool, origin, capabilities, media_mask, BUSY_LIMIT)?;
        let registry = Registries::new(registry, pins.descriptor, generation);
        let onboarding = onboarding::Onboarding::new(pins.boot, pool.clone())?;
        let wake = private.as_ref().map(PrivateLane::wake).unwrap_or_default();
        let mut service = Self {
            source,
            pool,
            work,
            origin: origin.into(),
            installed,
            media_mask,
            registry,
            pins,
            descriptor_expires_ns: expires,
            descriptor_hint: hint,
            polls: data::Polls::new(),
            uploads: upload::Uploads::new(wake.clone()),
            snapshots: snapshots::Snapshots::new(),
            onboarding,
            private,
            media,
            live,
            probe: None,
            retention_due: Instant::now(),
            failed: false,
            wake,
            deferred: std::collections::VecDeque::new(),
        };
        service.snapshots.recover_waiting(
            &service.source,
            &service.pool,
            &service.work,
            BUSY_LIMIT,
        )?;
        service.probe = service.source.aggregate_probe();
        Ok(service)
    }
    fn bind_registry(
        source: &CloudStoreSource,
        pool: &HostBytes,
        origin: &str,
        capabilities: u64,
        media: u64,
        retries: u32,
    ) -> Result<(CarrierRegistry, Pins, u64, u64, Vec<u8>)> {
        let (verified, expires, now, hint) = with_turn_waiting(
            source,
            pool,
            DeliveryOperation::Maintenance,
            retries,
            |turn| {
                let (verified, expires) = turn.install_descriptor(origin, capabilities, media)?;
                let hint = turn.descriptor_bytes(capabilities, verified.descriptor_digest())?;
                let now = turn.fresh_now_ns()?;
                Ok((verified, expires, now, hint))
            },
        )?;
        let registry = CarrierRegistry::new_unqualified(&verified, capabilities, now)?;
        let pins = Pins {
            boot: registry.boot_id(),
            descriptor: verified.descriptor_digest(),
            capabilities,
        };
        Ok((registry, pins, verified.generation(), expires, hint))
    }
    /// Negotiable data capabilities of this exact installed service.
    pub(crate) fn capabilities(&self) -> u64 {
        self.pins.capabilities
    }
    pub(crate) fn product_ready(&self) -> bool {
        !self.failed && self.pins.capabilities == PRODUCT_DATA_V1
    }
    pub(crate) fn descriptor_digest(&self) -> [u8; 32] {
        self.pins.descriptor
    }
    pub(crate) fn origin(&self) -> &str {
        &self.origin
    }
    /// Denied owner, or an advertised private lane whose broker stopped: the
    /// negotiated capability would no longer be genuinely installed.
    /// Which `is_failed` condition holds (content-free, for the stop log).
    pub(crate) fn failure_cause(&self) -> &'static str {
        if self.source.is_denied() {
            "source denied"
        } else if self.failed {
            "owner failed"
        } else {
            "private lane closed"
        }
    }
    pub(crate) fn is_failed(&self) -> bool {
        self.failed
            || self.source.is_denied()
            || (self.installed.private_commands
                && self
                    .private
                    .as_ref()
                    .is_none_or(|private| private.is_closed()))
    }

    pub(crate) fn handle(&mut self, request: BufferedRequest) {
        if let Some(request) = self.dispatch(request) {
            self.defer(request, Instant::now());
        }
    }
    /// Hold a request whose Store turn was busy before any effect. Bounded;
    /// beyond the bound (or its time) it is answered with the retryable 503.
    fn defer(&mut self, request: BufferedRequest, since: Instant) {
        let expired = since.elapsed() >= DEFER_LIMIT
            || request
                .deadline()
                .checked_sub(DEFER_MARGIN)
                .is_none_or(|last| Instant::now() >= last);
        if expired || self.deferred.len() >= MAX_DEFERRED {
            refuse_error(request, &OwnerBusy.into());
            return;
        }
        self.deferred.push_back(Deferred { request, since });
    }
    /// Retry deferred requests in order until one still finds the Store busy.
    fn retry_deferred(&mut self) {
        let mut pending = std::mem::take(&mut self.deferred);
        while let Some(Deferred { request, since }) = pending.pop_front() {
            if request.is_cancelled() {
                continue;
            }
            if let Some(request) = self.dispatch(request) {
                self.defer(request, since);
                // Still busy: keep the rest, in order, for the next tick.
                for rest in pending.drain(..) {
                    self.defer(rest.request, rest.since);
                }
            }
        }
    }
    /// Route one request. Returns it untouched when its Store turn was busy
    /// before any effect, so the caller can defer instead of answering.
    fn dispatch(&mut self, request: BufferedRequest) -> Option<BufferedRequest> {
        if request.is_cancelled() {
            return None;
        }
        if self.is_failed() {
            let (status, body) = if request.route() == Route::Descriptor {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    b"temporarily-unavailable".to_vec(),
                )
            } else {
                (StatusCode::FORBIDDEN, b"permission-denied".to_vec())
            };
            let _ = request.respond(status, body);
            return None;
        }
        match request.route() {
            Route::State => {
                if upload::partial_chunk(request.body()).is_some() {
                    self.uploads.start(
                        &self.source,
                        &self.pool,
                        &self.work,
                        &mut self.registry,
                        &self.pins,
                        request,
                    );
                    None
                } else {
                    self.respond(request, |service, request| service.state(request).map(Some))
                }
            }
            Route::EventsPoll => self.poll(request),
            Route::Acks => self.respond(request, |service, request| {
                service.ack(request).map(|()| Some(Vec::new()))
            }),
            Route::Detach => self.respond(request, |service, request| {
                service.detach(request).map(Some)
            }),
            // Snapshot work spans several turns; a busy Store answers 503.
            Route::Snapshot => self.respond_once(request, |service, request| {
                service
                    .snapshots
                    .request(
                        &service.source,
                        &service.pool,
                        &service.work,
                        &mut service.registry,
                        &service.pins,
                        request,
                    )
                    .map(Some)
            }),
            Route::SnapshotChunk => self.respond_once(request, |service, request| {
                service
                    .snapshots
                    .chunk(
                        &service.source,
                        &service.pool,
                        &mut service.registry,
                        &service.pins,
                        request,
                    )
                    .map(Some)
            }),
            Route::Private(_) | Route::PrivatePoll => match self.private.as_mut() {
                Some(private) => private.handle(
                    &self.source,
                    &self.pool,
                    &mut self.registry,
                    &self.pins,
                    request,
                ),
                None => {
                    refuse(request);
                    None
                }
            },
            Route::Descriptor => {
                let result = self.descriptor();
                let (status, body) = match result {
                    Ok(body) => (StatusCode::OK, body),
                    Err(_) => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        b"temporarily-unavailable".to_vec(),
                    ),
                };
                let _ = request.respond(status, body);
                None
            }
            Route::Bootstrap => self.respond(request, |service, request| {
                service.bootstrap(request.body()).map(Some)
            }),
            Route::Challenge => self.respond(request, |service, request| {
                service.challenge(request.body()).map(Some)
            }),
            Route::Sessions => self.respond(request, |service, request| {
                service.sessions(request.body()).map(Some)
            }),
            Route::Media(_) => {
                self.respond(request, |service, request| service.media(request).map(Some))
            }
            // Without an installed relay, 404 tells clients to stay on the
            // durable path (no live bit is advertised either).
            Route::LiveGrant if self.live.is_none() => {
                let _ = request.respond(StatusCode::NOT_FOUND, b"unsupported".to_vec());
                None
            }
            Route::LiveGrant => self.respond(request, |service, request| {
                service.live_grant(request).map(Some)
            }),
            Route::Onboarding(_) | Route::OnboardingChunk => {
                self.onboarding
                    .handle(&self.source, &self.pool, &self.pins, request)
            }
        }
    }
    /// Answer from `handler`; a busy Store before any effect hands the
    /// request back for deferral instead of answering.
    fn respond(
        &mut self,
        request: BufferedRequest,
        handler: impl FnOnce(&mut Self, &BufferedRequest) -> Result<Option<Vec<u8>>>,
    ) -> Option<BufferedRequest> {
        match handler(self, &request) {
            Ok(Some(body)) => {
                let _ = request.respond(StatusCode::OK, exact_body(body));
            }
            Ok(None) => refuse(request),
            Err(error) if deferrable(&error) => return Some(request),
            Err(error) => {
                tracing::debug!(%error, route = ?request.route(), "Cloud delivery request refused");
                refuse_error(request, &error)
            }
        }
        None
    }
    /// `respond` for handlers that are never re-dispatched.
    fn respond_once(
        &mut self,
        request: BufferedRequest,
        handler: impl FnOnce(&mut Self, &BufferedRequest) -> Result<Option<Vec<u8>>>,
    ) -> Option<BufferedRequest> {
        match handler(self, &request) {
            Ok(Some(body)) => {
                let _ = request.respond(StatusCode::OK, exact_body(body));
            }
            Ok(None) => refuse(request),
            Err(error) => {
                tracing::debug!(%error, route = ?request.route(), "Cloud delivery request refused");
                refuse_error(request, &error)
            }
        }
        None
    }

    /// Unauthenticated discovery serves the exact persisted signed hint bound
    /// at installation; clients verify it against their own current state.
    /// It is withdrawn at expiry until the maintenance renewal replaces it.
    fn descriptor(&self) -> Result<Vec<u8>> {
        ensure!(
            qos_relay::clock::now_ns() < self.descriptor_expires_ns,
            "descriptor expired"
        );
        Ok(self.descriptor_hint.clone())
    }

    /// Read-only governor-signed first contact; the Cloud authority is both
    /// the pinned governor and host, so its own Store key signs the response.
    fn bootstrap(&mut self, body: &[u8]) -> Result<Vec<u8>> {
        let request = BootstrapRequest::decode(body)?;
        let pins = self.pins;
        let registry = &mut self.registry;
        with_turn(
            &self.source,
            &self.pool,
            DeliveryOperation::Replay,
            |turn| {
                let context = turn.carrier_context(&pins, request.device, 0, 0)?;
                let now = context.now();
                registry.observe_time(now)?;
                let verification = context.verification();
                let host = turn.peer().authority();
                ensure!(
                    request.workspace == turn.workspace()
                        && request.root == host.user_public_key
                        && request.room == turn.peer().room()
                        && request.candidate_digest == pins.descriptor,
                    "bootstrap scope mismatch"
                );
                turn.authorize(
                    verification.binding,
                    verification.credentials.device,
                    host,
                    now,
                )?;
                VerifyingKey::from_bytes(&request.device)?.verify_strict(
                    &request.signing_digest()?,
                    &Signature::from_bytes(&media_wire::join_signature(request.signature)),
                )?;
                let key = turn.store.key();
                ensure!(
                    key.verifying_key().to_bytes() == host.device_public_key,
                    "governor custody mismatch"
                );
                let hint = turn
                    .store
                    .state
                    .delivery
                    .as_ref()
                    .and_then(|delivery| delivery.transport_hint())
                    .context("descriptor not installed")?;
                let metadata = descriptor::inspect_persisted_hint(hint, host)?;
                ensure!(
                    metadata.descriptor_digest == pins.descriptor,
                    "descriptor replaced"
                );
                let mut response = BootstrapResponse {
                    version: 1,
                    request,
                    host_certificate: postcard::to_stdvec(host)?,
                    hint: postcard::to_stdvec(hint)?,
                    generation: metadata.generation,
                    descriptor_digest: pins.descriptor,
                    issued_at_ns: now,
                    expires_at_ns: now
                        .checked_add(BOOTSTRAP_NS)
                        .context("bootstrap expiry overflow")?
                        .min(metadata.expires_at_ns),
                    signature: [[0; 32]; 2],
                };
                response.signature =
                    media_wire::split_signature(key.sign(&response.signing_digest()?).to_bytes());
                Ok(response.encode()?)
            },
        )
    }

    fn challenge(&mut self, body: &[u8]) -> Result<Vec<u8>> {
        let intent = CarrierRegistry::challenge_intent(body)?;
        let pins = self.pins;
        let registry = &mut self.registry;
        with_turn(
            &self.source,
            &self.pool,
            DeliveryOperation::Replay,
            |turn| {
                let context = turn.carrier_context(
                    &pins,
                    intent.device(),
                    intent.requested(),
                    intent.mandatory(),
                )?;
                let key = turn.store.key();
                registry.challenge_in(body, &context.verification(), &key)
            },
        )
    }

    /// Mutual authentication, then the durable consumer barrier, then Ready.
    /// Ready is published only from a context re-derived after that commit.
    fn sessions(&mut self, body: &[u8]) -> Result<Vec<u8>> {
        let intent = self.registry.finish_intent(body)?;
        let pins = self.pins;
        let registry = &mut self.registry;
        let (device, requested, mandatory) =
            (intent.device(), intent.requested(), intent.mandatory());
        let data = requested & pins.capabilities != 0;
        let operation = if data {
            DeliveryOperation::RegisterConsumer
        } else {
            DeliveryOperation::Replay
        };
        let ready = with_turn(&self.source, &self.pool, operation, |turn| {
            let finish = {
                let context = turn.carrier_context(&pins, device, requested, mandatory)?;
                registry.finish_in(body, &context.verification())?
            };
            if data {
                ensure!(
                    finish.session().capabilities() != 0,
                    "data session negotiated none"
                );
                turn.register_delivery_consumer(finish.session())
                    .map_err(delivery)?;
            } else {
                ensure!(
                    finish.session().capabilities() == 0,
                    "data consumer admission required"
                );
            }
            let context = turn.carrier_context(&pins, device, requested, mandatory)?;
            registry.publish_in(finish, &context.verification())
        })?;
        for retired in &ready.retired {
            self.fence_session(*retired);
        }
        Ok(ready.bytes)
    }

    fn fence_session(&mut self, id: [u8; 16]) {
        self.polls.fence(id);
        self.uploads.fence(id);
        if let Some(private) = self.private.as_mut() {
            private.retire_session(id);
        }
    }

    fn detach(&mut self, request: &BufferedRequest) -> Result<Vec<u8>> {
        let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
        let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
        let named = request
            .target()
            .strip_prefix("/peer/v3/sessions/")
            .and_then(|hex| hex::decode(hex).ok())
            .context("invalid detach target")?;
        let pins = self.pins;
        let registry = &mut self.registry;
        let id = with_turn(
            &self.source,
            &self.pool,
            DeliveryOperation::Replay,
            |turn| {
                let intent = registry.session_intent(proof.session_id())?;
                let context = turn.carrier_context(
                    &pins,
                    intent.device(),
                    intent.requested(),
                    intent.mandatory(),
                )?;
                let admitted = registry.admit_in(&proof, &exact, &context.verification())?;
                Ok(admitted.session().session_id())
            },
        )?;
        ensure!(named == id, "detach names another session");
        self.registry.retire_session(id);
        self.fence_session(id);
        Ok(Vec::new())
    }

    fn media(&mut self, request: &BufferedRequest) -> Result<Vec<u8>> {
        let media = self.media.clone().context("media service not installed")?;
        let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
        let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
        let pins = self.pins;
        let registry = &mut self.registry;
        with_turn(
            &self.source,
            &self.pool,
            DeliveryOperation::Replay,
            |turn| {
                let intent = registry.session_intent(proof.session_id())?;
                let context = turn.carrier_context(
                    &pins,
                    intent.device(),
                    intent.requested(),
                    intent.mandatory(),
                )?;
                let verification = context.verification();
                let binding = registry.binding_for(proof.session_id(), verification.binding)?;
                let verification = qos_relay::delivery_v3::auth::VerificationContext {
                    binding: &binding,
                    ..verification
                };
                let admitted = registry.admit_in(&proof, &exact, &verification)?;
                let result = media
                    .control_in(turn, &verification, &admitted)
                    .map_err(|error| anyhow!("media control refused: {error:?}"))?;
                Ok(ControlReply { version: 1, result }.encode()?)
            },
        )
    }

    /// Issue or renew a live grant for the proved data session, inside one
    /// reserved Store turn (current roster, role and host custody).
    fn live_grant(&mut self, request: &BufferedRequest) -> Result<Vec<u8>> {
        let live = self
            .live
            .as_ref()
            .context("live relay not installed")?
            .service
            .clone();
        let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
        let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
        let pins = self.pins;
        let registry = &mut self.registry;
        with_turn(
            &self.source,
            &self.pool,
            DeliveryOperation::Replay,
            |turn| {
                let intent = registry.session_intent(proof.session_id())?;
                let context = turn.carrier_context(
                    &pins,
                    intent.device(),
                    intent.requested(),
                    intent.mandatory(),
                )?;
                let admitted = registry.admit_in(&proof, &exact, &context.verification())?;
                drop(context);
                live.issue_in(turn, &admitted)
            },
        )
    }
    /// Forwarder for durably accepted inline items (never blocks; the durable
    /// path stays the source of truth).
    pub(super) fn live_forwarder(&self) -> Option<super::live::CloudLiveForwarder> {
        self.live.as_ref().map(|live| live.forwarder.clone())
    }

    /// LIVE_V1 lane C: the forwarder as a pre-barrier admission sink.
    pub(super) fn early_sink(&self) -> Option<crate::delivery::early_forward::EarlySink> {
        self.live_forwarder().map(|live| {
            std::sync::Arc::new(move |device: [u8; 32], canonical: &[u8]| {
                live.offer(device, canonical)
            }) as crate::delivery::early_forward::EarlySink
        })
    }

    /// Resume held work after any commit, near poll deadlines and on the tick.
    pub(crate) fn maintain(&mut self) {
        if self.is_failed() {
            for deferred in std::mem::take(&mut self.deferred) {
                refuse(deferred.request);
            }
            return;
        }
        self.retry_deferred();
        let sink = self.early_sink();
        let mut committed = crate::delivery::early_forward::scoped(sink.as_ref(), || {
            self.uploads
                .pump(&self.source, &self.pool, &mut self.registry, &self.pins)
        });
        if let Some(private) = self.private.as_mut() {
            private.pump(&self.source, &self.pool, &mut self.registry, &self.pins);
        }
        if let Err(error) = self
            .snapshots
            .maintain(&self.source, &self.pool, &self.work)
        {
            tracing::debug!(%error, "Cloud snapshot pin sweep deferred");
        }
        let forced = rotation_forced(&self.origin);
        if forced || Instant::now() >= self.retention_due {
            self.retention_due = Instant::now() + RETENTION_INTERVAL;
            match self.retain_delivery(forced) {
                Ok(retained) => committed |= retained,
                Err(error) if transient(&error) => {
                    // Busy Store: retry soon rather than after a full interval.
                    self.retention_due = Instant::now() + IDLE_TICK;
                    tracing::debug!(%error, "Cloud delivery retention deferred");
                }
                Err(error) => {
                    tracing::warn!(%error, "Cloud delivery retention deferred");
                    self.failed = self.source.is_denied();
                }
            }
        }
        if let Err(error) = self.renew_descriptor() {
            if transient(&error) {
                tracing::debug!(%error, "Cloud descriptor renewal deferred: owner busy");
            } else {
                tracing::warn!(%error, "Cloud descriptor renewal failed");
            }
            self.failed = self.source.is_denied();
        }
        let probe = self.source.aggregate_probe();
        if probe != self.probe {
            self.probe = probe;
            committed = true;
        }
        let due = self
            .polls
            .next_deadline()
            .is_some_and(|deadline| deadline <= Instant::now());
        if committed {
            if let Some(private) = self.private.as_ref() {
                private.notify_commit();
            }
        }
        if committed || due || self.polls.needs_retry() {
            // `self.probe` stays the observation taken before this wake: a
            // commit landing while held polls replay (another component's
            // worker, an agent step) changes the probe again and wakes them
            // on the next tick. Re-observing here would swallow it and leave
            // every held poll parked until an unrelated commit or its idle
            // deadline. Our own page/cursor turns never commit.
            self.polls
                .wake(&self.source, &self.pool, &mut self.registry, &self.pins);
        }
    }
    fn renew_descriptor(&mut self) -> Result<()> {
        let now = qos_relay::clock::now_ns();
        self.registry.prune(now);
        if self.descriptor_expires_ns.saturating_sub(now) > context::RENEW_MARGIN_NS
            && !context::renewal_forced(&self.origin)
        {
            return Ok(());
        }
        let (registry, pins, generation, expires, hint) = Self::bind_registry(
            &self.source,
            &self.pool,
            &self.origin,
            self.installed.capabilities(),
            self.media_mask,
            BUSY_INLINE,
        )?;
        if pins.descriptor == self.pins.descriptor {
            // Another holder already renewed within the margin; nothing changed.
            self.descriptor_expires_ns = expires;
            return Ok(());
        }
        // New sessions authenticate against the renewed descriptor. Sessions
        // authenticated under the superseded one keep their binding until
        // expiry or replacement: renewal is not a reason to re-handshake.
        let fenced = self
            .registry
            .rotate(registry, pins.descriptor, generation)?;
        self.descriptor_hint = hint;
        self.pins = pins;
        self.descriptor_expires_ns = expires;
        for id in fenced {
            self.fence_session(id);
        }
        Ok(())
    }
    /// Delivery retention (see `delivery::retention`): a cheap budget check
    /// over the retained decode, then one Maintenance commit only when due.
    fn retain_delivery(&mut self, force_rotation: bool) -> Result<bool> {
        let due = with_turn(
            &self.source,
            &self.pool,
            DeliveryOperation::Replay,
            |turn| {
                let pressure = turn.delivery_pressure()?;
                Ok(pressure.rotation_due() || pressure.receipts_due())
            },
        )?;
        if !(due || force_rotation) {
            return Ok(false);
        }
        let report = with_turn(
            &self.source,
            &self.pool,
            DeliveryOperation::Maintenance,
            |turn| turn.retain_delivery(force_rotation),
        )?;
        let Some(report) = report else {
            return Ok(false);
        };
        tracing::info!(
            event = "cloud_delivery_retention",
            rotated = report.rotated.is_some(),
            receipts_released = report.receipts_released,
            pool_bytes_before = report.before.pool_bytes,
            pool_bytes_after = report.after.pool_bytes,
            replay_before = report.before.replay,
            replay_after = report.after.replay,
        );
        Ok(true)
    }
    /// Waiting on this as well as `next_wakeup` answers completed upload
    /// work and ready private replies without polling.
    pub(crate) fn wake_handle(&self) -> Arc<tokio::sync::Notify> {
        self.wake.clone()
    }
    /// The idle tick and poll deadlines; completions arrive via `wake_handle`.
    /// Only a private reply held back by another component's Store lock is
    /// retried on the short tick (lock release has no event). The
    /// `QOS_HOST_PERF_BASELINE=1` build keeps the previous 20 ms busy tick.
    pub(crate) fn next_wakeup(&self) -> Instant {
        let now = Instant::now();
        let private = self.private.as_ref();
        let baseline = crate::storage::host_perf_baseline();
        let short = !self.deferred.is_empty()
            || self.polls.needs_retry()
            || if baseline {
                self.uploads.busy() || private.is_some_and(|p| p.busy())
            } else {
                private.is_some_and(|p| p.needs_retry())
            };
        let idle = IDLE_TICK;
        #[cfg(test)]
        let idle = if quiet_ticks::is_quiet(&self.source.0.root) {
            Duration::from_secs(60)
        } else {
            idle
        };
        let mut next = now + if short { BUSY_TICK } else { idle };
        if let Some(deadline) = self.polls.next_deadline() {
            next = next.min(deadline.max(now));
        }
        if !baseline {
            if let Some(deadline) = private.and_then(|p| p.next_deadline()) {
                next = next.min(deadline.max(now));
            }
        }
        next
    }
    /// Stop serving: held requests receive their idle/refusal answers and the
    /// private lane closes. Every admitted commit already passed its barrier.
    pub(crate) fn close(&mut self) {
        self.failed = true;
        for deferred in std::mem::take(&mut self.deferred) {
            let _ = deferred.request.respond(
                StatusCode::SERVICE_UNAVAILABLE,
                b"temporarily-unavailable".to_vec(),
            );
        }
        self.polls.idle_all();
        self.uploads.refuse_all();
        if let Some(private) = self.private.as_mut() {
            private.close();
        }
    }
}
