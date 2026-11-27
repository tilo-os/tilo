//! The seat's side of an app's connection: the desktop session attests a connection on its
//! app socket by its own launch record and hands it to the device's daemon, which serves it.
//!
//! The person's apps run as the person, beside their desktop session, which started them and
//! alone can tell their processes apart: the daemon runs as the device's own account and sees
//! neither those processes nor the sockets they could reach. So the session reads the launcher's
//! handoff off each connection, attests it against the launches it recorded
//! ([`LauncherServices`]), pins the app's process, and hands the connection itself to the daemon
//! over its shell link ([`runtime_collab::ShellClient::vouch`]). From then on the app speaks to
//! the daemon directly, admitted as the app, workspace and window the session vouched for.

use std::os::fd::OwnedFd;
use std::sync::Arc;

use runtime_collab::wire::shell::VouchedApp;

use crate::services::{LauncherServices, ProcessServices};

/// A connection on the seat's app socket, once attested.
pub enum Attested {
    /// A process of a launch the daemon decided and the seat started: its connection, the
    /// launch's ticket (the descriptor number left for [`runtime_collab::ShellClient::vouch`] to
    /// fill), and the pidfd of the connecting process.
    App {
        connection: OwnedFd,
        app: VouchedApp,
        process: OwnedFd,
    },
    /// A client that named no launch: it is admitted by what it presents, as any connection to
    /// the daemon's socket is.
    Unnamed(OwnedFd),
    /// Refused, and told why when it could be.
    Refused,
}

/// Attest `stream`, a connection on the seat's app socket, against `launcher`, the launches the
/// seat recorded.
#[allow(
    clippy::future_not_send,
    reason = "the launcher handoff is read where the seat accepts its connections"
)]
pub async fn attest(
    mut stream: tokio::net::UnixStream,
    launcher: Arc<dyn LauncherServices>,
) -> Attested {
    let services = ProcessServices {
        launcher: Some(launcher),
        ..ProcessServices::default()
    };
    let admitted = super::authenticate_launcher_handoff(&mut stream, &services).await;
    let handoff = match admitted {
        Ok(handoff) => handoff,
        Err(refusal) => {
            tracing::error!(
                peer = %refusal.peer,
                reason = %refusal.error,
                "refused an app connection before its first call",
            );
            if refusal.answerable {
                super::answer_refused_launch(stream, refusal.error).await;
            }
            return Attested::Refused;
        }
    };
    let connection = match stream.into_std() {
        Ok(stream) => OwnedFd::from(stream),
        Err(error) => {
            tracing::error!(%error, "an app connection cannot be handed over");
            return Attested::Refused;
        }
    };
    let Some(app) = handoff.local_media else {
        return Attested::Unnamed(connection);
    };
    // The daemon admits the connection by the launch it decided: one the seat started no
    // decided launch for is no app's.
    let Some(ticket) = handoff.launch.map(|launch| launch.ticket) else {
        tracing::error!(app = %app.app_id, "an app's connection belongs to no launch the daemon decided");
        return Attested::Refused;
    };
    let Some((process, _launcher)) = app.pidfds() else {
        tracing::error!(app = %app.app_id, "an app's process cannot be pinned on this system");
        return Attested::Refused;
    };
    Attested::App {
        connection,
        app: VouchedApp { ticket, process: 0 },
        process,
    }
}

/// The seat's app socket: where the launcher connects every app the seat starts, and where the
/// seat makes its windows' own connections. Owned for as long as it is held: the path is removed
/// when it is dropped, and a live socket at the path is never taken over.
pub struct AppSocket {
    listener: std::os::unix::net::UnixListener,
    _owned: super::SocketOwnership,
}

impl AppSocket {
    /// Bind the app socket at `path`, in a directory only this account reaches.
    ///
    /// # Errors
    /// The directory is not this account's own, another seat answers at `path`, or the socket
    /// could not be bound.
    pub fn bind(path: &std::path::Path) -> std::io::Result<Self> {
        let (listener, owned) = super::bind_owned_socket(path)?;
        Ok(Self {
            listener,
            _owned: owned,
        })
    }

    /// The bound listener, for the task that accepts on it, on the runtime it runs in.
    ///
    /// # Errors
    /// The listener could not be copied or made non-blocking.
    pub fn listener(&self) -> std::io::Result<tokio::net::UnixListener> {
        tokio::net::UnixListener::from_std(self.copy()?)
    }

    /// A non-blocking copy of the bound listener, for a runtime that is not running here yet.
    ///
    /// # Errors
    /// The listener could not be copied or made non-blocking.
    pub fn copy(&self) -> std::io::Result<std::os::unix::net::UnixListener> {
        let listener = self.listener.try_clone()?;
        listener.set_nonblocking(true)?;
        Ok(listener)
    }
}
