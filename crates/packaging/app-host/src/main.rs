//! qos-app-host daemon entry point.
//!
//! Loads the on-disk registry, binds the Cap'n Proto control socket,
//! and runs the accept loop. Best-effort systemd `READY=1` notify so a
//! `Type=notify` user unit can serialise dependent units behind us.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
    )
)]

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;

use app_host::{default_socket_path, default_store_root, gc, store::Store, transport};

#[derive(Debug, Parser)]
#[command(
    name = "qos-app-host",
    about = "qOS app host: content-addressed package store + Cap'n Proto control socket."
)]
struct Cli {
    /// Cap'n Proto control socket path. Default:
    /// `$XDG_RUNTIME_DIR/qos/services/app-host.sock`.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Store root directory. Default: `~/.local/share/qos-apps`.
    #[arg(long)]
    store: Option<PathBuf>,
    /// Soft cap on total disk usage. When exceeded, GC walks
    /// LRU-Tried entries until the total drops below the cap.
    /// `Pinned` entries are never evicted. Pass `0` to disable the
    /// background GC; explicit `diskUsage` reports still work.
    /// Default: 4 GiB.
    #[arg(long, default_value_t = 4 * 1024 * 1024 * 1024)]
    max_store_bytes: u64,
}

fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let socket = cli.socket.unwrap_or_else(default_socket_path);
    let root = cli.store.unwrap_or_else(default_store_root);
    tracing::info!(
        socket = %socket.display(),
        store  = %root.display(),
        max_store_bytes = cli.max_store_bytes,
        "qos-app-host starting"
    );

    // Open / create the store + registry; persistent state lives
    // under `root`. The store builds its on-disk layout lazily.
    let store = Arc::new(Store::open(&root)?);

    // One-shot GC pass on boot — cleans up leftover pressure from a
    // prior run before we start accepting new installs.
    if let Err(e) = gc::sweep(&store, cli.max_store_bytes) {
        tracing::warn!(error = %e, "startup gc sweep failed");
    }

    // capnp-rpc + tokio LocalSet (one task per connection, single
    // event-loop Vat). Matches `format-host`'s shape.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let listener = transport::bind(&socket)?;
        // Best-effort sd_notify — silently no-op if NOTIFY_SOCKET
        // isn't in env (e.g. when run by hand).
        let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);
        transport::serve(listener, store, cli.max_store_bytes).await
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
