//! Service discovery and connected transports for Tilo apps: the launcher opens
//! a Unix socket per system service and passes the connected descriptor in
//! `TILO_SVC_<NAME>_FD`. On stock Linux, where no service connection exists,
//! every client returns [`NotAvailable::ServiceFdMissing`] and the app falls
//! back to a local-only mode.

pub mod agent_transport;
#[cfg(unix)]
pub mod environment_socket;
/// How the device's shell runtime shows the session it holds the device identity.
#[cfg(unix)]
pub mod holder_proof;
/// What a process says first on its connection to its session's services:
/// the launch it is, written on a Unix socket. Only a broker attests it
/// (its own side reads `/proc`), so writing one is available wherever the
/// connection is.
pub mod launch_handoff;
/// Holds a child until the host that started it has recorded
/// its launch, so it can never reach the broker before it is attested.
#[cfg(unix)]
pub mod launch_hold;
/// A platform without a session to be placed by: nothing registers a launch, so
/// the variable is never named and waiting on one is refused rather than passed.
#[cfg(not(unix))]
pub mod launch_hold {
    /// The variable a session names for the hold a surface waits on.
    pub const LAUNCH_REGISTRATION_STDIN_ENV: &str = "TILO_INTERNAL_LAUNCH_REGISTRATION_STDIN";

    /// A launch registration is a Unix pipe, and this platform has none.
    ///
    /// # Errors
    ///
    /// Always [`std::io::ErrorKind::Unsupported`].
    pub fn wait_on_stdin() -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "a launch registration is a Unix pipe, which this platform does not have",
        ))
    }
}
/// The device's tilolang processes, as a desktop session hands them its windows' instances.
pub mod device_processes;
pub mod local_socket;
/// The messages that travel over a connected socket: Unix sockets and the
/// descriptors they carry, so Linux only, like the clients that use it.
#[cfg(unix)]
pub mod message;
pub mod origin_connector;
/// Typed messages on a packet socket, with the descriptors they name beside them.
pub mod packet;
pub mod pending_connection;
pub mod process_binding;
pub mod process_link;
#[cfg(unix)]
pub mod request_rendezvous;
pub mod service_socket;
/// The session services a launcher connects on an app's behalf, and what it says first on each.
pub mod session_services;
/// The actions connection a session hands a surface app.
pub mod surface_actions;
pub mod tools;
/// A hosted workspace's shell on a terminal, asked of the session by the device's shell runtime.
#[cfg(unix)]
pub mod workspace_terminal;

/// The app ↔ runtime interface's current version: what an app and the
/// session services the launcher connects it to (collaboration, the
/// filesystem and its document media, the app host, the identity broker,
/// shell-owned tools) speak. An app's manifest names the version it was
/// built against, and it runs where the image serves that version: the
/// current one or any since [`OLDEST_RUNTIME_INTERFACE_VERSION`]
/// (docs/architecture/compatibility.md). `interface.toml` holds the schema,
/// and the repository policy fails a change to it that keeps the number.
pub const RUNTIME_INTERFACE_VERSION: u32 = 41;

/// The oldest runtime interface version this build still works with. An app
/// built against any version from it to [`RUNTIME_INTERFACE_VERSION`] runs.
///
/// A version bump keeps it where it is: it moves only when the change is
/// breaking, to the version before the change, once this build no longer
/// reads or writes the wire that version used. A bump that keeps reading the
/// previous wire leaves apps built against it running, and a bump that breaks
/// it stops them, which is the one case raising this number is for. Set it by
/// hand in the bumping change, beside the schema change that broke the wire.
///
/// The image declares the two in `os/usr/share/tilo/system-interfaces.toml`
/// (`[interfaces] runtime` and `[runtime] oldest`), and
/// `tools/policy/check_interfaces.py` fails a change that moves one without
/// the other.
pub const OLDEST_RUNTIME_INTERFACE_VERSION: u32 = 41;

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

#[cfg(unix)]
use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
#[cfg(test)]
use tilo_lock::RwLockRecover as _;
use tokio::io::{AsyncRead, AsyncWrite};
#[cfg(unix)]
use tokio::net::UnixStream;

