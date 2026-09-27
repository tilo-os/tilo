//! Explicit Cloud HTTPS startup: prepared transport, the existing enrolled
//! Cloud Store and its agent owner, then the composed data/private/media
//! service. Ready is published only after every advertised route is installed.
use super::{
    CloudStoreSource, Current,
    config::OperatorConfig,
    live::InstalledLive,
    media::CloudMediaService,
    service::{CloudService, PrivateLane, spawn_private_broker},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use atomic_file::{Privacy, PublishMode, StorageDirectory};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};
use tilo_delivery_protocol::delivery_v3::capabilities::PRODUCT_DATA_V1;
use tilo_workspace_host::ingress::PreparedTransport;
struct PreparedCloud {
    config: OperatorConfig,
    transport: PreparedTransport,
    ready_directory: StorageDirectory,
    ready_name: OsString,
    ready_path: PathBuf,
}
impl PreparedCloud {
    async fn prepare(config_path: &Path, ready_path: &Path) -> Result<Self> {
        let config = OperatorConfig::load(config_path)?;
        ensure!(
            ready_path.is_absolute(),
            "absolute owner-only ready path required"
        );
        let parent = ready_path.parent().context("ready directory missing")?;
        let ready_name = ready_path
            .file_name()
            .context("ready filename missing")?
            .to_owned();
        let ready_directory = StorageDirectory::open(parent)?;
        ready_directory.validate_privacy(Privacy::OwnerOnly)?;
        match std::fs::symlink_metadata(ready_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
            Ok(_) => bail!("ready destination already exists"),
        }
        let transport = PreparedTransport::load_explicit(&config.transport_config).await?;
        transport.ensure_store_separate(&config.state_root)?;
        transport.ensure_store_separate(parent)?;
        Ok(Self {
            config,
            transport,
            ready_directory,
            ready_name,
            ready_path: ready_path.to_owned(),
        })
    }
    /// Bind the existing enrolled Store only. The startup reservation is sized
    /// from the persisted aggregate before the Store lock or decode; nothing
    /// here provisions, enrolls, or bootstraps a replacement owner.
    fn open_source(
        &self,
    ) -> Result<(
        CloudStoreSource,
        tilo_action_broker::peer_actions::OwnerPolicyClock,
    )> {
        let root = &self.config.state_root;
        let journal = std::fs::symlink_metadata(root.join("state.journal"))
            .ok()
            .filter(|metadata| metadata.is_file())
            .map_or(0, |metadata| metadata.len());
        let aggregate = std::fs::symlink_metadata(root.join("state.json"))
            .ok()
            .filter(|metadata| metadata.is_file())
            .context("existing enrolled Cloud Store required")?
            .len()
            .saturating_add(journal);
        let pool = self.transport.host_bytes()?;
        let _startup = usize::try_from(aggregate)
            .ok()
            .and_then(|length| length.checked_mul(4))
            .and_then(|bytes| bytes.checked_add(1024 * 1024))
            .and_then(|bytes| pool.try_acquire_bulk(bytes))
            .context("startup Store reservation unavailable")?;
        let open = || {
            crate::delivery::access::open(root, &self.config.machine_id).map_err(
                |error| match error {
                    crate::delivery::access::AccessError::Busy => anyhow!("Cloud Store busy"),
                    crate::delivery::access::AccessError::Denied(error) => error,
                },
            )
        };
        let store = match open() {
            Ok(store) => store,
            // Only an explicit operator choice selects delivery storage for an
            // existing enrolled pre-delivery Store; nothing is provisioned.
            Err(error) if !self.transport.activation_requested() => {
                return Err(error.context("activate_delivery true required for schema transition"));
            }
            Err(_) => {
                let mut legacy = crate::Store::open(root, &self.config.machine_id)?;
                self.config.owner_root(&legacy)?;
                ensure!(
                    legacy.state.bound.is_some(),
                    "enrolled Cloud Store required"
                );
                if legacy.state.version != crate::delivery::VERSION {
                    legacy.activate_delivery_storage(tilo_delivery_protocol::clock::now_ns())?;
                }
                drop(legacy);
                open()?
            }
        };
        self.config.owner_root(&store)?;
        ensure!(store.state.bound.is_some(), "enrolled Cloud Store required");
        let clock = tilo_action_broker::peer_actions::OwnerPolicyClock::new()?;
        Ok((
            CloudStoreSource::bind_with_clock(store, clock.clone())?,
            clock,
        ))
    }
}
struct ReadyRecord {
    directory: StorageDirectory,
    name: OsString,
    path: PathBuf,
}
impl ReadyRecord {
    fn publish(&self, service: &CloudService) -> Result<()> {
        let record = serde_json::json!({
            "version": 1,
            "origin": service.origin(),
            "capabilities": service.capabilities(),
            "descriptor_digest": hex::encode(service.descriptor_digest()),
            "pid": std::process::id(),
        });
        self.directory.publish(
            &self.name,
            &serde_json::to_vec(&record)?,
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
        )?;
        Ok(())
    }
}

