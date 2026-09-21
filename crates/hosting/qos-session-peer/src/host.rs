//! Existing packet relay + authenticated qOS relay, with a replaceable native subscription.
use crate::SessionOwner;
use anyhow::{Context, Result, bail};
use iroh::{Endpoint, RelayMap, RelayMode, endpoint::presets};
use iroh_base::{EndpointAddr, RelayUrl};
use iroh_relay::server::{RelayConfig as PacketConfig, Server, ServerConfig};
use serde_json::json;
use std::future::Future;
use std::sync::Arc;
use std::{io::BufRead, net::Ipv4Addr, path::Path, task::Poll, time::Duration};
use tilo_relay::{
    Inbound, Relay, RelayClient, RelayCoeditSession, RelayConfig, WorkspaceBinding,
    daemon::RelayIdentity,
};
use tilo_user_identity::RevocationList;
use tokio::net::TcpListener;

// Nondefault, local-process qualification only; never a wire capability.
#[cfg(feature = "test-support")]
#[derive(Default)]
struct QualificationHold {
    route: Option<(String, String)>,
    completion: Option<Box<dyn FnOnce()>>,
    deadline: Option<std::time::Instant>,
    expired: bool,
    action_registration: Option<ActionRegistrationHold>,
    action_attachment_exit: Option<
        tokio::sync::oneshot::Sender<qos_action_broker::peer_actions::PeerActionAttachmentError>,
    >,
}
#[cfg(feature = "test-support")]
struct ActionRegistrationHold {
    reached: Arc<std::sync::atomic::AtomicBool>,
    release: tokio::sync::watch::Sender<bool>,
    barrier: Option<qos_action_broker::peer_actions::ActionRegistrationBarrier>,
}
#[cfg(feature = "test-support")]
impl QualificationHold {
    fn release(&mut self) {
        if let Some(complete) = self.completion.take() {
            complete();
        }
        self.deadline = None;
    }
    fn command(&mut self, line: &str) -> bool {
        if line == "qualification action-registration hold" {
            if self.action_registration.is_none() {
                let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let (release, waiting) = tokio::sync::watch::channel(false);
                self.action_registration = Some(ActionRegistrationHold {
                    reached: reached.clone(),
                    release,
                    barrier: Some(
                        qos_action_broker::peer_actions::ActionRegistrationBarrier::new(
                            reached, waiting,
                        ),
                    ),
                });
            }
            return true;
        }
        if line == "qualification action-registration release" {
            if let Some(hold) = self.action_registration.as_ref() {
                hold.release.send_replace(true);
            }
            return true;
        }
        if let Some(kind) = line.strip_prefix("qualification action-attachment ") {
            let exit = match kind {
                "eof" => {
                    qos_action_broker::peer_actions::PeerActionAttachmentError::TransportClosed
                }
                "failure" => qos_action_broker::peer_actions::PeerActionAttachmentError::Failed,
                _ => return false,
            };
            if let Some(sender) = self.action_attachment_exit.take() {
                let _ = sender.send(exit);
            }
            return true;
        }
        if line == "qualification release" {
            self.release();
            return true;
        }
        if let Some(route) = line.strip_prefix("qualification hold ") {
            if let Some((app, record)) = route.split_once(' ') {
                if self.route.is_none() && self.completion.is_none() {
                    self.route = Some((app.into(), record.into()));
                    self.expired = false;
                }
            }
            return true;
        }
        false
    }

    fn take_action_registration_barrier(
        &mut self,
    ) -> Option<qos_action_broker::peer_actions::ActionRegistrationBarrier> {
        self.action_registration
            .as_mut()
            .and_then(|hold| hold.barrier.take())
    }

    fn replace_action_attachment_exit(
        &mut self,
    ) -> tokio::sync::oneshot::Receiver<qos_action_broker::peer_actions::PeerActionAttachmentError>
    {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.action_attachment_exit = Some(sender);
        receiver
    }
}
#[cfg(feature = "test-support")]
impl Drop for QualificationHold {
    fn drop(&mut self) {
        self.release();
    }
}

/// Owner-side checkpoint cadence (Stage 1 / Stage 9).
///
/// The native host re-derives and re-publishes its ENTIRE signed state — a
/// multi-megabyte room checkpoint plus a per-document snapshot — after every
/// committed inbound delta. Under the browser's publication window that became a
/// full checkpoint per key-batch: the relay fanned each ~2-3 MB checkpoint back
/// to the live browser (thousands of ~1.5 KB frames, the replay storm) and the
/// native's single owner loop fell far enough behind draining its own inbound
/// fan-out that the relay disconnected it for outbound backpressure, which
/// triggered a reconnect, a retained-baseline replay, and re-publication — the
/// amplification.
///
/// Coalesce them: publish a full checkpoint at most once per [`Self::MAX_DELTAS`]
/// committed deltas, or once the inbound stream has been idle for
/// [`Self::IDLE`]. Intermediate deltas are still committed locally with their
/// durability receipt (only the redundant *wire* checkpoint is coalesced), and
/// the relay retains only the newest checkpoint per channel, so an offline
/// device still bootstraps from exactly one.
struct CheckpointCadence {
    deltas_since_publish: u32,
    pending: bool,
    last_activity: std::time::Instant,
}
impl CheckpointCadence {
    /// Publish at latest every this many committed deltas even under continuous
    /// input, so a peer that never pauses still gets a bounded-staleness anchor.
    /// Set well above a full typing burst so that, in practice, the anchor
    /// fires on the idle path below and a continuous typist never floods a live
    /// subscriber with a multi-megabyte checkpoint mid-stream.
    const MAX_DELTAS: u32 = 1024;
    /// Flush a coalesced checkpoint once input pauses for this long. Set well
    /// above the browser's 100 ms publication window, normal inter-key gaps, and
    /// the momentary main-thread stalls a delivered frame causes, so that only a
    /// genuine pause in typing — never active typing, and not the bursty catch-up
    /// of a backpressured native draining its inbound queue — triggers the anchor.
    const IDLE: Duration = Duration::from_millis(5000);
    fn new() -> Self {
        Self {
            deltas_since_publish: 0,
            pending: false,
            last_activity: std::time::Instant::now(),
        }
    }
    /// A delta committed locally. Returns whether the coalesced full checkpoint
    /// is due now (the per-delta count reached the cap); otherwise the publish
    /// is deferred and will flush on idle or at the cap.
    fn on_commit(&mut self, now: std::time::Instant) -> bool {
        self.last_activity = now;
        self.deltas_since_publish = self.deltas_since_publish.saturating_add(1);
        self.pending = true;
        if self.deltas_since_publish >= Self::MAX_DELTAS {
            self.mark_published(now);
            true
        } else {
            false
        }
    }
    /// On an idle tick: flush the coalesced checkpoint once input has paused.
    fn due_on_idle(&mut self, now: std::time::Instant) -> bool {
        if self.pending && now.duration_since(self.last_activity) >= Self::IDLE {
            self.mark_published(now);
            true
        } else {
            false
        }
    }
    /// A checkpoint was published (through any path); reset the coalescing state.
    fn mark_published(&mut self, now: std::time::Instant) {
        self.deltas_since_publish = 0;
        self.pending = false;
        self.last_activity = now;
    }
}