#[derive(Debug, thiserror::Error)]
pub enum NotAvailable {
    /// The launcher did not pass an fd for this service: either we're outside
    /// Tilo (stock Linux / Flatpak) or the service wasn't declared in the manifest.
    #[error("service fd env var missing or fd is not open")]
    ServiceFdMissing,
    #[error("service connection failed: {0}")]
    ConnectionFailed(#[from] io::Error),
    #[error("protocol mismatch: expected {expected}, got {got}")]
    ProtocolMismatch { expected: u32, got: u32 },
    /// The service read the request and refused it, saying why.
    #[error("refused: {0}")]
    Refused(String),
}

/// True iff we appear to be running inside a Tilo session. Heuristic only —
/// apps must NOT use this as an authorisation check; try the relevant
/// service client and react to [`NotAvailable`] instead. Intended for
/// telemetry, boot-time toggles, and friendly first-run messaging.
pub fn is_tilo_native() -> bool {
    native::connection_metadata_is_current() && env::var_os("TILO_WORKSPACE_ID").is_some()
}

/// Look up a service fd by canonical name (e.g. "collab", "fs"). `None` if
/// the env var is unset, not a valid integer, or the fd isn't currently open.
pub fn service_fd(name: &str) -> Option<RawFd> {
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
        let var_name = session_services::fd_env(name);
        let raw = env::var(&var_name).ok()?;
        let Ok(fd) = raw.parse::<RawFd>() else {
            eprintln!(
                "tilo-portal: the inherited service descriptor {var_name}={raw:?} is not a number"
            );
            return None;
        };
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

#[cfg(unix)]
/// Take the process's connection to the launcher-inherited service `name`.
///
/// The launcher hands a process one connected socket per service, so this
/// takes it once: every client of the service builds on that connection, and a
/// second call reports it as taken instead of opening a second stream on the
/// same socket, which two peers driving it would corrupt.
///
/// # Errors
///
/// Returns [`NotAvailable::ServiceFdMissing`] if no fd is open for `name`, or
/// [`NotAvailable::ConnectionFailed`] if the connection is already taken or
/// cannot be marked close-on-exec.
pub fn connect_service_fd(name: &str) -> Result<OwnedFd, NotAvailable> {
    static TAKEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let fd = service_fd(name).ok_or(NotAvailable::ServiceFdMissing)?;
    let first = {
        let mut taken = TAKEN.get_or_init(Default::default).lock().map_err(|_| {
            NotAvailable::ConnectionFailed(io::Error::other(
                "the service connection registry poisoned",
            ))
        })?;
        taken.insert(name.to_owned())
    };
    if !first {
        return Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("the {name} service connection is already taken"),
        )));
    }
    // SAFETY: `service_fd` verified the slot is open, and the registry above
    // makes this the only owner of it.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    // The launcher clears close-on-exec only for this process's exec; children
    // it starts later must not inherit the service connection.
    fcntl_setfd(&owned, FdFlags::CLOEXEC).map_err(io::Error::from)?;
    Ok(owned)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let cases = [("garbage value", "not-a-number"), ("closed fd", "999")];
        for (case, value) in cases {
            let mut env = clear_tilo_env();
            env.set("TILO_SVC_COLLAB_FD", value);
            assert!(service_fd("collab").is_none(), "case {case}");
        }
    }

    /// One inherited socket is one connection: the first caller takes it
    /// close-on-exec, and a second sees it taken instead of driving the same
    /// stream alongside the first.
    #[cfg(unix)]
    #[test]
    fn a_service_connection_is_taken_once() {
        let mut env = clear_tilo_env();
        let (ours, _service) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        let fd = std::os::fd::IntoRawFd::into_raw_fd(OwnedFd::from(ours));
        env.set("TILO_SVC_PORTALTEST_FD", fd.to_string());

        let taken = connect_service_fd("portaltest").expect("the inherited connection");
        assert!(
            fcntl_getfd(&taken)
                .expect("the connection's flags")
                .contains(FdFlags::CLOEXEC),
            "a child must not inherit the service connection"
        );
        let error = connect_service_fd("portaltest").expect_err("the connection is taken");
        assert!(
            matches!(error, NotAvailable::ConnectionFailed(_)),
            "{error:?}"
        );
    }
}

#[cfg(unix)]
pub mod fd_transport;

#[cfg(unix)]
pub mod descriptors;

#[cfg(unix)]
pub type Descriptors = Option<descriptors::DescriptorSender>;
#[cfg(not(unix))]
pub type Descriptors = Option<std::convert::Infallible>;

/// A connected stream that carries a descriptor stream on the hosts that have
/// one. The wrapping lives here, beside the descriptor stream it makes, so a caller never
/// selects a target to hand a service what the person selected.
#[cfg(unix)]
pub trait DescriptorStreamSource: AsyncRead + AsyncWrite + Sized {
    fn with_descriptors(self) -> (impl AsyncRead + AsyncWrite, Descriptors);
}

#[cfg(unix)]
impl DescriptorStreamSource for UnixStream {
    fn with_descriptors(self) -> (impl AsyncRead + AsyncWrite, Descriptors) {
        let sender = descriptors::DescriptorSender::new();
        (
            descriptors::DescriptorStream::sending(self, sender.clone()),
            Some(sender),
        )
    }
}

#[cfg(not(unix))]
pub trait DescriptorStreamSource: AsyncRead + AsyncWrite + Sized {
    fn with_descriptors(self) -> (impl AsyncRead + AsyncWrite, Descriptors);
}

#[cfg(not(unix))]
impl<T: AsyncRead + AsyncWrite> DescriptorStreamSource for T {
    fn with_descriptors(self) -> (impl AsyncRead + AsyncWrite, Descriptors) {
        (self, None)
    }
}

/// A child forked between a descriptor's creation and its `exec` holds a copy of
/// that descriptor until the `exec` closes it (every descriptor here is
/// close-on-exec), so a test that waits for the last copy of one to close can
/// find a sibling test's child still holding it. Tests that spawn a child hold
/// the shared side of this lock for the spawn ([`spawning`]); tests that watch
/// for a leaked descriptor hold the exclusive side ([`no_spawns`]), so no
/// child is in that window while they look.
#[cfg(test)]
static CHILD_SPAWNS: std::sync::RwLock<()> = std::sync::RwLock::new(());

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn spawning() -> std::sync::RwLockReadGuard<'static, ()> {
    CHILD_SPAWNS.read_recover()
}

/// Held by a test that checks a descriptor was closed, so that no sibling test
/// has a child between its fork and its exec meanwhile.
#[cfg(test)]
pub(crate) fn no_spawns() -> std::sync::RwLockWriteGuard<'static, ()> {
    CHILD_SPAWNS.write_recover()
}

pub mod editor;

pub mod native;
