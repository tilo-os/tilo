//! The package service's socket: one tokio task per accepted connection, each serving
//! [`crate::service::PackageServiceServer`] over [`crate::wire`].

use std::path::Path;
use std::sync::Arc;

use tokio::net::UnixListener;

use crate::language_pack::LanguagePackInstalls;
use crate::service::PackageServiceServer;
use crate::wire::read_handoff_async;
use package_store::{Catalog, PackageStore};

/// Bind the Unix socket, creating its directory and locking it to user-only permissions.
pub fn bind(socket: &Path) -> anyhow::Result<UnixListener> {
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = tilo_files::native::private::bind_socket(socket)?;
    listener.set_nonblocking(true)?;
    Ok(UnixListener::from_std(listener)?)
}

/// Run the accept loop. Each accepted stream gets its own service instance over the shared package
/// store, catalogue and language packs.
pub async fn serve(
    listener: UnixListener,
    store: Arc<PackageStore>,
    catalog: Arc<Catalog>,
    max_store_bytes: u64,
    language_packs: Arc<LanguagePackInstalls>,
) -> anyhow::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let store = Arc::clone(&store);
        let catalog = Arc::clone(&catalog);
        let language_packs = Arc::clone(&language_packs);
        tokio::task::spawn_local(async move {
            if let Err(e) =
                handle_connection(stream, store, catalog, max_store_bytes, language_packs).await
            {
                tracing::warn!(error = %e, "package service connection terminated");
            }
        });
    }
}

// Driven via `tokio::task::spawn_local` in `serve`; its calls share the
// connection's `Rc` service and never cross a thread.
#[allow(
    clippy::future_not_send,
    reason = "the connection's calls are answered on a single-threaded LocalSet"
)]
pub async fn handle_connection(
    mut stream: tokio::net::UnixStream,
    store: Arc<PackageStore>,
    catalog: Arc<Catalog>,
    max_store_bytes: u64,
    language_packs: Arc<LanguagePackInstalls>,
) -> anyhow::Result<()> {
    let handoff = read_handoff_async(&mut stream).await?;
    let service = std::rc::Rc::new(PackageServiceServer::new(
        store,
        catalog,
        max_store_bytes,
        handoff.app_id,
        language_packs,
    ));
    let (reader, writer) = stream.into_split();
    crate::wire::serve(service, reader, writer)
        .await
        .map_err(Into::into)
}
