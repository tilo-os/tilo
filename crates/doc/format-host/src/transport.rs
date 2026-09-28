//! Cap'n Proto transport over Unix-domain stream sockets. Spawns one tokio
//! task per accepted connection; each connection runs a capnp-rpc system
//! that vends the [`FormatRouterService`] capability as the bootstrap.

use std::path::{Path, PathBuf};

use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use tokio::net::UnixListener;
use tokio_util::compat::TokioAsyncReadCompatExt;

use crate::format_router_capnp::format_router;
use crate::router::Router;
use crate::service::FormatRouterService;

pub fn default_socket_path() -> PathBuf {
    let base = xdg_paths::xdg::runtime_dir().unwrap_or_else(std::env::temp_dir);
    base.join("tilo")
        .join("services")
        .join("format-router.sock")
}

/// Bind the Unix socket, accept connections, and serve capnp RPC. Returns the
/// listener so the caller can spawn its own accept loop (so tests can
/// shut it down deterministically).
///
/// # Errors
///
/// Returns an error if creating the socket directory, binding to the socket,
/// or setting socket permissions fails.
pub fn bind(socket: &Path) -> anyhow::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Remove a stale socket from a previous run — Unix bind() fails if the
    // path already exists.
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600));
    Ok(listener)
}

/// Run an accept loop forever. Each accepted stream gets its own capnp-rpc
/// system + service instance backed by the shared [`Router`].
///
/// # Errors
///
/// Returns an error if accepting a connection fails.
pub async fn serve(listener: UnixListener, router: Router) -> anyhow::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let router = router.clone();
        tokio::task::spawn_local(async move {
            if let Err(e) = handle_connection(stream, router).await {
                tracing::warn!(error = %e, "connection terminated");
            }
        });
    }
}

/// Handle one accepted Unix stream as a capnp-rpc session.
///
/// # Errors
///
/// Returns an error if the RPC system encounters an error.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc's RpcSystem is !Send by design (single-threaded object model); this \
              future is only ever driven via tokio::task::spawn_local on a LocalSet, never sent \
              across threads"
)]
pub async fn handle_connection(
    stream: tokio::net::UnixStream,
    router: Router,
) -> anyhow::Result<()> {
    let (reader, writer) = stream.into_split();
    let reader = reader.compat();
    let writer = tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(writer);
    let network = Box::new(twoparty::VatNetwork::new(
        futures::io::BufReader::new(reader),
        futures::io::BufWriter::new(writer),
        rpc_twoparty_capnp::Side::Server,
        capnp::message::ReaderOptions::default(),
    ));
    let service = FormatRouterService::new(router);
    let client: format_router::Client = capnp_rpc::new_client(service);
    let rpc_system = RpcSystem::new(network, Some(client.client));
    rpc_system.await.map_err(|e| anyhow::anyhow!("{e}"))
}
