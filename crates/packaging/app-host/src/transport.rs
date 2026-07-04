//! Cap'n Proto transport over Unix-domain stream sockets. Spawns one
//! tokio task per accepted connection; each connection runs a
//! `capnp-rpc` system that vends the [`crate::service::AppHostServer`]
//! capability as the bootstrap. Mirrors `format-host`'s pattern so the
//! two daemons share a single review point for socket handling.

use std::path::Path;
use std::sync::Arc;

use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use tokio::net::UnixListener;
use tokio_util::compat::TokioAsyncReadCompatExt;

use crate::app_host_capnp::app_host;
use crate::install::TrustPolicy;
use crate::service::AppHostServer;
use crate::store::Store;

/// Bind the Unix socket. Ensures the parent directory exists, drops a
/// stale socket file from a previous run, and locks the socket to
/// user-only permissions.
pub fn bind(socket: &Path) -> anyhow::Result<UnixListener> {
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Remove a stale socket from a previous run — Unix bind() fails if
    // the path already exists.
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600));
    Ok(listener)
}

/// Run an accept loop forever. Each accepted stream gets its own
/// capnp-rpc system + service instance backed by the shared `Store`
/// and the shared publisher-trust policy.
pub async fn serve(
    listener: UnixListener,
    store: Arc<Store>,
    max_store_bytes: u64,
    trust_policy: Arc<TrustPolicy>,
) -> anyhow::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let store = Arc::clone(&store);
        let trust = Arc::clone(&trust_policy);
        tokio::task::spawn_local(async move {
            if let Err(e) = handle_connection(stream, store, max_store_bytes, trust).await {
                tracing::warn!(error = %e, "app-host connection terminated");
            }
        });
    }
}

/// Handle one accepted Unix stream as a capnp-rpc session.
pub async fn handle_connection(
    stream: tokio::net::UnixStream,
    store: Arc<Store>,
    max_store_bytes: u64,
    trust_policy: Arc<TrustPolicy>,
) -> anyhow::Result<()> {
    let (reader, writer) = stream.into_split();
    let reader = reader.compat();
    let writer = tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(writer);
    let network = Box::new(twoparty::VatNetwork::new(
        futures::io::BufReader::new(reader),
        futures::io::BufWriter::new(writer),
        rpc_twoparty_capnp::Side::Server,
        Default::default(),
    ));
    let service = AppHostServer::new(store, max_store_bytes, trust_policy);
    let client: app_host::Client = capnp_rpc::new_client(service);
    let rpc_system = RpcSystem::new(network, Some(client.client));
    rpc_system.await.map_err(|e| anyhow::anyhow!("{e}"))
}
