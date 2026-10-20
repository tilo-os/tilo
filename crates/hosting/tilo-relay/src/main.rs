//! `tilo-relay` daemon entrypoint.
//!
//! Subcommands:
//! - `init` — write a default config to `--config` (or
//!   `/etc/tilo/relay.toml`) and exit.
//! - `run` — long-running daemon (systemd `Type=notify`); with feature
//!   `web`, `--web <addr> --allowed-origin <origin> --tls dev|files:<chain>,<key>`
//!   also serves browsers (HTTPS + WebTransport).
//! - `dev` (feature `web`) — a throwaway local host for browser development:
//!   its own user root, the browser listener, and an approver that
//!   certifies every browser that asks.
//! - `room` (feature `web`) — a short-lived shared room: like `dev`, but it
//!   certifies only the browsers whose pairing codes its operator admits
//!   (`admit <code>` on stdin) and announces each on stdout.
//! - `--agents` on `run` and `dev` (feature `agents`) — agents on the host,
//!   with `--agent-model settings|scripted|openai:<url>|cloud:<url>`.
//! - `status` — print a snapshot of the running daemon's counters
//!   by reading the on-disk metrics dump.
//! - `bind-workspace <workspace_id> [--pin]` — add a workspace to
//!   the config file so the next `run` picks it up.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing_subscriber::{EnvFilter, fmt};

use tilo_relay::{RelayConfig, daemon::RelayIdentity};
use tilo_user_identity::{IdentityPaths, RevocationList, persist};

#[cfg(feature = "agents")]
mod persistent_moderation;
#[cfg(feature = "agents")]
mod persistent_room_cli;
#[cfg(feature = "agents")]
mod room_chat;
#[cfg(feature = "agents")]
mod room_strips;
#[cfg(feature = "web")]
mod web_cli;

/// Default location for the daemon config on a Fedora bootc image.
const DEFAULT_CONFIG_PATH: &str = "/etc/tilo/relay.toml";

#[derive(Parser)]
#[command(name = "tilo-relay", version, about = "Tilo host + relay daemon")]
struct Cli {
    /// Path to the relay TOML config.
    #[arg(long, global = true, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write a default config file and exit (no-op if the file
    /// already exists).
    Init,
    /// Run the relay daemon (long-running).
    Run {
        #[cfg(feature = "web")]
        #[command(flatten)]
        web: web_cli::WebArgs,
    },
    /// A throwaway local host for browser development (never in production:
    /// it certifies every browser device that asks).
    #[cfg(feature = "web")]
    Dev(web_cli::DevArgs),
    /// A short-lived shared room: its own user root and browser listener,
    /// certifying only the browsers its operator admits on stdin.
    #[cfg(feature = "web")]
    Room(Box<web_cli::RoomArgs>),
    /// Print current metrics + hosted workspace list.
    Status,
    /// Add a workspace to the config file.
    BindWorkspace {
        /// Numeric workspace id.
        id: u64,
        /// Pin the workspace (default true).
        #[arg(long, default_value_t = true)]
        pin: bool,
    },
    /// Remove a workspace from the config file.
    UnbindWorkspace {
        /// Numeric workspace id.
        id: u64,
    },
}

/// Logs go to stderr: a persistent room's stdout is the line protocol its
/// room service reads, which dropped every log line, so a hosted relay's
/// refusals and errors were never seen anywhere.
fn install_logging() {
    let filter = EnvFilter::try_from_env("TILO_RELAY_LOG")
        .unwrap_or_else(|_| EnvFilter::new("tilo_relay=info,warn"));
    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .init();
}

fn load_or_default(path: &std::path::Path) -> anyhow::Result<RelayConfig> {
    if path.exists() {
        Ok(RelayConfig::load(path).with_context(|| format!("load {}", path.display()))?)
    } else {
        Ok(RelayConfig::default())
    }
}

fn main() -> anyhow::Result<std::process::ExitCode> {
    // Automerge isolation workers re-enter this binary; serve them first.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return Ok(std::process::ExitCode::from(code));
    }
    // So do the confined workers agents and room actions run apps in.
    #[cfg(feature = "agents")]
    if let Some(code) = tilo_agent_participant::worker::dispatch_early() {
        return Ok(std::process::ExitCode::from(code));
    }
    run().map(|()| std::process::ExitCode::SUCCESS)
}

fn run() -> anyhow::Result<()> {
    install_logging();
    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Init => cmd_init(&cli.config),
        #[cfg(feature = "web")]
        Cmd::Run { web } => cmd_run(&cli.config, web),
        #[cfg(not(feature = "web"))]
        Cmd::Run {} => cmd_run(&cli.config),
        #[cfg(feature = "web")]
        Cmd::Dev(args) => {
            tilo_rt::daemon("tilo-relay").run(web_cli::dev(args, wait_for_shutdown()))?
        }
        #[cfg(feature = "web")]
        Cmd::Room(args) => {
            tilo_rt::daemon("tilo-relay").run(web_cli::room(*args, wait_for_shutdown()))?
        }
        Cmd::Status => cmd_status(&cli.config),
        Cmd::BindWorkspace { id, pin } => cmd_bind(&cli.config, id, pin),
        Cmd::UnbindWorkspace { id } => cmd_unbind(&cli.config, id),
    }
}