fn signed_commit_publication_due(cadence: &mut CheckpointCadence, now: std::time::Instant) -> bool {
    cadence.on_commit(now)
}

fn owner_commit_publication_due(
    cadence: &mut CheckpointCadence,
    now: std::time::Instant,
    desktop: bool,
) -> bool {
    if desktop {
        // Desktop commits append their room delta and any newly created record
        // to the durable outbox. Publish that gesture in this owner turn, not
        // after the background document checkpoint's idle window.
        cadence.mark_published(now);
        true
    } else {
        cadence.on_commit(now)
    }
}

pub struct NativeSubscription {
    session: Arc<RelayCoeditSession>,
    actions: Option<AttachedActions>,
    ready: bool,
    publication: std::sync::Mutex<crate::publication::PublicationCursor>,
}
struct AttachedActions {
    task: Option<
        tokio::task::JoinHandle<
            Result<(), qos_action_broker::peer_actions::PeerActionAttachmentError>,
        >,
    >,
    acknowledged: tokio::sync::watch::Receiver<bool>,
}
impl AttachedActions {
    fn is_ready(&self) -> bool {
        *self.acknowledged.borrow() && self.task.as_ref().is_some_and(|task| !task.is_finished())
    }
}
impl Drop for AttachedActions {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl std::ops::Deref for NativeSubscription {
    type Target = RelayCoeditSession;
    fn deref(&self) -> &Self::Target {
        &self.session
    }
}
impl NativeSubscription {
    pub fn is_ready(&self) -> bool {
        self.ready
    }
    pub async fn recv(&mut self) -> Option<Inbound> {
        let event = self.session.recv().await;
        if matches!(event, Some(Inbound::Ready { .. })) {
            self.ready = true;
        }
        event
    }
    pub fn into_client(self) -> RelayClient {
        Arc::try_unwrap(self.session)
            .unwrap_or_else(|_| panic!("active session attachment must be closed asynchronously"))
            .into_client()
    }
    async fn close(mut self) {
        if let Some(mut attached) = self.actions.take() {
            if let Some(task) = attached.task.take() {
                task.abort();
                let _ = task.await;
            }
        }
        // Each native attempt owns its endpoint. After joining the only
        // attachment, consume the client and flush CONNECTION_CLOSE before
        // dropping that endpoint, so the relay retires its subscriber.
        Arc::try_unwrap(self.session)
            .unwrap_or_else(|_| panic!("joined native attachment retained its session"))
            .into_client()
            .close()
            .await;
    }
    fn attach(
        &mut self,
        actions: &crate::action_host::ActionHost,
        room: uuid::Uuid,
        #[cfg(feature = "test-support")] barrier: Option<
            qos_action_broker::peer_actions::ActionRegistrationBarrier,
        >,
        #[cfg(feature = "test-support")] forced_exit: tokio::sync::oneshot::Receiver<
            qos_action_broker::peer_actions::PeerActionAttachmentError,
        >,
    ) -> Result<()> {
        if self.actions.is_some() {
            return Ok(());
        }
        let (queue, host) = actions
            .broker
            .app_action_broker()
            .context("broker unavailable")?
            .peer_action_attachment()
            .map_err(anyhow::Error::msg)?;
        let session = self.session.clone();
        let (acknowledged, readiness) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            #[cfg(feature = "test-support")]
            {
                let serving = async {
                    if let Some(barrier) = barrier {
                        queue
                            .serve_with_readiness_and_registration_barrier_for_test(
                                session,
                                room,
                                host,
                                acknowledged,
                                barrier,
                            )
                            .await
                    } else {
                        queue
                            .serve_with_readiness(session, room, host, acknowledged)
                            .await
                    }
                };
                tokio::pin!(serving);
                return tokio::select! {
                    result = &mut serving => result,
                    result = forced_exit => Err(result.unwrap_or(qos_action_broker::peer_actions::PeerActionAttachmentError::Failed)),
                };
            }
            #[cfg(not(feature = "test-support"))]
            queue
                .serve_with_readiness(session, room, host, acknowledged)
                .await
        });
        self.actions = Some(AttachedActions {
            task: Some(task),
            acknowledged: readiness,
        });
        Ok(())
    }
}
pub struct LocalHost {
    pub relay: Relay,
    packet: Server,
    pub address: EndpointAddr,
    pub packet_url: RelayUrl,
    mode: RelayMode,
}
struct Reconnect {
    task: tokio::task::JoinHandle<Result<NativeSubscription>>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Reconnect {
    async fn cancel(mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Ok(Ok(session)) = (&mut self.task).await {
            session.close().await;
        }
    }
}
impl Drop for Reconnect {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}
fn revocations(owner: &SessionOwner) -> Result<RevocationList> {
    let mut list = RevocationList::new();
    for entry in owner.peer().revocations() {
        list.insert(entry)?;
    }
    Ok(list)
}
impl LocalHost {
    pub async fn start(owner: &SessionOwner) -> Result<Self> {
        let mut config = ServerConfig::default();
        config.relay = Some(PacketConfig::new((Ipv4Addr::LOCALHOST, 0)));
        let packet = Server::spawn(config).await?;
        let packet_url: RelayUrl = format!(
            "http://{}",
            packet
                .http_addr()
                .context("packet relay HTTP listener unavailable")?
        )
        .parse()?;
        // This private server starts HTTP relay service only; URL conversion
        // otherwise advertises a default QAD listener that does not exist.
        let mode = RelayMode::Custom(RelayMap::from(iroh_relay::RelayConfig::new(
            packet_url.clone(),
            None,
        )));
        let workspace = owner.workspace_id();
        let state = owner.root().join("relay");
        let config = RelayConfig {
            state_root: state.clone(),
            identity_root: owner.root().join("host-identity"),
            full_replica_status: state.join("full-replica/status.json"),
            home_peer_bundle: state.join("home-peer.toml"),
            trust_policy: tilo_relay::TrustPolicy::AllowList {
                users: vec![hex::encode(owner.identity.root.verifying_key().to_bytes())],
            },
            iroh_listen: (Ipv4Addr::LOCALHOST, 0).into(),
            workspaces: vec![WorkspaceBinding {
                id: workspace,
                pin: true,
                replication: tilo_relay::ReplicationIntent::FullWorkspace,
                admitted_users: vec![],
                shell_authority: Some(
                    qos_peer_state::retained_proof::RetentionAuthority::new(
                        owner.peer().room(),
                        owner.peer().authority().clone(),
                    )
                    .map_err(anyhow::Error::msg)?,
                ),
                checkpoint_prefix_recovery:
                    tilo_relay::CheckpointPrefixRecovery::NativeOwnerDurableBaseline,
            }],
            ..Default::default()
        };
        let relay = Relay::start_with_relay_mode(
            config,
            RelayIdentity {
                user_public_key: owner.identity.root.verifying_key().to_bytes(),
                device_signing_key: owner.identity.host.clone(),
                device_cert: owner.identity.host_cert.clone(),
                revocations: revocations(owner)?,
            },
            mode.clone(),
        )
        .await?;
        let this = Self {
            address: EndpointAddr::new(relay.node_addr().id).with_relay_url(packet_url.clone()),
            relay,
            packet,
            packet_url,
            mode,
        };
        this.update_membership(owner).await?;
        tokio::time::timeout(Duration::from_secs(15), this.relay.endpoint.online())
            .await
            .context("qOS relay did not become online")?;
        Ok(this)
    }
    pub async fn update_membership(&self, owner: &SessionOwner) -> Result<()> {
        let revoked = revocations(owner)?;
        let members = owner
            .peer()
            .certificates()
            .into_iter()
            .filter(|cert| {
                !revoked.is_revoked(&cert.device_public_key)
                    && owner
                        .peer()
                        .permissions(cert.device_public_key, web_time::SystemTime::now())
                        .is_ok()
            })
            .map(|cert| cert.device_public_key)
            .collect();
        self.relay
            .set_workspace_membership(
                owner.workspace_id(),
                true,
                members,
                owner.root().join("relay"),
            )
            .await?;
        Ok(())
    }
    fn begin_connection(&self, owner: &SessionOwner, delay: Duration) -> Result<Reconnect> {
        let mode = self.mode.clone();
        let address = self.address.clone();
        let root = owner.identity.root.verifying_key().to_bytes();
        let host = owner.identity.host_cert.device_public_key;
        let certificate = owner.identity.native_cert.clone();
        let key = owner.identity.native.clone();
        let revoked = revocations(owner)?;
        let workspace = owner.workspace_id();
        let (cancel, mut cancelled) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            tokio::select! { _=tokio::time::sleep(delay)=>{}, _=&mut cancelled=>bail!("native reconnect cancelled") }
            let endpoint = Endpoint::builder(presets::Minimal)
                .relay_mode(mode)
                .bind_addr((Ipv4Addr::LOCALHOST, 0))?
                .bind()
                .await?;
            let attempt = async {
                endpoint.online().await;
                let client = RelayClient::connect_v2_to_enrolled_home_peer(
                    endpoint.clone(),
                    address,
                    root,
                    host,
                    certificate.clone(),
                    key,
                    revoked,
                )
                .await?;
                let session =
                    RelayCoeditSession::join(client, workspace, certificate.device_public_key)?;
                // The relay streams pending events BEFORE Ready. Admit them in the
                // ordinary serialized owner loop, with transport backpressure.
                Ok(NativeSubscription {
                    session: Arc::new(session),
                    actions: None,
                    ready: false,
                    publication: Default::default(),
                })
            };
            let result = tokio::select! {
                result=tokio::time::timeout(Duration::from_secs(4),attempt)=>result.context("native reconnect timed out").and_then(|value|value),
                _=&mut cancelled=>Err(anyhow::anyhow!("native reconnect cancelled")),
            };
            if result.is_err() {
                endpoint.close().await;
            }
            result
        });
        Ok(Reconnect {
            task,
            cancel: Some(cancel),
        })
    }
    pub async fn connect(&self, owner: &SessionOwner) -> Result<NativeSubscription> {
        let mut reconnect = self.begin_connection(owner, Duration::ZERO)?;
        (&mut reconnect.task)
            .await
            .context("native reconnect task failed")?
    }
    pub async fn shutdown(self) -> Result<()> {
        self.relay.shutdown().await;
        self.packet.shutdown().await?;
        Ok(())
    }
}
/// Publication never creates success: these exact messages were committed with the canonical aggregate.
pub fn publish(owner: &SessionOwner, session: &NativeSubscription) -> Result<()> {
    if !session.is_ready() {
        bail!("native subscription has not finished its initial replay");
    }
    use qos_peer_state::transport::{Retention, retention};
    if !owner.permits_publication() {
        bail!("durable barrier failed; publication is suspended until restore");
    }
    session
        .publication
        .lock()
        .map_err(|_| anyhow::anyhow!("publication cursor unavailable"))?
        .send(owner, |bytes, kind| {
            let message: qos_peer_state::NeighbourMsg = postcard::from_bytes(bytes)?;
            match retention(&message, kind == Retention::Checkpoint) {
                Some((channel, Retention::Checkpoint)) => {
                    session.publish_checkpoint(channel, bytes.to_vec())?
                }
                Some((channel, Retention::Delta)) => {
                    session.publish_retained_delta(channel, bytes.to_vec())?
                }
                None => session.publish_presence(bytes.to_vec())?,
            }
            Ok(())
        })
}

