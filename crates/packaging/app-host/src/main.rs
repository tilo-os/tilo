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

use std::os::fd::FromRawFd;
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
    /// Default: 4 GiB.
    #[arg(long, default_value_t = 4 * 1024 * 1024 * 1024)]
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
    let inherited_listener = systemd_listener(&socket)?;
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
        let max_store_bytes = cli.max_store_bytes;
        let _startup = tokio::task::spawn_blocking(move || {
            startup(&startup_store, max_store_bytes);
        });
        transport::serve(listener, store, catalog, cli.max_store_bytes).await
    })
}

/// Consume the one listener supplied by systemd socket activation.
///
/// A matching `LISTEN_PID` with anything other than one descriptor is an error,
/// rather than falling back to a second socket. The bound pathname is checked
/// before the descriptor becomes Tokio-owned so an unrelated inherited fd can
/// never become the privileged app-store endpoint.
#[allow(
    clippy::disallowed_methods,
    reason = "systemd socket activation: LISTEN_* must not leak to app-launch children; cleared in main right after the descriptor is taken, before the Tokio runtime or any other thread starts"
)]
fn systemd_listener(
    expected: &std::path::Path,
) -> anyhow::Result<Option<std::os::unix::net::UnixListener>> {
    const SYSTEMD_LISTEN_FD: libc::c_int = 3;

    let pid_matches = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        == Some(std::process::id());
    if !pid_matches {
        return Ok(None);
    }
    anyhow::ensure!(
        std::env::var("LISTEN_FDS").as_deref() == Ok("1"),
        "systemd must pass exactly one app-host listener"
    );
    // SAFETY: fcntl only checks whether the scalar descriptor is open.
    anyhow::ensure!(
        unsafe { libc::fcntl(SYSTEMD_LISTEN_FD, libc::F_GETFD) } != -1,
        "systemd app-host listener fd is closed"
    );
    // SAFETY: the LISTEN_PID/FDS contract transfers fd 3 exactly once here.
    let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(SYSTEMD_LISTEN_FD) };
    let actual = listener.local_addr()?;
    anyhow::ensure!(
        actual.as_pathname() == Some(expected),
        "systemd app-host listener has an unexpected path"
    );
    // Do not leak the authenticated store endpoint into app-launch children.
    anyhow::ensure!(
        unsafe { libc::fcntl(SYSTEMD_LISTEN_FD, libc::F_SETFD, libc::FD_CLOEXEC) } != -1,
        "cannot mark systemd app-host listener close-on-exec"
    );
    // SAFETY: activation metadata is consumed before the runtime starts any
    // worker threads, so no concurrent environment reader exists.
    unsafe {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    Ok(Some(listener))
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let env = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env)
        .with_target(false)
        .try_init();
}

/// Enforce the size cap. A failure is logged and retried on the next start.
fn startup(store: &PackageStore, max_store_bytes: u64) {
    if let Err(error) = store.sweep(max_store_bytes) {
        tracing::warn!(%error, "startup sweep failed");
    }
}
