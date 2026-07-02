//! `qos-runtime-collab` — Cap'n Proto client for the qOS
//! collaboration broker.
//!
//! ## Usage shape
//!
//! ```ignore
//! let local = tokio::task::LocalSet::new();
//! local.run_until(async {
//!     let broker = qos_runtime_collab::try_connect()
//!         .map_err(|e| { eprintln!("collab unavailable: {e}"); e })?;
//!     let channel = broker
//!         .join_channel("channel-id", identity_bytes)
//!         .await?;
//!     channel.send(op_bytes).await?;
//!     let mut sub = channel.subscribe().await?;
//!     while let Ok(next) = sub.read().await {
//!         handle(next);
//!     }
//! }).await;
//! ```
//!
//! Every call returns [`NotAvailable`] if the broker is missing or
//! the connection drops, never a panic.

use std::rc::Rc;

use qos_runtime_portal::{NotAvailable, UnixCapnpClient, service_fd};

// Generated schema lands in OUT_DIR with the filename
// `<schema>_capnp.rs`. The generated code references its own module
// path absolutely as `crate::collab_capnp`, so we MUST include it
// at the crate root with that exact module name. This is the same
// pattern capnp's own samples use.
#[allow(clippy::all)]
#[allow(unused)]
#[allow(dead_code)]
pub mod collab_capnp {
    include!(concat!(env!("OUT_DIR"), "/collab_capnp.rs"));
}

use capnp_rpc::rpc_twoparty_capnp;

pub use collab_capnp::collab_service::Client as CollabServiceClient;
pub use collab_capnp::cursor_stream::Client as CursorStreamClient;
pub use collab_capnp::stream::Client as StreamClient;

/// Try to dial the qOS collab broker via the launcher-supplied
/// service fd. Returns [`NotAvailable`] when the env var is unset
/// (most likely: app is running as a Flatpak on stock Linux) or
/// the fd is no longer open.
///
/// The returned client is the typed root capability; wrap it in a
/// [`CollabClient`] if you want the idiomatic Rust surface.
///
/// Must be called from within a `tokio::task::LocalSet`-driven
/// context — capnp-rpc futures are `!Send`.
pub fn try_connect() -> Result<CollabClient, NotAvailable> {
    let fd = service_fd("collab").ok_or(NotAvailable::ServiceFdMissing)?;
    let unix = UnixCapnpClient::from_connected_fd(fd, rpc_twoparty_capnp::Side::Client)?;
    let client: CollabServiceClient = unix.bootstrap();
    Ok(CollabClient::from_raw(client))
}

/// Idiomatic Rust wrapper over the generated [`CollabServiceClient`].
#[derive(Clone)]
pub struct CollabClient {
    inner: Rc<CollabServiceClient>,
}

impl CollabClient {
    /// Wrap an existing generated client (mainly useful for tests
    /// that own their own RPC system).
    pub fn from_raw(client: CollabServiceClient) -> Self {
        Self { inner: Rc::new(client) }
    }

    /// Join (or implicitly create) a channel by id, presenting the
    /// given opaque identity blob. `identity` is the postcard
    /// encoding of a `qos-collab-types` identity record — opaque
    /// from the broker's perspective.
    pub async fn join_channel(
        &self,
        channel_id: &str,
        identity: &[u8],
    ) -> Result<ChannelHandle, NotAvailable> {
        let mut req = self.inner.join_channel_request();
        {
            let mut builder = req.get();
            builder.set_channel_id(channel_id);
            builder.set_identity(identity);
        }
        let reply = req.send().promise.await?;
        let handle = reply.get()?.get_handle()?;
        Ok(ChannelHandle { inner: Rc::new(handle) })
    }
}

/// A subscribed-to channel. Holds a typed capability backed by the
/// broker; dropping the handle closes the subscription. Cheap to
/// clone (the underlying capability is reference-counted).
#[derive(Clone)]
pub struct ChannelHandle {
    inner: Rc<collab_capnp::channel_handle::Client>,
}

impl ChannelHandle {
    /// Submit a single signed op into the channel. The op blob is
    /// a postcard-encoded `qos-collab-types::SignedOp`.
    pub async fn send(&self, op: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.send_request();
        req.get().set_op(op);
        req.send().promise.await?;
        Ok(())
    }

    /// Open a server-pulled stream of ops. The returned
    /// [`OpStream`] yields one op per `read` call; the consumer is
    /// expected to drive it in a loop. Stream lifetime is bounded
    /// by the parent channel handle.
    pub async fn subscribe(&self) -> Result<OpStream, NotAvailable> {
        let req = self.inner.subscribe_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(OpStream { inner: Rc::new(stream) })
    }

    /// Open the channel's presence sub-handle.
    pub async fn presence(&self) -> Result<PresenceHandle, NotAvailable> {
        let req = self.inner.presence_request();
        let reply = req.send().promise.await?;
        let handle = reply.get()?.get_handle()?;
        Ok(PresenceHandle { inner: Rc::new(handle) })
    }
}

/// Client-pulled op stream. Each `.read()` is a single RPC.
#[derive(Clone)]
pub struct OpStream {
    inner: Rc<StreamClient>,
}

impl OpStream {
    /// Pull the next op. Resolves when the broker has one available.
    pub async fn read(&self) -> Result<Vec<u8>, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let op = reply.get()?.get_op()?;
        Ok(op.to_vec())
    }
}

/// Per-channel presence handle.
#[derive(Clone)]
pub struct PresenceHandle {
    inner: Rc<collab_capnp::presence_handle::Client>,
}

impl PresenceHandle {
    /// Publish a cursor position. The broker fan-outs to peers.
    pub async fn publish_cursor(&self, x: f32, y: f32) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_cursor_request();
        {
            let mut b = req.get();
            b.set_x(x);
            b.set_y(y);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Subscribe to remote-cursor updates.
    pub async fn subscribe_remote_cursors(&self) -> Result<CursorStream, NotAvailable> {
        let req = self.inner.subscribe_remote_cursors_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(CursorStream { inner: Rc::new(stream) })
    }
}

/// A single remote-cursor update.
#[derive(Debug, Clone)]
pub struct CursorUpdate {
    pub user_id: String,
    pub x: f32,
    pub y: f32,
}

/// Client-pulled remote-cursor stream.
#[derive(Clone)]
pub struct CursorStream {
    inner: Rc<CursorStreamClient>,
}

impl CursorStream {
    /// Pull the next remote-cursor update.
    pub async fn read(&self) -> Result<CursorUpdate, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let user_id = r
            .get_user_id()?
            .to_str()
            .map_err(|e| capnp::Error::failed(format!("invalid utf8 user_id: {e}")))?
            .to_owned();
        Ok(CursorUpdate {
            user_id,
            x: r.get_x(),
            y: r.get_y(),
        })
    }
}

/// Re-export the raw generated module for power users that need to
/// reach below the idiomatic wrappers (server impls, tests).
pub mod raw {
    pub use super::collab_capnp::*;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn clear_qos_env() {
        for (key, _) in env::vars() {
            if key.starts_with("QOS_") {
                unsafe { env::remove_var(&key) };
            }
        }
    }

    #[test]
    fn try_connect_without_env_returns_service_fd_missing() {
        clear_qos_env();
        // Not using `expect_err` because `CollabClient` intentionally
        // doesn't implement `Debug` (the underlying capnp client hook
        // isn't `Debug`).
        match try_connect() {
            Err(NotAvailable::ServiceFdMissing) => {}
            Err(other) => panic!("unexpected error: {other:?}"),
            Ok(_) => panic!("expected ServiceFdMissing"),
        }
    }
}
