//! Installed shared HTTPS control and exact-route H3 media ingress.
//! The serialized host retains one session registry and one native media service.
use crate::{
    SessionOwner,
    committed_authority::CommittedAuthority,
    delivery_http::HttpSessions,
    https::{self, BufferedRequest, HttpsAdapter, Ingress, IngressReceiver, Route},
    https_assets::AssetRoot,
    media_service::MediaService,
};
use anyhow::{Context, Result, ensure};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tilo_relay::delivery_v3::http_proof::{Request, RequestProof};
use tokio::{net::TcpListener, sync::watch, task::JoinSet, time::timeout};
pub(crate) struct IngressConfig {
    pub origin: String,
    pub assets: AssetRoot,
    pub certificates: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
    pub local_enrollment: Option<Arc<crate::https::LocalEnrollment>>,
}
// Operation time is captured only from the actual owner. Numeric time exists
// solely for the existing explicit fixture renewal path.
enum IngressClock {
    Native(qos_action_broker::peer_actions::PolicyTime),
    #[cfg(test)]
    Fixed(u64),
}
impl IngressClock {
    fn current(authority: &CommittedAuthority) -> Result<Self> {
        let guard = authority.read().map_err(anyhow::Error::msg)?;
        Ok(Self::Native(
            guard.view().policy_time().map_err(anyhow::Error::msg)?,
        ))
    }
    fn sample(&self, view: &crate::committed_authority::CommittedView) -> Result<u64> {
        match self {
            Self::Native(time) => view.check_policy_time(time).map_err(anyhow::Error::msg),
            #[cfg(test)]
            Self::Fixed(now) => Ok(*now),
        }
    }
    fn registry(&self, owner: &SessionOwner, current: &CommittedAuthority) -> Result<HttpSessions> {
        match self {
            Self::Native(_) => {
                self.sample(current.read().map_err(anyhow::Error::msg)?.view())?;
                HttpSessions::new_current(owner, current)
            }
            #[cfg(test)]
            Self::Fixed(now) => HttpSessions::new(owner, current, *now),
        }
    }
}
pub(crate) struct InstalledIngress {
    transport: transport::RunningTransport,
    sessions: HttpSessions,
    current: CommittedAuthority,
    media: MediaService,
    owner_root: std::path::PathBuf,
    maintenance_high_water: u64,
    data: crate::delivery_http::serve::DataRoutes,
    observed_generation: u64,
    /// Delivery retention scheduling (see `owner/retention.rs`).
    retention: RetentionSchedule,
    onboarding: Option<(
        crate::onboarding_http::OnboardingRegistry,
        crate::collaboration::CollaborationService,
    )>,
    /// LIVE_V1 grant authority and its admitted-change forwarder task.
    live: Option<(crate::live_service::LiveService, LiveForwardTask)>,
}
/// When the owner loop next evaluates delivery retention: after every owner
/// commit, otherwise at a bounded cadence, and after a failure only after a
/// backoff (a refused retention commit, e.g. disk headroom, is retried).
#[derive(Default)]
struct RetentionSchedule {
    generation: u64,
    next: Option<std::time::Instant>,
}
/// Snapshot pins signed this recently may still be downloading.
const RETENTION_SNAPSHOT_WINDOW_NS: u64 = 10_000_000_000;
const RETENTION_CADENCE: std::time::Duration = std::time::Duration::from_millis(500);
const RETENTION_BACKOFF: std::time::Duration = std::time::Duration::from_secs(5);
/// Held polls due within this long are answered before a retention commit.
const RETENTION_TURN_HORIZON: std::time::Duration = std::time::Duration::from_secs(3);
/// Installed-ingress unit tests account owner generations exactly, so they
/// opt in to background retention (`test_retention::with_auto_retention`).
#[cfg(test)]
pub(crate) mod test_retention {
    use std::cell::Cell;
    thread_local! { static ENABLED: Cell<bool> = const { Cell::new(false) }; }
    pub(crate) fn enabled() -> bool {
        ENABLED.with(Cell::get)
    }
    #[allow(dead_code, reason = "opt-in retention hook for media ingress tests")]
    pub(crate) fn with_auto_retention<T>(operation: impl FnOnce() -> T) -> T {
        let prior = ENABLED.with(|enabled| enabled.replace(true));
        let result = operation();
        ENABLED.with(|enabled| enabled.set(prior));
        result
    }
}
/// Aborts the live forwarder when the installed ingress is dropped.
struct LiveForwardTask(tokio::task::JoinHandle<()>);
impl Drop for LiveForwardTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl std::ops::Deref for InstalledIngress {
    type Target = transport::RunningTransport;
    fn deref(&self) -> &Self::Target {
        &self.transport
    }
}
impl std::ops::DerefMut for InstalledIngress {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.transport
    }
}

