//! A Terminal session in a workspace a server hosts: its shell runs where the server runs the
//! workspace's shells, and this device carries it to the window.
//!
//! The window asks for its session as in any workspace. Before this device's own environment
//! is considered, the session is asked whether a host runs the workspace
//! ([`runtime_portal::workspace_terminal`]): it holds the device's key and its connection to
//! that host, so it asks the host to open the session's shell on a terminal and carries the
//! host's terminal stream here. The window gets a terminal of this device's as it always does;
//! what is typed on it goes to the host's shell, what the shell draws comes back onto it, and
//! the window's size follows it there.
//!
//! The stream ending is told on the session's lease: the shell exited, another window took
//! it, or the connection was lost, in which case the host keeps the shell a while and the
//! window comes back to it by the host's session id ([`ServeResponse::Session`]'s
//! `host_session`). The host records what was run there; this device records nothing.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use runtime_portal::workspace_terminal::{Answer, Request};

use super::session_end::EndReport;
use super::{ServeRequest, ServeResponse, Served, SessionTransport, reply};
use crate::audit::{AuditEntry, AuditLog};
use crate::environment_terminals::{FromTerminal, ToTerminal, wire};
use crate::store::Store;

/// How often the window's terminal is looked at for a new size while nothing is typed.
const TICK: Duration = Duration::from_millis(100);

/// What the session said of the workspace's shell.
enum Route {
    /// No host other than this device runs the workspace.
    Local,
    Refused(String),
    /// The host's terminal stream, taken: the host's session id, and the stream.
    Opened {
        session: String,
        stream: UnixStream,
    },
}

/// Serve `req` from the shell of the workspace's host when a server hosts `store`'s workspace;
/// `None` when none does, and the session opens on this device.
///
/// # Errors
///
/// The window's connection failed.
pub(super) fn serve_hosted(
    transport: &SessionTransport<'_>,
    writer: &mut UnixStream,
    store: &Store,
    req: &ServeRequest,
) -> anyhow::Result<Option<Served>> {
    let ServeRequest::OpenSession {
        cols,
        rows,
        command,
        record,
        host_session,
        ..
    } = req
    else {
        return Ok(None);
    };
    // A program asked for by name runs where the window's own environment is.
    if !command.is_empty() || store.workspace_id().is_primary() {
        return Ok(None);
    }
    let Some(socket) = crate::mcp_server::session_api().filter(|socket| socket.exists()) else {
        return Ok(None);
    };
    let request = Request::new(
        store.workspace_id().as_str(),
        record.as_deref().unwrap_or_default(),
        (*cols, *rows),
        host_session.as_deref(),
    );
    let mut told = Ok(());
    let route = ask(&socket, &request, || {
        told = reply(writer, &ServeResponse::Waiting, None);
    });
    told?;
    let audit = AuditLog::at(store.audit_dir());
    let (session, stream) = match route {
        Route::Local => return Ok(None),
        Route::Refused(reason) => {
            tracing::info!(%reason, "ush: the workspace's host opened no shell");
            audit.append(
                &AuditEntry::new("serve", "hosted", "open-session", false).because(&reason),
            );
            reply(writer, &ServeResponse::Denied { reason }, None)?;
            return Ok(Some(Served::Refused));
        }
        Route::Opened { session, stream } => (session, stream),
    };
    let (master, slave) = super::open_pty(*cols, *rows)?;
    let slave = OwnedFd::from(slave);
    // Keys and what is drawn pass through untouched: the host's terminal does the rest.
    crate::hosted_vm::Raw::new(&slave).keep();
    let (supervisor_lease, terminal_lease) = UnixStream::pair()?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let mut bytes = serde_json::to_vec(&Leased {
        response: &ServeResponse::Session {
            session: id.clone(),
            environment: tilo_environment::DEFAULT_RECORD.into(),
            commands: false,
            editor_protocol: None,
            host_session: Some(session.clone()),
        },
        protocol: super::SERVICE_PROTOCOL,
    })?;
    bytes.push(b'\n');
    runtime_portal::fd_transport::send_with_fds(
        &*writer,
        &bytes,
        &[master.as_fd(), terminal_lease.as_fd()],
    )?;
    drop(master);
    drop(terminal_lease);
    audit.append(&AuditEntry::new(&id, "hosted", "open-session", true).because(&session));
    let active = transport.active.clone();
    active.fetch_add(1, Ordering::AcqRel);
    std::thread::Builder::new()
        .name("ush-hosted-terminal".into())
        .spawn(move || {
            let ended = carry(&slave, &stream, &supervisor_lease);
            let said = report(&supervisor_lease, &ended);
            audit.append(&AuditEntry::new(&id, "hosted", "close-session", true).because(&said));
            active.fetch_sub(1, Ordering::AcqRel);
        })?;
    Ok(Some(Served::Done))
}

/// A session's answer as the lease carries it: with the protocol it speaks.
#[derive(serde::Serialize)]
struct Leased<'a> {
    #[serde(flatten)]
    response: &'a ServeResponse,
    protocol: u16,
}

/// Ask the session at `socket` for `request`'s shell and take the stream it carries;
/// `hosted` runs once the session says a host is asked.
fn ask(socket: &std::path::Path, request: &Request, hosted: impl FnOnce()) -> Route {
    match runtime_portal::workspace_terminal::open(socket, request, hosted) {
        Ok(Answer::Local) => Route::Local,
        Ok(Answer::Refused(reason)) => Route::Refused(reason),
        Ok(Answer::Opened {
            stream, connection, ..
        }) => take(stream, connection),
        // No session answers there: nothing of this login reaches a host.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Route::Local
        }
        Err(error) => Route::Refused(format!(
            "this device's session did not reach the workspace's host: {error}"
        )),
    }
}

