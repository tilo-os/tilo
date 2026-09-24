//! Explicit Cloud HTTPS startup: prepared transport, the existing enrolled
//! Cloud Store and its agent owner, then the composed data/private/media
//! service. Ready is published only after every advertised route is installed.
use super::{
    CloudStoreSource, Current,
    config::OperatorConfig,
    live::{CloudLiveService, InstalledLive},
    media::CloudMediaService,
    service::{CloudService, PrivateLane, spawn_private_broker},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use atomic_file::{Privacy, PublishMode, StorageDirectory};
use qos_relay::delivery_v3::capabilities::PRODUCT_DATA_V1;
use qos_session_peer::ingress::PreparedTransport;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};
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
        qos_action_broker::peer_actions::OwnerPolicyClock,
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
                    legacy.activate_delivery_storage(qos_relay::clock::now_ns())?;
                }
                drop(legacy);
                open()?
            }
        };
        self.config.owner_root(&store)?;
        ensure!(store.state.bound.is_some(), "enrolled Cloud Store required");
        let clock = qos_action_broker::peer_actions::OwnerPolicyClock::new()?;
        Ok((
            CloudStoreSource::bind_with_clock(&store, clock.clone())?,
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
    let audience = format!("{origin}/moq/v21/voice");
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
    transport: &qos_session_peer::ingress::RunningTransport,
) -> Result<InstalledLive> {
    let mut bound = None;
    for _ in 0..500 {
        match source.with_current(|turn| CloudLiveService::bind(source.clone(), turn, origin)) {
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

async fn shutdown_signal() {
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

pub(crate) async fn serve(config_path: &Path, ready_path: &Path) -> Result<()> {
    serve_until(config_path, ready_path, shutdown_signal()).await
}

pub(crate) async fn serve_until(
    config_path: &Path,
    ready_path: &Path,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    serve_inner(config_path, ready_path, shutdown, AgentOwner::Runtime).await
}
/// Component composition without the agent execution owner: private commands
/// reach the Store-backed collaboration broker only and agent control answers
/// unavailable. Production startup always uses the actual agent runtime.
#[cfg(test)]
pub(in crate::https_owner) async fn serve_broker_only_until(
    config_path: &Path,
    ready_path: &Path,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    serve_inner(config_path, ready_path, shutdown, AgentOwner::BrokerOnly).await
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum AgentOwner {
    Runtime,
    #[cfg(test)]
    BrokerOnly,
}

async fn serve_inner(
    config_path: &Path,
    ready_path: &Path,
    shutdown: impl std::future::Future<Output = ()>,
    mode: AgentOwner,
) -> Result<()> {
    let prepared = PreparedCloud::prepare(config_path, ready_path).await?;
    let root = prepared.config.state_root.clone();
    let machine = prepared.config.machine_id.clone();
    let pool = prepared.transport.host_bytes()?;
    let work = prepared.transport.host_work()?;
    let origin = prepared.transport.origin().to_owned();
    let (source, clock) = prepared.open_source()?;
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
    // The agent owner shares the actual source and its policy clock; it is
    // the only execution owner for private agent commands and reads.
    let source_time = current(&source, |turn| turn.policy_time())??;
    let (mut agents, commands, policy) = match mode {
        AgentOwner::Runtime => {
            let agents = crate::agent_runtime::CloudAgentRuntime::start_with_policy_time(
                &root,
                &machine,
                true,
                clock,
                source_time,
                source.clone(),
            )
            .await
            .map_err(anyhow::Error::msg)?;
            let commands = agents.certified_commands();
            let policy = commands.policy_context();
            (Some(agents), Some(commands), policy)
        }
        #[cfg(test)]
        AgentOwner::BrokerOnly => {
            let policy = Arc::new(crate::agent_owner::RuntimePolicyClock::new(clock));
            policy
                .bind_source_owner(source_time, source.clone())
                .map_err(anyhow::Error::msg)?;
            (None, None, policy)
        }
    };
    let result = async {
        let (queue, broker, observer) =
            spawn_private_broker(root.clone(), machine.clone(), commands, policy);
        let private = PrivateLane::install(&source, &queue, broker, observer)?;
        let media = Arc::new(media_service(&source, &origin).await?);
        let authority: Arc<dyn qos_session_peer::ingress::RelayAuthority> = media.clone();
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
        let mut service = CloudService::install(
            source.clone(),
            &origin,
            pool,
            work,
            Some(private),
            Some(media),
            live,
        )?;
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
        tokio::pin!(shutdown);
        // `QOS_HOST_PERF_BASELINE=1` keeps the previous polling-only loop.
        let wake = (!crate::storage::host_perf_baseline()).then(|| service.wake_handle());
        let served: Result<()> = loop {
            let outbound = async {
                match agents.as_mut() {
                    Some(agents) => agents.outbound.recv().await,
                    None => std::future::pending().await,
                }
            };
            let woken = async {
                match &wake {
                    Some(wake) => wake.notified().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                _ = &mut shutdown => break Ok(()),
                request = transport.recv() => match request {
                    Some(request) => service.handle(request),
                    None => break Err(anyhow!("Cloud HTTPS transport stopped")),
                },
                outbound = outbound => match outbound {
                    // Live presence has no HTTPS carrier; durable state is
                    // delivered only through the Store replay ledger.
                    Some(_) => {}
                    None => break Err(anyhow!("agent runtime owner stopped")),
                },
                // Completed upload work / private replies (a wake raised
                // while `maintain` ran is kept as the permit: no lost wake-up).
                _ = woken => {}
                _ = tokio::time::sleep_until(service.next_wakeup()) => {}
            }
            #[cfg(test)]
            let began = std::time::Instant::now();
            service.maintain();
            #[cfg(test)]
            super::service::quiet_ticks::count(&root, began.elapsed());
            if service.is_failed() {
                break Err(anyhow!("Cloud owner failed; restart with a fresh binding"));
            }
        };
        // Held requests get their idle/refusal answers; every admitted commit
        // already passed its Store barrier, so no owner state remains buffered.
        service.close();
        let _ = std::fs::remove_file(&ready.path);
        if let Err(error) = &served {
            tracing::warn!(%error, "Cloud HTTPS service stopped");
        }
        let stopped = transport.shutdown().await;
        served.and(stopped)
    }
    .await;
    let closed = match agents.as_mut() {
        Some(agents) => agents.close().await.map_err(anyhow::Error::msg),
        None => Ok(()),
    };
    source.retire();
    // Orderly shutdown folds the Store journal into the checkpoint.
    if let Err(error) = crate::storage::compact(&root) {
        tracing::warn!(%error, "Cloud state journal compaction deferred");
    }
    result.and(closed)
}
#[cfg(test)]
pub(in crate::https_owner) mod tests;
