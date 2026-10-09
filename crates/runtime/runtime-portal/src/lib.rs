//! `runtime-portal` — service discovery and connected transport → RPC plumbing for
//! Tilo apps.
//!
//! Tilo apps run inside a launcher-managed sandbox. The launcher opens a Unix
//! socket per system service (collab, fs, ...) and passes the connected fd
//! to the child via `TILO_SVC_<NAME>_FD`. This crate provides:
//!
//! - [`service_fd`] — pull the raw fd out of the environment.
//! - [`is_tilo_native`] — heuristic: running under a Tilo compositor, or as a
//!   stock Flatpak on a vanilla desktop?
//! - [`CapnpClient`] — lift an owned connected transport into a tokio Cap'n Proto
//!   two-party RPC client; higher-level service crates build typed clients
//!   on top of the bootstrap returned here.
//! - [`NotAvailable`] — the single error type service clients surface when
//!   the platform can't provide the requested service.
//! - [`tools`] — typed requests asking the shell to open a tool, such as
//!   Terminal in a directory.
//!
//! Tilo apps must also run as Flatpaks on stock Linux, where there is no
//! service connection: every client returns
//! [`NotAvailable::ServiceFdMissing`] rather than panicking, and the app is
//! expected to fall back to a local-only mode.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod agent_transport;
pub mod local_socket;
pub mod process_binding;
pub mod service_socket;
pub mod tools;

use std::env;
use std::io;

#[cfg(unix)]
use std::os::fd::BorrowedFd;
#[cfg(unix)]
pub use std::os::fd::RawFd;
#[cfg(unix)]
use std::os::fd::{FromRawFd, OwnedFd};
/// Placeholder descriptor type on hosts that cannot receive Tilo Unix-socket
/// service descriptors. Service discovery always returns `None` there.
#[cfg(not(unix))]
pub type RawFd = i32;

