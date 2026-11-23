//! `tilo-server` daemon entrypoint.
//!
//! Subcommands:
//! - `init` — write a default config to `--config` (or
//!   `/etc/tilo/server.toml`) and exit.
//! - `run` — long-running daemon (systemd `Type=notify`); with feature
//!   `web`, `--web <addr> --allowed-origin <origin> --tls dev|files:<chain>,<key>`
//!   also serves browsers (HTTPS).
//! - `dev` (feature `web`) — a throwaway local host for browser development:
//!   its own user root, the browser listener, and an approver that
//!   certifies every browser that asks.
//! - `workspace` (feature `web`) — a short-lived shared workspace: like `dev`, but it
//!   certifies only the browsers whose pairing codes its operator admits
//!   (`admit <code>` on stdin) and announces each on stdout.
//! - `--workspace-documents` on `run` and `dev` (feature `agents`) — the workspaces'
//!   records and roster agents on the host, each agent working on the model its workspace linked.
//! - `status` — print a snapshot of the running daemon's counters
//!   by reading the on-disk metrics dump.

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

use tilo_device_identity::{IdentityPaths, persist};
use tilo_server::{ServerConfig, daemon::ServerIdentity};
use tilo_user_identity::RevocationList;

// The tests in this binary open records, which an app worker serves: libtest
// owns `main`, so the worker is one more test here.
#[cfg(all(test, feature = "agents"))]
tilo_agent_session::app_worker!();

#[cfg(feature = "agents")]
mod persistent_moderation;
#[cfg(feature = "agents")]
mod persistent_workspace_cli;
#[cfg(feature = "web")]
mod web_cli;
#[cfg(feature = "agents")]
mod workspace_chat;
#[cfg(feature = "agents")]
mod workspace_discussions;
#[cfg(feature = "agents")]
mod workspace_strips;

/// Default location for the daemon config on a Fedora bootc image.
const DEFAULT_CONFIG_PATH: &str = "/etc/tilo/server.toml";

/// How long a piece of work, or the whole runtime, may go without an answer before the
/// server says what it was doing: shorter than anything a person calls a wait.
const STALL: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Parser)]
#[command(name = "tilo-server", version, about = "Tilo host + server daemon")]
struct Cli {
    /// Path to the server TOML config.
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
    /// Run the server daemon (long-running).
    Run {
        #[cfg(feature = "web")]
        #[command(flatten)]
        web: Box<web_cli::WebArgs>,
    },
    /// A short-lived shared workspace: its own user root and browser listener,
    /// certifying only the browsers its operator admits on stdin.
    #[cfg(feature = "web")]
    Workspace(Box<web_cli::WorkspaceArgs>),
}

/// Logs go to stderr: a persistent workspace's stdout is the line protocol its
/// workspace service reads, which dropped every log line, so a hosted server's
/// refusals and errors were never seen anywhere.
fn install_logging() {
    // The channel service says at info what an operator acts on once: state
    // an earlier build saved upgraded in place, or a copied machine joining
    // as itself; so does a workspace's governance brought up from an earlier
    // release's, with each grant it holds no grant for and why. The agents this server runs
    // say at info what they did, as the rest of the server does.
    let filter = EnvFilter::try_from_env("TILO_SERVER_LOG").unwrap_or_else(|_| {
        EnvFilter::new(
            "tilo_server=info,tilo_workspace_service::agents=info,tilo_workspace_keys=info,\
             tilo_peer_state::state::rebase=info,warn",
        )
    });
    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .init();
}

fn load_or_default(path: &std::path::Path) -> anyhow::Result<ServerConfig> {
    if path.exists() {
        Ok(ServerConfig::load(path).with_context(|| format!("load {}", path.display()))?)
    } else {
        Ok(ServerConfig::default())
    }
}

fn main() -> anyhow::Result<std::process::ExitCode> {
    // Automerge isolation workers re-enter this binary; serve them first.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return Ok(std::process::ExitCode::from(code));
    }
    // So do the confined workers agents and workspace actions run apps in.
    #[cfg(feature = "agents")]
    if let Some(code) = tilo_agent_session::worker::dispatch_early() {
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
        Cmd::Run { web } => cmd_run(&cli.config, *web),
        #[cfg(not(feature = "web"))]
        Cmd::Run {} => cmd_run(&cli.config),
        #[cfg(feature = "web")]
        Cmd::Workspace(args) => tilo_rt::daemon("tilo-server")
            .run(watched(web_cli::workspace(*args, wait_for_shutdown())))?,
    }
}

