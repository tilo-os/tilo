//! Cap'n Proto transport over Unix-domain stream sockets. Spawns one
//! tokio task per accepted connection; each connection runs a
//! `capnp-rpc` system that vends the [`crate::service::AppHostServer`]
//! capability as the bootstrap. Mirrors `format-host`'s pattern so the
//! two daemons share a single review point for socket handling.

use std::path::Path;
use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio::net::UnixListener;

use crate::app_host_capnp::app_host;

/// After the `QAH1` marker: a big-endian `u32` length, then the launcher's
/// JSON handoff.
const HANDOFF_FRAMING: wire_frame::Framing = wire_frame::Framing::big_endian(1024).reject_empty();
use crate::runtime_request::RuntimeRequests;
use crate::service::AppHostServer;
use package_store::{Catalog, PackageStore};

/// Bind the Unix socket. Ensures the parent directory exists, drops a
/// stale socket file from a previous run, and locks the socket to
/// user-only permissions.
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created or the
/// socket cannot be bound.
pub fn bind(socket: &Path) -> anyhow::Result<UnixListener> {
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = tilo_files::native::private::bind_socket(socket)?;
    listener.set_nonblocking(true)?;
    Ok(UnixListener::from_std(listener)?)
}

/// Run an accept loop forever. Each accepted stream gets its own
/// capnp-rpc system + service instance backed by the shared package store
/// and its `catalog`.
///
/// # Errors
///
/// Returns an error if accepting a connection off the listener fails.
pub async fn serve(
    listener: UnixListener,
    store: Arc<PackageStore>,
    catalog: Arc<Catalog>,
    max_store_bytes: u64,
) -> anyhow::Result<()> {
    // One set of installs for every app that asks for a runtime it declared.
    let requests = RuntimeRequests::new(Arc::clone(&catalog));
    loop {
        let (stream, _) = listener.accept().await?;
        let store = Arc::clone(&store);
        let catalog = Arc::clone(&catalog);
        let requests = Arc::clone(&requests);
        tokio::task::spawn_local(async move {
            if let Err(e) =
                handle_connection(stream, store, catalog, requests, max_store_bytes).await
            {
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
    store: Arc<PackageStore>,
    catalog: Arc<Catalog>,
    requests: Arc<RuntimeRequests>,
    max_store_bytes: u64,
) -> anyhow::Result<()> {
    let mut magic = [0u8; 4];
    stream.read_exact(&mut magic).await?;
    anyhow::ensure!(magic == *b"QAH1", "missing authenticated launcher handoff");
    let bytes = HANDOFF_FRAMING
        .read_async(&mut stream)
        .await?
        .ok_or(wire_frame::FrameError::Closed)?;
    let handoff: serde_json::Value = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        handoff.get("protocol").and_then(serde_json::Value::as_u64) == Some(1),
        "unsupported app-host handoff"
    );
    let app_id = handoff
        .get("app_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("caller is not authorized for app-host"))?
        .to_owned();
    if app_id != "so.tilo.store" {
        // The only thing any other app is served is the narrow request for a
        // runtime its own manifest declares; the package-management interface
        // is the Store's alone.
        let stream = stream.into_std()?;
        stream.set_nonblocking(false)?;
        return tokio::task::spawn_blocking(move || requests.serve(&app_id, stream))
            .await?
            .map_err(Into::into);
    }
    let service = AppHostServer::new(store, catalog, max_store_bytes);
    let client: app_host::Client = capnp_rpc::new_client(service);
    runtime_portal::serve_connected_io(stream, client.client)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}
