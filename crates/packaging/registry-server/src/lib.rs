//! `registry-server` — the self-hostable qOS app registry.
//!
//! The library surface exposes:
//!
//! - [`storage::Storage`] — on-disk state (entries + identity +
//!   allowlist), atomic-rename writes.
//! - [`allowlist::PublisherAllowlist`] — owner-controlled
//!   `publishers.toml`.
//! - [`serve::AppState`] + [`serve::handle`] — the HTTP handler
//!   plumbed into hyper.
//! - [`run_server`] / [`bind_and_run`] — the tokio + hyper glue
//!   that spawns the listener.
//!
//! TLS is deliberately out of scope: deployments put nginx or
//! Caddy in front of this. Every mutating endpoint is signature-
//! verified, so the transport-security layer only exists to
//! protect the round-trip of GETs and to authenticate the base
//! URL clients pin.

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

use std::net::SocketAddr;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tracing::{error, info};

pub mod allowlist;
pub mod serve;
pub mod storage;

pub use allowlist::{AllowlistError, PublisherAllowlist, PublisherRow};
pub use serve::{AppState, handle};
pub use storage::{ServerIdentity, Storage, StorageError};

/// Bind the server to `addr` and run until the listener errors or
/// the process is killed. Public so integration tests can drive
/// the same code path the CLI does.
///
/// Returns `Ok(bound_addr)` — useful when the caller passed
/// `0.0.0.0:0` for tests and needs to know the ephemeral port.
///
/// # Errors
///
/// Returns an error if `addr` can't be bound, or if reading the bound
/// local address back from the listener fails.
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

/// Same as [`bind_and_run`] but blocks the current tokio task
/// on a ctrl-c so callers embedding the server can await
/// shutdown.
///
/// # Errors
///
/// Returns an error if [`bind_and_run`] fails, or if installing the
/// ctrl-c signal handler fails.
pub async fn run_server(addr: SocketAddr, state: AppState) -> std::io::Result<()> {
    let _bound = bind_and_run(addr, state).await?;
    tokio::signal::ctrl_c().await?;
    info!("shutdown signal received — exiting");
    Ok(())
}

/// Convenience: default XDG state dir for the registry data.
/// Falls back to `~/.local/state/qos-registry`.
pub fn default_data_dir() -> std::path::PathBuf {
    if let Ok(v) = std::env::var("XDG_STATE_HOME")
        && !v.is_empty()
    {
        return std::path::PathBuf::from(v).join("qos-registry");
    }
    if let Ok(home) = std::env::var("HOME") {
        return std::path::PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("qos-registry");
    }
    std::path::PathBuf::from("/var/lib/qos-registry")
}
