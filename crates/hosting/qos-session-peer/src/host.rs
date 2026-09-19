//! Existing packet relay + authenticated qOS relay, with a replaceable native subscription.
use crate::SessionOwner;
use anyhow::{Context, Result, bail};
use iroh::{Endpoint, RelayMap, RelayMode, endpoint::presets};
use iroh_base::{EndpointAddr, RelayUrl};
use iroh_relay::server::{RelayConfig as PacketConfig, Server, ServerConfig};
use qos_relay::{
    Inbound, Relay, RelayClient, RelayCoeditSession, RelayConfig, WorkspaceBinding,
    daemon::RelayIdentity,
};
use qos_user_identity::RevocationList;
use serde_json::json;
use std::sync::Arc;
use std::{net::Ipv4Addr, path::Path, time::Duration};
use tokio::{io::AsyncBufReadExt, net::TcpListener};

// Nondefault, local-process qualification only; never a wire capability.
#[cfg(feature = "test-support")]
#[derive(Default)]
struct QualificationHold {
    route: Option<(String, String)>,
    completion: Option<Box<dyn FnOnce()>>,
    deadline: Option<std::time::Instant>,
    expired: bool,
    action_registration: Option<ActionRegistrationHold>,
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
}
#[cfg(feature = "test-support")]
impl Drop for QualificationHold {
    fn drop(&mut self) {
        self.release();
    }
}