#[cfg(feature = "rpc")]
use capnp::capability::Promise;
#[cfg(feature = "rpc")]
use capnp_rpc::twoparty;
#[cfg(feature = "rpc")]
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp};
#[cfg(unix)]
use rustix::io::fcntl_getfd;
#[cfg(all(unix, feature = "rpc"))]
use rustix::io::{FdFlags, fcntl_setfd};
#[cfg(all(unix, feature = "rpc"))]
use tokio::net::UnixStream;
#[cfg(feature = "rpc")]
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// All ways a system service can be unavailable to an app.
#[derive(Debug, thiserror::Error)]
pub enum NotAvailable {
    /// The launcher did not pass an fd for this service: either we're outside
    /// Tilo (stock Linux / Flatpak) or the service wasn't declared in the manifest.
    #[error("service fd env var missing or fd is not open")]
    ServiceFdMissing,
    /// The fd was present but the socket closed or the peer hung up before RPC could start.
    #[error("service connection failed: {0}")]
    ConnectionFailed(#[from] io::Error),
    /// The service responded but its protocol version doesn't match this client's.
    #[error("protocol mismatch: expected {expected}, got {got}")]
    ProtocolMismatch { expected: u32, got: u32 },
    /// Capability error talking to the service (rejected request, malformed reply).
    #[cfg(feature = "rpc")]
    #[error("capnp error: {0}")]
    Capnp(#[from] capnp::Error),
}

/// True iff we appear to be running inside a Tilo session. Heuristic only —
/// apps must NOT use this as an authorisation gate; try the relevant
/// service client and react to [`NotAvailable`] instead. Intended for
/// telemetry, boot-time toggles, and friendly first-run messaging.
pub fn is_tilo_native() -> bool {
    native::connection_metadata_is_current() && env::var_os("TILO_WORKSPACE_ID").is_some()
}

/// Look up a service fd by canonical name (e.g. "collab", "fs"). `None` if
/// the env var is unset, not a valid integer, or the fd isn't currently open.
pub fn service_fd(name: &str) -> Option<RawFd> {
    // Automatic native transfers belong to one exec process. Its later child
    // may reuse these descriptor numbers for unrelated files or sockets, even
    // in diagnostic/local modes that intentionally skip automatic bootstrap.
    if (name.eq_ignore_ascii_case("collab")
        || name.eq_ignore_ascii_case("identity")
        || name.eq_ignore_ascii_case("blobs"))
        && !native::connection_metadata_is_current()
    {
        return None;
    }
    #[cfg(not(unix))]
    {
        let _ = name;
        None
    }
    #[cfg(unix)]
    {
        let var_name = format!("TILO_SVC_{}_FD", name.to_uppercase());
        let raw = env::var(&var_name).ok()?;
        let fd: RawFd = raw.parse().ok()?;
        if fd < 0 {
            return None;
        }
        // F_GETFD only needs to succeed; EBADF means the fd slot is empty.
        // SAFETY: we don't mutate the fd; if the slot is closed, fcntl
        // returns EBADF and we return None. We hand the BorrowedFd
        // immediately back to fcntl and discard it.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        fcntl_getfd(borrowed).ok()?;
        Some(fd)
    }
}

/// Take ownership of the inherited service descriptor that [`service_fd`]
/// finds for `name`.
///
/// # Safety
///
/// The caller becomes the descriptor's sole owner: nothing else in the
/// process may use, close or take the same service descriptor afterwards.
#[cfg(unix)]
#[must_use]
pub unsafe fn take_service_fd(name: &str) -> Option<OwnedFd> {
    // SAFETY: `service_fd` verified the slot is open; the caller guarantees
    // exclusive ownership.
    service_fd(name).map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(feature = "rpc")]
/// A tokio Cap'n Proto two-party RPC client wrapped around an owned,
/// already-connected transport. Typed service crates call
/// [`Self::bootstrap`] for the root capability; this owns the driver task.
pub struct CapnpClient {
    rpc_system: RpcSystem<rpc_twoparty_capnp::Side>,
    bootstrap_side: rpc_twoparty_capnp::Side,
}

#[cfg(feature = "rpc")]
/// Compatibility name for clients using launcher-provided Unix descriptors.
pub type UnixCapnpClient = CapnpClient;

#[cfg(feature = "rpc")]
impl CapnpClient {
    /// Take ownership of a connected asynchronous byte stream. Platform launch
    /// adapters supply the connection; this constructor does not discover a
    /// service, authenticate either peer, or grant app/workspace authority.
    ///
    /// Uses the same protocol and local driver as the Unix descriptor adapter.
    /// Windows adapters can supply an owned named pipe without casting handles
    /// to Unix descriptors. Call [`Self::bootstrap`] inside a tokio `LocalSet`.
    pub fn from_connected_io<S>(stream: S, side: rpc_twoparty_capnp::Side) -> Self
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + 'static,
    {
        let (read_half, write_half) = tokio::io::split(stream);
        let network = Box::new(twoparty::VatNetwork::new(
            read_half.compat(),
            write_half.compat_write(),
            side,
            capnp::message::ReaderOptions::default(),
        ));
        Self {
            rpc_system: RpcSystem::new(network, None),
            bootstrap_side: match side {
                rpc_twoparty_capnp::Side::Client => rpc_twoparty_capnp::Side::Server,
                rpc_twoparty_capnp::Side::Server => rpc_twoparty_capnp::Side::Client,
            },
        }
    }

    /// Wrap an owned `RawFd` for a connected Unix stream socket; takes
    /// ownership, so the caller must not close it afterwards. A descriptor
    /// named by `TILO_SVC_<NAME>_FD` is shared by the whole process and is
    /// never owned by one client: open those with [`connect_service`]. `side` is which
    /// end of the two-party protocol we are — SDK clients are `Client`, services are `Server`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::ConnectionFailed`] if the fd cannot be put in
    /// non-blocking mode or handed to tokio's reactor.
    pub fn from_connected_fd(
        fd: RawFd,
        side: rpc_twoparty_capnp::Side,
    ) -> Result<Self, NotAvailable> {
        #[cfg(not(unix))]
        {
            let _ = (fd, side);
            Err(NotAvailable::ServiceFdMissing)
        }
        #[cfg(unix)]
        {
            // Hands exclusive ownership to tokio.
            // SAFETY: the launcher guarantees the fd is a connected
            // SOCK_STREAM Unix socket. We checked F_GETFD on it before
            // this call (via service_fd) so the slot is at least open.
            let owned = unsafe { OwnedFd::from_raw_fd(fd) };
            // The launcher clears CLOEXEC only for the initial app exec. Once
            // the SDK owns the connection, later children must not retain it.
            fcntl_setfd(&owned, FdFlags::CLOEXEC)
                .map_err(io::Error::from)
                .map_err(NotAvailable::ConnectionFailed)?;
            let std_stream = std::os::unix::net::UnixStream::from(owned);
            std_stream
                .set_nonblocking(true)
                .map_err(NotAvailable::ConnectionFailed)?;
            let stream =
                UnixStream::from_std(std_stream).map_err(NotAvailable::ConnectionFailed)?;
            Ok(Self::from_connected_io(stream, side))
        }
    }

    /// Take the remote bootstrap capability as a typed client, spawning the RPC
    /// driver task on the current tokio `LocalSet` (capnp-rpc needs `!Send`
    /// futures, so callers must run this from a `LocalSet`-aware context).
    /// Service availability must be established by a service call: capability
    /// resolution alone is not a health or admission check.
    pub fn bootstrap<C>(mut self) -> C
    where
        C: capnp::capability::FromClientHook,
    {
        let client: C = self.rpc_system.bootstrap(self.bootstrap_side);
        // capnp-rpc futures are !Send, so the driver is spawned via `spawn_local`.
        tokio::task::spawn_local(async move {
            // Resolves when the connection closes; higher layers observe a
            // Disconnected error on outstanding or subsequent service calls.
            let _ = self.rpc_system.await;
        });
        client
    }
}

#[cfg(feature = "rpc")]
/// A client connection of its own to the launcher-inherited service `name`.
///
/// The process inherits one socket per service, which never belongs to a
/// single client: every call returns a private connection whose calls are
/// forwarded over that socket, so any number of clients (one per record,
/// say) may be open and dropped in any order.
///
/// # Errors
///
/// Returns [`NotAvailable::ServiceFdMissing`] if no fd is open for `name`,
/// or [`NotAvailable::ConnectionFailed`] if the connection can't be set up
/// or the inherited one has ended.
pub fn connect_service(name: &str) -> Result<CapnpClient, NotAvailable> {
    #[cfg(unix)]
    {
        CapnpClient::from_connected_fd(
            std::os::fd::IntoRawFd::into_raw_fd(connect_service_fd(name)?),
            rpc_twoparty_capnp::Side::Client,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = name;
        Err(NotAvailable::ServiceFdMissing)
    }
}

#[cfg(all(unix, feature = "rpc"))]
/// [`connect_service`] for a client that drives its own RPC system: the
/// connected socket of its own, speaking Cap'n Proto two-party as the client.
///
/// # Errors
///
/// As [`connect_service`].
pub fn connect_service_fd(name: &str) -> Result<OwnedFd, NotAvailable> {
    let fd = service_fd(name).ok_or(NotAvailable::ServiceFdMissing)?;
    shared::connect(name, fd)
}

#[cfg(feature = "rpc")]
/// Helper for service crates: open a connection to an inherited service
/// ([`connect_service`]) and pull out the bootstrap.
///
/// # Errors
///
/// Returns [`NotAvailable::ServiceFdMissing`] if no fd is open for `name`,
/// or [`NotAvailable::ConnectionFailed`] if the connection can't be set up.
pub fn try_open_service<C>(name: &str) -> Result<C, NotAvailable>
where
    C: capnp::capability::FromClientHook,
{
    Ok(connect_service(name)?.bootstrap::<C>())
}

#[cfg(feature = "rpc")]
/// The same, for a caller that already holds the connection.
/// [`try_open_service`] reads its fd from the environment, right for a
/// launched process; a caller with its own connection reaching for the
/// environment anyway risks two clients racing `set_var`, which is unsound
/// while any other thread reads the environment.
///
/// # Errors
///
/// Returns [`NotAvailable::ConnectionFailed`] if `fd` can't be lifted to an
/// RPC connection.
pub fn open_service_on_fd<C>(fd: RawFd) -> Result<C, NotAvailable>
where
    C: capnp::capability::FromClientHook,
{
    let client = UnixCapnpClient::from_connected_fd(fd, rpc_twoparty_capnp::Side::Client)?;
    Ok(client.bootstrap::<C>())
}

#[cfg(feature = "rpc")]
/// `Promise` made return-friendly for `Result<_, capnp::Error>`, so wrappers
/// can write `let p: Promise<_, _> = ...` without importing capnp.
pub type RpcPromise<T> = Promise<T, capnp::Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "rpc")]
    #[tokio::test(flavor = "current_thread")]
    async fn connected_io_reports_peer_close() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (connection, peer) = tokio::io::duplex(512);
                let client: capnp::capability::Client =
                    CapnpClient::from_connected_io(connection, rpc_twoparty_capnp::Side::Client)
                        .bootstrap();
                drop(peer);
                // Service clients await RPC responses. Exercise that public
                // failure path, including a call queued before the driver polls.
                let request = client
                    .new_call::<capnp::any_pointer::Owned, capnp::any_pointer::Owned>(0, 0, None);
                let error = tilo_test_env::within(
                    "the closed peer to resolve the outstanding RPC",
                    request.send().promise,
                )
                .await
                .err()
                .expect("closed transport cannot complete a service call");
                assert_eq!(error.kind, capnp::ErrorKind::Disconnected);
            })
            .await;
    }

    fn clear_tilo_env() -> tilo_test_env::ScopedEnv {
        let mut scoped = tilo_test_env::ScopedEnv::new();
        for (key, _) in env::vars() {
            if key.starts_with("TILO_") {
                scoped.remove(&key);
            }
        }
        scoped
    }

    #[test]
    fn service_fds_resolve_only_from_a_valid_open_descriptor() {
        {
            let _env = clear_tilo_env();
            assert!(!is_tilo_native(), "case: no env is not native");
            for service in ["collab", "fs", "identity"] {
                assert!(service_fd(service).is_none(), "case unset: {service}");
            }
        }
        // (case, TILO_SVC_COLLAB_FD value)
        let cases = [
            ("garbage value", "not-a-number"),
            // A fd well above stdin/stdout/stderr, unlikely to be open in a test process.
            ("closed fd", "999"),
        ];
        for (case, value) in cases {
            let mut env = clear_tilo_env();
            env.set("TILO_SVC_COLLAB_FD", value);
            assert!(service_fd("collab").is_none(), "case {case}");
        }
    }
}

#[cfg(unix)]
pub mod fd_transport;

#[cfg(all(unix, feature = "rpc"))]
mod shared;

pub mod native;
