// `Promise::from_future(...)` uses `impl Future` internally which
// silently "refines" the `Promise`-shaped return type declared in
// the capnp-generated `Server` traits. That's the intended usage
// pattern in capnp-rpc 0.26, but it generates a per-method
// `refining_impl_trait` warning we don't want in the output. This
// pin is safe: `capnp::capability::Promise` IS what the trait
// expects; the "refining" here is a compiler diagnostic, not a
// wire-level change.
#![allow(refining_impl_trait)]

//! `collab-bridge` — Cap'n Proto SDK front-end for the shell's
//! legacy serde-JSON `CollabRequest` / `CollabEvent` broker.
//!
//! The SDK (`runtime-collab`) speaks Cap'n Proto over the well-
//! known socket `$XDG_RUNTIME_DIR/qos/services/collab.sock`. Every
//! qOS app links against `runtime-collab` and expects that socket
//! to answer with the schema in
//! `crates/runtime/runtime-collab/schema/collab.capnp`. This crate
//! provides that answer by translating every capnp method onto one
//! or more length-prefixed serde-JSON frames sent to the shell's
//! existing broker at wherever the shell binds its collab socket
//! (typically `$XDG_RUNTIME_DIR/qos-shell-collab.sock`).
//!
//! One SDK connection maps to one upstream broker connection. The
//! bridge holds no long-lived state between connections — every
//! reader task terminates when its upstream closes and every SDK-
//! side `.read()` unblocks with `NotAvailable`.
//!
//! ## Public surface
//!
//! - [`serve`] — daemon entry point (bind SDK socket, accept loop,
//!   graceful shutdown).
//! - [`service::BridgeCollabService`] — the capnp `CollabService`
//!   server implementation; useful for tests that stand up their
//!   own RPC system.
//! - [`translate`] / [`wire`] — pure translation helpers, exposed
//!   so tests can round-trip a wire frame end-to-end.

pub mod service;
pub mod translate;
pub mod upstream;
pub mod wire;

use std::path::PathBuf;
use std::rc::Rc;

use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use futures::io::{BufReader, BufWriter};
use tokio::net::UnixListener;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use runtime_collab::raw::collab_service;

use crate::service::{BridgeCollabService, service_for_path};
use crate::upstream::connect;

/// Bind the SDK-facing socket at `listen`. Removes any stale
/// inode from a prior run first, then applies user-only
/// permissions. Best-effort — the caller decides whether an
/// existing socket is a fatal error.
pub fn bind(listen: &std::path::Path) -> std::io::Result<UnixListener> {
    if let Some(dir) = listen.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(listen);
    let listener = UnixListener::bind(listen)?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(listen, std::fs::Permissions::from_mode(0o600));
    Ok(listener)
}

/// Daemon entry point. Runs forever, accepting SDK connections
/// and spawning one capnp-rpc system per connection against a
/// fresh upstream Unix connection to `upstream`.
///
/// Runs on the current tokio LocalSet — capnp-rpc futures are
/// `!Send` so the caller must be inside a `LocalSet::run_until` /
/// `LocalSet::block_on`.
pub async fn serve(listener: UnixListener, upstream: PathBuf) -> anyhow::Result<()> {
    tracing::info!(
        listen = ?listener.local_addr().ok(),
        upstream = %upstream.display(),
        "collab-bridge serving"
    );
    loop {
        let (stream, _) = listener.accept().await?;
        let upstream = upstream.clone();
        tokio::task::spawn_local(async move {
            if let Err(e) = handle_connection(stream, upstream).await {
                tracing::warn!(error = %e, "collab-bridge connection terminated");
            }
        });
    }
}

/// Handle one SDK connection: stand up a capnp-rpc system whose
/// bootstrap capability is a fresh [`BridgeCollabService`] wired
/// to open its upstream against `upstream`.
///
/// Every `joinChannel` call on the SDK side dials `upstream` once;
/// the upstream reader task is torn down when the SDK connection
/// closes.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    upstream: PathBuf,
) -> anyhow::Result<()> {
    let (reader, writer) = stream.into_split();
    let reader = reader.compat();
    let writer = writer.compat_write();
    let network = Box::new(twoparty::VatNetwork::new(
        BufReader::new(reader),
        BufWriter::new(writer),
        rpc_twoparty_capnp::Side::Server,
        Default::default(),
    ));
    let service = service_for_path(upstream);
    let client: collab_service::Client = capnp_rpc::new_client(service);
    let rpc_system = RpcSystem::new(network, Some(client.client));
    rpc_system.await.map_err(|e| anyhow::anyhow!("{e}"))
}

/// Test-only shim: stand up a [`BridgeCollabService`] whose
/// opener returns a pre-connected upstream every time. Useful for
/// round-trip tests that want an in-memory upstream instead of a
/// real socket.
#[doc(hidden)]
pub fn test_service_with_opener(
    opener: crate::service::UpstreamOpener,
) -> BridgeCollabService {
    BridgeCollabService::new(opener)
}

/// Convenience: build the opener that dials `path` on demand.
/// Exposed so tests can plug it into `test_service_with_opener`.
#[doc(hidden)]
pub fn opener_for_path(path: PathBuf) -> crate::service::UpstreamOpener {
    Rc::new(move || {
        let p = path.clone();
        Box::pin(async move { connect(p).await })
    })
}