fn prepare_ready_publication(owner: &SessionOwner) -> Result<()> {
    if !owner.permits_publication() || owner.outgoing().is_empty() {
        bail!("native owner has no committed publication evidence");
    }
    Ok(())
}
/// Variant name for diagnostics; never inspects payloads.
pub fn message_kind(message: &qos_peer_state::NeighbourMsg) -> &'static str {
    use qos_peer_state::NeighbourMsg as M;
    match message {
        M::SyncReq { .. } => "SyncReq",
        M::SyncResp { .. } => "SyncResp",
        M::AppDocument { .. } => "AppDocument",
        M::IdentityHello { .. } => "IdentityHello",
        M::PersonalContinuityCheckpoint { .. } => "PersonalContinuityCheckpoint",
        M::PresenceMoved { .. } => "PresenceMoved",
        M::PresenceCleared { .. } => "PresenceCleared",
        _ => "other",
    }
}
/// How long a subscription must stay ready before its recovery counts as
/// settled. A shorter session continues the same reconnect episode.
const STABLE_SESSION: Duration = Duration::from_secs(30);
/// Bound repeated retained-baseline replay when a connection cannot stay up.
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(15);

fn next_reconnect_delay(current: Duration, ready_for: Option<Duration>) -> Duration {
    if ready_for.is_some_and(|elapsed| elapsed >= STABLE_SESSION) {
        return Duration::ZERO;
    }
    if current.is_zero() {
        Duration::from_secs(1)
    } else {
        (current * 2).min(MAX_RECONNECT_DELAY)
    }
}

