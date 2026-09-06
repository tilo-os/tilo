//! Cap'n Proto transport over Unix-domain stream sockets. Spawns one
//! tokio task per accepted connection; each connection runs a
//! `capnp-rpc` system that vends the [`crate::service::AppHostServer`]
//! capability as the bootstrap. Mirrors `format-host`'s pattern so the
//! two daemons share a single review point for socket handling.

use std::path::Path;
use std::sync::Arc;

use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use tokio::io::AsyncReadExt;
use tokio::net::UnixListener;
use tokio_util::compat::TokioAsyncReadCompatExt;

use crate::app_host_capnp::app_host;
use crate::service::AppHostServer;
use crate::store::Store;

/// Bind the Unix socket. Ensures the parent directory exists, drops a
/// stale socket file from a previous run, and locks the socket to
/// user-only permissions.
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created or the
/// socket cannot be bound.
pub fn bind(socket: &Path) -> anyhow::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Remove a stale socket from a previous run — Unix bind() fails if
    // the path already exists.
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600));
    Ok(listener)
}

/// Run an accept loop forever. Each accepted stream gets its own
/// capnp-rpc system + service instance backed by the shared `Store`.
///
/// # Errors
///
/// Returns an error if accepting a connection off the listener fails.
pub async fn serve(
    listener: UnixListener,
    store: Arc<Store>,
    max_store_bytes: u64,
) -> anyhow::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let store = Arc::clone(&store);
        tokio::task::spawn_local(async move {
            if let Err(e) = handle_connection(stream, store, max_store_bytes).await {
                tracing::warn!(error = %e, "app-host connection terminated");
            }
        });
    }
}

/// Handle one accepted Unix stream as a capnp-rpc session.
///
/// # Errors
///
/// Returns an error if the capnp-rpc system fails (a malformed message,
/// a protocol violation, or the peer disconnecting mid-call).
// This future is driven via `tokio::task::spawn_local` in `serve` and is
// never meant to cross a thread: capnp-rpc's `RpcSystem` future is
// inherently non-`Send` (the same non-atomic `capnp::capability::Rc`
// design as `service::AppHostServer`'s methods).
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc's RpcSystem future runs on a single-threaded LocalSet and is never Send by design"
)]
pub async fn handle_connection(
    mut stream: tokio::net::UnixStream,
    store: Arc<Store>,
    max_store_bytes: u64,
) -> anyhow::Result<()> {
    let mut magic = [0u8; 4];
    stream.read_exact(&mut magic).await?;
    anyhow::ensure!(magic == *b"QAH1", "missing authenticated launcher handoff");
    let mut length = [0u8; 4];
    stream.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    anyhow::ensure!(
        length > 0 && length <= 1024,
        "invalid launcher handoff length"
    );
    let mut bytes = vec![0u8; length];
    stream.read_exact(&mut bytes).await?;
    let handoff: serde_json::Value = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        handoff.get("protocol").and_then(serde_json::Value::as_u64) == Some(1)
            && handoff.get("app_id").and_then(serde_json::Value::as_str) == Some("dev.qos.store"),
        "caller is not authorized for app-host"
    );
    let (reader, writer) = stream.into_split();
    let reader = reader.compat();
    let writer = tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(writer);
    let network = Box::new(twoparty::VatNetwork::new(
        futures::io::BufReader::new(reader),
        futures::io::BufWriter::new(writer),
        rpc_twoparty_capnp::Side::Server,
        capnp::message::ReaderOptions::default(),
    ));
    let service = AppHostServer::new(store, max_store_bytes);
    let client: app_host::Client = capnp_rpc::new_client(service);
    let rpc_system = RpcSystem::new(network, Some(client.client));
    rpc_system.await.map_err(|e| anyhow::anyhow!("{e}"))
}
