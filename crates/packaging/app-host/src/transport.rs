//! Cap'n Proto transport over Unix-domain stream sockets. Spawns one
//! tokio task per accepted connection; each connection runs a
//! `capnp-rpc` system that vends the [`crate::service::AppHostServer`]
//! capability as the bootstrap. Mirrors `format-host`'s pattern so the
//! two daemons share a single review point for socket handling.

use std::path::Path;
use std::sync::Arc;

use tokio::net::UnixListener;

use crate::app_host_capnp::app_host;
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

/// Read the launcher's handoff and the app it stamped on the connection.
///
/// The bytes are `service_handoff::APP_HOST`'s: the `QAH1` marker, a
/// big-endian `u32` length, then the app-only envelope. The handoff names the
/// app but grants nothing by itself: the host still decides what that app may
/// ask for.
async fn read_handoff(stream: &mut tokio::net::UnixStream) -> anyhow::Result<String> {
    use service_handoff::{APP_HOST, Handoff, HandoffError};
    let refused = match APP_HOST.read_async(stream).await {
        Ok(Handoff::App { app_id }) => return Ok(app_id),
        Ok(_) | Err(HandoffError::Protocol { .. }) => "unsupported app-host handoff",
        Err(HandoffError::Missing | HandoffError::NotHandoff { .. } | HandoffError::Marker) => {
            "missing authenticated launcher handoff"
        }
        Err(error) => return Err(error.into()),
    };
    anyhow::bail!(refused)
}

/// Handle one accepted Unix stream as a capnp-rpc session.
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
    let app_id = read_handoff(&mut stream).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use service_handoff::{APP_HOST, Handoff};
    use tokio::io::AsyncWriteExt as _;

    /// The launcher's own bytes: `launcher::services` calls this same
    /// `APP_HOST` contract, so a change to either end has to pass both tests.
    fn launcher_bytes(app_id: &str) -> Vec<u8> {
        APP_HOST.encode(&Handoff::app(app_id)).unwrap()
    }

    #[tokio::test]
    async fn the_handoff_the_launcher_writes_names_the_app_it_was_opened_for() {
        let (mut launcher, mut host) = tokio::net::UnixStream::pair().unwrap();
        launcher
            .write_all(&launcher_bytes("so.tilo.store"))
            .await
            .unwrap();
        launcher.flush().await.unwrap();
        assert_eq!(read_handoff(&mut host).await.unwrap(), "so.tilo.store");
        drop(launcher);
    }

    /// A handoff split across writes is still one handoff: the launcher
    /// writes it before the fork, and the pipe may deliver it in pieces.
    #[tokio::test]
    async fn a_handoff_split_across_writes_is_read_whole() {
        let (mut launcher, mut host) = tokio::net::UnixStream::pair().unwrap();
        let bytes = APP_HOST.encode(&Handoff::app("so.tilo.browser")).unwrap();
        for byte in bytes {
            launcher.write_all(&[byte]).await.unwrap();
            launcher.flush().await.unwrap();
        }
        assert_eq!(read_handoff(&mut host).await.unwrap(), "so.tilo.browser");
        drop(launcher);
    }

    #[tokio::test]
    async fn a_missing_malformed_oversized_or_foreign_handoff_is_refused() {
        // Nothing at all: the peer connected and closed without its handoff.
        {
            let (launcher, mut host) = tokio::net::UnixStream::pair().unwrap();
            drop(launcher);
            let error = read_handoff(&mut host).await.unwrap_err();
            assert!(error.to_string().contains("missing"), "{error}");
        }
        // A marker and length, but no envelope.
        {
            let (mut launcher, mut host) = tokio::net::UnixStream::pair().unwrap();
            launcher.write_all(b"QAH1").await.unwrap();
            launcher.write_all(&64_u32.to_be_bytes()).await.unwrap();
            launcher.flush().await.unwrap();
            drop(launcher);
            assert!(read_handoff(&mut host).await.is_err());
        }
        // A length far past the contract's bound, refused before it is read.
        {
            let (mut launcher, mut host) = tokio::net::UnixStream::pair().unwrap();
            launcher.write_all(b"QAH1").await.unwrap();
            launcher.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
            launcher.flush().await.unwrap();
            drop(launcher);
            assert!(read_handoff(&mut host).await.is_err());
        }
        // Another protocol version.
        {
            let (mut launcher, mut host) = tokio::net::UnixStream::pair().unwrap();
            let envelope = br#"{"protocol":2,"app_id":"so.tilo.store"}"#;
            launcher.write_all(b"QAH1").await.unwrap();
            launcher
                .write_all(&u32::try_from(envelope.len()).unwrap().to_be_bytes())
                .await
                .unwrap();
            launcher.write_all(envelope).await.unwrap();
            launcher.flush().await.unwrap();
            drop(launcher);
            let error = read_handoff(&mut host).await.unwrap_err();
            assert!(error.to_string().contains("unsupported"), "{error}");
        }
        // Another service's handoff, and an envelope with an empty app id.
        for foreign in [
            service_handoff::COLLAB
                .encode(&Handoff::room("so.tilo.store", "room-7"))
                .unwrap(),
            {
                let mut bytes = b"QAH1".to_vec();
                let envelope = br#"{"protocol":1,"app_id":""}"#;
                bytes.extend_from_slice(&u32::try_from(envelope.len()).unwrap().to_be_bytes());
                bytes.extend_from_slice(envelope);
                bytes
            },
        ] {
            let (mut launcher, mut host) = tokio::net::UnixStream::pair().unwrap();
            launcher.write_all(&foreign).await.unwrap();
            launcher.flush().await.unwrap();
            drop(launcher);
            assert!(read_handoff(&mut host).await.is_err());
        }
    }
}