/// `work` as this process's server, watched: what the server is doing is named where it
/// does it ([`tilo_rt::stall::doing`]), and a piece of work or a whole runtime that goes
/// [`STALL`] without an answer logs it, with every thread's stack, rather than leaving a
/// person waiting with nothing said.
async fn watched<F: std::future::Future>(work: F) -> F::Output {
    tilo_rt::stall::watch(STALL);
    tilo_rt::stall::heartbeat(&tokio::runtime::Handle::current());
    work.await
}

fn cmd_init(path: &std::path::Path) -> anyhow::Result<()> {
    if path.exists() {
        println!("config already exists: {}", path.display());
        return Ok(());
    }
    let cfg = ServerConfig::default();
    cfg.save(path)
        .with_context(|| format!("save {}", path.display()))?;
    println!("wrote default config: {}", path.display());
    Ok(())
}

fn cmd_run(path: &Path, #[cfg(feature = "web")] web: web_cli::WebArgs) -> anyhow::Result<()> {
    let cfg = load_or_default(path)?;
    // One server per state, and nothing else changing it while it runs
    // (`web_cli::start_server` takes it in a build with `web`).
    #[cfg(not(feature = "web"))]
    let _lock =
        tilo_server::state_lock::StateLock::acquire(&cfg.state_root).map_err(anyhow::Error::msg)?;
    tilo_rt::daemon("tilo-server").run(watched(async move {
        // Every change the server receives is decided against its app's
        // surface: the one it runs the app with, or one the workspace keeps
        // beside it.
        tilo_server::surfaces::decide_receipt(&cfg.state_root.join("surfaces"));
        #[cfg(feature = "web")]
        web_cli::await_certificate(&cfg.identity_root, &web, &cfg.state_root).await?;
        let identity = load_identity(&cfg.identity_root, &cfg.state_root)?;
        #[cfg(feature = "web")]
        let state_root = cfg.state_root.clone();
        #[cfg(feature = "web")]
        let hosted = web_cli::start_server(cfg, identity, &web).await?;
        #[cfg(feature = "web")]
        let server = &hosted.server;
        #[cfg(not(feature = "web"))]
        let server = tilo_server::daemon::Server::start(cfg, identity).await?;
        #[cfg(feature = "web")]
        let _web = web_cli::serve(server, &web, &state_root).await?;
        #[cfg(feature = "agents")]
        let public_moderation = persistent_moderation::follow_published(server)?;
        let addr = server.node_addr();
        tracing::info!(?addr, "server online");
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
        server.shutdown().await;
        Ok::<(), anyhow::Error>(())
    }))?
}

fn load_identity(
    root: &std::path::Path,
    state_root: &std::path::Path,
) -> anyhow::Result<ServerIdentity> {
    let paths = IdentityPaths::under(root);
    let device_signing_key = persist::read_device_key(&paths.device_privkey)
        .with_context(|| format!("read device secret {}", paths.device_privkey.display()))?;
    // A volume that joined with a ticket keeps the certificate the ticket's login answered with,
    // and the person it names, with the login; a volume that founded its own identity keeps them
    // in the identity root.
    let joined = tilo_device_identity::login::Place::here(IdentityPaths::under(root))
        .and_then(|place| tilo_device_identity::login::load(&place))
        .ok()
        .flatten()
        .filter(|(key, _)| key.verifying_key() == device_signing_key.verifying_key());
    let (user_public_key, device_cert) = match joined {
        Some((_, device)) => (device.cert.user_public_key, device.cert),
        None => (
            persist::read_public_key(&paths.user_pubkey)
                .with_context(|| format!("read user pubkey {}", paths.user_pubkey.display()))?,
            persist::read_device_cert(&paths.device_cert)
                .with_context(|| format!("read device cert {}", paths.device_cert.display()))?,
        ),
    };
    tilo_server::daemon::with_persisted_revocations(
        ServerIdentity {
            user_public_key,
            device_signing_key,
            device_cert,
            revocations: RevocationList::new(),
        },
        state_root,
    )
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

// Workspaces the binary's own tests govern keep their documents in workers this
// test executable serves, as `main` does through `dispatch_early`.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();
