//! `qos-collab-bridge` — daemon entry point.
//!
//! Binds the SDK-facing Cap'n Proto socket, accepts one connection per SDK
//! client, and spawns a bridge task shuttling requests/events between the
//! SDK's capnp calls and the legacy shell broker's serde-JSON dialect.
//!
//! ```text
//!  SDK (runtime-collab)      qos-collab-bridge                shell broker
//!  ─────────────────────>    ────────────────────>    ───────────────
//!   joinChannel(id, ident)    Hello { app_id: id,      registers peer
//!                              cluster_id: v5(ident) }
//!   send(op)                  SignedSyncIncremental    fan-out signed
//!   subscribe().read()   <────  SignedCRDTIncremental
//!   presence.publishCursor    Presence { Pointer }
//!   subscribeContext().read <── Context { .. }
//! ```

use std::path::PathBuf;

use clap::Parser;
use tokio::signal::unix::{SignalKind, signal};

#[derive(Debug, Parser)]
#[command(
    name = "qos-collab-bridge",
    about = "SDK Cap'n Proto <-> shell serde-JSON broker bridge"
)]
struct Cli {
    /// SDK-facing Cap'n Proto socket to bind (default `$XDG_RUNTIME_DIR/qos/services/collab.sock`).
    #[arg(long)]
    listen: Option<PathBuf>,
    /// Upstream shell-broker Unix socket path (default `$XDG_RUNTIME_DIR/qos-shell-collab.sock`).
    #[arg(long)]
    upstream: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let listen = cli.listen.unwrap_or_else(default_listen_path);
    let upstream = cli.upstream.unwrap_or_else(default_upstream_path);

    tracing::info!(
        listen = %listen.display(),
        upstream = %upstream.display(),
        "qos-collab-bridge starting"
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let listener = collab_bridge::bind(&listen)?;
        let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

        let serve = collab_bridge::serve(listener, upstream);
        tokio::select! {
            r = serve => r,
            _ = shutdown_signal() => {
                tracing::info!("qos-collab-bridge shutting down");
                let _ = std::fs::remove_file(&listen);
                Ok(())
            }
        }
    })
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let env = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env)
        .with_target(false)
        .try_init();
}

fn default_listen_path() -> PathBuf {
    runtime_dir().join("qos/services/collab.sock")
}

fn default_upstream_path() -> PathBuf {
    runtime_dir().join("qos-shell-collab.sock")
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

async fn shutdown_signal() {
    let sigint = async {
        signal(SignalKind::interrupt())
            .expect("register SIGINT")
            .recv()
            .await
    };
    let sigterm = async {
        signal(SignalKind::terminate())
            .expect("register SIGTERM")
            .recv()
            .await
    };
    tokio::select! {
        _ = sigint => {}
        _ = sigterm => {}
    }
}
