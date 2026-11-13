//! `registry-server`: the self-hostable Tilo app registry. [`storage::Storage`] holds the on-disk
//! state (entries, identity, allowlist) with atomic-rename writes,
//! [`allowlist::PublisherAllowlist`] the owner-controlled `publishers.toml`, and
//! [`serve::AppState`] with [`serve::handle`] the HTTP handler that [`run_server`] and
//! [`bind_and_run`] put behind a listener. TLS is deliberately out of scope: deployments put nginx
//! or Caddy in front of this. Every mutating endpoint is signature-verified, so the transport layer
//! only protects the round-trip of GETs and authenticates the base URL clients pin.

use std::net::SocketAddr;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tracing::{error, info};

pub mod allowlist;
pub mod applications;
pub mod bot_review;
pub mod bundle_review;
mod catalogue;
mod metrics;
pub mod moderation;
pub mod moderation_api;
pub mod serve;
pub mod storage;
pub mod submission_review;

pub use allowlist::{AllowlistError, PublisherAllowlist, PublisherRow};
pub use bot_review::{BotReviewError, InspectedProgram};
pub use bundle_review::{
    BundleReviewError, InspectedVersion, InspectionPolicy, fetch_entry_bytes, fetch_version_bytes,
    inspect_bytes, inspect_entry, inspect_stored_bytes, review_entry_bytes, review_version_bytes,
};
pub use moderation::{ModerationError, ModerationService};
pub use moderation_api::{ModerationApiError, ModerationApiService};
pub use serve::{AppState, handle};
pub use storage::{
    DroppedVersion, HistoryCompaction, HistoryCompactionReport, PendingSubmission,
    PromotionJournal, PrunePreview, PruneReport, RetentionPreview, ReviewDecision, ReviewRecord,
    ReviewSubject, ServerIdentity, StagedArtifact, StagedPromotion, Storage, StorageError,
    TwinReport,
};
pub use submission_review::Inspection;

/// Bind the server to `addr` and run until the listener errors or the process is killed. Public so
/// integration tests drive the same code path the CLI does; returns the bound address, which a
/// caller that passed port 0 needs.
pub async fn bind_and_run(addr: SocketAddr, state: AppState) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    info!("registry server listening on {bound}");
    tokio::spawn(async move {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(x) => x,
                Err(e) => {
                    error!("accept: {e}");
                    return;
                }
            };
            let state = state.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = service_fn(move |req| handle(state.clone(), req));
                if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                    // A client hangup mid-request is normal; log at debug.
                    tracing::debug!("http1 connection from {peer} ended: {e}");
                }
            });
        }
    });
    Ok(bound)
}

/// [`bind_and_run`], waiting on ctrl-c so callers embedding the server can await shutdown.
pub async fn run_server(addr: SocketAddr, state: AppState) -> std::io::Result<()> {
    let _bound = bind_and_run(addr, state).await?;
    tokio::signal::ctrl_c().await?;
    info!("shutdown signal received — exiting");
    Ok(())
}

/// The default XDG state dir for the registry data, else `~/.local/state/tilo-registry`.
pub fn default_data_dir() -> std::path::PathBuf {
    tilo_paths::xdg::state_home().map_or_else(
        || std::path::PathBuf::from("/var/lib/tilo-registry"),
        |state| state.join("tilo-registry"),
    )
}