pub struct NativeSubscription {
    session: Arc<RelayCoeditSession>,
    actions: Option<AttachedActions>,
    ready: bool,
    publication: std::sync::Mutex<crate::publication::PublicationCursor>,
}
struct AttachedActions {
    task: Option<tokio::task::JoinHandle<Result<(), ()>>>,
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
            if let Some(barrier) = barrier {
                return queue
                    .serve_with_readiness_and_registration_barrier_for_test(
                        session,
                        room,
                        host,
                        acknowledged,
                        barrier,
                    )
                    .await;
            }
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
        let workspace = qos_relay::room_workspace_id(owner.peer().room())?;
        let state = owner.root().join("relay");
        let config = RelayConfig {
            state_root: state.clone(),
            identity_root: owner.root().join("host-identity"),
            full_replica_status: state.join("full-replica/status.json"),
            home_peer_bundle: state.join("home-peer.toml"),
            trust_policy: qos_relay::TrustPolicy::AllowList {
                users: vec![hex::encode(owner.identity.root.verifying_key().to_bytes())],
            },
            iroh_listen: (Ipv4Addr::LOCALHOST, 0).into(),
            workspaces: vec![WorkspaceBinding {
                id: workspace,
                pin: true,
                replication: qos_relay::ReplicationIntent::FullWorkspace,
                admitted_users: vec![],
                shell_authority: Some(
                    qos_peer_state::retained_proof::RetentionAuthority::new(
                        owner.peer().room(),
                        owner.peer().authority().clone(),
                    )
                    .map_err(anyhow::Error::msg)?,
                ),
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
                qos_relay::room_workspace_id(owner.peer().room())?,
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
        let workspace = qos_relay::room_workspace_id(owner.peer().room())?;
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
pub async fn run(mut owner: SessionOwner, ready_path: &Path) -> Result<()> {
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
    let mut error = None;
    let mut reconnect: Option<Reconnect> = None;
    let mut delay = Duration::ZERO;
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    #[cfg(feature = "test-support")]
    let mut qualification = QualificationHold::default();
    let mut stdin_open = true;
    let metadata = json!({"version":2,"enrollment_version":2,"challenge_path":"/enrollment/challenge","complete_path":"/enrollment/complete","pid":std::process::id(),"platform":std::env::consts::OS,"fixture_origin":format!("http://{}",listener.local_addr()?),"status_path":"/status","packet_relay":host.packet_url,"relay_address":host.address,"room":owner.peer().room(),"workspace_id":qos_relay::room_workspace_id(owner.peer().room())?.to_string(),"state_root":owner.root(),"records":owner.peer().rows()});
    atomic_file::write_with_mode(ready_path, &serde_json::to_vec_pretty(&metadata)?, 0o600)?;
    crate::store::sync_dir(
        ready_path
            .parent()
            .context("ready file requires parent directory")?,
    )?;
    let run_result: Result<()> = async {
    loop {
        if session.is_none() && reconnect.is_none() && owner.permits_publication() {
            let _timing = crate::action_timing::Timing::new("subscription_connecting");
            reconnect = Some(host.begin_connection(&owner, delay)?);
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = pump.tick() => {
                let _timing = crate::action_timing::Timing::slow("owner_pump");
                #[cfg(feature = "test-support")]
                if qualification.deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                    qualification.expired = true; qualification.release();
                }

                if let Some(runtime) = actions.as_mut() {
                    if let Err(problem) = runtime.pump(&owner) { action_error.get_or_insert(problem); }
                    if action_error.is_none() && runtime.ready() {
                        if let Some(current) = session.as_mut().filter(|s| s.is_ready()) {
                            if current.actions.is_none() {
                                #[cfg(feature = "test-support")]
                                let action_registration_barrier = qualification.take_action_registration_barrier();
                                if let Err(problem) = current.attach(
                                    runtime,
                                    owner.peer().room().parse()?,
                                    #[cfg(feature = "test-support")]
                                    action_registration_barrier,
                                ) { action_error = Some(problem.to_string()); }
                            }
                            if current.actions.as_ref().is_some_and(|attached| attached.task.as_ref().is_some_and(|task| task.is_finished())) { action_error = Some("private broker attachment stopped; outcome unknown".into()); }
                        }
                    }
                }
                if action_error.is_some() {
                    *authority.0.lock().map_err(|_| anyhow::anyhow!("authority unavailable"))? = None;
                    if session.as_ref().is_some_and(|s| s.actions.is_some()) { if let Some(old) = session.take() { old.close().await; } }
                }
            }
            // Replacement keeps bounded durable work pending until its exact
            // retained session can publish before any completion is exposed.
            pending = commits.recv(), if session.as_ref().is_some_and(|current| current.is_ready()) => if let Some(pending) = pending {
                let _timing = crate::action_timing::Timing::slow("owner_commit");
                let mut current = authority.0.lock().map_err(|_| anyhow::anyhow!("authority unavailable"))?;
                let result = if pending.is_desktop() { pending.apply_desktop(&mut owner) } else { pending.apply(&mut owner).map(|_| ()) };
                *current = (owner.permits_publication() && action_error.is_none()).then(|| owner.peer().clone());
                let published = session.as_ref().filter(|s| s.is_ready()).ok_or_else(|| anyhow::anyhow!("owner committed while disconnected; publication pending")).and_then(|s| publish(&owner, s));
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
                let published = session.as_ref().filter(|s| s.is_ready()).ok_or_else(|| anyhow::anyhow!("signed commit publication pending; native peer unavailable")).and_then(|s| publish(&owner, s));
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
                        session=Some(next); error=Some("Native subscription is applying retained state".into()); delay=Duration::from_secs(1);
                    }
                    Err(problem) => { error = Some(problem.to_string()); delay = if delay.is_zero() { Duration::from_secs(1) } else { (delay * 2).min(Duration::from_secs(4)) }; }
                }
            }
            inbound = async { match session.as_mut() { Some(current) => current.recv().await, None => std::future::pending().await } } => {
                match inbound {
                    Some(Inbound::Ready {..}) => {
                        let _timing = crate::action_timing::Timing::new("subscription_ready_work");
                        subscription_generation = subscription_generation.checked_add(1).context("native subscription generation exhausted")?;
                        owner.refresh_checkpoint()?;
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
                            match publish(&owner,current) {
                                Ok(())=>error=None,
                                Err(problem)=>{error=Some(problem.to_string());if let Some(old)=session.take(){old.close().await;}}
                            }
                        }
                    },
                    Some(message) => {
                        let _timing = crate::action_timing::Timing::slow("owner_inbound");
                        let (origin, bytes) = match message {
                            Inbound::Delta {author,data} => (qos_peer_state::TransportOrigin::RelayDelta(author),data),
                            Inbound::Presence {author,payload} => (qos_peer_state::TransportOrigin::UntrustedPresence(author),payload),
                            Inbound::Ready {..} => unreachable!(),
                        };
                        let received = {
                            let mut current = authority.0.lock().map_err(|_| anyhow::anyhow!("authority unavailable"))?;
                            let received = postcard::from_bytes(&bytes).map_err(anyhow::Error::from).and_then(|message|owner.receive(origin,message));
                            *current = (owner.permits_publication() && action_error.is_none()).then(|| owner.peer().clone());
                            received
                        };
                        if received.as_ref().is_ok_and(|receipt| receipt.is_some()) { if let Some(runtime) = actions.as_ref() { runtime.committed(&owner).map_err(anyhow::Error::msg)?; } host.update_membership(&owner).await?; }
                        match received {
                            Ok(receipt) => {
                                if !owner.permits_publication() {error=owner.peer().incomplete_reason();if let Some(old)=session.take(){old.close().await;}}
                                else if receipt.is_some() { if let Some(current) = session.as_ref().filter(|current|current.is_ready()) { if let Err(problem) = publish(&owner,current) { error = Some(problem.to_string()); if let Some(old)=session.take(){ old.close().await; } } } }
                            },
                            Err(problem) => { error = Some(problem.to_string()); if !owner.permits_publication() { if let Some(old)=session.take(){old.close().await;} } }
                        }
                    }
                    None => { let _timing = crate::action_timing::Timing::new("subscription_disconnected"); error=Some("native subscription disconnected; reconnecting with durable state".into()); if let Some(old)=session.take(){old.close().await;} }
                }
            }
            command = enrollments.recv() => {
                if let Some(command)=command {
                    let result=if !session.as_ref().is_some_and(|current|current.is_ready()) {
                        Err(crate::enrollment::EnrollmentError::new(503,"native peer is reconnecting; retry enrollment with the same identity"))
                    } else {
                        match command.request {
                            crate::enrollment_http::EnrollmentRequest::Challenge(request)=>enrollment.challenge(&owner,request,qos_relay::clock::now_ns()).and_then(|response|serde_json::to_vec(&response).map_err(|error|crate::enrollment::EnrollmentError::new(503,error.to_string()))),
                            crate::enrollment_http::EnrollmentRequest::Complete(request)=>{
                                let enrolled = {
                                    let mut current = authority.0.lock().map_err(|_| anyhow::anyhow!("authority unavailable"))?;
                                    let enrolled = enrollment.complete(&mut owner,request,qos_relay::clock::now_ns());
                                    *current = (owner.permits_publication() && action_error.is_none()).then(|| owner.peer().clone());
                                    enrolled
                                };
                                match enrolled {
                                    Err(error)=>Err(error),
                                    Ok(certificate)=>match host.update_membership(&owner).await {
                                        Err(error)=>Err(crate::enrollment::EnrollmentError::new(503,format!("committed enrollment awaits relay membership reconciliation: {error}"))),
                                        Ok(())=>{
                                            let published=session.as_ref().ok_or_else(||anyhow::anyhow!("native subscription unavailable")).and_then(|current|publish(&owner,current));
                                            published.map_err(|error|crate::enrollment::EnrollmentError::new(503,error.to_string())).and_then(|()|crate::enrollment::public_bootstrap(&owner,&certificate,serde_json::to_value(&host.address).map_err(|error|crate::enrollment::EnrollmentError::new(503,error.to_string()))?,&host.packet_url.to_string(),qos_relay::clock::now_ns())).and_then(|value|serde_json::to_vec(&value).map_err(|error|crate::enrollment::EnrollmentError::new(503,error.to_string())))
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
                        "publication": {
                            "room_heads": owner.peer().admitted_room().map(|room| room.clone().get_heads().iter().map(ToString::to_string).collect::<Vec<_>>()),
                            "receipts": owner.peer().rows().iter().map(|row| owner.peer().durable_coverage(&row.app_id, &row.record_id)).collect::<Result<Vec<_>, _>>().map_err(anyhow::Error::msg)?,
                        }});
                }
                status["native_subscription_generation"] = json!(subscription_generation);
                let authorization=authorization.clone();let commands=enrollment_commands.clone();
                tokio::spawn(async move { let _=crate::enrollment_http::serve(socket,authorization,commands,status).await; });
            }
            line = stdin.next_line(), if stdin_open => {
                match line? {
                    None => stdin_open=false,
                    #[cfg(feature = "test-support")]
                    Some(line) if qualification.command(&line) => {},
                    Some(line) if line == "shutdown" => break,
                    Some(line) if line == "reconnect" => { if let Some(old)=session.take(){old.close().await;} delay=Duration::ZERO; },
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