/// Present `id` on `stream`, the host's terminal stream, and read whether it is ours.
fn take(id: String, mut stream: UnixStream) -> Route {
    let welcomed = wire::write_hello(&mut stream, &wire::Hello { stream: id })
        .and_then(|()| stream.flush())
        .and_then(|()| wire::read_welcome(&mut stream));
    match welcomed {
        Ok(Some(wire::Welcome::Opened { session })) => Route::Opened { session, stream },
        Ok(Some(wire::Welcome::Refused { reason })) => Route::Refused(reason),
        Ok(None) => Route::Refused("the workspace's host closed the terminal stream".into()),
        Err(error) => Route::Refused(format!(
            "the workspace's host did not take the terminal stream: {error}"
        )),
    }
}

/// How a carried stream ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ended {
    Exited(i32),
    Superseded,
    Lost(String),
    /// The window let its terminal go.
    Closed,
}

/// Carry the host's `stream` to the window's terminal, whose other end is `slave`, until the
/// shell exits, another window takes it, the connection goes, or the window lets `lease` go.
fn carry(slave: &OwnedFd, stream: &UnixStream, lease: &UnixStream) -> Ended {
    let closed = Arc::new(AtomicBool::new(false));
    let (Ok(typed), Ok(lease_watch), Ok(keys)) = (
        stream.try_clone(),
        lease.try_clone(),
        slave.try_clone().map(File::from),
    ) else {
        return Ended::Lost("the terminal could not be carried".into());
    };
    let gone = Arc::clone(&closed);
    let keys = std::thread::Builder::new()
        .name("ush-hosted-keys".into())
        .spawn(move || carry_keys(&keys, typed, &lease_watch, &gone));
    let mut drawn = File::from(match slave.try_clone() {
        Ok(slave) => slave,
        Err(error) => return Ended::Lost(error.to_string()),
    });
    let mut from = stream;
    let ended = loop {
        match wire::read_from(&mut from) {
            Ok(Some(FromTerminal::Out(bytes))) => {
                if drawn.write_all(&bytes).is_err() {
                    break Ended::Closed;
                }
            }
            Ok(Some(FromTerminal::Exited(status))) => break Ended::Exited(status),
            Ok(Some(FromTerminal::Superseded)) => break Ended::Superseded,
            Ok(None) => break Ended::Lost("the workspace's host closed the connection".into()),
            Err(error) => {
                break Ended::Lost(format!("the connection to the workspace's host: {error}"));
            }
        }
    };
    let ended = if closed.load(Ordering::SeqCst) {
        Ended::Closed
    } else {
        ended
    };
    // What is still typed has nowhere to go.
    closed.store(true, Ordering::SeqCst);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    if let Ok(keys) = keys {
        let _ = keys.join();
    }
    ended
}

/// Carry what the window types on its terminal, and its size as it changes, to the host
/// until the window lets go of its lease or its terminal, or `closed` says the stream ended.
fn carry_keys(keys: &File, mut to: UnixStream, lease: &UnixStream, closed: &AtomicBool) {
    let mut size = crate::hosted_vm::terminal_size(keys);
    let mut buffer = [0_u8; 4096];
    let tick = libc::c_int::try_from(TICK.as_millis()).unwrap_or(100);
    while !closed.load(Ordering::SeqCst) {
        let mut fds = [
            libc::pollfd {
                fd: keys.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: lease.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: poll reads and writes only the two pollfds owned here.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, tick) };
        let [terminal, leased] = fds;
        if ready > 0 && leased.revents != 0 {
            // The window closed its end: the session is over for it.
            break;
        }
        if ready > 0 && terminal.revents & libc::POLLIN != 0 {
            match (&*keys).read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let typed = ToTerminal::In(buffer.get(..read).unwrap_or_default().to_vec());
                    if wire::write_to(&mut to, &typed).is_err() {
                        return;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                // The window's end of the terminal is gone.
                Err(_) => break,
            }
        } else if ready > 0 && terminal.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
            break;
        }
        let now = crate::hosted_vm::terminal_size(keys);
        if now != size {
            size = now;
            let resized = ToTerminal::Resize {
                cols: size.cols,
                rows: size.rows,
            };
            if wire::write_to(&mut to, &resized).is_err() {
                return;
            }
        }
    }
    if !closed.swap(true, Ordering::SeqCst) {
        // The window went first: the host keeps the shell a while for it to come back.
        let _ = to.shutdown(std::net::Shutdown::Both);
    }
}

/// Tell the window on `lease` how its stream ended, and close the lease: in words for the log.
fn report(lease: &UnixStream, ended: &Ended) -> String {
    let (report, said) = match ended {
        Ended::Exited(status) => (Some(EndReport::Exited), format!("exit {status}")),
        Ended::Superseded => (Some(EndReport::Superseded), "superseded".to_owned()),
        Ended::Lost(reason) => (
            Some(EndReport::Lost {
                reason: reason.as_str(),
            }),
            format!("lost: {reason}"),
        ),
        Ended::Closed => (None, "terminal closed".to_owned()),
    };
    if let Some(report) = report
        && let Ok(mut line) = serde_json::to_vec(&report)
    {
        line.push(b'\n');
        let _ = (&*lease).write_all(&line);
    }
    let _ = lease.shutdown(std::net::Shutdown::Both);
    said
}
