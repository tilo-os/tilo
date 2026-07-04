//! `qos-relay` daemon entrypoint.
//!
//! Subcommands:
//! - `init`   — write a default config to `--config` (or
//!              `/etc/qos/relay.toml`) and exit.
//! - `run`    — long-running daemon (systemd `Type=notify`).
//! - `status` — print a snapshot of the running daemon's counters
//!              by reading the on-disk metrics dump.
//! - `bind-workspace <workspace_id> [--pin]` — add a workspace to
//!              the config file so the next `run` picks it up.

use std::path::PathBuf;
use std::time::Duration;

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

fn load_or_default(path: &PathBuf) -> anyhow::Result<RelayConfig> {
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

fn cmd_init(path: &PathBuf) -> anyhow::Result<()> {
    if path.exists() {
        println!("config already exists: {}", path.display());
        return Ok(());
    }
    let cfg = RelayConfig::default();
    cfg.save(path).with_context(|| format!("save {}", path.display()))?;
    println!("wrote default config: {}", path.display());
    Ok(())
}

fn cmd_bind(path: &PathBuf, id: u64, pin: bool) -> anyhow::Result<()> {
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

fn cmd_unbind(path: &PathBuf, id: u64) -> anyhow::Result<()> {
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

fn cmd_status(path: &PathBuf) -> anyhow::Result<()> {
    let cfg = load_or_default(path)?;
    println!("qos-relay {} (config: {})", qos_relay::VERSION, path.display());
    println!("identity_root: {}", cfg.identity_root.display());
    println!("state_root:    {}", cfg.state_root.display());
    println!("iroh_listen:   {}", cfg.iroh_listen);
    println!("workspaces bound:");
    for ws in &cfg.workspaces {
        println!("  - {} (pin={})", ws.id, ws.pin);
    }
    Ok(())
}

fn cmd_run(path: &PathBuf) -> anyhow::Result<()> {
    let cfg = load_or_default(path)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    rt.block_on(async move {
        let identity = load_identity(&cfg.identity_root)?;
        let relay = Relay::start(cfg, identity).await?;
        let addr = relay.node_addr();
        tracing::info!(?addr, "relay online");
        // Best-effort sd_notify (silently no-ops outside systemd).
        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);
        // Park until Ctrl-C / SIGTERM.
        wait_for_shutdown().await;
        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Stopping]);
        drop(relay);
        Ok::<(), anyhow::Error>(())
    })
}

fn load_identity(root: &std::path::Path) -> anyhow::Result<RelayIdentity> {
    let paths = IdentityPaths::under(root);
    let user_public_key = persist::read_public_key(&paths.user_pubkey)
        .with_context(|| format!("read user pubkey {}", paths.user_pubkey.display()))?;
    let device_signing_key = persist::read_signing_key(&paths.device_privkey)
        .with_context(|| format!("read device secret {}", paths.device_privkey.display()))?;
    let device_cert = persist::read_device_cert(&paths.device_cert)
        .with_context(|| format!("read device cert {}", paths.device_cert.display()))?;
    Ok(RelayIdentity {
        user_public_key,
        device_signing_key,
        device_cert,
        revocations: RevocationList::new(),
    })
}

async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("signal");
        let mut intr = signal(SignalKind::interrupt()).expect("signal");
        tokio::select! {
            _ = term.recv() => tracing::info!("SIGTERM"),
            _ = intr.recv() => tracing::info!("SIGINT"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    // Give in-flight tasks a tick to close.
    tokio::time::sleep(Duration::from_millis(100)).await;
}