impl InstalledIngress {
    /// Called only by the serialized owner. Both live services must still own
    /// their registrations; replacing the hint invalidates this witness.
    pub(crate) fn validate_installation(
        &self,
        owner: &SessionOwner,
        view: &crate::committed_authority::CommittedView,
        origin: &str,
    ) -> Result<()> {
        ensure!(
            !self.failed && !*self.stop.borrow() && self.registrations.load(Ordering::Acquire) == 3,
            "installed ingress unavailable"
        );
        ensure!(
            owner.root() == self.owner_root
                && origin == self.origin
                && owner.permits_publication()
                && view.generation() == owner.committed_generation()
                && view.peer().room() == owner.peer().room()
                && view.peer().authority() == owner.peer().authority()
                && view.committed_peer_identity() == owner.committed_peer_identity(),
            "installed ingress owner mismatch"
        );
        let persisted = tilo_relay::delivery_v3::descriptor::inspect_persisted_hint(
            owner.stored_delivery_hint()?,
            owner.host_certificate(),
        )?;
        ensure!(
            persisted.descriptor_digest == self.sessions.descriptor_digest(),
            "installed descriptor replaced"
        );
        Ok(())
    }

    pub(crate) fn refresh_current(&mut self, owner: &mut SessionOwner) -> Result<bool> {
        self.refresh_clock(owner, IngressClock::current(&self.current)?)
    }
    #[cfg(test)]
    pub(crate) fn refresh_at(&mut self, owner: &mut SessionOwner, now: u64) -> Result<bool> {
        self.refresh_clock(owner, IngressClock::Fixed(now))
    }
    fn refresh_clock(&mut self, owner: &mut SessionOwner, clock: IngressClock) -> Result<bool> {
        let old = self.sessions.descriptor_digest();
        let current = self.current.clone();
        current.with_owner_view(owner, |view, owner| -> Result<_> {
            let now = clock.sample(view)?;
            ensure!(
                now >= self.maintenance_high_water,
                "ingress clock regressed"
            );
            self.maintenance_high_water = now;
            if let Some((registry, _)) = self.onboarding.as_mut() {
                // Releases only expired nonces and pinned handoff responses.
                let _ = registry.maintain(now);
            }
            owner.install_running_media_transport(
                view,
                self,
                &self.origin,
                web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now),
            )
        })??;
        let persisted = tilo_relay::delivery_v3::descriptor::inspect_persisted_hint(
            owner.stored_delivery_hint()?,
            owner.host_certificate(),
        )?;
        if persisted.descriptor_digest == old {
            return Ok(false);
        }
        // Install the renewed registry only after the actual owner barrier was
        // published. New sessions bind the renewed descriptor; live sessions
        // keep their original binding (verified against a fresh current
        // context) until they expire or are replaced, so a renewal does not
        // force every browser through a fresh bootstrap and handshake.
        let renewed = clock.registry(owner, &self.current)?;
        let previous = std::mem::replace(&mut self.sessions, renewed);
        self.sessions.inherit_renewed(previous);
        tracing::info!(
            generation = persisted.generation,
            retained_sessions = self.sessions.live_sessions(),
            "delivery descriptor renewed"
        );
        Ok(true)
    }

    pub(crate) async fn start(
        owner: &mut SessionOwner,
        current: CommittedAuthority,
        listener: TcpListener,
        config: IngressConfig,
    ) -> Result<Self> {
        let endpoint = endpoint(
            listener.local_addr()?,
            config.certificates.clone(),
            config.key.clone_key(),
        )?;
        let channels = https::try_ingress_channel()?;
        Self::start_bound(owner, current, listener, config, endpoint, channels).await
    }
    async fn start_bound(
        owner: &mut SessionOwner,
        current: CommittedAuthority,
        listener: TcpListener,
        config: IngressConfig,
        endpoint: quinn::Endpoint,
        channels: (Ingress, https::IngressReceiver),
    ) -> Result<Self> {
        let guard = current.read().map_err(anyhow::Error::msg)?;
        let time = guard.view().policy_time().map_err(anyhow::Error::msg)?;
        let now = guard
            .view()
            .check_policy_time(&time)
            .map_err(anyhow::Error::msg)?;
        let descriptor = owner.verified_delivery_transport(
            guard.view(),
            web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now),
            crate::delivery_http::serve::INSTALLED_DATA_CAPABILITIES,
        )?;
        ensure!(
            descriptor.origin() == config.origin,
            "installed descriptor origin mismatch"
        );
        drop(guard);
        let sessions = HttpSessions::new_current(owner, &current)?;
        let media = MediaService::for_owner(
            owner,
            current.clone(),
            &format!("{}/moq/v21/voice", config.origin),
        )?;
        let relay = qos_moq::relay::Relay::new(Arc::new(media.clone()));
        let live_origin = config.origin.clone();
        let transport =
            transport::RunningTransport::start_bound(listener, config, endpoint, channels, relay)
                .await?;
        let mut data = crate::delivery_http::serve::DataRoutes::new(transport.host_work());
        // LIVE_V1 is optional: without it the durable path serves everything
        // and the descriptor does not advertise the live bit.
        let live =
            crate::live_service::LiveService::for_owner(owner, current.clone(), &live_origin)
                .ok()
                .map(|service| {
                    let relay = qos_moq::live::LiveRelay::new(Arc::new(service.clone()));
                    let (forwarder, task) = service.forwarder(relay.clone());
                    transport.install_live(relay);
                    data.install_admitted_sink(Arc::new(move |device, canonical| {
                        forwarder.offer(device, canonical)
                    }));
                    (service, LiveForwardTask(task))
                });
        let mut installed = Self {
            transport,
            sessions,
            current,
            media,
            owner_root: owner.root().to_owned(),
            maintenance_high_water: 0,
            data,
            observed_generation: owner.committed_generation(),
            retention: RetentionSchedule::default(),
            onboarding: None,
            live,
        };
        installed.refresh_current(owner)?;
        Ok(installed)
    }
    pub(crate) fn discovery_hint(
        &self,
        owner: &SessionOwner,
    ) -> Result<qos_peer_state::join::TransportHint> {
        self.discovery_hint_clock(owner, IngressClock::current(&self.current)?)
    }
    #[cfg(test)]
    pub(crate) fn discovery_hint_at(
        &self,
        owner: &SessionOwner,
        now: u64,
    ) -> Result<qos_peer_state::join::TransportHint> {
        self.discovery_hint_clock(owner, IngressClock::Fixed(now))
    }
    fn discovery_hint_clock(
        &self,
        owner: &SessionOwner,
        clock: IngressClock,
    ) -> Result<qos_peer_state::join::TransportHint> {
        let guard = self.current.read().map_err(anyhow::Error::msg)?;
        let now = clock.sample(guard.view())?;
        self.validate_installation(owner, guard.view(), &self.origin)?;
        let descriptor = owner.verified_delivery_transport(
            guard.view(),
            web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now),
            crate::delivery_http::serve::INSTALLED_DATA_CAPABILITIES,
        )?;
        ensure!(
            descriptor.descriptor_digest() == self.sessions.descriptor_digest(),
            "installed descriptor replaced"
        );
        Ok(owner.stored_delivery_hint()?.clone())
    }
    /// The returned location is inert; the private caller still signs with this
    /// owner's actual host key. Configuration alone never reaches this path.
    pub(crate) fn origin_for_discovery(&self, owner: &SessionOwner) -> Result<String> {
        self.discovery_hint(owner)?;
        Ok(self.origin.clone())
    }
    fn descriptor_bytes(&self, owner: &SessionOwner) -> Result<Vec<u8>> {
        let hint = self.discovery_hint(owner)?;
        let mut bounded = [0; tilo_relay::delivery_v3::descriptor::MAX_HINT_BYTES];
        Ok(postcard::to_slice(&hint, &mut bounded)?.to_vec())
    }
    #[cfg(test)]
    fn descriptor_bytes_at(&self, owner: &SessionOwner, now: u64) -> Result<Vec<u8>> {
        let hint = self.discovery_hint_at(owner, now)?;
        let mut bounded = [0; tilo_relay::delivery_v3::descriptor::MAX_HINT_BYTES];
        Ok(postcard::to_slice(&hint, &mut bounded)?.to_vec())
    }
    pub(crate) async fn recv(&mut self) -> Option<BufferedRequest> {
        self.transport.recv().await
    }
    pub(crate) fn handle(&mut self, owner: &mut SessionOwner, request: BufferedRequest) {
        if request.is_cancelled() {
            return;
        }
        if crate::delivery_http::serve::DataRoutes::owns(request.route()) {
            if self.failed {
                let _ =
                    request.respond(hyper::StatusCode::FORBIDDEN, b"permission-denied".to_vec());
                return;
            }
            let budget = self.transport.host_bytes();
            self.data
                .handle(&mut self.sessions, &self.current, owner, &budget, request);
            self.maintain(owner);
            return;
        }
        let result = self.dispatch(owner, &request);
        let (status, body) = match result {
            Ok(body) => (hyper::StatusCode::OK, body),
            Err(_) if request.route() == Route::Descriptor => (
                hyper::StatusCode::SERVICE_UNAVAILABLE,
                b"temporarily-unavailable".to_vec(),
            ),
            // A superseded media participation (the call epoch advanced while
            // the person stays permitted) is not a permission denial: the
            // client rejoins the same call instead of ending it.
            Err(error)
                if matches!(request.route(), Route::Media(_))
                    && matches!(
                        error.downcast_ref::<crate::media_authority::MediaError>(),
                        Some(crate::media_authority::MediaError::StaleGeneration)
                    ) =>
            {
                (hyper::StatusCode::CONFLICT, b"stale-participation".to_vec())
            }
            // LIVE_V1 clients fall back to the durable path on 404.
            Err(_) if request.route() == Route::LiveGrant && self.live.is_none() => {
                (hyper::StatusCode::NOT_FOUND, b"unsupported".to_vec())
            }
            Err(_) => (hyper::StatusCode::FORBIDDEN, b"permission-denied".to_vec()),
        };
        let _ = request.respond(status, body);
        self.maintain(owner);
    }
    /// Serve fresh and invited onboarding through the owner's collaboration
    /// service. Without this the onboarding routes refuse.
    pub(crate) fn install_onboarding(
        &mut self,
        service: crate::collaboration::CollaborationService,
    ) -> Result<()> {
        use rand::RngCore;
        let mut boot = [0; 16];
        while boot == [0; 16] {
            rand::rngs::OsRng.try_fill_bytes(&mut boot)?;
        }
        let registry =
            crate::onboarding_http::OnboardingRegistry::new(boot, self.transport.host_bytes())?;
        self.onboarding = Some((registry, service));
        Ok(())
    }
    /// Onboarding and handoff chunks. Membership changes reconcile the host's
    /// relay access before the signed response is released, so this awaits.
    pub(crate) async fn onboard(
        &mut self,
        owner: &mut SessionOwner,
        host: &crate::host::LocalHost,
        request: BufferedRequest,
    ) {
        let refuse = |request: BufferedRequest| {
            let _ = request.respond(hyper::StatusCode::FORBIDDEN, b"permission-denied".to_vec());
        };
        if request.is_cancelled() {
            return;
        }
        let Some((registry, service)) = self.onboarding.as_mut().filter(|_| !self.transport.failed)
        else {
            return refuse(request);
        };
        let now = match IngressClock::current(&self.current)
            .and_then(|clock| clock.sample(self.current.read().map_err(anyhow::Error::msg)?.view()))
        {
            Ok(now) => now,
            Err(_) => return refuse(request),
        };
        if request.route() == Route::OnboardingChunk {
            let Ok(guard) = self.current.read() else {
                return refuse(request);
            };
            // The registry answers refusals itself.
            if let Err(error) = registry.chunk(owner, guard.view(), request, now) {
                tracing::debug!(%error, "onboarding handoff chunk refused");
            }
            return;
        }
        let budget = self.transport.host_bytes();
        let mut scope = match owner.prepare_owner_scope(
            &budget,
            crate::owner::resources::OwnerOperation::Join {
                input_len: request.body().len(),
            },
        ) {
            Ok(scope) => scope,
            Err(_) => return refuse(request),
        };
        // Admission answers its own refusals; an invocation failure drops the
        // original exchange, which rejects it.
        let invoked = owner.with_owner_view_scope(&mut scope, &self.current, |view, owner| {
            let mut exchange = registry.admit(owner, view, request, now)?;
            let outcome = exchange.invoke(owner, service, now)?;
            anyhow::Ok((exchange, outcome))
        });
        let (exchange, outcome) = match invoked {
            Ok(Ok(value)) => value,
            _ => return,
        };
        // Keep the scope and request custody, but no Current guard, across
        // relay membership reconciliation.
        if outcome.reconcile {
            if let Err(error) = host.update_membership(owner).await {
                tracing::debug!(%error, "onboarding membership reconciliation failed");
                return;
            }
        }
        let Some((registry, _)) = self.onboarding.as_mut() else {
            return;
        };
        let Ok(guard) = self.current.read() else {
            return;
        };
        if let Err(error) = registry.finish(exchange, owner, guard.view(), outcome.response, now) {
            tracing::debug!(%error, "onboarding response withheld");
        }
        drop(guard);
        drop(scope);
        self.maintain(owner);
    }
    /// Local OS-owner enrollment reply over HTTPS. The complete JSON reply stays
    /// inline when it fits the frozen 1 MiB bound (older runtimes read only
    /// that); otherwise it carries a host-signed governance handoff manifest
    /// whose ranges the newly certified device fetches from
    /// `/peer/v3/onboarding/chunks`. Call in the same host turn as the committed
    /// enrollment, after relay membership reconciliation and publication, with
    /// the receipt minted by `EnrollmentAuthority::complete_for_https`.
    pub(crate) fn local_enrollment_reply(
        &mut self,
        owner: &SessionOwner,
        certificate: &tilo_user_identity::DeviceCert,
        proof: crate::enrollment::LocalOnboardingProof,
        metadata: serde_json::Value,
    ) -> Result<Vec<u8>> {
        ensure!(!self.transport.failed, "installed ingress unavailable");
        let Some((registry, _)) = self.onboarding.as_mut() else {
            anyhow::bail!("local onboarding handoff unavailable");
        };
        let clock = IngressClock::current(&self.current)?;
        // Read-only: pins the committed publication roots (reference counts
        // plus small charged metadata, no payload copy) and signs a manifest.
        // It runs in the same legacy unscoped host turn as the enrollment
        // commit itself; a full owner scope would charge ~6x the aggregate
        // and refuse exactly the large workspaces this path exists for.
        let guard = self.current.read().map_err(anyhow::Error::msg)?;
        let view = guard.view();
        let now = clock.sample(view)?;
        let pin = registry.pin_local(owner)?;
        registry.local_bootstrap(owner, view, certificate, proof, pin, metadata, now)
    }
    /// Bind private routes to the host's current action runtime, if any.
    pub(crate) fn sync_actions(&mut self, actions: Option<&crate::action_host::ActionHost>) {
        self.data.sync_private(actions);
    }
    /// Resume held polls after any owner commit, or when one nears its deadline.
    /// Called by the host after every owner-affecting turn; cheap otherwise.
    pub(crate) fn maintain(&mut self, owner: &mut SessionOwner) {
        self.data.observe_turn(tokio::time::Instant::now());
        if self.data.has_pending_work() {
            let budget = self.transport.host_bytes();
            self.data
                .pump(&mut self.sessions, &self.current, owner, &budget);
        }
        self.maintain_retention(owner);
        let generation = owner.committed_generation();
        let due = self
            .data
            .next_deadline()
            .is_some_and(|deadline| deadline <= tokio::time::Instant::now());
        if generation == self.observed_generation && !due {
            return;
        }
        self.observed_generation = generation;
        let budget = self.transport.host_bytes();
        self.data
            .wake(&mut self.sessions, &self.current, owner, &budget);
    }
    /// Commit due delivery retention (certified checkpoint, receipt release or
    /// snapshot + suffix rotation) in the serialized owner, outside requests.
    /// Held polls are woken by the generation change like any other commit.
    fn maintain_retention(&mut self, owner: &mut SessionOwner) {
        let now = std::time::Instant::now();
        let generation = owner.committed_generation();
        if generation == self.retention.generation
            && self.retention.next.is_some_and(|next| now < next)
        {
            return;
        }
        self.retention.generation = generation;
        self.retention.next = Some(now + RETENTION_CADENCE);
        #[cfg(test)]
        if !test_retention::enabled() {
            return;
        }
        if self.failed || !owner.permits_publication() {
            return;
        }
        let wall = web_time::SystemTime::now();
        let Ok(wall_ns) = wall
            .duration_since(web_time::SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos() as u64)
        else {
            return;
        };
        let busy = self
            .data
            .snapshot_recently_pinned(wall_ns, RETENTION_SNAPSHOT_WINDOW_NS);
        match owner.delivery_retention_due(wall, busy) {
            Ok(Some(_)) => {}
            Ok(None) => return,
            Err(error) => {
                tracing::debug!(%error, "delivery retention pressure unavailable");
                return;
            }
        }
        // Retention (especially a snapshot rotation) can hold the owner for
        // up to about a second: answer held polls that would come due in that
        // time first instead of letting their HTTPS deadline answer 503.
        let budget = self.transport.host_bytes();
        self.data.answer_idle_within(
            &mut self.sessions,
            &self.current,
            owner,
            &budget,
            RETENTION_TURN_HORIZON,
        );
        let started = std::time::Instant::now();
        let current = self.current.clone();
        let result = current
            .with_owner_view(owner, |_, owner| owner.maintain_delivery(wall, busy))
            .and_then(|result| result);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(Some(report)) => {
                tracing::info!(
                    action = ?report.action,
                    rotated = report.rotated,
                    receipts_released = report.receipts_released,
                    pool_before = report.before.pool_bytes,
                    pool_after = report.after.pool_bytes,
                    history_before = report.before.history_bytes,
                    replay_before = report.before.replay,
                    replay_after = report.after.replay,
                    tail_before = report.before.tail_entries,
                    generation = owner.committed_generation(),
                    elapsed_ms,
                    "delivery retention committed"
                );
                self.retention.generation = owner.committed_generation();
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "delivery retention refused; retrying after backoff");
                self.retention.next = Some(now + RETENTION_BACKOFF);
            }
        }
    }
    fn dispatch(&mut self, owner: &mut SessionOwner, request: &BufferedRequest) -> Result<Vec<u8>> {
        ensure!(
            !self.failed && !request.is_cancelled(),
            "ingress unavailable"
        );
        match request.route() {
            Route::Descriptor => self.descriptor_bytes(owner),
            Route::Bootstrap => {
                self.sessions
                    .bootstrap_current(owner, &self.current, request.body())
            }
            Route::Challenge => {
                self.sessions
                    .challenge_current(owner, &self.current, request.body())
            }
            Route::Sessions => {
                let budget = self.transport.host_bytes();
                self.data.finish_session(
                    &mut self.sessions,
                    &self.current,
                    owner,
                    &budget,
                    request.body(),
                )
            }
            Route::Media(_) => {
                let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
                let exact =
                    Request::new(request.method().as_str(), request.target(), request.body())?;
                let verified = self
                    .sessions
                    .admit_current(owner, &self.current, &proof, &exact)?;
                Ok(crate::media_control::dispatch(
                    owner,
                    &self.media,
                    &verified,
                )?)
            }
            Route::LiveGrant => {
                let (live, _) = self.live.as_ref().context("live relay not installed")?;
                let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
                let exact =
                    Request::new(request.method().as_str(), request.target(), request.body())?;
                let verified = self
                    .sessions
                    .admit_current(owner, &self.current, &proof, &exact)?;
                live.issue(owner, &verified)
            }
            _ => anyhow::bail!("data handler not installed"),
        }
    }
    pub(crate) async fn shutdown(self) -> Result<()> {
        self.transport.shutdown().await
    }
}
// Constructed only inside the two real service tasks. Panic, cancellation or
// normal service completion immediately invalidates publication, even before recv.
struct ServiceRegistration {
    registrations: Arc<AtomicU8>,
    bit: u8,
}
impl ServiceRegistration {
    fn enter(registrations: Arc<AtomicU8>, bit: u8) -> Self {
        registrations.fetch_or(bit, Ordering::AcqRel);
        Self { registrations, bit }
    }
}
impl Drop for ServiceRegistration {
    fn drop(&mut self) {
        self.registrations.fetch_and(!self.bit, Ordering::AcqRel);
    }
}

