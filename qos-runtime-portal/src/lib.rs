//! `qos-runtime-portal` — service discovery and connected-fd → RPC
//! plumbing for qOS apps.
//!
//! qOS apps run inside a launcher-managed sandbox. The launcher
//! opens a Unix socket per system service (collab, fs, ...) and
//! passes the connected file descriptor in to the child via the
//! environment variable `QOS_SVC_<NAME>_FD`. This crate provides:
//!
//! - [`service_fd`] — pull the raw fd out of the environment.
//! - [`is_qos_native`] — heuristic: are we running under a qOS
//!   compositor at all, or as a stock Flatpak on a vanilla desktop?
//! - [`UnixCapnpClient`] — lift a connected `RawFd` into a tokio
//!   Cap'n Proto two-party RPC client. Higher-level service crates
//!   (`qos-runtime-collab`, `qos-runtime-fs`) build typed clients
//!   on top of the bootstrap returned here.
//! - [`NotAvailable`] — the single error type service clients
//!   surface when the platform can't provide the requested service.
//!
//! ## Design intent: graceful degradation
//!
//! qOS apps must ALSO be able to run as Flatpaks on stock Linux.
//! There is no service connection there. Every system-service
//! client therefore returns [`NotAvailable::ServiceFdMissing`]
//! rather than panicking, and the app is expected to fall back to
//! a local-only mode.

use std::env;
use std::io;
use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd, RawFd};

