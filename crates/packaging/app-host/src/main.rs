//! tilo-app-host daemon entry point.
//!
//! Loads the on-disk registry, binds the Cap'n Proto control socket,
//! and runs the accept loop. Best-effort systemd `READY=1` notify so a
//! `Type=notify` user unit can serialise dependent units behind us.

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

use std::os::fd::FromRawFd;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;

use app_host::service::SystemCatalog;
use app_host::{default_socket_path, default_store_root, gc, store::Store, transport};

#[derive(Debug, Parser)]
#[command(
    name = "tilo-app-host",
    about = "Tilo app host: content-addressed package store + Cap'n Proto control socket."
)]
struct Cli {
    /// Cap'n Proto control socket path. Default:
    /// `$XDG_RUNTIME_DIR/tilo/services/app-host.sock`.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Store root directory. Default: `~/.local/share/tilo-apps`.
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
    let inherited_listener = systemd_listener(&socket)?;
    tracing::info!(
        socket = %socket.display(),
        store  = %root.display(),
        max_store_bytes = cli.max_store_bytes,
        "tilo-app-host starting"
    );

    // Open / create the store + registry; persistent state lives
    // under `root`. The store builds its on-disk layout lazily.
    let store = Arc::new(Store::open(&root)?);
    let catalog = Arc::new(SystemCatalog::from_env());

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
        // A cold store walk must not delay the first socket-activated app.
        // Store mutations are internally serialized, and the sweep already
        // treats a concurrent pin/remove as a benign race. Install operations
        // also run a post-write sweep, so accepting requests immediately does
        // not weaken the configured size cap.
        let gc_store = Arc::clone(&store);
        let max_store_bytes = cli.max_store_bytes;
        let _gc = tokio::task::spawn_blocking(move || {
            if let Err(error) = gc::sweep(&gc_store, max_store_bytes) {
                tracing::warn!(%error, "startup gc sweep failed");
            }
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