fn cmd_init(path: &std::path::Path) -> anyhow::Result<()> {
    if path.exists() {
        println!("config already exists: {}", path.display());
        return Ok(());
    }
    let cfg = RelayConfig::default();
    cfg.save(path)
        .with_context(|| format!("save {}", path.display()))?;
    println!("wrote default config: {}", path.display());
    Ok(())
}

fn cmd_bind(path: &Path, id: u64, pin: bool) -> anyhow::Result<()> {
    let mut cfg = load_or_default(path)?;
    let added = cfg.add_workspace(id, pin);
    cfg.save(path)?;
    if added {
        println!("bound workspace {id} (pin={pin})");
    } else {
        println!("workspace {id} already bound");
    }
    Ok(())
}

fn cmd_unbind(path: &Path, id: u64) -> anyhow::Result<()> {
    let mut cfg = load_or_default(path)?;
    let removed = cfg.remove_workspace(id);
    cfg.save(path)?;
    if removed {
        println!("unbound workspace {id}");
    } else {
        println!("workspace {id} was not bound");
    }
    Ok(())
}

fn cmd_status(path: &Path) -> anyhow::Result<()> {
    let cfg = load_or_default(path)?;
    println!(
        "tilo-relay {} (config: {})",
        tilo_relay::VERSION,
        path.display()
    );
    println!("identity_root: {}", cfg.identity_root.display());
    println!("state_root:    {}", cfg.state_root.display());
    println!("full replica:  {}", cfg.full_replica_status.display());
    println!("iroh_listen:   {}", cfg.iroh_listen);
    println!("workspaces bound:");
    for ws in &cfg.workspaces {
        println!("  - {} (pin={})", ws.id, ws.pin);
    }
    match std::fs::read(&cfg.full_replica_status)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    {
        Some(status) => println!(
            "full_content_replication: {}",
            status
                .get("full_content_replication")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
                && status.get("state").and_then(serde_json::Value::as_str) == Some("ready")
        ),
        None => println!("full_content_replication: false"),
    }
    if let Some(metrics) = tilo_relay::store::load_metrics(&cfg.state_root)? {
        println!(
            "connections: {}/{} authenticated",
            metrics.conns_authed, metrics.conns_accepted
        );
        println!("auth rejected: {}", metrics.auth_rejected);
        println!(
            "deltas: {} received, {} fanned out",
            metrics.deltas_received, metrics.deltas_fanned_out
        );
        println!("bytes queued: {}", metrics.bytes_queued);
        println!(
            "bytes sent: {} ({} live fan-out)",
            metrics.bytes_sent, metrics.bytes_sent_live
        );
    }
    Ok(())
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay must keep serving until shutdown().await consumes it"
)]
fn cmd_run(path: &Path, #[cfg(feature = "web")] web: web_cli::WebArgs) -> anyhow::Result<()> {
    let cfg = load_or_default(path)?;
    tilo_rt::daemon("tilo-relay").run(async move {
        // Every change the relay receives is decided against its app's
        // surface, fetched by hash from the registry.
        tilo_relay::surfaces::decide_receipt_from_registry(
            &std::env::var("TILO_REGISTRY")
                .unwrap_or_else(|_| tilo_relay::surfaces::DEFAULT_REGISTRY.to_owned()),
        );
        #[cfg(feature = "web")]
        web_cli::await_certificate(&cfg.identity_root, &web, &cfg.state_root).await?;
        let identity = load_identity(&cfg.identity_root, &cfg.state_root)?;
        #[cfg(feature = "web")]
        let state_root = cfg.state_root.clone();
        #[cfg(feature = "web")]
        let hosted = web_cli::start_relay(cfg, identity, &web, &state_root).await?;
        #[cfg(feature = "web")]
        let relay = &hosted.relay;
        #[cfg(not(feature = "web"))]
        let relay = tilo_relay::daemon::Relay::start(cfg, identity).await?;
        #[cfg(feature = "web")]
        let _web = web_cli::serve(relay, &web, &state_root).await?;
        #[cfg(feature = "agents")]
        let public_moderation = persistent_moderation::follow_published(relay)?;
        let addr = relay.node_addr();
        tracing::info!(?addr, "relay online");
        // Best-effort sd_notify (silently no-ops outside systemd).
        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);
        // Park until Ctrl-C / SIGTERM.
        wait_for_shutdown().await?;
        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Stopping]);
        #[cfg(feature = "agents")]
        if let Some(moderation) = public_moderation {
            moderation.abort();
        }
        #[cfg(feature = "web")]
        hosted.shutdown().await;
        #[cfg(not(feature = "web"))]
        relay.shutdown().await;
        Ok::<(), anyhow::Error>(())
    })?
}

