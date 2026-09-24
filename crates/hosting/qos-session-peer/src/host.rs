//! Existing packet relay + authenticated qOS relay, with a replaceable native subscription.
mod refresh;
use crate::SessionOwner;
use anyhow::{Context, Result, bail};
use iroh::{Endpoint, RelayMap, RelayMode, endpoint::presets};
use iroh_base::{EndpointAddr, RelayUrl};
use iroh_relay::server::{RelayConfig as PacketConfig, Server, ServerConfig};
#[cfg(feature = "test-support")]
pub use refresh::CheckpointRefreshTestOwner;
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
    publication_failure: QualificationPublicationFailure,
}
#[cfg(feature = "test-support")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum QualificationPublicationFailure {
    #[default]
    Absent,
    ArmedDisconnect,
    ArmedEncode,
    FiredDisconnect,
    FiredEncode,
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
        if line == "qualification publication-disconnect" {
            if self.publication_failure == QualificationPublicationFailure::Absent {
                self.publication_failure = QualificationPublicationFailure::ArmedDisconnect;
            }
            return true;
        }
        if line == "qualification publication-encode" {
            if self.publication_failure == QualificationPublicationFailure::Absent {
                self.publication_failure = QualificationPublicationFailure::ArmedEncode;
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

    fn publication_failure_once(&mut self) -> Option<anyhow::Error> {
        match self.publication_failure {
            QualificationPublicationFailure::ArmedDisconnect => {
                self.publication_failure = QualificationPublicationFailure::FiredDisconnect;
                Some(anyhow::Error::new(tilo_relay::CoeditError::Disconnected))
            }
            QualificationPublicationFailure::ArmedEncode => {
                self.publication_failure = QualificationPublicationFailure::FiredEncode;
                Some(anyhow::Error::new(tilo_relay::CoeditError::Encode(
                    postcard::Error::SerializeBufferFull,
                )))
            }
            _ => None,
        }
    }

    fn publication_failure_status(&self) -> &'static str {
        match self.publication_failure {
            QualificationPublicationFailure::Absent => "absent",
            QualificationPublicationFailure::ArmedDisconnect => "armed_disconnect",
            QualificationPublicationFailure::ArmedEncode => "armed_encode",
            QualificationPublicationFailure::FiredDisconnect => "fired_disconnect",
            QualificationPublicationFailure::FiredEncode => "fired_encode",
        }
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

/// Owner-side cadence for durable baseline replacement.
///
/// Ordinary commits append a bounded room delta and only their dirty route
/// snapshots. Those selective tails publish immediately. When the durable tail
/// reaches its count/byte bound, the owner replaces it with a complete recovery
/// basis; that multi-record replacement and any following tails remain coalesced
/// until idle or the hard commit cap. Reconnect/catch-up paths can still force
/// the latest complete basis immediately.
struct CheckpointCadence {
    deltas_since_publish: u32,
    pending: bool,
    replacement_pending: bool,
    last_activity: std::time::Instant,
}
impl CheckpointCadence {
    /// Publish a pending replacement at latest after this many committed
    /// changes even under continuous input.
    const MAX_DELTAS: u32 = 1024;
    /// Flush a pending complete basis once input pauses for this long.
    const IDLE: Duration = Duration::from_millis(5000);
    fn new() -> Self {
        Self {
            deltas_since_publish: 0,
            pending: false,
            replacement_pending: false,
            last_activity: std::time::Instant::now(),
        }
    }
    fn publication_due(
        &mut self,
        advance: crate::owner::PublicationAdvance,
        now: std::time::Instant,
        transition_ready: bool,
        force: bool,
    ) -> bool {
        use crate::owner::PublicationAdvance;
        if advance == PublicationAdvance::None && !force {
            return false;
        }
        self.last_activity = now;
        self.pending = true;
        if advance != PublicationAdvance::None {
            self.deltas_since_publish = self.deltas_since_publish.saturating_add(1);
        }
        if matches!(advance, PublicationAdvance::BaselineReplacement { .. }) {
            self.replacement_pending = true;
        }
        force
            || (transition_ready
                && matches!(advance, PublicationAdvance::BaselineReplacement { .. }))
            || (!self.replacement_pending
                && matches!(advance, PublicationAdvance::SelectiveTail { .. }))
            || self.deltas_since_publish >= Self::MAX_DELTAS
    }
    /// On an idle tick: flush the coalesced checkpoint once input has paused.
    fn due_on_idle(&self, now: std::time::Instant) -> bool {
        self.pending && now.duration_since(self.last_activity) >= Self::IDLE
    }
    /// A checkpoint was published (through any path); reset the coalescing state.
    fn mark_published(&mut self, now: std::time::Instant) {
        self.deltas_since_publish = 0;
        self.pending = false;
        self.replacement_pending = false;
        self.last_activity = now;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationDispatch {
    None,
    DeferredBaseline { entries: usize, bytes: usize },
    PublishedSelective { entries: usize, bytes: usize },
    PublishedBaseline { entries: usize, bytes: usize },
    PublishedTransition { entries: usize, bytes: usize },
    PublishedForced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationFailureAction {
    Reconnect,
    Fatal,
}

fn committed_publication_failure(problem: &anyhow::Error) -> PublicationFailureAction {
    match problem.downcast_ref::<tilo_relay::CoeditError>() {
        Some(tilo_relay::CoeditError::Disconnected) => PublicationFailureAction::Reconnect,
        Some(tilo_relay::CoeditError::Encode(_)) | None => PublicationFailureAction::Fatal,
    }
}

fn dispatch_committed_publication(
    cadence: &mut CheckpointCadence,
    owner: &SessionOwner,
    before: crate::owner::PublicationRevision,
    now: std::time::Instant,
    transition_ready: bool,
    force: bool,
    send: impl FnOnce() -> Result<()>,
) -> Result<PublicationDispatch> {
    use crate::owner::PublicationAdvance;
    let started = std::time::Instant::now();
    let advance = owner.publication_advance(before);
    let (class, entries, bytes) = match advance {
        PublicationAdvance::None => ("none", 0, 0),
        PublicationAdvance::SelectiveTail { entries, bytes } => ("selective_tail", entries, bytes),
        PublicationAdvance::BaselineReplacement { entries, bytes } => {
            ("baseline_replacement", entries, bytes)
        }
    };
    let (published, deferred) = match advance {
        PublicationAdvance::None if !force => {
            return Ok(PublicationDispatch::None);
        }
        PublicationAdvance::None => (PublicationDispatch::PublishedForced, None),
        PublicationAdvance::SelectiveTail { entries, bytes } => {
            let deferred =
                cadence
                    .replacement_pending
                    .then(|| PublicationDispatch::DeferredBaseline {
                        entries: owner.outgoing().len(),
                        bytes: owner.outgoing().iter().map(|p| p.bytes.len()).sum(),
                    });
            let published = if cadence.replacement_pending {
                PublicationDispatch::PublishedBaseline {
                    entries: owner.outgoing().len(),
                    bytes: owner.outgoing().iter().map(|p| p.bytes.len()).sum(),
                }
            } else {
                PublicationDispatch::PublishedSelective { entries, bytes }
            };
            (published, deferred)
        }
        PublicationAdvance::BaselineReplacement { entries, bytes } => {
            let published = if transition_ready {
                let transition = owner
                    .live_publication_transition()
                    .expect("ready transition exists");
                PublicationDispatch::PublishedTransition {
                    entries: transition.messages().len(),
                    bytes: transition
                        .messages()
                        .iter()
                        .map(|(bytes, _)| bytes.len())
                        .sum(),
                }
            } else {
                PublicationDispatch::PublishedBaseline { entries, bytes }
            };
            (
                published,
                (!transition_ready)
                    .then_some(PublicationDispatch::DeferredBaseline { entries, bytes }),
            )
        }
    };
    let due = cadence.publication_due(advance, now, transition_ready, force);
    if !due {
        if std::env::var_os("QOS_RELAY_PHASE_DIAGNOSTICS").is_some() {
            tracing::info!(
                phase = "owner_publication_dispatch",
                class,
                entries,
                bytes,
                decision = "deferred",
                success = true,
                elapsed_us = started.elapsed().as_micros() as u64
            );
        }
        return Ok(deferred.unwrap());
    }
    if let Err(problem) = send() {
        if std::env::var_os("QOS_RELAY_PHASE_DIAGNOSTICS").is_some() {
            tracing::info!(
                phase = "owner_publication_dispatch",
                class,
                entries,
                bytes,
                decision = "publish",
                success = false,
                elapsed_us = started.elapsed().as_micros() as u64
            );
        }
        return Err(problem);
    }
    cadence.mark_published(now);
    if std::env::var_os("QOS_RELAY_PHASE_DIAGNOSTICS").is_some() {
        tracing::info!(
            phase = "owner_publication_dispatch",
            class,
            entries,
            bytes,
            decision = "publish",
            success = true,
            elapsed_us = started.elapsed().as_micros() as u64
        );
    }
    Ok(published)
}

fn dispatch_owner_commit_publication(
    cadence: &mut CheckpointCadence,
    owner: &SessionOwner,
    before: crate::owner::PublicationRevision,
    now: std::time::Instant,
    desktop: bool,
    transition_ready: impl FnOnce(&SessionOwner) -> bool,
    send: impl FnOnce(bool) -> Result<()>,
) -> Result<PublicationDispatch> {
    let transition_ready = transition_ready(owner);
    dispatch_committed_publication(
        cadence,
        owner,
        before,
        now,
        transition_ready,
        desktop,
        || send(transition_ready),
    )
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
    async fn detach_actions(&mut self) {
        if let Some(mut attached) = self.actions.take() {
            if let Some(task) = attached.task.take() {
                task.abort();
                let _ = task.await;
            }
        }
    }
    async fn close(mut self) {
        self.detach_actions().await;
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
        let (join, _commands) = crate::collaboration::CollaborationHandle::bounded(1);
        Self::start_with_collaboration(owner, join).await
    }

    pub(crate) async fn start_with_collaboration(
        owner: &SessionOwner,
        join: crate::collaboration::CollaborationHandle,
    ) -> Result<Self> {
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
        let relay = Relay::start_with_relay_mode_and_protocol(
            config,
            RelayIdentity {
                user_public_key: owner.identity.root.verifying_key().to_bytes(),
                device_signing_key: owner.identity.host.clone(),
                device_cert: owner.identity.host_cert.clone(),
                revocations: revocations(owner)?,
            },
            mode.clone(),
            tilo_relay::workspace_join::WORKSPACE_JOIN_ALPN,
            tilo_relay::workspace_join::WorkspaceJoinProtocol::new(join),
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
        let access = owner
            .peer()
            .governed_access(web_time::SystemTime::now())
            .map_err(anyhow::Error::msg)?;
        self.relay
            .replace_governed_workspace_access(
                owner.workspace_id(),
                true,
                access,
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
            if !relay_publishable(owner, &message) {
                return Ok(());
            }
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

/// Delivery commits publish uploaded items as their authors' original signed
/// bytes (see `owner/retention.rs`). The relay co-edit carrier attributes
/// every delta to this host's device, and its receivers admit a document only
/// from its transport author, so foreign-authored items stay on the delivery
/// carrier (which keeps original signer provenance). Relay subscribers
/// converge on the next certified checkpoint, which this host signs.
fn relay_publishable(owner: &SessionOwner, message: &qos_peer_state::NeighbourMsg) -> bool {
    match message {
        qos_peer_state::NeighbourMsg::AppDocument { envelope, .. } => {
            envelope.author.as_bytes() == &owner.peer().authority().device_public_key
        }
        _ => true,
    }
}

fn publish_committed(owner: &SessionOwner, session: &NativeSubscription) -> Result<()> {
    if !session.is_ready() {
        bail!("native subscription has not finished its initial replay");
    }
    use qos_peer_state::transport::{Retention, retention};
    session
        .publication
        .lock()
        .map_err(|_| anyhow::anyhow!("publication cursor unavailable"))?
        .send_committed(owner, |bytes, kind| {
            let message: qos_peer_state::NeighbourMsg = postcard::from_bytes(bytes)?;
            if !relay_publishable(owner, &message) {
                return Ok(());
            }
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

fn committed_transition_ready(owner: &SessionOwner, session: Option<&NativeSubscription>) -> bool {
    session
        .filter(|session| session.is_ready())
        .and_then(|session| session.publication.lock().ok())
        .is_some_and(|cursor| cursor.can_send_committed(owner))
}

fn prepare_ready_publication(owner: &SessionOwner) -> Result<bool> {
    if !owner.durability_healthy() {
        bail!("native owner durability unavailable");
    }
    // A replay-ready receive stream remains useful while prerequisites are
    // missing. It must not publish an incomplete owner's outgoing state.
    if !owner.permits_publication() {
        return Ok(false);
    }
    if owner.outgoing().is_empty() {
        bail!("native owner has no committed publication evidence");
    }
    Ok(true)
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
    value["application_state"] = json!(if owner.schema_read_only_reason().is_some() {
        "read_only"
    } else if owner.permits_publication() {
        "admitted"
    } else {
        "incomplete"
    });
    value["error"] = json!(
        error
            .as_ref()
            .cloned()
            .or_else(|| owner.schema_read_only_reason().map(str::to_owned))
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

async fn handle_collaboration_exchange(
    owner: &mut SessionOwner,
    committed: &crate::committed_authority::CommittedAuthority,
    host: &LocalHost,
    service: &crate::collaboration::CollaborationService,
    remote_endpoint: [u8; 32],
    request: qos_peer_state::join::JoinTransportRequest,
) -> qos_peer_state::join::JoinTransportResponse {
    use qos_peer_state::join::{JoinInspection, JoinTransportRequest, JoinTransportResponse};

    let now = tilo_relay::clock::now_ns();
    let request_id = match &request {
        JoinTransportRequest::Inspect { signed_invitation } => {
            *blake3::hash(signed_invitation).as_bytes()
        }
        JoinTransportRequest::Challenge(request) => request.request_id,
        JoinTransportRequest::Complete(request) => request.proof.challenge.request_id,
        JoinTransportRequest::MembershipChallenge(request) => request.request_id,
        JoinTransportRequest::MembershipComplete(request) => request.proof.challenge.request_id,
    };
    // All service mutations finish and publish committed authority before the
    // asynchronous relay reconciliation. A relay failure cannot undo admission.
    let mut reconcile = false;
    let result = committed
        .with_owner(owner, |owner| {
            let result: Result<_, (u16, crate::collaboration::CollaborationError)> = match request {
                JoinTransportRequest::Inspect { signed_invitation } => service
                    .inspect(owner, &signed_invitation, now)
                    .map(|verified| {
                        JoinTransportResponse::Inspection(JoinInspection {
                            invite_id: verified.invite_id(),
                            grant: verified.grant().clone(),
                        })
                    })
                    .map_err(|error| (403, error)),
                JoinTransportRequest::Challenge(request) => service
                    .challenge(owner, remote_endpoint, request, now)
                    .map(JoinTransportResponse::Challenge)
                    .map_err(|error| (403, error)),
                JoinTransportRequest::Complete(request) => service
                    .complete(owner, remote_endpoint, request, now)
                    .map(|completion| {
                        reconcile = true;
                        JoinTransportResponse::Complete(completion.bootstrap)
                    })
                    .map_err(|error| (403, error)),
                JoinTransportRequest::MembershipChallenge(request) => service
                    .membership_challenge(owner, remote_endpoint, request, now)
                    .map(JoinTransportResponse::MembershipChallenge)
                    .map_err(|error| (403, error)),
                JoinTransportRequest::MembershipComplete(request) => service
                    .membership_complete(owner, remote_endpoint, request, now)
                    .map(|completion| {
                        reconcile = true;
                        JoinTransportResponse::MembershipComplete(completion.bootstrap)
                    })
                    .map_err(|error| (403, error)),
            };
            result
        })
        .unwrap_or_else(|error| {
            Err((
                503,
                crate::collaboration::CollaborationError::new(error.to_string()),
            ))
        });
    let result = if reconcile {
        match host.update_membership(owner).await {
            Ok(()) => result,
            Err(error) => Err((
                503,
                crate::collaboration::CollaborationError::new(format!(
                    "membership is durable and awaits relay reconciliation: {error}"
                )),
            )),
        }
    } else {
        result
    };
    match result {
        Ok(response) => response,
        Err((code, error)) => {
            tracing::warn!(
                room = %owner.peer().room(),
                request_id = %hex::encode(request_id),
                device = %hex::encode(remote_endpoint),
                outcome = "denied",
                reason = %error,
                "workspace collaboration exchange denied"
            );
            JoinTransportResponse::Error {
                code,
                message: error.to_string(),
            }
        }
    }
}

async fn handle_collaboration_admin(
    owner: &mut SessionOwner,
    committed: &crate::committed_authority::CommittedAuthority,
    host: &LocalHost,
    service: &crate::collaboration::CollaborationService,
    request: qos_action_broker::collaboration_admin::CollaborationAdminRequest,
) -> Result<serde_json::Value, qos_action_broker::collaboration_admin::CollaborationAdminError> {
    use qos_action_broker::collaboration_admin::{
        CollaborationAdminError, CollaborationAdminOperation, CollaborationRole,
    };
    use qos_peer_state::join::MemberRole;

    let rejected = |message: String| CollaborationAdminError::Rejected(message);
    if request.room != owner.peer().room() {
        return Err(rejected(
            "collaboration action is not bound to this workspace".into(),
        ));
    }
    let mut request_material = Vec::with_capacity(32 + request.room.len());
    request_material.extend_from_slice(b"tilo/workspace-collaboration-action/v1\0");
    request_material.extend_from_slice(request.invocation.0.as_bytes());
    request_material.extend_from_slice(request.room.as_bytes());
    let request_id = *blake3::hash(&request_material).as_bytes();
    let now = tilo_relay::clock::now_ns();
    let caller = request.caller.device_public_key;
    let role = |role| match role {
        CollaborationRole::Viewer => MemberRole::Viewer,
        CollaborationRole::Commenter => MemberRole::Commenter,
        CollaborationRole::Editor => MemberRole::Editor,
    };
    let participant = |value: &str| -> Result<[u8; 32], CollaborationAdminError> {
        let bytes = hex::decode(value).map_err(|_| rejected("participant ID is invalid".into()))?;
        bytes
            .try_into()
            .map_err(|_| rejected("participant ID is invalid".into()))
    };
    let invite_id = |value: &str| -> Result<[u8; 32], CollaborationAdminError> {
        let bytes = hex::decode(value).map_err(|_| rejected("invitation ID is invalid".into()))?;
        bytes
            .try_into()
            .map_err(|_| rejected("invitation ID is invalid".into()))
    };
    let mut reconcile = false;
    let result = committed.with_owner(owner, |owner| {
    let result = match request.operation.clone() {
        CollaborationAdminOperation::Issue(input) => {
            let completion = service
                .issue_from_action(
                    owner,
                    caller,
                    request_id,
                    role(input.role),
                    input.invitee_name,
                    input.expires_in_seconds,
                    now,
                )
                .map_err(|error| rejected(error.to_string()))?;
            reconcile = true;
            serde_json::json!({
                "invite": qos_peer_state::join::encode_invitation_text(&completion.issued.signed_invitation),
                "invite_id": hex::encode(completion.issued.invite_id),
                "expires_at_ms": completion.expires_at_ns / 1_000_000,
            })
        }
        CollaborationAdminOperation::Cancel(input) => {
            service
                .cancel(
                    owner,
                    caller,
                    crate::collaboration::CancelInvitationRequest {
                        request_id,
                        invite_id: invite_id(&input.invite_id)?,
                    },
                )
                .map_err(|error| rejected(error.to_string()))?;
            serde_json::json!({})
        }
        CollaborationAdminOperation::List => service
            .invitation_inventory(owner, caller, now)
            .map_err(|error| rejected(error.to_string()))?,
        CollaborationAdminOperation::SetRole(input) => {
            let completion = service
                .set_member_role(
                    owner,
                    caller,
                    crate::collaboration::SetMemberRoleRequest {
                        request_id,
                        user_public_key: participant(&input.participant_id)?,
                        role: role(input.role),
                    },
                    now,
                )
                .map_err(|error| rejected(error.to_string()))?;
            reconcile = true;
            serde_json::json!({"generation": completion.generation.to_string()})
        }
        CollaborationAdminOperation::Remove(input) => {
            let completion = service
                .remove_member(
                    owner,
                    caller,
                    crate::collaboration::RemoveMemberRequest {
                        request_id,
                        user_public_key: participant(&input.participant_id)?,
                    },
                    now,
                )
                .map_err(|error| rejected(error.to_string()))?;
            reconcile = true;
            serde_json::json!({"generation": completion.generation.to_string()})
        }
        CollaborationAdminOperation::MediaCreate(_)
        | CollaborationAdminOperation::MediaEnd(_)
        | CollaborationAdminOperation::MediaRestart(_)
        | CollaborationAdminOperation::MediaSetDirections(_)
        | CollaborationAdminOperation::MediaSetParticipantDenial(_) => {
            serde_json::to_value(service.media_from_action(owner, &request, now)?)
                .map_err(|error| CollaborationAdminError::Uncertain(error.to_string()))?
        }
    };
    Ok::<_, CollaborationAdminError>(result)
    }).map_err(|error| CollaborationAdminError::Uncertain(error.to_string()))??;
    if reconcile {
        host.update_membership(owner)
            .await
            .map_err(|error| CollaborationAdminError::Uncertain(error.to_string()))?;
    }
    Ok(result)
}

#[derive(Default)]
struct InboundPostCommit {
    action_error: Option<String>,
    relay_error: Option<String>,
}
async fn post_inbound_commit(
    owner: &SessionOwner,
    runtime: Option<&crate::action_host::ActionHost>,
    host: &LocalHost,
) -> Result<InboundPostCommit> {
    if !owner.durability_healthy() {
        bail!("native owner durability unavailable");
    }
    // Authority was already published at the owner's durable barrier. Action
    // fanout and relay projection failures are secondary service failures.
    let action_error = if owner.permits_publication() {
        runtime.and_then(|runtime| runtime.committed(owner).err())
    } else {
        None
    };
    let relay_error = host
        .update_membership(owner)
        .await
        .err()
        .map(|error| error.to_string());
    Ok(InboundPostCommit {
        action_error,
        relay_error,
    })
}
/// Routes that only move committed data (or renew a live-relay grant); they
/// never feed hosted action models directly, so they do not hold the action
/// pump at its active cadence. Live grants renew every few seconds while a
/// page is open and previously kept the pump at 10 ms most of the time.
#[cfg(feature = "media")]
pub(crate) fn data_plane_route(route: &crate::https::Route) -> bool {
    use crate::https::Route;
    matches!(
        route,
        Route::State
            | Route::EventsPoll
            | Route::Acks
            | Route::Snapshot
            | Route::SnapshotChunk
            | Route::Detach
            | Route::LiveGrant
    )
}
fn pump_ready_actions(
    owner: &SessionOwner,
    runtime: &mut crate::action_host::ActionHost,
) -> Result<(), String> {
    if !owner.durability_healthy() {
        return Err("native owner durability unavailable".into());
    }
    if owner.permits_publication() {
        runtime.pump(owner)
    } else {
        Ok(())
    }
}
/// The committed enrollment's public reply. An HTTPS local-owner completion on
/// an installed ingress gets inline governance when the whole reply fits and a
/// signed bounded chunked handoff otherwise, so a long history never makes the
/// owner unable to reconnect. A renewal that asked to omit governance (it
/// already holds the history and only needs fresh delivery discovery) gets
/// none. Other callers keep the bounded inline reply.
fn enrollment_reply(
    owner: &mut SessionOwner,
    certificate: &tilo_user_identity::DeviceCert,
    local_proof: Option<crate::enrollment::LocalOnboardingProof>,
    omit_governance: bool,
    host: &LocalHost,
    #[cfg(feature = "media")] delivery_ingress: Option<&mut crate::media_ingress::InstalledIngress>,
) -> Result<Vec<u8>, crate::enrollment::EnrollmentError> {
    use crate::enrollment::EnrollmentError;
    let relay = serde_json::to_value(&host.address)
        .map_err(|error| EnrollmentError::new(503, error.to_string()))?;
    let packet = host.packet_url.to_string();
    let now = tilo_relay::clock::now_ns();
    #[cfg(feature = "media")]
    match (delivery_ingress, local_proof) {
        (Some(installed), Some(proof)) => {
            let mut metadata =
                crate::enrollment::bootstrap_metadata(owner, certificate, relay, &packet, now)?;
            crate::enrollment::append_delivery_hint(&mut metadata, owner, installed)?;
            if omit_governance {
                drop(proof);
                let mut bytes = serde_json::to_vec(&metadata)
                    .map_err(|error| EnrollmentError::new(503, error.to_string()))?;
                bytes.shrink_to_fit();
                // The local HTTPS gateway's frozen full-reply bound.
                if bytes.len() > 1024 * 1024 {
                    return Err(EnrollmentError::new(
                        503,
                        "enrollment metadata exceeds bound",
                    ));
                }
                return Ok(bytes);
            }
            installed
                .local_enrollment_reply(owner, certificate, proof, metadata)
                .map_err(|error| {
                    EnrollmentError::new(
                        503,
                        format!("local enrollment handoff unavailable: {error}"),
                    )
                })
        }
        (installed, _) => {
            let mut value =
                crate::enrollment::public_bootstrap(owner, certificate, relay, &packet, now)?;
            if let Some(installed) = installed {
                crate::enrollment::append_delivery_hint(&mut value, owner, installed)?;
            }
            serde_json::to_vec(&value).map_err(|error| EnrollmentError::new(503, error.to_string()))
        }
    }
    #[cfg(not(feature = "media"))]
    {
        let _ = (local_proof, omit_governance);
        let value = crate::enrollment::public_bootstrap(owner, certificate, relay, &packet, now)?;
        serde_json::to_vec(&value).map_err(|error| EnrollmentError::new(503, error.to_string()))
    }
}

/// A failed Store barrier cannot publish any authority or successful reply.
/// Dropping its update permanently denies the committed view. For a healthy
/// Store, authority identity/generation errors remain fatal internal errors.
fn publish_owner_authority(
    update: crate::committed_authority::OwnerUpdate<'_>,
    owner: &SessionOwner,
) -> Result<bool> {
    if !owner.durability_healthy() {
        drop(update);
        return Ok(false);
    }
    update.publish(owner)?;
    Ok(true)
}

fn reconnect_for_owner(
    owner: &SessionOwner,
    host: &LocalHost,
    delay: Duration,
    action_error: &Option<String>,
) -> Result<Option<Reconnect>> {
    if action_error.is_some() {
        return Ok(None);
    }
    if !owner.durability_healthy() {
        bail!("native owner durability unavailable");
    }
    host.begin_connection(owner, delay).map(Some)
}

/// All action queues belong to one worker runtime and its registered authority.
struct ActionRuntime {
    committed: crate::committed_authority::CommittedAuthority,
    authority: crate::action_host::Authority,
    signing: crate::host_signing::HostSigning,
    signatures: crate::host_signing::SignatureMailbox,
    commits: crate::owner_commit::OwnerCommitMailbox,
    network_sender: tokio::sync::mpsc::Sender<qos_peer_state::NeighbourMsg>,
    network: tokio::sync::mpsc::Receiver<qos_peer_state::NeighbourMsg>,
    actions: Option<crate::action_host::ActionHost>,
    policy_time: Option<qos_action_broker::peer_actions::PolicyTime>,
    retired: bool,
}
impl ActionRuntime {
    fn new(committed: crate::committed_authority::CommittedAuthority) -> Self {
        let authority = crate::action_host::Authority::new(committed.clone());
        let (commit, commits) = crate::owner_commit::OwnerCommitMailbox::bounded(32);
        let (signing, signatures) = crate::host_signing::HostSigning::new(commit);
        let (network_sender, network) = tokio::sync::mpsc::channel(64);
        Self {
            committed,
            authority,
            signing,
            signatures,
            commits,
            network_sender,
            network,
            actions: None,
            policy_time: None,
            retired: false,
        }
    }

    fn needs_retirement(&self) -> bool {
        !self.retired
            && self
                .policy_time
                .as_ref()
                .is_some_and(|time| time.now_ns().is_err())
    }

    fn current_actions(&self) -> Option<&crate::action_host::ActionHost> {
        if self.retired {
            return None;
        }
        let time = self.policy_time.as_ref()?;
        self.committed
            .read()
            .ok()?
            .view()
            .check_policy_time(time)
            .ok()?;
        self.actions.as_ref()
    }

    fn retire(&mut self) -> Result<(), String> {
        use qos_peer_state::owner_commit::OwnerCommitService;
        if self.retired {
            return Ok(());
        }
        self.authority.set_ready(false);
        self.retired = true;
        // Record intrinsic failure before orderly refusal; a replacement must
        // never erase a previously failed signing/owner handoff.
        let mut failure = self.signing.health().err();
        // Close producer admission atomically with each queue's final send
        // before consuming queued custody. This does not poison receipt ACKs.
        if let Err(problem) = self.signing.close_admission() {
            failure.get_or_insert(problem);
        }
        if let Err(problem) = self.commits.close_admission() {
            failure.get_or_insert(problem);
        }
        if let Some(runtime) = self.actions.as_mut() {
            runtime.retire();
        }
        let reason = "action runtime authority retired before owner execution";
        while let Ok(pending) = self.signatures.try_recv() {
            if let Err(problem) = pending.reject_unstarted(reason.into()) {
                failure.get_or_insert(problem);
            }
        }
        while let Ok(pending) = self.commits.try_recv() {
            if let Err(problem) = pending.reject_unstarted(reason.into()) {
                failure.get_or_insert(problem);
            }
        }
        while self.network.try_recv().is_ok() {}
        failure.map_or(Ok(()), Err)
    }

    fn start_current(
        &mut self,
        owner: &mut SessionOwner,
        collaboration_admin: Option<crate::collaboration::CollaborationAdminHandle>,
        pool: Option<&crate::host_bytes::HostBytes>,
    ) -> Result<()> {
        // A healthy owner alone is insufficient: recovery must have published
        // an actual new Current view on this same owner clock.
        self.committed
            .read()
            .map_err(anyhow::Error::msg)?
            .view()
            .policy_time()
            .map_err(anyhow::Error::msg)?;
        if self.needs_retirement() {
            self.retire().map_err(anyhow::Error::msg)?;
        }
        if self.retired {
            anyhow::ensure!(
                self.signing
                    .retirement_ready()
                    .map_err(anyhow::Error::msg)?,
                "retired action runtime retains unresolved signing custody"
            );
            if let Some(runtime) = self.actions.as_mut() {
                // Only the recovered committed owner may settle pending record
                // heads. Dirty or uncertain close custody remains terminal.
                runtime.close(owner).map_err(anyhow::Error::msg)?;
            }
            let committed = self.committed.clone();
            let old = std::mem::replace(self, Self::new(committed));
            // Drop joins the old broker before reopening its exact journal.
            drop(old);
        }
        if self.actions.is_none() {
            // Only a new/replacement worker runtime requires a fresh baseline.
            // The normal owner barrier and Current publication precede factories.
            refresh::before_action_mount(owner, &self.committed, pool)
                .map_err(anyhow::Error::msg)?;
            self.authority.set_ready(true);
            let runtime = crate::action_host::ActionHost::start_with_collaboration(
                owner,
                self.authority.clone(),
                self.signing.clone(),
                self.network_sender.clone(),
                collaboration_admin,
            )?;
            self.policy_time = Some(runtime.registered_policy_time().clone());
            self.actions = Some(runtime);
        }
        self.authority.set_ready(true);
        Ok(())
    }
}

async fn refresh_actions(
    owner: &mut SessionOwner,
    runtime: &mut ActionRuntime,
    session: &mut Option<NativeSubscription>,
    collaboration_admin: Option<crate::collaboration::CollaborationAdminHandle>,
    action_error: &mut Option<String>,
    pool: Option<&crate::host_bytes::HostBytes>,
    cadence: &mut CheckpointCadence,
) {
    if runtime.needs_retirement() {
        runtime.authority.set_ready(false);
        if let Some(session) = session.as_mut() {
            session.detach_actions().await;
        }
        if let Err(problem) = runtime.retire() {
            action_error.get_or_insert(problem);
        }
    }
    if action_error.is_none()
        && owner.permits_publication()
        && session.as_ref().is_some_and(NativeSubscription::is_ready)
        && matches!(
            runtime.committed.observe(),
            crate::committed_authority::Observation::Current(_)
        )
    {
        let publication_before = owner.publication_revision();
        if let Err(problem) = runtime.start_current(owner, collaboration_admin, pool) {
            action_error.get_or_insert_with(|| runtime.signing.fail(problem.to_string()));
            runtime.authority.set_ready(false);
        }
        // Ready publishes immediately; replacement runtimes can start while
        // that same subscription is already Ready. Preserve their actual new
        // baseline in the existing idle/reconnect publication path as well.
        let _ = cadence.publication_due(
            owner.publication_advance(publication_before),
            std::time::Instant::now(),
            false,
            false,
        );
    }
}

#[cfg(test)]
#[path = "host/action_lifecycle_tests.rs"]
mod action_lifecycle_tests;

/// Longest a peer that cannot reach readiness withholds its ready file.
const READY_FALLBACK: Duration = Duration::from_secs(30);

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
    #[cfg(not(feature = "media"))]
    anyhow::ensure!(
        std::env::var_os("QOS_SESSION_HTTPS_CONFIG").is_none(),
        "configured HTTPS ingress requires the media feature"
    );
    #[cfg(feature = "media")]
    let prepared_ingress = crate::media_ingress::PreparedIngress::from_env().await?;
    #[cfg(feature = "media")]
    if let Some(prepared) = prepared_ingress.as_ref() {
        prepared.activate_owner(&mut owner)?;
        // Data routes reserve turns on the ingress pool; attach it before the
        // committed view is captured so views carry the same retained credit.
        owner
            .attach_data_resources(&prepared.host_bytes()?)
            .map_err(anyhow::Error::from)?;
        owner.permit_unscoped_legacy_commits()?;
    }
    let (collaboration_handle, mut collaboration_commands) =
        crate::collaboration::CollaborationHandle::bounded(64);
    let host = LocalHost::start_with_collaboration(&owner, collaboration_handle).await?;
    let (refresh_mailbox, mut refresh_requests) =
        tilo_relay::peer::sync_refresh::Mailbox::bounded();
    host.relay
        .install_sync_refresh(owner.workspace_id(), refresh_mailbox)
        .await
        .map_err(anyhow::Error::msg)?;
    let host_descriptor = tilo_relay::workspace_join::IrohWorkspaceHostHintV1 {
        version: 1,
        mode: tilo_relay::workspace_join::WorkspaceHostMode::CertifiedHost,
        room: owner.peer().room().into(),
        workspace_id: owner.workspace_id(),
        governing_authority: owner.peer().authority().clone(),
        endpoint_addr: host.address.clone(),
        host_user_public_key: owner.host_certificate().user_public_key,
        host_device_certificate: owner.host_certificate().clone(),
        packet_relay_url: host.packet_url.to_string(),
    };
    let collaboration_hint = owner.sign_workspace_host_hint(&host_descriptor)?;
    let (collaboration_admin, mut collaboration_admin_commands) =
        crate::collaboration::CollaborationAdminHandle::bounded(
            owner.peer().room().into(),
            64,
            owner.supports_media_governance(),
        );
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let authorization = std::sync::Arc::new(crate::enrollment_http::Authorization::local(
        owner.root(),
        listener.local_addr()?.to_string(),
    )?);
    let (enrollment_commands, mut enrollments) =
        tokio::sync::mpsc::channel::<crate::enrollment_http::EnrollmentCommand>(64);
    let mut enrollment = crate::enrollment::EnrollmentAuthority::default();
    let mut session: Option<NativeSubscription> = None;
    let committed = crate::committed_authority::CommittedAuthority::new(&owner);
    #[cfg(feature = "media")]
    let mut delivery_ingress = match prepared_ingress {
        Some(prepared) => match prepared
            .start(
                &mut owner,
                committed.clone(),
                Some((authorization.clone(), enrollment_commands.clone())),
            )
            .await
        {
            Ok(installed) => Some(installed),
            Err(error) => {
                host.shutdown().await?;
                return Err(error);
            }
        },
        None => None,
    };
    let mut invitation_transports = vec![collaboration_hint.clone()];
    #[cfg(feature = "media")]
    if let Some(installed) = delivery_ingress.as_ref() {
        match owner.sign_delivery_origin_hint(installed) {
            Ok(locator) => invitation_transports.push(locator),
            Err(error) => {
                host.shutdown().await?;
                return Err(error);
            }
        }
    }
    #[cfg(feature = "media")]
    if let Some(installed) = delivery_ingress.as_mut() {
        // Onboarding over HTTPS uses the same stateless invitation service.
        let service =
            crate::collaboration::CollaborationService::new(invitation_transports.clone());
        if let Err(error) = installed.install_onboarding(service) {
            host.shutdown().await?;
            return Err(error);
        }
    }
    let collaboration = crate::collaboration::CollaborationService::new(invitation_transports);
    let mut action_runtime = ActionRuntime::new(committed.clone());
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
    let metadata = json!({"version":2,"enrollment_version":2,"challenge_path":"/enrollment/challenge","complete_path":"/enrollment/complete","pid":std::process::id(),"platform":std::env::consts::OS,"fixture_origin":format!("http://{}",listener.local_addr()?),"status_path":"/status","packet_relay":host.packet_url,"relay_address":host.address,"workspace_join_alpn":String::from_utf8_lossy(tilo_relay::workspace_join::WORKSPACE_JOIN_ALPN),"workspace_join_transport":collaboration_hint,"room":owner.peer().room(),"workspace_id":owner.workspace_id().to_string(),"state_root":owner.root(),"records":owner.peer().rows()});
    #[cfg(feature = "media")]
    let mut metadata = {
        let mut metadata = metadata;
        if let Some(installed) = delivery_ingress.as_ref() {
            metadata["delivery_version"] = json!(3);
            metadata["delivery_bootstrap_path"] = json!("/peer/v3/bootstrap");
            metadata["delivery_hint"] = serde_json::to_value(installed.discovery_hint(&owner)?)?;
        }
        metadata
    };
    // The ready file announces a peer that accepts enrollment: it is written
    // once the native subscription is ready and the owner permits publication
    // (the same gate enrollment uses), or after a bounded deadline so an
    // operator can still reach the status route of a peer that cannot connect.
    let ready_deadline = std::time::Instant::now() + READY_FALLBACK;
    let mut ready_published = false;
    #[cfg(feature = "media")]
    let mut ingress_maintenance = tokio::time::interval(Duration::from_secs(1));
    let mut action_pump =
        crate::action_pump_cadence::ActionPumpCadence::new(std::time::Instant::now());
    let mut woke_by_pump = false;
    let run_result: Result<()> = async {
    loop {
        if !std::mem::take(&mut woke_by_pump) {
            action_pump.activity(std::time::Instant::now());
        }
        if !ready_published
            && ((owner.permits_publication() && session.as_ref().is_some_and(|current| current.is_ready()))
                || std::time::Instant::now() >= ready_deadline)
        {
            atomic_file::write_with_mode(ready_path, &serde_json::to_vec_pretty(&metadata)?, 0o600)?;
            crate::store::sync_dir(ready_path.parent().context("ready file requires parent directory")?)?;
            ready_published = true;
        }
        // Terminal owner custody remains inspectable, but cannot publish,
        // execute queued mutations, or reconnect until an explicit reopen.
        if !owner.durability_healthy() {
            committed.invalidate();
            action_error.get_or_insert_with(|| action_runtime.signing.fail("native owner durability unavailable"));
            if let Some(attempt) = reconnect.take() { attempt.cancel().await; }
            if let Some(old) = session.take() { close_session(old, "native owner durability unavailable").await; }
        }
        // Held polls observe every owner commit made by any arm below.
        #[cfg(feature = "media")]
        if let Some(installed) = delivery_ingress.as_mut() {
            let before = owner.committed_generation();
            installed.maintain(&mut owner);
            // Completed staged uploads commit here, outside any request arm.
            if owner.committed_generation() != before && owner.permits_publication() {
                if let Some(runtime) = action_runtime.current_actions() {
                    if let Err(problem) = runtime.committed(&owner) { action_error = Some(action_runtime.signing.fail(problem)); }
                }
                if let Some(current) = session.as_ref().filter(|session| session.is_ready()) {
                    if let Err(problem) = publish(&owner, current) { error = Some(problem.to_string()); }
                }
            }
        }
        #[cfg(feature = "media")]
        let action_pool = delivery_ingress.as_ref().map(|installed| installed.host_bytes());
        #[cfg(not(feature = "media"))]
        let action_pool: Option<crate::host_bytes::HostBytes> = None;
        refresh_actions(&mut owner, &mut action_runtime, &mut session, Some(collaboration_admin.clone()), &mut action_error, action_pool.as_ref(), &mut cadence).await;
        // Private HTTPS routes follow the current action runtime, if any.
        #[cfg(feature = "media")]
        if let Some(installed) = delivery_ingress.as_mut() {
            installed.sync_actions(action_runtime.current_actions());
        }
        // Local workers still need current committed authority to consume exact
        // precommitted receipts while the remote subscription is reconnecting.
        // New owner work and remote attachments retain their session-ready gates.
        action_runtime.authority.set_ready(!action_runtime.retired && owner.permits_publication() && action_error.is_none());
        reconnect_policy.observe_session(session.is_some(), std::time::Instant::now());
        if session.is_none() && reconnect.is_none() {
            let _timing = crate::action_timing::Timing::new("subscription_connecting");
            reconnect = reconnect_for_owner(&owner, &host, reconnect_policy.delay(), &action_error)?;
        }
        tokio::select! {
            _ = &mut interrupt => break,
            request = async {
                #[cfg(feature="media")]
                if let Some(installed)=delivery_ingress.as_mut(){return installed.recv().await;}
                std::future::pending::<Option<crate::https::BufferedRequest>>().await
            } => {
                #[cfg(feature="media")]
                if let Some(installed)=delivery_ingress.as_mut(){
                    // Data-plane requests reach hosted models only through the
                    // committed generation they change (see action_pump_cadence).
                    if request.as_ref().is_some_and(|request| data_plane_route(&request.route())) {
                        woke_by_pump = true;
                        action_pump.data_request(std::time::Instant::now());
                    }
                    let before = owner.committed_generation();
                    match request{
                        Some(request) if matches!(request.route(), crate::https::Route::Onboarding(_) | crate::https::Route::OnboardingChunk) => installed.onboard(&mut owner, &host, request).await,
                        Some(request)=>installed.handle(&mut owner,request),
                        None=>bail!("installed HTTPS ingress stopped"),
                    }
                    // HTTPS data, onboarding and staged admissions commit on the
                    // owner; follow up exactly as other inbound commits do.
                    if owner.committed_generation() != before && owner.permits_publication() {
                        if let Some(runtime) = action_runtime.current_actions() {
                            if let Err(problem) = runtime.committed(&owner) { action_error = Some(action_runtime.signing.fail(problem)); }
                        }
                        if let Some(current) = session.as_ref().filter(|session| session.is_ready()) {
                            if let Err(problem) = publish(&owner, current) { error = Some(problem.to_string()); }
                        }
                    }
                }
                #[cfg(not(feature="media"))]
                let _=request;
            }
            _ = async {
                #[cfg(feature="media")]
                { ingress_maintenance.tick().await; }
                #[cfg(not(feature="media"))]
                std::future::pending::<()>().await;
            } => {
                // A periodic timer, not activity: it must not hold the action pump at 10 ms.
                woke_by_pump = true;
                #[cfg(feature="media")]
                if let Some(installed)=delivery_ingress.as_mut(){
                    // A refused descriptor renewal (e.g. disk capacity below the
                    // store's headroom) must not terminate the host: keep serving,
                    // and retry on the next tick once the condition clears.
                    let refreshed = match installed.refresh_current(&mut owner) {
                        Ok(refreshed) => refreshed,
                        Err(problem) => {
                            tracing::warn!(%problem, "delivery descriptor renewal refused; retrying");
                            error = Some(format!("delivery descriptor renewal refused: {problem}"));
                            false
                        }
                    };
                    if refreshed {
                        metadata["delivery_hint"] = serde_json::to_value(installed.discovery_hint(&owner)?)?;
                        if ready_published {
                            atomic_file::write_with_mode(ready_path,&serde_json::to_vec_pretty(&metadata)?,0o600)?;
                            crate::store::sync_dir(ready_path.parent().context("ready file requires parent directory")?)?;
                        }
                        if let Some(runtime)=action_runtime.current_actions(){
                            if let Err(problem)=runtime.committed(&owner){ action_error=Some(action_runtime.signing.fail(problem)); }
                        }
                    }
                }
            }
            request = refresh_requests.recv() => {
                if let Some(request) = request {
                    let before = owner.committed_generation();
                    #[cfg(feature = "media")]
                    let refresh_pool = delivery_ingress.as_ref().map(|installed| installed.host_bytes());
                    #[cfg(not(feature = "media"))]
                    let refresh_pool: Option<crate::host_bytes::HostBytes> = None;
                    if let Err(problem) = refresh::execute(&mut owner, &committed, request, refresh_pool.as_ref()).await {
                        tracing::debug!(%problem, "authenticated checkpoint refresh refused");
                    }
                    // Even a source closing during the barrier cannot discard a
                    // completed owner generation. It grants that old source no reply.
                    if owner.committed_generation() != before && owner.permits_publication() {
                        let followup = post_inbound_commit(&owner, action_runtime.current_actions(), &host).await?;
                        if let Some(problem) = followup.action_error {
                            action_error = Some(action_runtime.signing.fail(problem));
                            action_runtime.authority.set_ready(false);
                        }
                        if let Some(problem) = followup.relay_error { error = Some(problem); }
                        if let Some(current) = session.as_ref().filter(|session| session.is_ready()) {
                            if let Err(problem) = publish(&owner, current) { error = Some(problem.to_string()); }
                        }
                    }
                }
            }
            command = collaboration_commands.recv() => {
                if let Some(command) = command {
                    let before = owner.committed_generation();
                    let response = handle_collaboration_exchange(
                        &mut owner,
                        &committed,
                        &host,
                        &collaboration,
                        command.remote_endpoint,
                        command.request,
                    ).await;
                    if owner.committed_generation() != before && owner.permits_publication() {
                        if let Some(runtime) = action_runtime.current_actions() {
                            if let Err(problem) = runtime.committed(&owner) {
                                action_error = Some(action_runtime.signing.fail(problem));
                            }
                        }
                        if let Some(current) = session.as_ref().filter(|session| session.is_ready()) {
                            if let Err(problem) = publish(&owner, current) {
                                error = Some(problem.to_string());
                            }
                        }
                    }
                    let _ = command.reply.send(response);
                }
            }
            command = collaboration_admin_commands.recv() => {
                if let Some(command) = command {
                    let before = owner.committed_generation();
                    let response = handle_collaboration_admin(
                        &mut owner,
                        &committed,
                        &host,
                        &collaboration,
                        command.request,
                    ).await;
                    if owner.committed_generation() != before && owner.permits_publication() {
                        if let Some(runtime) = action_runtime.current_actions() {
                            if let Err(problem) = runtime.committed(&owner) {
                                action_error = Some(action_runtime.signing.fail(problem));
                            }
                        }
                        if let Some(current) = session.as_ref().filter(|session| session.is_ready()) {
                            if let Err(problem) = publish(&owner, current) {
                                error = Some(problem.to_string());
                            }
                        }
                    }
                    let _ = command.reply.send(response);
                }
            }
            _ = pump.tick(), if owner.durability_healthy() => {
                woke_by_pump = true;
                let _timing = crate::action_timing::Timing::slow("owner_pump");
                #[cfg(feature = "test-support")]
                if qualification.deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                    qualification.expired = true; qualification.release();
                }
                // A recovery-basis replacement and every following tail stay
                // ordered behind this idle flush. Ordinary selective tails do
                // not reach this path because they publish in their commit turn.
                if owner.permits_publication() && cadence.due_on_idle(std::time::Instant::now()) {
                    if let Some(current) = session.as_ref().filter(|s| s.is_ready()) {
                        tracing::debug!(site = "idle_flush", "publish site");
                        match publish(&owner, current) {
                            Ok(()) => cadence.mark_published(std::time::Instant::now()),
                            Err(problem) if committed_publication_failure(&problem) == PublicationFailureAction::Reconnect => {
                                error = Some(problem.to_string());
                                if let Some(old) = session.take() { close_session(old, &format!("idle checkpoint flush failed: {problem}")).await; }
                            },
                            Err(problem) => action_error = Some(action_runtime.signing.fail(problem.to_string())),
                        }
                    }
                }

                if let Some(runtime) = action_runtime.actions.as_mut().filter(|_| !action_runtime.retired && action_pump.due(std::time::Instant::now(), owner.committed_generation())) {
                    if let Err(problem) = pump_ready_actions(&owner, runtime) { action_error.get_or_insert(problem); }
                    if owner.permits_publication() && action_error.is_none() && runtime.ready() {
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
                    action_runtime.authority.set_ready(false);
                    if let Some(old) = session.take() { close_session(old, &format!("action attachment failed: {}", action_error.as_deref().unwrap_or(""))).await; }
                }
            }
            // Replacement keeps bounded durable work pending until its exact
            // retained session can publish before any completion is exposed.
            pending = action_runtime.commits.recv(), if !action_runtime.retired && action_runtime.policy_time.is_some() && owner.permits_publication() && session.as_ref().is_some_and(|current| current.is_ready()) => if let Some(pending) = pending {
                let _timing = crate::action_timing::Timing::slow("owner_commit");
                let update = committed.begin(&owner)?;
                let desktop = pending.is_desktop();
                let publication_before = owner.publication_revision();
                let complete = pending.apply_deferred(&mut owner);
                if !publish_owner_authority(update, &owner)? {
                    // This opaque completion may contain a receipt. Withhold it;
                    // its existing drop path retains the request's uncertainty.
                    drop(complete);
                    continue;
                }
                let result = complete();
                if let Ok(Some(delivery)) = &result {
                    if let Some(session) = session.as_ref() {
                        if let Err(problem) = session.send_action(delivery.clone()) {
                            tracing::debug!(%problem, "direct desktop delivery unavailable; retained publication follows");
                        }
                    }
                }
                let published = dispatch_owner_commit_publication(
                    &mut cadence,
                    &owner,
                    publication_before,
                    std::time::Instant::now(),
                    desktop,
                    |owner| committed_transition_ready(owner, session.as_ref()),
                    |transition_ready| {
                        tracing::debug!(site = "owner_commit", "publish site");
                        #[cfg(feature = "test-support")]
                        if let Some(problem) = qualification.publication_failure_once() {
                            return Err(problem);
                        }
                        session
                            .as_ref()
                            .filter(|s| s.is_ready())
                            .ok_or_else(|| anyhow::anyhow!("owner committed while disconnected; publication pending"))
                            .and_then(|s| if transition_ready { publish_committed(&owner, s) } else { publish(&owner, s) })
                    },
                );
                if let Some(runtime) = action_runtime.current_actions() { if let Err(problem) = runtime.committed(&owner) { action_error = Some(action_runtime.signing.fail(problem)); } }
                if let Err(problem) = result { action_error = Some(problem); }
                let reconnect_publication = match published {
                    Err(problem) if committed_publication_failure(&problem) == PublicationFailureAction::Reconnect => {
                        error = Some(problem.to_string());
                        Some(problem.to_string())
                    }
                    Err(problem) => {
                        action_error = Some(action_runtime.signing.fail(problem.to_string()));
                        None
                    }
                    Ok(_) => None,
                };
                action_runtime.authority.set_ready(action_error.is_none());
                if let Some(problem) = reconnect_publication {
                    if let Some(old) = session.take() { close_session(old, &format!("owner publication transport failed: {problem}")).await; }
                }
            },
            pending = action_runtime.signatures.recv(), if !action_runtime.retired && action_runtime.policy_time.is_some() && owner.permits_publication() && session.as_ref().is_some_and(|current| current.is_ready()) => if let Some(pending) = pending {
                let timing = pending.diagnostic_timing();
                #[cfg(feature = "test-support")]
                let hold = qualification.route.as_ref().is_some_and(|(app, record)| pending.matches_route(app, record));
                let update = committed.begin(&owner)?;
                timing.mark("apply_start");
                let publication_before = owner.publication_revision();
                let (complete, result) = pending.apply(&mut owner);
                timing.mark("apply_end");
                if !publish_owner_authority(update, &owner)? {
                    // Only the already-known failure may cross the denied
                    // authority boundary; never release a successful signature.
                    if let Err(problem) = result {
                        action_error.get_or_insert_with(|| action_runtime.signing.fail(problem));
                        complete();
                    }
                    continue;
                }
                timing.mark("publication_start");
                let transition_ready = committed_transition_ready(&owner, session.as_ref());
                let published = dispatch_committed_publication(
                    &mut cadence,
                    &owner,
                    publication_before,
                    std::time::Instant::now(),
                    transition_ready,
                    false,
                    || {
                        tracing::debug!(site = "signed_commit", "publish site");
                        #[cfg(feature = "test-support")]
                        if let Some(problem) = qualification.publication_failure_once() {
                            return Err(problem);
                        }
                        session
                            .as_ref()
                            .filter(|s| s.is_ready())
                            .ok_or_else(|| anyhow::anyhow!("signed commit publication pending; native peer unavailable"))
                            .and_then(|s| if transition_ready { publish_committed(&owner, s) } else { publish(&owner, s) })
                    },
                );
                timing.mark("publication_end");
                timing.mark("runtime_committed_start");
                if let Some(runtime) = action_runtime.current_actions() { if let Err(problem) = runtime.committed(&owner) { action_error = Some(action_runtime.signing.fail(problem)); } }
                timing.mark("runtime_committed_end");
                if let Err(problem) = result { action_error = Some(problem); }
                let reconnect_publication = match published {
                    Err(problem) if committed_publication_failure(&problem) == PublicationFailureAction::Reconnect => {
                        error = Some(problem.to_string());
                        Some(problem.to_string())
                    }
                    Err(problem) => {
                        action_error = Some(action_runtime.signing.fail(problem.to_string()));
                        None
                    }
                    Ok(_) => None,
                };
                action_runtime.authority.set_ready(action_error.is_none());
                #[cfg(feature = "test-support")]
                if hold {
                    qualification.route = None;
                    qualification.completion = Some(Box::new(complete));
                    qualification.deadline = Some(std::time::Instant::now() + Duration::from_secs(4));
                } else { complete(); }
                #[cfg(not(feature = "test-support"))]
                complete();
                if let Some(problem) = reconnect_publication {
                    if let Some(old) = session.take() { close_session(old, &format!("signed publication transport failed: {problem}")).await; }
                }
            },
            message = action_runtime.network.recv(), if !action_runtime.retired && action_runtime.policy_time.is_some() => if let Some(message) = message {
                if let Some(current) = session.as_ref().filter(|s| owner.permits_publication() && s.is_ready()) {
                    if let Err(problem) = current.publish_presence(postcard::to_stdvec(&message)?) { action_error = Some(action_runtime.signing.fail(problem.to_string())); }
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
                        if !prepare_ready_publication(&owner)? {
                            error = owner.peer().incomplete_reason();
                            action_runtime.authority.set_ready(false);
                            continue;
                        }
                        if action_error.is_none() {
                            refresh_actions(&mut owner, &mut action_runtime, &mut session, Some(collaboration_admin.clone()), &mut action_error, action_pool.as_ref(), &mut cadence).await;
                        }
                        if let Some(current)=session.as_ref() {
                            tracing::debug!(site = "ready", "publish site");
                            match publish(&owner,current) {
                                Ok(())=>{error=None; cadence.mark_published(std::time::Instant::now());}
                                Err(problem) if committed_publication_failure(&problem) == PublicationFailureAction::Reconnect => {error=Some(problem.to_string());if let Some(old)=session.take(){close_session(old, &format!("initial publication failed: {problem}")).await;}},
                                Err(problem)=>{action_error=Some(action_runtime.signing.fail(problem.to_string()));action_runtime.authority.set_ready(false);if let Some(old)=session.take(){close_session(old, &format!("initial publication invalid: {problem}")).await;}}
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
                        let publication_before = owner.publication_revision();
                        let received = {
                            let update = committed.begin(&owner)?;
                            let received = decoded.and_then(|message|owner.receive(origin,message));
                            if !publish_owner_authority(update, &owner)? { continue; }
                            received
                        };
                        match &received {
                            Ok(receipt) => tracing::debug!(kind, bytes = inbound_bytes, committed = receipt.is_some(), elapsed_us = started.elapsed().as_micros() as u64, "inbound applied"),
                            Err(problem) => tracing::warn!(kind, bytes = inbound_bytes, %problem, "inbound rejected"),
                        }
                        if received.as_ref().is_ok_and(|receipt| receipt.is_some()) {
                            let followup = post_inbound_commit(&owner, action_runtime.current_actions(), &host).await?;
                            if let Some(problem) = followup.action_error { action_error = Some(action_runtime.signing.fail(problem)); action_runtime.authority.set_ready(false); }
                            if let Some(problem) = followup.relay_error { error = Some(problem); }
                        }
                        match received {
                            Ok(receipt) => {
                                if !owner.permits_publication() { error=owner.peer().incomplete_reason(); action_runtime.authority.set_ready(false); }
                                else if receipt.is_some() {
                                    let now = std::time::Instant::now();
                                    let force = kind != "AppDocument";
                                    let transition_ready = !force && committed_transition_ready(&owner, session.as_ref());
                                    let published = dispatch_committed_publication(
                                        &mut cadence,
                                        &owner,
                                        publication_before,
                                        now,
                                        transition_ready,
                                        force,
                                        || {
                                            tracing::debug!(site = if kind == "AppDocument" { "inbound_selective" } else { "inbound_bootstrap" }, "publish site");
                                            session
                                                .as_ref()
                                                .filter(|current| current.is_ready())
                                                .ok_or_else(|| anyhow::anyhow!("inbound publication pending; native peer unavailable"))
                                                .and_then(|current| if transition_ready { publish_committed(&owner, current) } else { publish(&owner, current) })
                                        },
                                    );
                                    match published {
                                        Err(problem) if committed_publication_failure(&problem) == PublicationFailureAction::Reconnect => { error = Some(problem.to_string()); if let Some(old)=session.take(){ close_session(old, &format!("publication failed after receipt: {problem}")).await; } },
                                        Err(problem) => { action_error=Some(action_runtime.signing.fail(problem.to_string())); action_runtime.authority.set_ready(false); if let Some(old)=session.take(){close_session(old, &format!("publication invalid after receipt: {problem}")).await;} },
                                        Ok(_) => {},
                                    }
                                }
                            },
                            Err(problem) => { error = Some(problem.to_string()); if !owner.permits_publication() { action_runtime.authority.set_ready(false); } }
                        }
                    }
                    None => { let _timing = crate::action_timing::Timing::new("subscription_disconnected"); error=Some("native subscription disconnected; reconnecting with durable state".into()); if let Some(old)=session.take(){close_session(old, "relay stream ended").await;} }
                }
            }
            command = enrollments.recv() => {
                if let Some(command)=command {
                    if command.is_cancelled() { continue; }
                    let local_https = command.is_https();
                    let omit_governance = command.omits_governance();
                    let result=if !owner.permits_publication() || !session.as_ref().is_some_and(|current|current.is_ready()) {
                        Err(crate::enrollment::EnrollmentError::new(503,"native peer is reconnecting; retry enrollment with the same identity"))
                    } else {
                        match command.request {
                            crate::enrollment_http::EnrollmentRequest::Challenge(request)=>enrollment.challenge(&owner,request,tilo_relay::clock::now_ns()).and_then(|response|serde_json::to_vec(&response).map_err(|error|crate::enrollment::EnrollmentError::new(503,error.to_string()))),
                            crate::enrollment_http::EnrollmentRequest::Complete(request)=>{
                                let publication_before = owner.publication_revision();
                                let enrolled = {
                                    let update = committed.begin(&owner)?;
                                    // HTTPS local-owner completions keep the receipt that binds a
                                    // bounded governance handoff to this exact request.
                                    let enrolled = if local_https {
                                        enrollment.complete_for_https(&mut owner,request,tilo_relay::clock::now_ns()).map(|(certificate,proof)|(certificate,Some(proof)))
                                    } else {
                                        enrollment.complete(&mut owner,request,tilo_relay::clock::now_ns()).map(|certificate|(certificate,None))
                                    };
                                    if !publish_owner_authority(update, &owner)? {
                                        let _ = command.reply.send(Err(crate::enrollment::EnrollmentError::new(503,"native owner durability unavailable")));
                                        continue;
                                    }
                                    enrolled
                                };
                                match enrolled {
                                    Err(error)=>Err(error),
                                    Ok((certificate,local_proof))=>{
                                        // Enrollment durably replaces the recovery basis before
                                        // membership reconciliation or transport publication can
                                        // fail. Arm that cut now; a later forced dispatch observes
                                        // no additional revision and clears it only after success.
                                        let publication_after = owner.publication_revision();
                                        let _ = cadence.publication_due(owner.publication_advance(publication_before),std::time::Instant::now(),false,false);
                                        match host.update_membership(&owner).await {
                                        Err(error)=>Err(crate::enrollment::EnrollmentError::new(503,format!("committed enrollment awaits relay membership reconciliation: {error}"))),
                                        Ok(())=>{
                                            let published=dispatch_committed_publication(&mut cadence,&owner,publication_after,std::time::Instant::now(),false,true,||session.as_ref().filter(|current|current.is_ready()).ok_or_else(||anyhow::anyhow!("native subscription unavailable")).and_then(|current|publish(&owner,current)));
                                            match published {
                                                Ok(_) => {
                                                    #[cfg(feature="media")]
                                                    let reply=enrollment_reply(&mut owner,&certificate,local_proof,omit_governance,&host,delivery_ingress.as_mut());
                                                    #[cfg(not(feature="media"))]
                                                    let reply=enrollment_reply(&mut owner,&certificate,local_proof,omit_governance,&host);
                                                    reply
                                                },
                                                Err(problem) if committed_publication_failure(&problem) == PublicationFailureAction::Reconnect => { error=Some(problem.to_string()); if let Some(old)=session.take(){close_session(old,&format!("enrollment publication failed: {problem}")).await;} Err(crate::enrollment::EnrollmentError::new(503,problem.to_string())) },
                                                Err(problem) => { action_error=Some(action_runtime.signing.fail(problem.to_string())); action_runtime.authority.set_ready(false); if let Some(old)=session.take(){close_session(old,&format!("enrollment publication invalid: {problem}")).await;} Err(crate::enrollment::EnrollmentError::new(503,problem.to_string())) },
                                            }
                                        }
                                    }}
                                }
                            }
                        }
                    };
                    let _=command.reply.send(result);
                }
            }
            result = listener.accept() => {
                let (socket,_)=result?; let mut status=public_status(&owner,session.as_ref().is_some_and(|current|current.is_ready()),&error);
                status["actions"] = json!(if action_error.is_some() { "unavailable" } else if action_runtime.actions.as_ref().is_some_and(|runtime| runtime.ready()) && session.as_ref().is_some_and(|s| s.actions.as_ref().is_some_and(AttachedActions::is_ready)) { "ready" } else { "starting" });
                status["action_error"] = json!(action_error);
                #[cfg(feature = "test-support")]
                {
                    status["qualification"] = json!({"armed": qualification.route.is_some(), "held": qualification.completion.is_some(), "expired": qualification.expired,
                        "signing": action_runtime.signing.qualification_signing(),
                        "workers": action_runtime.actions.as_mut().map(|runtime| runtime.qualification_heads(owner.peer().room())),
                        "action_registration": qualification.action_registration.as_ref().map(|hold| json!({
                            "armed": hold.barrier.is_some(),
                            "reached": hold.reached.load(std::sync::atomic::Ordering::Acquire),
                        })),
                        "action_attachment_exit_armed": qualification.action_attachment_exit.is_some(),
                        "publication": {
                            "room_heads": owner.peer().admitted_room().map(|room| room.clone().get_heads().iter().map(ToString::to_string).collect::<Vec<_>>()),
                            "receipts": owner.peer().rows().iter().map(|row| owner.peer().durable_coverage(&row.app_id, &row.record_id)).collect::<Result<Vec<_>, _>>().map_err(anyhow::Error::msg)?,
                        },
                        "publication_failure": qualification.publication_failure_status(),
                    });
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
                    Some(line) if (line == "flush" || line.starts_with("export ")) && !owner.durability_healthy() => eprintln!("Native owner durability unavailable; reopen required"),
                    Some(line) if line == "flush" => {
                        let update = committed.begin(&owner)?;
                        let flushed = owner.flush();
                        if publish_owner_authority(update, &owner)? {
                            match flushed { Ok(receipt) => println!("{}", serde_json::to_string(&receipt)?), Err(problem) => eprintln!("Flush failed: {problem}") }
                        } else { eprintln!("Native owner durability unavailable; reopen required"); }
                    },
                    Some(line) if line.starts_with("export ") => {
                        let update = committed.begin(&owner)?;
                        let exported = owner.export(Path::new(&line[7..]));
                        if publish_owner_authority(update, &owner)? {
                            match exported { Ok(()) => println!("Export committed"), Err(problem) => eprintln!("Export failed: {problem}") }
                        } else { eprintln!("Native owner durability unavailable; reopen required"); }
                    },
                    // Operator membership administration as the governing native authority
                    // (browser owner devices may only administer invitations).
                    Some(line) if line.starts_with("admin ") => {
                        let mut parts = line[6..].splitn(2, ' ');
                        let (action, argument) = (parts.next().unwrap_or(""), parts.next());
                        let reply = match qos_action_broker::collaboration_admin::parse_operation(action, argument) {
                            Ok(operation) if matches!(action, "member.remove" | "member.role.set") => {
                                let before = owner.committed_generation();
                                let request = qos_action_broker::collaboration_admin::CollaborationAdminRequest {
                                    room: owner.peer().room().into(),
                                    invocation: tilo_actions::InvocationId::new(),
                                    caller: owner.peer().authority().clone(),
                                    operation,
                                };
                                let response = handle_collaboration_admin(&mut owner, &committed, &host, &collaboration, request).await;
                                if owner.committed_generation() != before && owner.permits_publication() {
                                    if let Some(runtime) = action_runtime.current_actions() {
                                        if let Err(problem) = runtime.committed(&owner) { action_error = Some(action_runtime.signing.fail(problem)); }
                                    }
                                    if let Some(current) = session.as_ref().filter(|session| session.is_ready()) {
                                        if let Err(problem) = publish(&owner, current) { error = Some(problem.to_string()); }
                                    }
                                }
                                match response { Ok(value) => json!({"ok": true, "result": value}), Err(problem) => json!({"ok": false, "error": format!("{problem:?}")}) }
                            }
                            Ok(_) => json!({"ok": false, "error": "only member.remove and member.role.set are operator actions"}),
                            Err(problem) => json!({"ok": false, "error": problem}),
                        };
                        println!("{reply}");
                    },
                    Some(_) => eprintln!("Use status, flush, reconnect, export /absolute/path, admin ACTION JSON, or shutdown"),
                }
            }
        }
    }
        Ok(())
    }.await;
    action_runtime.authority.set_ready(false);
    #[cfg(feature = "media")]
    if let Some(installed) = delivery_ingress.take() {
        installed.shutdown().await?;
    }
    committed.invalidate();
    if let Some(attempt) = reconnect.take() {
        attempt.cancel().await;
    }
    let workers_closed = action_runtime
        .actions
        .as_mut()
        .map(|runtime| runtime.close(&owner))
        .transpose();
    let flushed = owner.flush();
    if let Some(current) = session {
        current.close().await;
    }
    host.shutdown().await?;
    match std::fs::remove_file(ready_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        other => other?,
    }
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
        let outgoing = outgoing_bytes(&owner).to_vec();
        let kinds = outgoing_kinds(&owner).to_vec();
        drop(owner);

        let owner = SessionOwner::open(&root).unwrap();
        assert_eq!(owner.status().generation, generation);
        assert_eq!(owner.publication_baseline(), baseline);
        assert_eq!(outgoing_bytes(&owner), outgoing);

        assert!(prepare_ready_publication(&owner).unwrap());

        assert_eq!(owner.status().generation, generation);
        assert_eq!(owner.publication_baseline(), baseline);
        assert_eq!(outgoing_bytes(&owner), outgoing);
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
    async fn committed_authority_publishes_collaboration_before_failed_relay_reconciliation() {
        use crate::committed_authority::{CommittedAuthority, Observation};
        use qos_action_broker::collaboration_admin::{
            CollaborationAdminOperation, CollaborationAdminRequest, CollaborationRole,
            IssueInvitationInput,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
        let host = LocalHost::start(&owner).await.unwrap();
        let committed = CommittedAuthority::new(&owner);
        let service = crate::collaboration::CollaborationService::new(vec![]);
        let request = CollaborationAdminRequest {
            room: owner.peer().room().into(),
            invocation: tilo_actions::InvocationId::new(),
            caller: owner.peer().authority().clone(),
            operation: CollaborationAdminOperation::Issue(IssueInvitationInput {
                role: CollaborationRole::Viewer,
                expires_in_seconds: 60,
                invitee_name: None,
            }),
        };
        // Fail only the relay's separate projection file; the owner barrier
        // still succeeds and its current committed authority must stay usable.
        let relay_access = owner
            .root()
            .join("relay/workspaces")
            .join(owner.workspace_id().to_string())
            .join(tilo_relay::store::GOVERNED_ACCESS_FILE);
        std::fs::remove_file(&relay_access).unwrap();
        std::fs::create_dir(&relay_access).unwrap();
        let before = owner.committed_generation();
        let mut operation = Box::pin(handle_collaboration_admin(
            &mut owner, &committed, &host, &service, request,
        ));
        let first = std::future::poll_fn(|cx| Poll::Ready(operation.as_mut().poll(cx))).await;
        let published_before_completion = match committed.observe() {
            Observation::Current(view) => view.generation() > before,
            _ => false,
        };
        let result = match first {
            Poll::Ready(result) => {
                drop(operation);
                result
            }
            Poll::Pending => tokio::time::timeout(Duration::from_secs(5), operation)
                .await
                .unwrap(),
        };
        host.shutdown().await.unwrap();
        assert!(
            published_before_completion,
            "authority was held or stale at asynchronous reconciliation"
        );
        assert!(
            result.is_err(),
            "real relay projection failure must reach the caller"
        );
        assert!(owner.permits_publication());
        let Observation::Current(view) = committed.observe() else {
            panic!("transport failure erased committed membership")
        };
        assert_eq!(view.generation(), owner.committed_generation());
        assert!(view.generation() > before);
    }

    async fn inbound_followup_case(incomplete: bool) {
        use crate::committed_authority::{CommittedAuthority, Observation};
        use qos_peer_state::{NeighbourMsg, TransportOrigin};
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let host = LocalHost::start(&owner).await.unwrap();
        let committed = CommittedAuthority::new(&owner);
        let authority = crate::action_host::Authority::new(committed.clone());
        authority.set_ready(true);
        let (commit, _commits) = crate::owner_commit::OwnerCommitMailbox::bounded(32);
        let (signing, _signatures) = crate::host_signing::HostSigning::new(commit);
        let (network, _messages) = tokio::sync::mpsc::channel(64);
        let mut runtime =
            crate::action_host::ActionHost::start(&owner, authority, signing, network).unwrap();
        use ui::automerge::{AutoCommit, ROOT, transaction::Transactable};
        let mut candidate = owner.peer().clone();
        let app = "so.tilo.word-processor";
        let record = "tab:0000000000000001";
        let mut document = AutoCommit::load(&candidate.document(app, record).unwrap()).unwrap();
        let before = document.get_heads();
        document.put(ROOT, "inbound-authority-test", true).unwrap();
        document.commit();
        let mut message = candidate
            .publish_incremental(
                app,
                record,
                document.save_after(&before),
                &owner.identity.native,
                web_time::SystemTime::now(),
            )
            .unwrap();
        let NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            record_id,
            envelope,
            ..
        } = &mut message
        else {
            panic!("signed snapshot")
        };
        envelope.seq += 1;
        if incomplete {
            envelope.encryption = Some(collab_types::EncryptionMetadata {
                epoch: 1,
                channel: collab_types::document_channel_id(app_id, cluster_id, record_id),
            });
            envelope.payload = vec![1, 2, 3];
        }
        collab_types::sign_op(envelope, &owner.identity.native).unwrap();
        let author = owner.identity.native.verifying_key().to_bytes();
        committed
            .with_owner(&mut owner, |owner| {
                owner.receive(TransportOrigin::RelayDelta(author), message)
            })
            .unwrap()
            .unwrap()
            .unwrap();
        let evidence = owner.peer().durable().unwrap();
        if !incomplete {
            let path = owner
                .root()
                .join("relay/workspaces")
                .join(owner.workspace_id().to_string())
                .join(tilo_relay::store::GOVERNED_ACCESS_FILE);
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(path).unwrap();
        }
        let followup = post_inbound_commit(&owner, Some(&runtime), &host).await;
        let ready = prepare_ready_publication(&owner);
        let tick = if incomplete {
            Some(pump_ready_actions(&owner, &mut runtime))
        } else {
            None
        };
        let mut reconnect = if incomplete {
            reconnect_for_owner(&owner, &host, Duration::ZERO, &None).unwrap()
        } else {
            None
        };
        let could_reconnect = reconnect.is_some();
        if let Some(attempt) = reconnect.as_mut() {
            let mut session = (&mut attempt.task).await.unwrap().unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if matches!(
                        session.recv().await.expect("live prerequisite stream"),
                        Inbound::Ready { .. }
                    ) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            assert!(session.is_ready());
            assert_eq!(prepare_ready_publication(&owner).unwrap(), !incomplete);
            if incomplete {
                pump_ready_actions(&owner, &mut runtime).unwrap();
            }
            session.close().await;
        }
        let _ = runtime.close(&owner);
        host.shutdown().await.unwrap();
        let followup = followup.expect("secondary publication failure must not end the host loop");
        assert!(owner.durability_healthy());
        assert_eq!(owner.peer().durable().unwrap(), evidence);
        if incomplete {
            assert!(
                followup.action_error.is_none(),
                "incomplete state pauses actions without poisoning the signer"
            );
            assert!(
                !ready.unwrap(),
                "ready stream must pause publication while retaining receive"
            );
            assert!(
                tick.unwrap().is_ok(),
                "subsequent pump must keep prerequisite receive alive"
            );
            assert!(
                could_reconnect,
                "incomplete state must retain the real receive/reconnect path"
            );
            assert!(matches!(committed.observe(), Observation::Denied));
            committed
                .with_owner(&mut owner, SessionOwner::flush)
                .unwrap()
                .unwrap();
            assert_eq!(owner.peer().durable().unwrap(), evidence);
        } else {
            assert!(
                followup.relay_error.is_some(),
                "real relay projection failure must remain observable"
            );
            assert!(matches!(committed.observe(), Observation::Current(_)));
        }
    }
    #[tokio::test]
    async fn committed_authority_inbound_incomplete_keeps_tick_and_receive_alive() {
        inbound_followup_case(true).await;
    }
    #[tokio::test]
    async fn committed_authority_inbound_relay_failure_preserves_healthy_owner() {
        inbound_followup_case(false).await;
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
    use super::{
        CheckpointCadence, PublicationDispatch, PublicationFailureAction,
        committed_publication_failure, dispatch_committed_publication,
        dispatch_owner_commit_publication,
    };
    use super::{outgoing_bytes, outgoing_kinds};
    use crate::{SessionOwner, owner::PublicationAdvance, publication::PublicationCursor};
    use qos_peer_state::desktop_command::*;
    use qos_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
    use qos_peer_state::transport::Retention;
    use std::time::{Duration, Instant, SystemTime};
    use ui::automerge::{AutoCommit, ROOT, transaction::Transactable};

    fn commit_small_changed_record(owner: &mut SessionOwner, value: u64) {
        let app = "so.tilo.word-processor";
        let record = "tab:0000000000000001";
        let mut candidate = owner.peer().clone();
        let mut document = AutoCommit::load(&candidate.document(app, record).unwrap()).unwrap();
        document.put(ROOT, "cadence-test", value).unwrap();
        document.commit();
        candidate
            .publish_document(
                app,
                record,
                document.save(),
                &owner.identity.native,
                SystemTime::now(),
            )
            .unwrap();
        owner
            .commit_mutation_candidate(candidate, owner.directory().to_vec())
            .unwrap();
    }

    fn desktop_command(owner: &SessionOwner, name: &str) -> OwnerCommitRequest {
        OwnerCommitRequest {
            room: owner.peer().room().into(),
            operation: CommitOperation::DesktopCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: owner.peer().authority().clone(),
                command: BoundCommand {
                    source: CommandSource {
                        room: owner.peer().room().into(),
                        heads: owner
                            .peer()
                            .admitted_room()
                            .unwrap()
                            .clone()
                            .get_heads()
                            .into_iter()
                            .map(|head| head.0)
                            .collect(),
                    },
                    command: DesktopCommand::CreateStrip { name: name.into() },
                },
            },
        }
    }

    fn commit_desktop(owner: &mut SessionOwner, name: &str) {
        owner
            .commit_desktop_command(&desktop_command(owner, name))
            .unwrap();
    }

    #[test]
    fn selective_owner_tail_publishes_in_commit_turn_without_replaying_baseline() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let mut cursor = PublicationCursor::default();
        cursor.send(&owner, |_, _| Ok(())).unwrap();
        let before = owner.publication_revision();
        commit_small_changed_record(&mut owner, 1);

        let now = Instant::now();
        let mut cadence = CheckpointCadence::new();
        let mut delivered = Vec::new();
        let dispatch =
            dispatch_committed_publication(&mut cadence, &owner, before, now, false, false, || {
                cursor.send(&owner, |bytes, kind| {
                    delivered.push((bytes.to_vec(), kind));
                    Ok(())
                })
            })
            .unwrap();

        assert!(matches!(
            dispatch,
            PublicationDispatch::PublishedSelective { entries: 2, .. }
        ));
        assert_eq!(delivered.len(), 2, "only the durable selective tail is new");
        assert_eq!(
            delivered.iter().map(|(_, kind)| *kind).collect::<Vec<_>>(),
            vec![Retention::Delta, Retention::Checkpoint]
        );
        assert!(
            !cadence.due_on_idle(now + CheckpointCadence::IDLE),
            "a successful immediate send leaves no five-second retry"
        );
    }

    #[test]
    fn failed_selective_transport_publish_remains_dirty_for_reconnect_retry() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let before = owner.publication_revision();
        commit_small_changed_record(&mut owner, 2);

        let now = Instant::now();
        let mut cadence = CheckpointCadence::new();
        let error =
            dispatch_committed_publication(&mut cadence, &owner, before, now, false, false, || {
                Err(anyhow::Error::new(tilo_relay::CoeditError::Disconnected))
            })
            .unwrap_err();
        assert_eq!(
            committed_publication_failure(&error),
            PublicationFailureAction::Reconnect
        );
        assert!(
            cadence.due_on_idle(now + CheckpointCadence::IDLE),
            "failed publication must retain a bounded retry"
        );
    }

    #[test]
    fn local_publication_failure_remains_fatal() {
        let corrupt = anyhow::anyhow!("committed publication bytes are corrupt");
        assert_eq!(
            committed_publication_failure(&corrupt),
            PublicationFailureAction::Fatal
        );
    }

    #[test]
    fn caught_up_cursor_sends_fresh_compaction_transition_immediately() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let original_baseline = owner.publication_baseline();
        let mut cursor = PublicationCursor::default();
        cursor.send(&owner, |_, _| Ok(())).unwrap();
        for value in 0..31 {
            commit_small_changed_record(&mut owner, value);
            cursor.send(&owner, |_, _| Ok(())).unwrap();
        }
        assert_eq!(
            owner.publication_baseline(),
            original_baseline,
            "62 selective tail entries fit without compaction"
        );

        let now = Instant::now();
        let mut cadence = CheckpointCadence::new();
        let before_compaction = owner.publication_revision();
        commit_small_changed_record(&mut owner, 31);
        let transition = owner
            .live_publication_transition()
            .expect("threshold compaction installs a fresh live transition");
        let expected = transition.messages().to_vec();
        assert_eq!(
            expected
                .iter()
                .skip(1)
                .map(|(bytes, _)| bytes)
                .collect::<Vec<_>>(),
            outgoing_bytes(&owner).iter().skip(1).collect::<Vec<_>>(),
            "the live transition reuses the persisted fresh snapshot frames"
        );
        let transition_bytes = expected.iter().map(|(bytes, _)| bytes.len()).sum::<usize>();
        assert!(expected.len() <= 64);
        assert!(transition_bytes <= 4 * 1024 * 1024);
        eprintln!(
            "compaction_transition_entries={} bytes={} persisted_entries={} persisted_bytes={}",
            expected.len(),
            transition_bytes,
            outgoing_bytes(&owner).len(),
            outgoing_bytes(&owner).iter().map(Vec::len).sum::<usize>()
        );
        let mut delivered = Vec::new();
        let compacted = dispatch_committed_publication(
            &mut cadence,
            &owner,
            before_compaction,
            now,
            true,
            false,
            || {
                cursor.send_committed(&owner, |bytes, kind| {
                    delivered.push((bytes.to_vec(), kind));
                    Ok(())
                })
            },
        )
        .unwrap();
        assert!(matches!(
            compacted,
            PublicationDispatch::PublishedTransition { .. }
        ));
        assert_eq!(
            delivered, expected,
            "a caught-up native cursor receives the compact fresh transition"
        );
        assert!(
            !cadence.due_on_idle(now + CheckpointCadence::IDLE),
            "the valid transition avoids the five-second compaction delay"
        );
        cursor
            .send(&owner, |_, _| {
                panic!("transition send must advance this cursor")
            })
            .unwrap();
    }

    #[test]
    fn caught_up_desktop_owner_commit_routes_the_compact_transition() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let mut native = PublicationCursor::default();
        let mut worker = PublicationCursor::default();
        native.send(&owner, |_, _| Ok(())).unwrap();
        worker.send(&owner, |_, _| Ok(())).unwrap();

        commit_desktop(&mut owner, "pre-threshold");
        native.send(&owner, |_, _| Ok(())).unwrap();
        worker.send(&owner, |_, _| Ok(())).unwrap();
        for value in 0..31 {
            commit_small_changed_record(&mut owner, value);
            native.send(&owner, |_, _| Ok(())).unwrap();
            worker.send(&owner, |_, _| Ok(())).unwrap();
        }

        let before = owner.publication_revision();
        commit_desktop(&mut owner, "threshold");
        let expected = owner
            .live_publication_transition()
            .expect("desktop commit reaches the real 64-frame compaction threshold")
            .messages()
            .to_vec();
        let mut cadence = CheckpointCadence::new();
        let mut native_delivered = Vec::new();
        let native_transition_ready = native.can_send_committed(&owner);
        let dispatch = dispatch_owner_commit_publication(
            &mut cadence,
            &owner,
            before,
            Instant::now(),
            true,
            move |_| native_transition_ready,
            |committed| {
                if committed {
                    native.send_committed(&owner, |bytes, kind| {
                        native_delivered.push((bytes.to_vec(), kind));
                        Ok(())
                    })
                } else {
                    native.send(&owner, |bytes, kind| {
                        native_delivered.push((bytes.to_vec(), kind));
                        Ok(())
                    })
                }
            },
        )
        .unwrap();
        assert!(matches!(
            dispatch,
            PublicationDispatch::PublishedTransition { .. }
        ));
        assert_eq!(native_delivered, expected);
        let mut worker_delivered = Vec::new();
        worker
            .send_committed(&owner, |bytes, kind| {
                worker_delivered.push((bytes.to_vec(), kind));
                Ok(())
            })
            .unwrap();
        assert_eq!(worker_delivered, expected);

        let mut mismatched = PublicationCursor::default();
        let mut mismatch_delivered = Vec::new();
        let mut mismatch_cadence = CheckpointCadence::new();
        let mismatch_transition_ready = mismatched.can_send_committed(&owner);
        let mismatch = dispatch_owner_commit_publication(
            &mut mismatch_cadence,
            &owner,
            before,
            Instant::now(),
            true,
            move |_| mismatch_transition_ready,
            |committed| {
                assert!(!committed);
                mismatched.send(&owner, |bytes, kind| {
                    mismatch_delivered.push((bytes.to_vec(), kind));
                    Ok(())
                })
            },
        )
        .unwrap();
        assert!(matches!(
            mismatch,
            PublicationDispatch::PublishedBaseline { .. }
        ));
        assert_eq!(
            mismatch_delivered,
            outgoing_bytes(&owner)
                .iter()
                .cloned()
                .zip(outgoing_kinds(&owner).iter().copied())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn replacement_and_following_tails_flush_once_after_idle_success() {
        let mut cadence = CheckpointCadence::new();
        let start = Instant::now();
        assert!(!cadence.publication_due(
            PublicationAdvance::BaselineReplacement {
                entries: 32,
                bytes: 1
            },
            start,
            false,
            false,
        ));
        for i in 1..50u32 {
            let now = start + Duration::from_millis(u64::from(i) * 10);
            assert!(!cadence.publication_due(
                PublicationAdvance::SelectiveTail {
                    entries: 2,
                    bytes: 1
                },
                now,
                false,
                false,
            ));
        }
        let idle = start + Duration::from_millis(50 * 10) + CheckpointCadence::IDLE;
        assert!(cadence.due_on_idle(idle));
        cadence.mark_published(idle);
        assert!(!cadence.due_on_idle(idle));
        assert!(!cadence.pending);
    }

    #[test]
    fn continuous_input_still_anchors_at_the_hard_delta_cap() {
        let mut cadence = CheckpointCadence::new();
        let start = Instant::now();
        assert!(!cadence.publication_due(
            PublicationAdvance::BaselineReplacement {
                entries: 32,
                bytes: 1
            },
            start,
            false,
            false,
        ));
        for i in 1..CheckpointCadence::MAX_DELTAS {
            let due = cadence.publication_due(
                PublicationAdvance::SelectiveTail {
                    entries: 2,
                    bytes: 1,
                },
                start + Duration::from_millis(u64::from(i)),
                false,
                false,
            );
            assert_eq!(due, i + 1 == CheckpointCadence::MAX_DELTAS);
        }
        cadence.mark_published(start + Duration::from_millis(1024));
        assert!(cadence.publication_due(
            PublicationAdvance::SelectiveTail {
                entries: 2,
                bytes: 1
            },
            start + Duration::from_millis(1025),
            false,
            false,
        ));
    }

    #[test]
    fn idle_does_not_fire_during_continuous_input() {
        let mut cadence = CheckpointCadence::new();
        let start = Instant::now();
        assert!(!cadence.publication_due(
            PublicationAdvance::BaselineReplacement {
                entries: 32,
                bytes: 1
            },
            start,
            false,
            false,
        ));
        assert!(!cadence.publication_due(
            PublicationAdvance::SelectiveTail {
                entries: 2,
                bytes: 1
            },
            start + CheckpointCadence::IDLE - Duration::from_millis(1),
            false,
            false,
        ));
        assert!(!cadence.due_on_idle(start + CheckpointCadence::IDLE - Duration::from_millis(1)));
        assert!(cadence.pending);
    }

    #[test]
    fn forced_publication_flushes_pending_replacement_only_after_success() {
        let mut cadence = CheckpointCadence::new();
        let start = Instant::now();
        let replacement = PublicationAdvance::BaselineReplacement {
            entries: 32,
            bytes: 1,
        };
        assert!(!cadence.publication_due(replacement, start, false, false));
        let gesture = start + Duration::from_millis(1);
        assert!(cadence.publication_due(PublicationAdvance::None, gesture, false, true));
        assert!(
            cadence.pending,
            "the decision alone is not a successful send"
        );
        cadence.mark_published(gesture);
        assert!(!cadence.due_on_idle(gesture + CheckpointCadence::IDLE));
    }
}

#[cfg(test)]
fn outgoing_bytes(owner: &crate::SessionOwner) -> Vec<Vec<u8>> {
    owner.outgoing().iter().map(|p| p.bytes.to_vec()).collect()
}
#[cfg(test)]
fn outgoing_kinds(owner: &crate::SessionOwner) -> Vec<qos_peer_state::transport::Retention> {
    owner.outgoing().iter().map(|p| p.kind).collect()
}

#[cfg(all(test, feature = "media"))]
#[path = "host/media_action_tests.rs"]
mod room_media_broker_tests;
