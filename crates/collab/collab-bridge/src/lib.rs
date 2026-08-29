// `Promise::from_future` uses `impl Future`, which "refines" the
// `Promise`-shaped return type the capnp-generated `Server` traits
// declare — intended in capnp-rpc 0.26, but triggers `refining_impl_trait`.
// Safe to silence: `Promise` IS what the trait expects; this is a
// compiler diagnostic, not a wire-level change.
#![allow(refining_impl_trait)]

//! `collab-bridge` — Cap'n Proto SDK front-end for the shell's legacy
//! serde-JSON `CollabRequest` / `CollabEvent` broker.
//!
//! Every qOS app links `runtime-collab`, which speaks Cap'n Proto over
//! `$XDG_RUNTIME_DIR/qos/services/collab.sock` per
//! `crates/runtime/runtime-collab/schema/collab.capnp`. This crate
//! answers that socket by translating each capnp method into one or more
//! length-prefixed serde-JSON frames sent to the shell's existing broker
//! socket (typically `$XDG_RUNTIME_DIR/qos-shell-collab.sock`).
//!
//! One SDK connection maps to one upstream broker connection; the bridge
//! holds no state between connections — a closed upstream tears down its
//! reader task and every SDK-side `.read()` unblocks with `NotAvailable`.
//!
//! Public surface: [`serve`] (daemon entry point),
//! [`service::BridgeCollabService`] (the capnp server, for tests that
//! stand up their own RPC system), and [`translate`]/[`wire`] (pure
//! translation helpers for round-tripping a wire frame in tests).

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

/// Binds the SDK-facing socket at `listen`, removing a stale inode from a prior run and applying user-only permissions (best-effort).
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

/// Daemon entry point: runs forever, accepting SDK connections and
/// spawning one capnp-rpc system per connection against a fresh upstream
/// connection. Must run on the current tokio `LocalSet` — capnp-rpc
/// futures are `!Send`.
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

/// Handles one SDK connection: stands up a capnp-rpc system whose
/// bootstrap capability is a fresh [`BridgeCollabService`] dialing
/// `upstream` once per `joinChannel`, torn down when the SDK connection closes.
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

/// Test-only shim: a [`BridgeCollabService`] whose opener returns a
/// pre-connected upstream every time, for round-trip tests using an
/// in-memory upstream.
#[doc(hidden)]
pub fn test_service_with_opener(
    opener: crate::service::UpstreamOpener,
) -> BridgeCollabService {
    BridgeCollabService::new(opener)
}

/// Builds the opener that dials `path` on demand, for tests to plug into `test_service_with_opener`.
#[doc(hidden)]
pub fn opener_for_path(path: PathBuf) -> crate::service::UpstreamOpener {
    Rc::new(move || {
        let p = path.clone();
        Box::pin(async move { connect(p).await })
    })
}