fn current<R>(
    source: &CloudStoreSource,
    action: impl Fn(&super::CloudStoreTurn) -> R,
) -> Result<R> {
    for _ in 0..1000 {
        match source.with_current(&action) {
            Current::Current(value) => return Ok(value),
            Current::Denied => bail!("Cloud owner denied"),
            Current::Busy => std::thread::sleep(std::time::Duration::from_millis(2)),
        }
    }
    bail!("Cloud owner busy")
}

/// The media core binds under a current turn; another Cloud component may
/// briefly hold the Store lock at startup, which is not a denial.
async fn media_service(source: &CloudStoreSource, origin: &str) -> Result<CloudMediaService> {
    let audience = format!("{origin}/moq/voice");
    for _ in 0..500 {
        match CloudMediaService::new_unqualified(source.clone(), &audience) {
            Ok(service) => return Ok(service),
            Err(_) if !source.is_denied() => {
                tokio::time::sleep(std::time::Duration::from_millis(4)).await
            }
            Err(error) => bail!("media service: {error:?}"),
        }
    }
    bail!("media service: Cloud owner remained busy")
}

/// LIVE_V1 on this endpoint: bind the live authority to the actual source,
/// install the relay on the running QUIC endpoint, start the forwarder. Any
/// failure leaves live uninstalled (clients stay on the durable path and the
/// descriptor does not advertise the live bit).
async fn live_service(
    source: &CloudStoreSource,
    origin: &str,
    transport: &tilo_workspace_host::ingress::RunningTransport,
) -> Result<InstalledLive<CloudStoreSource>> {
    let mut bound = None;
    for _ in 0..500 {
        match source.with_current(|turn| super::live::bind(source.clone(), turn, origin)) {
            Current::Current(service) => {
                bound = Some(service?);
                break;
            }
            Current::Denied => bail!("live service: Cloud owner denied"),
            Current::Busy => tokio::time::sleep(std::time::Duration::from_millis(4)).await,
        }
    }
    let live = bound
        .context("live service: Cloud owner remained busy")?
        .install();
    ensure!(
        transport.install_live(live.relay.clone()) && transport.live_installed(),
        "live relay not installed on the endpoint"
    );
    Ok(live)
}

/// Agent live publisher on the installed relay (room and host from a current
/// turn); `None` leaves agents on the durable path only.
fn agent_live_publisher(
    source: &CloudStoreSource,
    live: &InstalledLive<CloudStoreSource>,
) -> Option<super::agent_live::AgentLivePublisher> {
    let (room, host) = current(source, |turn| {
        (
            turn.peer().room().to_owned(),
            turn.peer().authority().device_public_key,
        )
    })
    .inspect_err(|error| tracing::warn!(%error, "agent live publisher unavailable"))
    .ok()?;
    Some(super::agent_live::AgentLivePublisher::start(
        live.relay.clone(),
        room,
        host,
    ))
}

pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

/// The agent lane the HTTPS private route answers through.
pub(crate) enum AgentLane {
    /// The host's one agent runtime, shared with the relay private lane.
    Runtime(crate::CloudAgentCommands),
    /// Private commands reach the Store-backed collaboration broker only and
    /// agent control answers unavailable.
    #[cfg(test)]
    BrokerOnly,
}

/// Cloud HTTPS delivery for one enrolled Store, prepared and bound to its
/// long-lived owner before anything is served.
pub(crate) struct Https {
    prepared: PreparedCloud,
    source: CloudStoreSource,
    clock: tilo_action_broker::peer_actions::OwnerPolicyClock,
}

