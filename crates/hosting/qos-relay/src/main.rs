//! `qos-relay` daemon entrypoint.
//!
//! Subcommands:
//! - `init` — write a default config to `--config` (or
//!   `/etc/qos/relay.toml`) and exit.
//! - `run` — long-running daemon (systemd `Type=notify`).
//! - `status` — print a snapshot of the running daemon's counters
//!   by reading the on-disk metrics dump.
//! - `bind-workspace <workspace_id> [--pin]` — add a workspace to
//!   the config file so the next `run` picks it up.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing_subscriber::{EnvFilter, fmt};

use qos_relay::{
    RelayConfig,
    daemon::{Relay, RelayIdentity},
};
use qos_user_identity::{IdentityPaths, RevocationList, persist};

/// Default location for the daemon config on a Fedora bootc image.
const DEFAULT_CONFIG_PATH: &str = "/etc/qos/relay.toml";

#[derive(Parser)]
#[command(name = "qos-relay", version, about = "qOS host + relay daemon")]
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
    Run,
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

fn install_logging() {
    let filter = EnvFilter::try_from_env("QOS_RELAY_LOG")
        .unwrap_or_else(|_| EnvFilter::new("qos_relay=info,warn"));
    fmt().with_env_filter(filter).with_target(true).init();
}

fn load_or_default(path: &std::path::Path) -> anyhow::Result<RelayConfig> {
    if path.exists() {
        Ok(RelayConfig::load(path).with_context(|| format!("load {}", path.display()))?)
    } else {
        Ok(RelayConfig::default())
    }
}

fn main() -> anyhow::Result<()> {
    install_logging();
    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Init => cmd_init(&cli.config),
        Cmd::Run => cmd_run(&cli.config),
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
        "qos-relay {} (config: {})",
        qos_relay::VERSION,
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
    if let Some(metrics) = qos_relay::store::load_metrics(&cfg.state_root)? {
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
    }
    Ok(())
}

fn cmd_run(path: &Path) -> anyhow::Result<()> {
    let cfg = load_or_default(path)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    rt.block_on(async move {
        let identity = load_identity(&cfg.identity_root, &cfg.state_root)?;
        let relay = Relay::start(cfg, identity).await?;
        let addr = relay.node_addr();
        tracing::info!(?addr, "relay online");
        // Best-effort sd_notify (silently no-ops outside systemd).
        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);
        // Park until Ctrl-C / SIGTERM.
        wait_for_shutdown().await?;
        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Stopping]);
        relay.shutdown().await;
        Ok::<(), anyhow::Error>(())
    })
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
    for entry in qos_relay::store::load_revocations(state_root)? {
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