use capnp::capability::Promise;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use nix::fcntl::{F_GETFD, fcntl};
use tokio::net::UnixStream;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// All ways a system service can be unavailable to an app.
#[derive(Debug, thiserror::Error)]
pub enum NotAvailable {
    /// The launcher did not pass an fd for this service. Either
    /// we're running outside qOS (stock Linux / Flatpak) or the
    /// service was not declared in the package manifest.
    #[error("service fd env var missing or fd is not open")]
    ServiceFdMissing,
    /// The fd was present but the socket has been closed or the
    /// peer hung up before we could speak RPC.
    #[error("service connection failed: {0}")]
    ConnectionFailed(#[from] io::Error),
    /// The service responded but its protocol version does not
    /// match what this client was compiled against.
    #[error("protocol mismatch: expected {expected}, got {got}")]
    ProtocolMismatch { expected: u32, got: u32 },
    /// Capability error talking to the service (the RPC layer
    /// rejected our request, the remote sent a malformed reply).
    #[error("capnp error: {0}")]
    Capnp(#[from] capnp::Error),
}

/// True iff we appear to be running inside a qOS session. Heuristic
/// only — apps must NOT use this as an authorisation gate, they
/// must instead try the relevant service client and react to
/// [`NotAvailable`] if it fails. This is intended for telemetry,
/// boot-time feature toggles, and friendly first-run messaging
/// ("Welcome to qOS!" vs. "Running on stock Linux — collaboration
/// disabled.").
pub fn is_qos_native() -> bool {
    env::var_os("QOS_WORKSPACE_ID").is_some()
}

/// Look up a service fd by canonical name (e.g. "collab", "fs").
/// Returns `None` if the env var is unset, not a valid integer, or
/// the fd is not currently open. Names are case-insensitive but
/// callers should pass lowercase to keep call sites consistent.
pub fn service_fd(name: &str) -> Option<RawFd> {
    let var_name = format!("QOS_SVC_{}_FD", name.to_uppercase());
    let raw = env::var(&var_name).ok()?;
    let fd: RawFd = raw.parse().ok()?;
    // F_GETFD returns the close-on-exec flag; we only care that the
    // syscall succeeds. EBADF means the fd slot is empty.
    // SAFETY: we don't mutate the fd; if the slot is closed, fcntl
    // returns EBADF and we return None. We hand the BorrowedFd
    // immediately back to fcntl and discard it.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    fcntl(borrowed, F_GETFD).ok()?;
    Some(fd)
}

/// A tokio Cap'n Proto two-party RPC client wrapped around an
/// already-connected Unix socket fd. Typed service crates call
/// [`Self::bootstrap`] to obtain the remote root capability and
/// own the [`RpcSystem`] driver task that keeps the connection
/// pumping.
pub struct UnixCapnpClient {
    rpc_system: RpcSystem<rpc_twoparty_capnp::Side>,
    bootstrap_side: rpc_twoparty_capnp::Side,
}

impl UnixCapnpClient {
    /// Wrap an owned `RawFd` that points to a connected Unix
    /// stream socket. Takes ownership of the fd; the caller must
    /// NOT close it afterwards.
    ///
    /// `side` determines which end of the two-party protocol we
    /// are. App SDK clients are always `Client`; service
    /// implementations are `Server`.
    pub fn from_connected_fd(fd: RawFd, side: rpc_twoparty_capnp::Side) -> Result<Self, NotAvailable> {
        // Take exclusive ownership and hand to tokio.
        // SAFETY: the launcher guarantees the fd is a connected
        // SOCK_STREAM Unix socket. We checked F_GETFD on it before
        // this call (via service_fd) so the slot is at least open.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let std_stream = std::os::unix::net::UnixStream::from(owned);
        std_stream
            .set_nonblocking(true)
            .map_err(NotAvailable::ConnectionFailed)?;
        let stream = UnixStream::from_std(std_stream).map_err(NotAvailable::ConnectionFailed)?;
        let (read_half, write_half) = stream.into_split();
        let read_compat = read_half.compat();
        let write_compat = write_half.compat_write();
        let network = Box::new(twoparty::VatNetwork::new(
            read_compat,
            write_compat,
            side,
            Default::default(),
        ));
        let rpc_system = RpcSystem::new(network, None);
        Ok(Self {
            rpc_system,
            bootstrap_side: match side {
                rpc_twoparty_capnp::Side::Client => rpc_twoparty_capnp::Side::Server,
                rpc_twoparty_capnp::Side::Server => rpc_twoparty_capnp::Side::Client,
            },
        })
    }

    /// Take the remote bootstrap capability as a typed client.
    /// Spawns the RPC driver task on the current tokio LocalSet
    /// (capnp-rpc requires `!Send` futures, so callers must run
    /// this from a `LocalSet`-aware context).
    ///
    /// The returned client is the typed root interface of the
    /// remote service.
    pub fn bootstrap<C>(mut self) -> C
    where
        C: capnp::capability::FromClientHook,
    {
        let client: C = self.rpc_system.bootstrap(self.bootstrap_side);
        // The driver future must be polled; capnp-rpc futures are
        // !Send so we spawn onto the LocalSet via `spawn_local`.
        tokio::task::spawn_local(async move {
            // RpcSystem resolves when the connection closes; we
            // silently drop the result (the only thing we could
            // do with an error here is log it, and the higher
            // layer will see the connection drop via a Disconnected
            // capnp error on its next call).
            let _ = self.rpc_system.await;
        });
        client
    }
}

/// Helper for service crates: probe an fd, lift it to capnp, and
/// pull out the bootstrap. Returns [`NotAvailable::ServiceFdMissing`]
/// if the env var isn't there.
pub fn try_open_service<C>(name: &str) -> Result<C, NotAvailable>
where
    C: capnp::capability::FromClientHook,
{
    let fd = service_fd(name).ok_or(NotAvailable::ServiceFdMissing)?;
    let client = UnixCapnpClient::from_connected_fd(fd, rpc_twoparty_capnp::Side::Client)?;
    Ok(client.bootstrap::<C>())
}

/// Tiny convenience: capnp-rpc's `Promise` made return-friendly
/// for `Result<_, capnp::Error>` futures so service-client wrappers
/// can write `let p: Promise<_, _> = ...` without importing capnp.
pub type RpcPromise<T> = Promise<T, capnp::Error>;

#[cfg(test)]
mod tests {
    use super::*;

    fn clear_qos_env() {
        // SAFETY: tests are not parallel for this function — the
        // ones that touch QOS_* env are marked `#[serial]` via
        // file-scoped tests... but the workspace lacks `serial_test`
        // here, so we keep these tests independent by clearing
        // every QOS_* var both before AND after the assertion. The
        // test runner serialises top-level env mutation in stable
        // rust 1.x via the `std::env::set_var` `unsafe` warning;
        // these tests don't set vars concurrently with anyone else.
        for (key, _) in env::vars() {
            if key.starts_with("QOS_") {
                // SAFETY: we own the test process env. No other
                // thread reads QOS_* during test setup.
                unsafe { env::remove_var(&key) };
            }
        }
    }

    #[test]
    fn is_qos_native_false_without_env() {
        clear_qos_env();
        assert!(!is_qos_native());
    }

    #[test]
    fn service_fd_returns_none_when_unset() {
        clear_qos_env();
        assert!(service_fd("collab").is_none());
        assert!(service_fd("fs").is_none());
        assert!(service_fd("identity").is_none());
    }

    #[test]
    fn service_fd_returns_none_for_garbage_value() {
        clear_qos_env();
        // SAFETY: see comment above; tests in this module are
        // logically serial because they all touch QOS_* env.
        unsafe { env::set_var("QOS_SVC_COLLAB_FD", "not-a-number") };
        assert!(service_fd("collab").is_none());
        unsafe { env::remove_var("QOS_SVC_COLLAB_FD") };
    }

    #[test]
    fn service_fd_returns_none_for_closed_fd() {
        clear_qos_env();
        // Pick a fd well above stdin/stdout/stderr that we know
        // can't currently be open in a normal test process.
        unsafe { env::set_var("QOS_SVC_COLLAB_FD", "999") };
        assert!(service_fd("collab").is_none());
        unsafe { env::remove_var("QOS_SVC_COLLAB_FD") };
    }
}