impl Https {
    /// Load the operator configuration for `root` and bind its Store. The
    /// configuration must name the same state root and machine as the host.
    pub(crate) async fn prepare(
        config_path: &Path,
        ready_path: &Path,
        root: &Path,
        machine: &str,
    ) -> Result<Self> {
        let prepared = PreparedCloud::prepare(config_path, ready_path).await?;
        ensure!(
            std::fs::canonicalize(&prepared.config.state_root)? == std::fs::canonicalize(root)?
                && prepared.config.machine_id == machine,
            "HTTPS configuration names another Cloud Store"
        );
        let (source, clock) = prepared.open_source()?;
        Ok(Self {
            prepared,
            source,
            clock,
        })
    }
    pub(crate) fn source(&self) -> &CloudStoreSource {
        &self.source
    }
    pub(crate) fn clock(&self) -> tilo_action_broker::peer_actions::OwnerPolicyClock {
        self.clock.clone()
    }
    /// The source's policy time, captured under one of its turns.
    pub(crate) fn source_time(&self) -> Result<tilo_action_broker::peer_actions::PolicyTime> {
        current(&self.source, |turn| turn.policy_time())?
    }

    /// Serve until `stop` resolves or the service fails. Ready is published
    /// only after every advertised route is installed.
    pub(crate) async fn run(
        self,
        lane: AgentLane,
        stop: impl std::future::Future<Output = ()>,
    ) -> Result<()> {
        let Self {
            prepared,
            source,
            clock,
        } = self;
        let root = prepared.config.state_root.clone();
        let machine = prepared.config.machine_id.clone();
        let pool = prepared.transport.host_bytes()?;
        let work = prepared.transport.host_work()?;
        let origin = prepared.transport.origin().to_owned();
        let PreparedCloud {
            transport,
            ready_directory,
            ready_name,
            ready_path,
            ..
        } = prepared;
        let ready = ReadyRecord {
            directory: ready_directory,
            name: ready_name,
            path: ready_path,
        };
        let (commands, policy) = match lane {
            AgentLane::Runtime(commands) => {
                let policy = commands.policy_context();
                (Some(commands), policy)
            }
            #[cfg(test)]
            AgentLane::BrokerOnly => {
                let source_time = current(&source, |turn| turn.policy_time())??;
                let policy = Arc::new(crate::agent_owner::RuntimePolicyClock::new(clock));
                policy
                    .bind_source_owner(source_time, source.clone())
                    .map_err(anyhow::Error::msg)?;
                (None, policy)
            }
        };
        #[cfg(not(test))]
        drop(clock);
        let (queue, broker, observer) =
            spawn_private_broker(root.clone(), machine.clone(), commands, policy);
        let private = PrivateLane::install(&source, &queue, broker, observer)
            .context("private lane install")?;
        let media = Arc::new(media_service(&source, &origin).await?);
        let authority: Arc<dyn tilo_workspace_host::ingress::RelayAuthority> = media.clone();
        let mut transport = transport.start(authority).await?;
        ensure!(
            transport.is_live() && transport.origin() == origin,
            "transport not live"
        );
        let live = match live_service(&source, &origin, &transport).await {
            Ok(live) => Some(live),
            Err(error) => {
                tracing::warn!(%error, "Cloud live relay unavailable; durable path only");
                None
            }
        };
        // Speak for agents in their own host-fed live namespaces. The
        // bus never blocks the Store owner; dropped at shutdown.
        let agent_live = live
            .as_ref()
            .and_then(|live| agent_live_publisher(&source, live));
        let mut service = CloudService::install(
            source.clone(),
            &origin,
            pool,
            work,
            Some(private),
            Some(media),
            live,
        )
        .context("Cloud service install")?;
        ensure!(
            service.product_ready() && service.capabilities() == PRODUCT_DATA_V1,
            "complete product data routes unavailable"
        );
        ready.publish(&service)?;
        tracing::info!(
            event = "cloud_https_ready",
            origin = %origin,
            capabilities = service.capabilities(),
            result = "ready",
        );
        tokio::pin!(stop);
        // `TILO_HOST_PERF_BASELINE=1` keeps the previous polling-only loop.
        let wake = (!crate::storage::host_perf_baseline()).then(|| service.wake_handle());
        // A Store commit by any component (agent worker, retention) wakes held polls at once.
        let mut commits = crate::delivery::commit_wake::CommitWake::subscribe();
        let served: Result<()> = loop {
            let woken = async {
                match &wake {
                    Some(wake) => wake.notified().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                _ = &mut stop => break Ok(()),
                request = transport.recv() => match request {
                    Some(request) => service.handle(request),
                    None => break Err(anyhow!("Cloud HTTPS transport stopped")),
                },
                // Completed upload work / private replies (a wake raised
                // while `maintain` ran is kept as the permit: no lost wake-up).
                _ = woken => {}
                _ = commits.changed() => {}
                _ = tokio::time::sleep_until(service.next_wakeup()) => {}
            }
            #[cfg(test)]
            let began = std::time::Instant::now();
            service.maintain();
            #[cfg(test)]
            super::service::quiet_ticks::count(&root, began.elapsed());
            if service.is_failed() {
                tracing::warn!(cause = service.failure_cause(), "Cloud owner failed");
                break Err(anyhow!("Cloud owner failed; restart with a fresh binding"));
            }
        };
        // Held requests get their idle/refusal answers; every admitted commit
        // already passed its Store barrier, so no owner state remains buffered.
        service.close();
        drop(agent_live);
        let _ = std::fs::remove_file(&ready.path);
        if let Err(error) = &served {
            tracing::warn!(%error, "Cloud HTTPS service stopped");
        }
        // The caller retires the source once the agent owner has stopped: the
        // owner's final flush still needs the source's policy time.
        transport.shutdown().await.and(served)
    }
}

/// Cloud HTTPS with its own agent runtime and no relay: the delivery half of
/// `serve`, wired exactly as the host wires it, for component tests.
#[cfg(test)]
pub(crate) async fn serve_until(
    config_path: &Path,
    ready_path: &Path,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let config = OperatorConfig::load(config_path)?;
    let (root, machine) = (config.state_root.clone(), config.machine_id.clone());
    let https = Https::prepare(config_path, ready_path, &root, &machine).await?;
    // No relay here: this component stands for a host whose relay replay completed.
    crate::owner_cell::mark_governance_reconciled(&root);
    let mut agents = crate::agent_runtime::CloudAgentRuntime::start_with_policy_time(
        &root,
        &machine,
        true,
        https.clock(),
        https.source_time()?,
        https.source().clone(),
    )
    .await
    .map_err(anyhow::Error::msg)?;
    let commands = agents.certified_commands();
    let source = https.source().clone();
    let served = {
        let outbound = &mut agents.outbound;
        let run = https.run(AgentLane::Runtime(commands), shutdown);
        tokio::pin!(run);
        loop {
            tokio::select! {
                result = &mut run => break result,
                // Live presence has no HTTPS carrier; durable state is
                // delivered only through the Store replay ledger.
                message = outbound.recv() => if message.is_none() {
                    break Err(anyhow!("agent runtime owner stopped"));
                },
            }
        }
    };
    let closed = agents
        .close_within(AGENT_STOP_BOUND)
        .await
        .map_err(anyhow::Error::msg);
    source.retire();
    served.and(closed)
}
/// Component composition without the agent execution owner.
#[cfg(test)]
pub(in crate::https_owner) async fn serve_broker_only_until(
    config_path: &Path,
    ready_path: &Path,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let config = OperatorConfig::load(config_path)?;
    let https = Https::prepare(
        config_path,
        ready_path,
        &config.state_root,
        &config.machine_id,
    )
    .await?;
    // No relay here: this component stands for a host whose relay replay completed.
    crate::owner_cell::mark_governance_reconciled(&config.state_root);
    let source = https.source().clone();
    let served = https.run(AgentLane::BrokerOnly, shutdown).await;
    source.retire();
    served
}
/// Upper bound for the agent owner to stop during shutdown.
#[cfg(not(test))]
pub(crate) const AGENT_STOP_BOUND: std::time::Duration = std::time::Duration::from_secs(6);
/// Tests run many actual runtimes at once on a shared runner, where joining
/// the owner's worker threads alone can take longer than production's bound;
/// they check a clean stop, not its latency.
#[cfg(test)]
pub(crate) const AGENT_STOP_BOUND: std::time::Duration = std::time::Duration::from_secs(60);
#[cfg(test)]
pub(in crate::https_owner) mod tests;
