//! tilo-app-host daemon entry point.
//!
//! Loads the on-disk registry, binds the Cap'n Proto control socket,
//! and runs the accept loop. Best-effort systemd `READY=1` notify so a
//! `Type=notify` user unit can serialise dependent units behind us.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;

use app_host::{default_socket_path, transport};
use package_store::{Catalog, PackageStore, SystemStore};

#[derive(Debug, Parser)]
#[command(
    name = "tilo-app-host",
    about = "Tilo app host: the package store's privileged door, over a Cap'n Proto control socket."
)]
struct Cli {
    /// Cap'n Proto control socket path. Default:
    /// `$XDG_RUNTIME_DIR/tilo/services/app-host.sock`.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Package store root. Default: `$XDG_DATA_HOME/tilo-apps`, the store
    /// the launcher and `tilo-install` use.
    #[arg(long)]
    store: Option<PathBuf>,
    /// Soft cap on distinct payload bytes. When exceeded, unpinned packages
    /// are evicted least recently used first until the total drops below
    /// the cap. Pinned packages are never evicted. Pass `0` to disable the
    /// sweep; explicit `diskUsage` reports still work.
    /// Default: 4 GiB, the cap `tilo-install` applies too.
    #[arg(long, default_value_t = package_store::DEFAULT_MAX_STORE_BYTES)]
    max_store_bytes: u64,
}

fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let socket = cli
        .socket
        .or_else(default_socket_path)
        .ok_or_else(|| anyhow::anyhow!("XDG_RUNTIME_DIR not set: pass --socket"))?;
    let root = cli
        .store
        .or_else(package_store::layout::default_user_root)
        .ok_or_else(|| anyhow::anyhow!("HOME / XDG_DATA_HOME not set: pass --store"))?;
    // SAFETY: taken in main before the Tokio runtime or any other thread
    // starts, so nothing else touches the environment.
    let inherited_listener = unsafe { tilo_files::native::private::activated_socket(&socket)? };
    tracing::info!(
        socket = %socket.display(),
        store  = %root.display(),
        max_store_bytes = cli.max_store_bytes,
        "tilo-app-host starting"
    );

    // The store builds its on-disk layout lazily; the catalogue lists it
    // together with the image's preinstalled packages.
    let store = Arc::new(PackageStore::at(&root));
    let catalog = Arc::new(Catalog::at(&root, SystemStore::from_env()));

    // capnp-rpc + tokio LocalSet (one task per connection, single
    // event-loop Vat). Matches `format-host`'s shape.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let listener = if let Some(listener) = inherited_listener {
            listener.set_nonblocking(true)?;
            tokio::net::UnixListener::from_std(listener)?
        } else {
            transport::bind(&socket)?
        };
        // Best-effort sd_notify — silently no-op if NOTIFY_SOCKET
        // isn't in env (e.g. when run by hand).
        let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);
        // A cold sweep must not delay the first socket-activated app. Store
        // mutations are serialised by the store's lock, so accepting requests
        // meanwhile is safe.
        let startup_store = Arc::clone(&store);
        let startup_catalog = Arc::clone(&catalog);
        let max_store_bytes = cli.max_store_bytes;
        let _startup = tokio::task::spawn_blocking(move || {
            startup(&startup_store, &startup_catalog, max_store_bytes);
        });
        transport::serve(listener, store, catalog, cli.max_store_bytes).await
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

/// Enforce the size cap and reclaim what the system store no longer needs:
/// a new image or a swapped userspace changes which runtime builds the
/// shipped apps pin (the Browser's engine among them) without any install
/// running, so the builds an earlier one left are collected here. A failure
/// is logged and retried on the next start.
fn startup(store: &PackageStore, catalog: &Catalog, max_store_bytes: u64) {
    app_host::service::collect_after_change(store, catalog, max_store_bytes);
}
