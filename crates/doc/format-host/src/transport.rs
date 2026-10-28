//! Cap'n Proto transport over Unix-domain stream sockets. Spawns one tokio
//! task per accepted connection; each connection runs a capnp-rpc system
//! that vends the [`FormatRouterService`] capability as the bootstrap.

use std::path::{Path, PathBuf};

use tokio::net::UnixListener;

use crate::format_router_capnp::format_router;
use crate::router::Router;
use crate::service::FormatRouterService;

pub fn default_socket_path() -> Option<PathBuf> {
    tilo_paths::xdg::runtime_dir().map(|base| {
        base.join("tilo")
            .join("services")
            .join("format-router.sock")
    })
}

/// Bind the Unix socket, accept connections, and serve capnp RPC. Returns the
/// listener so the caller can spawn its own accept loop (so tests can
/// shut it down deterministically).
pub fn bind(socket: &Path) -> anyhow::Result<UnixListener> {
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = tilo_files::native::private::bind_socket(socket)?;
    listener.set_nonblocking(true)?;
    Ok(UnixListener::from_std(listener)?)
}

/// Run an accept loop forever. Each accepted stream gets its own capnp-rpc
/// system + service instance backed by the shared [`Router`].
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
    let client: format_router::Client = capnp_rpc::new_client(FormatRouterService::new(router));
    runtime_portal::serve_connected_io(stream, client.client)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}