fn endpoint(
    address: std::net::SocketAddr,
    certificates: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<quinn::Endpoint> {
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_no_client_auth()
    .with_single_cert(certificates, key)?;
    tls.alpn_protocols = vec![web_transport_quinn::ALPN.as_bytes().to_vec()];
    tls.max_early_data_size = 0;
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    transport
        .max_concurrent_bidi_streams(32u32.into())
        .max_concurrent_uni_streams(32u32.into())
        .receive_window((2 * 1024 * 1024u32).into())
        .stream_receive_window((256 * 1024u32).into())
        .send_window(2 * 1024 * 1024)
        .max_idle_timeout(Some(Duration::from_secs(30).try_into()?));
    config.transport_config(Arc::new(transport));
    Ok(quinn::Endpoint::server(config, address)?)
}
async fn serve_h3(
    endpoint: quinn::Endpoint,
    authority: String,
    origin: String,
    relay: qos_moq::relay::Relay,
    live: transport::LiveSlot,
    ingress: Ingress,
    h3: Option<crate::https::h3::H3Service>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let mut tasks = JoinSet::new();
    // Per-kind bounds (HTTP/3 data, voice, live) instead of one shared cap.
    let limits = limits::EndpointLimits::new();
    loop {
        if *stop.borrow() {
            break;
        }
        tokio::select! {biased;
         _=stop.changed()=>break,
         _=tasks.join_next(),if !tasks.is_empty()=>{},
         incoming=endpoint.accept()=>{
          let Some(incoming)=incoming else{break};
          if tasks.len()>=limits::MAX_TASKS{incoming.refuse();continue}
          let Some(handshake)=limits.handshake() else{incoming.refuse();continue};
          let Some(charge)=ingress.reserve_media_transport() else{incoming.refuse();continue};
          let authority=authority.clone();let origin=origin.clone();let relay=relay.clone();let live=live.clone();let h3=h3.clone();let limits=limits.clone();
          tasks.spawn(async move {
           let _charge=charge;
           // One endpoint serves WebTransport (media, live) and plain HTTP/3
           // delivery requests; the first request stream classifies it.
           let accepted=timeout(qos_moq::relay::HANDSHAKE_BUDGET,async{let conn=incoming.await?;crate::https::h3::accept(conn).await.map_err(|e|anyhow::anyhow!("{e}"))}).await;
           let request=match accepted{
            Ok(Ok(crate::https::h3::Accepted::WebTransport(request)))=>request,
            Ok(Ok(crate::https::h3::Accepted::Http(connection)))=>{
             let Some(_kind)=limits.classify(handshake,limits::Kind::Http) else{return};
             if let Some(h3)=h3{h3.serve(connection).await;}return;
            }
            _=>return,
           };
           // Fresh comparison for this exact CONNECT, before response or MoQ decoding.
           let route=request.raw_authority().zip(request.raw_target()).and_then(|(raw,target)|qos_moq::relay::Relay::validate_route(raw,&authority,target));
           // LIVE_V1 shares this endpoint at its own exact route, once installed.
           let live_route=request.raw_authority().zip(request.raw_target()).and_then(|(raw,target)|qos_moq::live::LiveRelay::validate_route(raw,&authority,target)).and_then(|route|live.get().cloned().map(|live|(route,live)));
           if (route.is_none() && live_route.is_none()) || request.headers.get("origin").is_some_and(|v|v!=&origin) || !request.protocols.iter().any(|v|v=="moqt-21") {
            let _=timeout(Duration::from_millis(100),request.reject(web_transport_quinn::proto::http::StatusCode::NOT_FOUND)).await;return;
           }
           let kind=if live_route.is_some(){limits::Kind::Live}else{limits::Kind::Media};
           let Some(_kind)=limits.classify(handshake,kind) else{
            let _=timeout(Duration::from_millis(100),request.reject(web_transport_quinn::proto::http::StatusCode::SERVICE_UNAVAILABLE)).await;return;
           };
           // SETUP needs at least one round trip after the CONNECT response; high-RTT
           // links (satellite) plus one loss recovery exceed the former 900 ms.
           let accepted=timeout(qos_moq::relay::SETUP_BUDGET,async{
            let session=request.respond(web_transport_quinn::proto::ConnectResponse::OK.with_protocol("moqt-21")).await?;
            moq_net::Server::new().with_versions("moq-transport-21".parse::<moq_net::Version>().expect("fixed version").into()).accept_request(session).await.map_err(anyhow::Error::from)
           }).await;
           match accepted {
            Ok(Ok(request))=>match (route,live_route) {
             (_,Some((route,live)))=>{live.serve_accepted(route,request).await;tracing::debug!(counters=?live.counters(),"live relay session ended");}
             (Some(route),None)=>{relay.serve_accepted(route,request).await;tracing::debug!(counters=?relay.counters(),"media relay session ended");}
             (None,None)=>{}
            },
            Ok(Err(error))=>tracing::debug!(%error,"media relay setup failed"),
            Err(_)=>tracing::debug!("media relay setup timed out"),
           }
          });
         }
        }
    }
    endpoint.close(0u32.into(), b"host shutdown");
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}
#[cfg(test)]
mod data_tests;
#[cfg(test)]
mod live_forward_tests;
#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod local_enrollment_tests;
#[cfg(test)]
mod tests;

mod config;
mod limits;

pub(crate) use config::PreparedIngress;

pub(crate) mod transport;
pub use config::PreparedTransport;