/// Tracks one reconnect episode. Establishing a transport does not reset its
/// delay; only a subscription that remains ready for `STABLE_SESSION` does.
#[derive(Default)]
struct ReconnectPolicy {
    delay: Duration,
    first_ready: Option<std::time::Instant>,
    had_session: bool,
}

impl ReconnectPolicy {
    fn delay(&self) -> Duration {
        self.delay
    }

    fn observe_session(&mut self, present: bool, now: std::time::Instant) {
        if present {
            self.had_session = true;
        } else if std::mem::take(&mut self.had_session) {
            let ready_for = self
                .first_ready
                .take()
                .map(|ready| now.saturating_duration_since(ready));
            self.delay = next_reconnect_delay(self.delay, ready_for);
        }
    }

    fn connect_failed(&mut self) {
        self.delay = next_reconnect_delay(self.delay, None);
    }

    fn ready(&mut self, now: std::time::Instant) {
        self.first_ready.get_or_insert(now);
    }

    fn operator_reconnect(&mut self) {
        self.delay = Duration::ZERO;
        self.first_ready = None;
        self.had_session = false;
    }

    #[cfg(test)]
    fn had_session(&self) -> bool {
        self.had_session
    }
}

/// Every session drop names its reason so a reconnect can be traced to its cause.
async fn close_session(old: NativeSubscription, reason: &str) {
    tracing::warn!(reason, "native session dropped");
    old.close().await;
}
fn public_status(
    owner: &SessionOwner,
    connected: bool,
    error: &Option<String>,
) -> serde_json::Value {
    let mut value = serde_json::to_value(owner.status()).unwrap_or_else(|_| json!({}));
    value["native_peer"] = json!(if connected {
        "subscribed"
    } else {
        "disconnected"
    });
    value["application_state"] = json!(if owner.permits_publication() {
        "admitted"
    } else {
        "incomplete"
    });
    value["error"] = json!(
        error
            .as_ref()
            .cloned()
            .or_else(|| owner.peer().incomplete_reason())
    );
    value
}

fn console_lines() -> Result<tokio::sync::mpsc::Receiver<std::io::Result<String>>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(16);
    // Tokio implements stdin reads on its blocking pool, whose shutdown waits for an idle
    // pipe forever. This OS thread is deliberately detached so process shutdown never joins
    // the console read; the bounded channel still applies backpressure while the host runs.
    std::thread::Builder::new()
        .name("qos-console-input".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let failed = line.is_err();
                if sender.blocking_send(line).is_err() || failed {
                    break;
                }
            }
        })
        .context("spawn native console input reader")?;
    Ok(receiver)
}