fn load_identity(
    root: &std::path::Path,
    state_root: &std::path::Path,
) -> anyhow::Result<RelayIdentity> {
    let paths = IdentityPaths::under(root);
    let user_public_key = persist::read_public_key(&paths.user_pubkey)
        .with_context(|| format!("read user pubkey {}", paths.user_pubkey.display()))?;
    let device_signing_key = persist::read_device_key(&paths.device_privkey)
        .with_context(|| format!("read device secret {}", paths.device_privkey.display()))?;
    let device_cert = persist::read_device_cert(&paths.device_cert)
        .with_context(|| format!("read device cert {}", paths.device_cert.display()))?;
    let mut revocations = RevocationList::new();
    for entry in tilo_relay::store::load_revocations(state_root)? {
        revocations
            .insert(entry)
            .context("verify persisted relay revocation")?;
    }
    Ok(RelayIdentity {
        user_public_key,
        device_signing_key,
        device_cert,
        revocations,
    })
}

async fn wait_for_shutdown() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
        let mut intr = signal(SignalKind::interrupt()).context("install SIGINT handler")?;
        tokio::select! {
            _ = term.recv() => tracing::info!("SIGTERM"),
            _ = intr.recv() => tracing::info!("SIGINT"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
mod tests {
    use super::*;
    use iroh_base::EndpointAddr;
    use tilo_relay::daemon::Relay;
    use tilo_relay::mesh::{ReplicationIntent, ServiceCapabilities};
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity,
        sign_device_cert,
    };

    /// Exercise the same stock init + config reload used by firstboot, then
    /// start the daemon with the handoff bundle that firstboot writes. This
    /// catches regressions where the NAS appears configured but silently
    /// starts as a relay-only peer or forgets its workspace bindings.
    #[tokio::test]
    async fn init_reload_and_home_peer_handoff_enable_full_replication() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("relay.toml");
        cmd_init(&config_path).unwrap();

        let mut config = RelayConfig::load(&config_path).unwrap();
        assert_eq!(
            config.home_peer_bundle,
            PathBuf::from("/etc/tilo/home-peer.toml")
        );
        assert_eq!(
            config.full_replica_status,
            PathBuf::from("/var/lib/tilo/full-replica/status.json")
        );
        assert!(!config.services.full_content_replication);

        let (user_sk, user) = UserIdentity::generate("home owner", 1);
        let (device_sk, device) = DeviceIdentity::generate("home server", 2);
        let mut cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: device.public_key,
            device_label: device.label,
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 3,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &user_sk).unwrap();

        let status_path = temp.path().join("full-replica/status.json");
        std::fs::create_dir_all(status_path.parent().unwrap()).unwrap();
        let filesystem_endpoint =
            EndpointAddr::new(iroh::SecretKey::from_bytes(&device_sk.to_bytes()).public())
                .with_ip_addr("127.0.0.1:43210".parse().unwrap());
        let mut status = fs::full_replica::FullReplicaStatus::syncing();
        status.state = "ready".into();
        status.full_content_replication = true;
        status.filesystem_endpoints.push(filesystem_endpoint);
        fs::full_replica::save_status(&status_path, &status).unwrap();
        // The relay judges the stamp by a clock stopped where it was written,
        // so it stays recent however long the relay takes to start.
        let stamped = fs::full_replica::load_status(&status_path)
            .unwrap()
            .unwrap()
            .updated_at_ms;

        let bundle_path = temp.path().join("home-peer.toml");
        let bundle = fs::full_replica::HomePeerSet {
            status_file: status_path.clone(),
            workspaces: vec![fs::full_replica::HomePeerBundle {
                ticket: fs::Ticket {
                    node: EndpointAddr::new(iroh::SecretKey::from_bytes(&[7; 32]).public()),
                    workspace: [9; 32],
                }
                .encode()
                .unwrap(),
                cache_dir: temp.path().join("cache"),
                capability_json: "[]".into(),
                authority: None,
                principals: Vec::new(),
                relay_workspace_ids: vec![42],
            }],
        };
        std::fs::write(&bundle_path, toml::to_string(&bundle).unwrap()).unwrap();

        // These paths model the firstboot-generated handoff while keeping the
        // test hermetic instead of writing under /etc or /var/lib.
        config.home_peer_bundle = bundle_path;
        config.full_replica_status = status_path.clone();
        config.state_root = temp.path().join("relay-state");
        config.identity_root = temp.path().join("identity");
        config.services = ServiceCapabilities::home_peer();
        config.save(&config_path).unwrap();
        let config = RelayConfig::load(&config_path).unwrap();

        let relay = Relay::start_with_replica_clock(
            config,
            RelayIdentity {
                user_public_key: user.public_key,
                device_signing_key: device_sk,
                device_cert: cert,
                revocations: RevocationList::new(),
            },
            std::sync::Arc::new(tilo_clock::FixedClock(stamped)),
        )
        .await
        .unwrap();
        let profile = relay.handle().mesh_profile();
        assert_eq!(
            profile.replication.get(&42),
            Some(&ReplicationIntent::FullWorkspace)
        );
        assert!(profile.services.full_content_replication);
        assert_eq!(profile.filesystem_endpoints.len(), 1);
        relay.shutdown().await;
    }
}