pub async fn run(mut owner: SessionOwner, ready_path: &Path) -> Result<()> {
    let mut interrupt = Box::pin(tokio::signal::ctrl_c());
    let interrupted = std::future::poll_fn(|context| match interrupt.as_mut().poll(context) {
        Poll::Ready(result) => Poll::Ready(result.map(|()| true)),
        Poll::Pending => Poll::Ready(Ok(false)),
    })
    .await
    .context("install SIGINT handler")?;
    if interrupted {
        return Ok(());
    }
    let host = LocalHost::start(&owner).await?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let authorization = std::sync::Arc::new(crate::enrollment_http::Authorization::local(
        owner.root(),
        listener.local_addr()?.to_string(),
    )?);
    let (enrollment_commands, mut enrollments) =
        tokio::sync::mpsc::channel::<crate::enrollment_http::EnrollmentCommand>(64);
    let mut enrollment = crate::enrollment::EnrollmentAuthority::default();
    let mut session: Option<NativeSubscription> = None;
    let authority = crate::action_host::Authority::default();
    let (commit_handle, mut commits) = crate::owner_commit::OwnerCommitMailbox::bounded(32);
    let (signing, mut signatures) = crate::host_signing::HostSigning::new(commit_handle);
    let (network_sender, mut network) =
        tokio::sync::mpsc::channel::<qos_peer_state::NeighbourMsg>(64);
    let mut actions: Option<crate::action_host::ActionHost> = None;
    let mut pump = tokio::time::interval(Duration::from_millis(10));
    let mut action_error: Option<String> = None;
    let mut subscription_generation = 0u64;
    let mut replaying = false;
    let mut error = None;
    let mut reconnect: Option<Reconnect> = None;
    let mut reconnect_policy = ReconnectPolicy::default();
    let mut cadence = CheckpointCadence::new();
    let mut stdin = console_lines()?;
    #[cfg(feature = "test-support")]
    let mut qualification = QualificationHold::default();
    let mut stdin_open = true;
    let metadata = json!({"version":2,"enrollment_version":2,"challenge_path":"/enrollment/challenge","complete_path":"/enrollment/complete","pid":std::process::id(),"platform":std::env::consts::OS,"fixture_origin":format!("http://{}",listener.local_addr()?),"status_path":"/status","packet_relay":host.packet_url,"relay_address":host.address,"room":owner.peer().room(),"workspace_id":owner.workspace_id().to_string(),"state_root":owner.root(),"records":owner.peer().rows()});
    atomic_file::write_with_mode(ready_path, &serde_json::to_vec_pretty(&metadata)?, 0o600)?;
    crate::store::sync_dir(
        ready_path
            .parent()
            .context("ready file requires parent directory")?,
    )?;
    let run_result: Result<()> = async {
    loop {
        reconnect_policy.observe_session(session.is_some(), std::time::Instant::now());
        if session.is_none()
            && reconnect.is_none()
            && owner.permits_publication()
            && action_error.is_none()
        {
            let _timing = crate::action_timing::Timing::new("subscription_connecting");
            tracing::info!(delay_ms = reconnect_policy.delay().as_millis() as u64, "native reconnect starting");
            reconnect = Some(host.begin_connection(&owner, reconnect_policy.delay())?);
        }
        tokio::select! {
            _ = &mut interrupt => break,
            _ = pump.tick() => {
                let _timing = crate::action_timing::Timing::slow("owner_pump");
                #[cfg(feature = "test-support")]
                if qualification.deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                    qualification.expired = true; qualification.release();
                }
                // Flush a coalesced checkpoint once inbound input has paused
                // (Stage 1/9 cadence). Publishing the latest full state is
                // idempotent; the relay retains only the newest per channel.
                if cadence.due_on_idle(std::time::Instant::now()) {
                    if let Some(current) = session.as_ref().filter(|s| s.is_ready()) {
                        tracing::debug!(site = "idle_flush", "publish site");
                        if let Err(problem) = publish(&owner, current) {
                            error = Some(problem.to_string());
                            if let Some(old) = session.take() { close_session(old, &format!("idle checkpoint flush failed: {problem}")).await; }
                        }
                    }
                }

                if let Some(runtime) = actions.as_mut() {
                    if let Err(problem) = runtime.pump(&owner) { action_error.get_or_insert(problem); }
                    if action_error.is_none() && runtime.ready() {
                        if let Some(current) = session.as_mut().filter(|s| s.is_ready()) {
                            if current.actions.is_none() {
                                #[cfg(feature = "test-support")]
                                let action_registration_barrier = qualification.take_action_registration_barrier();
                                #[cfg(feature = "test-support")]
                                let action_attachment_exit = qualification.replace_action_attachment_exit();
                                if let Err(problem) = current.attach(
                                    runtime,
                                    owner.peer().room().parse()?,
                                    #[cfg(feature = "test-support")]
                                    action_registration_barrier,
                                    #[cfg(feature = "test-support")]
                                    action_attachment_exit,
                                ) { action_error = Some(problem.to_string()); }
                            }
                        }
                    }
                }
                let attachment_exit = if action_error.is_none() {
                    if let Some(current) = session.as_mut().filter(|current| current.actions.as_ref().is_some_and(|attached| attached.task.as_ref().is_some_and(|task| task.is_finished()))) {
                        let mut attached = current.actions.take().expect("finished attachment exists");
                        Some(attached.task.take().expect("finished attachment task exists").await)
                    } else { None }
                } else { None };
                match attachment_exit {
                    Some(Ok(Err(qos_action_broker::peer_actions::PeerActionAttachmentError::TransportClosed))) => {
                        error = Some("private broker transport stopped; reconnecting shared session".into());
                        if let Some(old) = session.take() { old.close().await; }
                    }
                    Some(_) => action_error = Some("private broker attachment stopped; outcome unknown".into()),
                    None => {}
                }
                if action_error.is_some() {
                    *authority.0.lock().map_err(|_| anyhow::anyhow!("authority unavailable"))? = None;
                    if let Some(old) = session.take() { close_session(old, &format!("action attachment failed: {}", action_error.as_deref().unwrap_or(""))).await; }
                }
            }
            // Replacement keeps bounded durable work pending until its exact
            // retained session can publish before any completion is exposed.
            pending = commits.recv(), if session.as_ref().is_some_and(|current| current.is_ready()) => if let Some(pending) = pending {
                let _timing = crate::action_timing::Timing::slow("owner_commit");
                let mut current = authority.0.lock().map_err(|_| anyhow::anyhow!("authority unavailable"))?;
                let desktop = pending.is_desktop();
                let result = if desktop { pending.apply_desktop(&mut owner) } else { pending.apply(&mut owner).map(|_| None) };
                if let Ok(Some(delivery)) = &result {
                    if let Some(session) = session.as_ref() {
                        if let Err(problem) = session.send_action(delivery.clone()) {
                            tracing::debug!(%problem, "direct desktop delivery unavailable; retained publication follows");
                        }
                    }
                }
                *current = (owner.permits_publication() && action_error.is_none()).then(|| owner.peer().clone());
                // An owner commit reacting to remote input (an acknowledgement or
                // effect receipt the native's apps derive from each browser delta)
                // is coalesced by the same cadence as the inbound path; otherwise
                // a live browser typing drives one native checkpoint per key. The
                // commit is durable locally regardless; only the wire checkpoint
                // is deferred and flushes on idle or at the cap. Explicit
                // desktop gestures publish immediately through the same cursor.
                let due = owner_commit_publication_due(&mut cadence, std::time::Instant::now(), desktop);
                let published = if due { tracing::debug!(site = "owner_commit", "publish site"); session.as_ref().filter(|s| s.is_ready()).ok_or_else(|| anyhow::anyhow!("owner committed while disconnected; publication pending")).and_then(|s| publish(&owner, s)) } else { Ok(()) };
                if let Some(runtime) = actions.as_ref() { if let Err(problem) = runtime.committed(&owner) { action_error = Some(signing.fail(problem)); } }
                if let Err(problem) = result { action_error = Some(problem); }
                if let Err(problem) = published { action_error = Some(signing.fail(problem.to_string())); }
                *current = (owner.permits_publication() && action_error.is_none()).then(|| owner.peer().clone());
            },
            pending = signatures.recv(), if session.as_ref().is_some_and(|current| current.is_ready()) => if let Some(pending) = pending {
                let timing = pending.diagnostic_timing();
                #[cfg(feature = "test-support")]
                let hold = qualification.route.as_ref().is_some_and(|(app, record)| pending.matches_route(app, record));
                let mut current = authority.0.lock().map_err(|_| anyhow::anyhow!("authority unavailable"))?;
                timing.mark("apply_start");
                let (complete, result) = pending.apply(&mut owner);
                timing.mark("apply_end");
                *current = (owner.permits_publication() && action_error.is_none()).then(|| owner.peer().clone());
                timing.mark("publication_start");
                let published = if signed_commit_publication_due(&mut cadence, std::time::Instant::now()) {
                    tracing::debug!(site = "signed_commit", "publish site");
                    session.as_ref().filter(|s| s.is_ready()).ok_or_else(|| anyhow::anyhow!("signed commit publication pending; native peer unavailable")).and_then(|s| publish(&owner, s))
                } else {
                    Ok(())
                };
                timing.mark("publication_end");
                timing.mark("runtime_committed_start");
                if let Some(runtime) = actions.as_ref() { if let Err(problem) = runtime.committed(&owner) { action_error = Some(signing.fail(problem)); } }
                timing.mark("runtime_committed_end");
                if let Err(problem) = result { action_error = Some(problem); }
                if let Err(problem) = published { action_error = Some(signing.fail(problem.to_string())); }
                *current = (owner.permits_publication() && action_error.is_none()).then(|| owner.peer().clone());
                #[cfg(feature = "test-support")]
                if hold {
                    qualification.route = None;
                    qualification.completion = Some(Box::new(complete));
                    qualification.deadline = Some(std::time::Instant::now() + Duration::from_secs(4));
                } else { complete(); }
                #[cfg(not(feature = "test-support"))]
                complete();
            },
            message = network.recv() => if let Some(message) = message {
                if let Some(current) = session.as_ref().filter(|s| s.is_ready()) {
                    if let Err(problem) = current.publish_presence(postcard::to_stdvec(&message)?) { action_error = Some(signing.fail(problem.to_string())); }
                }
            },
            connection = async { match reconnect.as_mut() {Some(attempt)=>(&mut attempt.task).await.context("native reconnect task failed")?,None=>std::future::pending().await} }, if reconnect.is_some() => {
                reconnect.take();
                match connection {
                    Ok(next) => {
                        let _timing = crate::action_timing::Timing::new("subscription_replaying");
                        session=Some(next); replaying=true; error=Some("Native subscription is applying retained state".into());
                    }
                    Err(problem) => { tracing::warn!(%problem, "native reconnect failed"); error = Some(problem.to_string()); reconnect_policy.connect_failed(); }
                }
            }
            inbound = async { match session.as_mut() { Some(current) => current.recv().await, None => std::future::pending().await } } => {
                match inbound {
                    Some(Inbound::Ready { pending }) => {
                        replaying = false;
                        reconnect_policy.ready(std::time::Instant::now());
                        let _timing = crate::action_timing::Timing::new("subscription_ready_work");
                        subscription_generation = subscription_generation.checked_add(1).context("native subscription generation exhausted")?;
                        tracing::info!(replayed = pending, generation = subscription_generation, "native subscription ready after relay replay");
                        prepare_ready_publication(&owner)?;
                        if action_error.is_none() {
                            *authority.0.lock().map_err(|_| anyhow::anyhow!("authority unavailable"))? = Some(owner.peer().clone());
                            if actions.is_none() {
                                match crate::action_host::ActionHost::start(&owner, authority.clone(), signing.clone(), network_sender.clone()) {
                                    Ok(runtime) => actions = Some(runtime),
                                    Err(problem) => { action_error = Some(signing.fail(problem.to_string())); *authority.0.lock().unwrap() = None; }
                                }
                            }
                        }
                        if let Some(current)=session.as_ref() {
                            tracing::debug!(site = "ready", "publish site");
                            match publish(&owner,current) {
                                Ok(())=>{error=None; cadence.mark_published(std::time::Instant::now());}
                                Err(problem)=>{error=Some(problem.to_string());if let Some(old)=session.take(){close_session(old, &format!("initial publication failed: {problem}")).await;}}
                            }
                        }
                    },
                    Some(message) => {
                        let _timing = crate::action_timing::Timing::slow("owner_inbound");
                        let (origin, bytes) = match message {
                            Inbound::Delta {author,data} => (
                                if replaying { qos_peer_state::TransportOrigin::RetainedRelayDelta(author) }
                                else { qos_peer_state::TransportOrigin::RelayDelta(author) },
                                data,
                            ),
                            Inbound::Presence {author,payload} => (qos_peer_state::TransportOrigin::UntrustedPresence(author),payload),
                            Inbound::Ready {..} => unreachable!(),
                        };
                        let inbound_bytes = bytes.len();
                        let decoded: Result<qos_peer_state::NeighbourMsg> = postcard::from_bytes(&bytes).map_err(anyhow::Error::from);
                        let kind = decoded.as_ref().map(message_kind).unwrap_or("undecodable");
                        let started = std::time::Instant::now();
                        let received = {
                            let mut current = authority.0.lock().map_err(|_| anyhow::anyhow!("authority unavailable"))?;
                            let received = decoded.and_then(|message|owner.receive(origin,message));
                            *current = (owner.permits_publication() && action_error.is_none()).then(|| owner.peer().clone());
                            received
                        };
                        match &received {
                            Ok(receipt) => tracing::debug!(kind, bytes = inbound_bytes, committed = receipt.is_some(), elapsed_us = started.elapsed().as_micros() as u64, "inbound applied"),
                            Err(problem) => tracing::warn!(kind, bytes = inbound_bytes, %problem, "inbound rejected"),
                        }
                        if received.as_ref().is_ok_and(|receipt| receipt.is_some()) { if let Some(runtime) = actions.as_ref() { runtime.committed(&owner).map_err(anyhow::Error::msg)?; } host.update_membership(&owner).await?; }
                        match received {
                            Ok(receipt) => {
                                if !owner.permits_publication() {error=owner.peer().incomplete_reason();if let Some(old)=session.take(){close_session(old, "owner incomplete after inbound").await;}}
                                else if receipt.is_some() {
                                    // Steady-state document deltas are coalesced by the cadence so a
                                    // continuous typist never floods a live peer with a checkpoint;
                                    // a SyncReq or any non-document commit is a bootstrap/catch-up
                                    // request and publishes immediately so the requester converges.
                                    let now = std::time::Instant::now();
                                    let due = if kind == "AppDocument" { cadence.on_commit(now) } else { cadence.mark_published(now); true };
                                    if due { tracing::debug!(site = if kind == "AppDocument" { "inbound_delta_cap" } else { "inbound_bootstrap" }, "publish site"); if let Some(current) = session.as_ref().filter(|current|current.is_ready()) { if let Err(problem) = publish(&owner,current) { error = Some(problem.to_string()); if let Some(old)=session.take(){ close_session(old, &format!("publication failed after receipt: {problem}")).await; } } } }
                                }
                            },
                            Err(problem) => { error = Some(problem.to_string()); if !owner.permits_publication() { if let Some(old)=session.take(){close_session(old, "inbound rejected and owner incomplete").await;} } }
                        }
                    }
                    None => { let _timing = crate::action_timing::Timing::new("subscription_disconnected"); error=Some("native subscription disconnected; reconnecting with durable state".into()); if let Some(old)=session.take(){close_session(old, "relay stream ended").await;} }
                }
            }
            command = enrollments.recv() => {
                if let Some(command)=command {
                    let result=if !session.as_ref().is_some_and(|current|current.is_ready()) {
                        Err(crate::enrollment::EnrollmentError::new(503,"native peer is reconnecting; retry enrollment with the same identity"))
                    } else {
                        match command.request {
                            crate::enrollment_http::EnrollmentRequest::Challenge(request)=>enrollment.challenge(&owner,request,tilo_relay::clock::now_ns()).and_then(|response|serde_json::to_vec(&response).map_err(|error|crate::enrollment::EnrollmentError::new(503,error.to_string()))),
                            crate::enrollment_http::EnrollmentRequest::Complete(request)=>{
                                let enrolled = {
                                    let mut current = authority.0.lock().map_err(|_| anyhow::anyhow!("authority unavailable"))?;
                                    let enrolled = enrollment.complete(&mut owner,request,tilo_relay::clock::now_ns());
                                    *current = (owner.permits_publication() && action_error.is_none()).then(|| owner.peer().clone());
                                    enrolled
                                };
                                match enrolled {
                                    Err(error)=>Err(error),
                                    Ok(certificate)=>match host.update_membership(&owner).await {
                                        Err(error)=>Err(crate::enrollment::EnrollmentError::new(503,format!("committed enrollment awaits relay membership reconciliation: {error}"))),
                                        Ok(())=>{
                                            let published=session.as_ref().ok_or_else(||anyhow::anyhow!("native subscription unavailable")).and_then(|current|publish(&owner,current));
                                            published.map_err(|error|crate::enrollment::EnrollmentError::new(503,error.to_string())).and_then(|()|crate::enrollment::public_bootstrap(&owner,&certificate,serde_json::to_value(&host.address).map_err(|error|crate::enrollment::EnrollmentError::new(503,error.to_string()))?,&host.packet_url.to_string(),tilo_relay::clock::now_ns())).and_then(|value|serde_json::to_vec(&value).map_err(|error|crate::enrollment::EnrollmentError::new(503,error.to_string())))
                                        }
                                    }
                                }
                            }
                        }
                    };
                    let _=command.reply.send(result);
                }
            }
            result = listener.accept() => {
                let (socket,_)=result?; let mut status=public_status(&owner,session.as_ref().is_some_and(|current|current.is_ready()),&error);
                status["actions"] = json!(if action_error.is_some() { "unavailable" } else if actions.as_ref().is_some_and(|runtime| runtime.ready()) && session.as_ref().is_some_and(|s| s.actions.as_ref().is_some_and(AttachedActions::is_ready)) { "ready" } else { "starting" });
                status["action_error"] = json!(action_error);
                #[cfg(feature = "test-support")]
                {
                    status["qualification"] = json!({"armed": qualification.route.is_some(), "held": qualification.completion.is_some(), "expired": qualification.expired,
                        "workers": actions.as_mut().map(|runtime| runtime.qualification_heads(owner.peer().room())),
                        "action_registration": qualification.action_registration.as_ref().map(|hold| json!({
                            "armed": hold.barrier.is_some(),
                            "reached": hold.reached.load(std::sync::atomic::Ordering::Acquire),
                        })),
                        "action_attachment_exit_armed": qualification.action_attachment_exit.is_some(),
                        "publication": {
                            "room_heads": owner.peer().admitted_room().map(|room| room.clone().get_heads().iter().map(ToString::to_string).collect::<Vec<_>>()),
                            "receipts": owner.peer().rows().iter().map(|row| owner.peer().durable_coverage(&row.app_id, &row.record_id)).collect::<Result<Vec<_>, _>>().map_err(anyhow::Error::msg)?,
                        }});
                }
                status["native_subscription_generation"] = json!(subscription_generation);
                let authorization=authorization.clone();let commands=enrollment_commands.clone();
                tokio::spawn(async move { let _=crate::enrollment_http::serve(socket,authorization,commands,status).await; });
            }
            line = stdin.recv(), if stdin_open => {
                match line.transpose()? {
                    None => stdin_open=false,
                    #[cfg(feature = "test-support")]
                    Some(line) if qualification.command(&line) => {},
                    Some(line) if line == "shutdown" => break,
                    Some(line) if line == "reconnect" => { if let Some(old)=session.take(){close_session(old, "operator reconnect").await;} reconnect_policy.operator_reconnect(); },
                    Some(line) if line == "status" => println!("{}",public_status(&owner,session.as_ref().is_some_and(|current|current.is_ready()),&error)),
                    Some(line) if line == "flush" => println!("{}",serde_json::to_string(&owner.flush()?)?),
                    Some(line) if line.starts_with("export ") => { match owner.export(Path::new(&line[7..])) {Ok(())=>println!("Export committed"),Err(problem)=>eprintln!("Export failed: {problem}")} },
                    Some(_) => eprintln!("Use status, flush, reconnect, export /absolute/path, or shutdown"),
                }
            }
        }
    }
        Ok(())
    }.await;
    if let Some(attempt) = reconnect.take() {
        attempt.cancel().await;
    }
    *authority
        .0
        .lock()
        .map_err(|_| anyhow::anyhow!("authority unavailable"))? = None;
    let workers_closed = actions
        .as_mut()
        .map(|runtime| runtime.close(&owner))
        .transpose();
    let flushed = owner.flush();
    if let Some(current) = session {
        current.close().await;
    }
    host.shutdown().await?;
    std::fs::remove_file(ready_path)?;
    workers_closed.map_err(anyhow::Error::msg)?;
    run_result?;
    flushed?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::Watcher;

    #[test]
    fn ready_reuses_the_exact_committed_publication() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("owner");
        let owner = SessionOwner::initialize(&root).unwrap();
        let generation = owner.status().generation;
        let baseline = owner.publication_baseline();
        let outgoing = owner.outgoing().to_vec();
        let kinds = owner.outgoing_kinds().to_vec();
        drop(owner);

        let owner = SessionOwner::open(&root).unwrap();
        assert_eq!(owner.status().generation, generation);
        assert_eq!(owner.publication_baseline(), baseline);
        assert_eq!(owner.outgoing(), outgoing);

        prepare_ready_publication(&owner).unwrap();

        assert_eq!(owner.status().generation, generation);
        assert_eq!(owner.publication_baseline(), baseline);
        assert_eq!(owner.outgoing(), outgoing);
        let mut delivered = Vec::new();
        let mut cursor = crate::publication::PublicationCursor::default();
        cursor
            .send(&owner, |bytes, kind| {
                delivered.push((bytes.to_vec(), kind));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            delivered,
            outgoing.into_iter().zip(kinds).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn private_packet_map_matches_actual_server_capabilities() {
        let temp = tempfile::tempdir().unwrap();
        let owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        // Exercise the real construction, including the exact mode cloned into
        // the qOS relay endpoint and retained for every fresh native endpoint.
        let host = LocalHost::start(&owner).await.unwrap();
        let map = host.mode.relay_map();
        let advertised = map.get(&host.packet_url).unwrap();
        let actual_qad_port = host.packet.quic_addr().map(|address| address.port());
        let advertised_qad_port = advertised.quic.as_ref().map(|config| config.port);
        let status = host.relay.endpoint.home_relay_status().get();
        let connected = status
            .iter()
            .any(|relay| relay.url() == &host.packet_url && relay.is_connected());
        eprintln!(
            "private_packet_capabilities {}",
            json!({"server_qad_port": actual_qad_port, "map_qad_port": advertised_qad_port, "connected": connected, "url": host.packet_url})
        );
        host.shutdown().await.unwrap();
        assert!(connected, "the actual private HTTP relay must be online");
        assert_eq!(map.len(), 1);
        assert_eq!(
            advertised_qad_port, actual_qad_port,
            "private endpoints must advertise only the packet server's real QAD listener"
        );
    }
}

#[cfg(test)]
mod reconnect_policy_tests {
    use super::{MAX_RECONNECT_DELAY, ReconnectPolicy, STABLE_SESSION};
    use std::time::{Duration, Instant};

    #[test]
    fn rapid_session_losses_and_failed_connects_share_the_capped_ladder() {
        let start = Instant::now();
        let mut policy = ReconnectPolicy::default();

        policy.observe_session(true, start);
        policy.ready(start);
        policy.observe_session(false, start + STABLE_SESSION - Duration::from_millis(1));
        assert_eq!(policy.delay(), Duration::from_secs(1));

        policy.observe_session(true, start + STABLE_SESSION);
        policy.observe_session(false, start + STABLE_SESSION);
        assert_eq!(policy.delay(), Duration::from_secs(2));

        for expected in [4, 8, 15, 15] {
            policy.connect_failed();
            assert_eq!(policy.delay(), Duration::from_secs(expected));
        }
        assert_eq!(policy.delay(), MAX_RECONNECT_DELAY);
    }

    #[test]
    fn first_ready_timestamp_credits_only_a_stable_session() {
        let start = Instant::now();
        let mut policy = ReconnectPolicy::default();
        policy.connect_failed();
        policy.connect_failed();
        assert_eq!(policy.delay(), Duration::from_secs(2));

        policy.observe_session(true, start);
        policy.ready(start);
        policy.ready(start + Duration::from_secs(29));
        policy.observe_session(false, start + STABLE_SESSION);
        assert_eq!(policy.delay(), Duration::ZERO);
    }

    #[test]
    fn operator_reconnect_stays_immediate_after_loop_loss_accounting() {
        let start = Instant::now();
        let mut policy = ReconnectPolicy::default();
        policy.connect_failed();
        policy.observe_session(true, start);
        policy.ready(start);

        policy.operator_reconnect();
        policy.observe_session(false, start + Duration::from_secs(1));

        assert_eq!(policy.delay(), Duration::ZERO);
        assert!(!policy.had_session());
    }
}

#[cfg(test)]
mod cadence_tests {
    use super::{CheckpointCadence, owner_commit_publication_due, signed_commit_publication_due};
    use std::time::{Duration, Instant};

    #[test]
    fn burst_of_fifty_deltas_yields_one_checkpoint_at_the_cadence_not_per_delta() {
        // A 50-delta burst tighter than IDLE must NOT publish a full checkpoint
        // per delta (that fanned a multi-megabyte checkpoint back to the browser
        // per key — the storm). With the cap well above a burst, the whole burst
        // coalesces and flushes as exactly one checkpoint when input pauses. The
        // cadence only ever defers, so no reconnect can result from it.
        let mut cadence = CheckpointCadence::new();
        let start = Instant::now();
        let mut during_burst = 0u32;
        for i in 0..50u32 {
            // 10 ms spacing keeps the whole 490 ms burst inside one IDLE window.
            let now = start + Duration::from_millis(u64::from(i) * 10);
            if cadence.on_commit(now) {
                during_burst += 1;
            }
        }
        assert_eq!(
            during_burst, 0,
            "a 50-delta burst under the cap publishes nothing mid-stream"
        );
        assert!(
            cadence.pending,
            "the burst is coalesced, awaiting an idle flush"
        );
        let idle = start + Duration::from_millis(50 * 10) + CheckpointCadence::IDLE;
        assert!(
            cadence.due_on_idle(idle),
            "the paused burst flushes exactly one checkpoint"
        );
        assert!(
            !cadence.due_on_idle(idle),
            "an already-flushed idle does not re-fire"
        );
        assert!(!cadence.pending);
    }

    #[test]
    fn continuous_input_still_anchors_at_the_hard_delta_cap() {
        // Even a typist who never pauses gets a bounded-staleness anchor: the
        // per-delta cap forces exactly one checkpoint once it is reached.
        let mut cadence = CheckpointCadence::new();
        let start = Instant::now();
        let mut publishes = 0u32;
        for i in 0..(CheckpointCadence::MAX_DELTAS + 5) {
            let now = start + Duration::from_millis(u64::from(i)); // 1 ms apart, never idle
            if cadence.on_commit(now) {
                publishes += 1;
            }
        }
        assert_eq!(
            publishes, 1,
            "the hard cap fires exactly once across MAX_DELTAS+5 deltas"
        );
    }

    #[test]
    fn idle_does_not_fire_during_continuous_input() {
        let mut cadence = CheckpointCadence::new();
        let start = Instant::now();
        cadence.on_commit(start);
        // A tick shorter than IDLE after activity must not flush.
        assert!(!cadence.due_on_idle(start + CheckpointCadence::IDLE - Duration::from_millis(1)));
        assert!(cadence.pending);
    }

    #[test]
    fn signed_app_commits_are_coalesced_by_the_host_cadence() {
        let mut cadence = CheckpointCadence::new();
        let start = Instant::now();
        for offset in 0..50 {
            assert!(!signed_commit_publication_due(
                &mut cadence,
                start + Duration::from_millis(offset),
            ));
        }
        assert!(cadence.pending);
        assert!(cadence.due_on_idle(start + Duration::from_millis(50) + CheckpointCadence::IDLE));
    }

    #[test]
    fn desktop_gesture_flushes_pending_publication_without_waiting_for_idle() {
        let mut cadence = CheckpointCadence::new();
        let start = Instant::now();
        assert!(!owner_commit_publication_due(&mut cadence, start, false));
        let gesture = start + Duration::from_millis(20);
        assert!(owner_commit_publication_due(&mut cadence, gesture, true));
        assert!(!cadence.due_on_idle(gesture + CheckpointCadence::IDLE));
        assert!(!owner_commit_publication_due(
            &mut cadence,
            gesture + CheckpointCadence::IDLE,
            false
        ));
    }
}
