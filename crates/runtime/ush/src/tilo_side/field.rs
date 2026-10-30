//! The line at the session's prompt as Terminal's native editor field: the
//! editor capability Terminal negotiated (`editor_protocol`), served by the
//! shell's own line editor ([`ush_shell::field`]).
//!
//! The supervisor hands the capability only to the session process it
//! started itself, which is this one: no program in the guest holds it, so
//! the producer is Tilo's own by construction and needs no proof of which
//! process it is. Requests are read on a thread of their own and queued;
//! while the line editor waits for a key, a request wakes its read of the
//! terminal ([`Keys`]).

use std::collections::VecDeque;
use std::io::{self, Read};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use runtime_portal::editor;
use ush_shell::field::{Field, Lease, Reply, Request, Status};

/// The editor capability, as the line editor's field.
pub struct SessionField {
    socket: Arc<editor::Socket>,
    incarnation: [u8; 16],
    queue: Mutex<VecDeque<(u64, Request)>>,
    listening: AtomicBool,
    wake_read: OwnedFd,
    wake_write: OwnedFd,
}

const fn lease_in(lease: &editor::Lease) -> Lease {
    Lease {
        incarnation: lease.incarnation,
        generation: lease.generation,
        revision: lease.revision,
        expires_ns: lease.expires_ns,
        anchor: lease.anchor,
        focus: lease.focus,
    }
}

const fn lease_out(lease: Lease) -> editor::Lease {
    editor::Lease {
        incarnation: lease.incarnation,
        generation: lease.generation,
        revision: lease.revision,
        expires_ns: lease.expires_ns,
        anchor: lease.anchor,
        focus: lease.focus,
    }
}

fn request_in(request: editor::Request) -> Request {
    match request {
        editor::Request::Inspect => Request::Inspect,
        editor::Request::Revoke => Request::Revoke,
        editor::Request::Replace {
            lease,
            transaction,
            text,
        } => Request::Replace {
            lease: lease_in(&lease),
            transaction,
            text,
        },
        editor::Request::Undo { lease, transaction } => Request::Undo {
            lease: lease_in(&lease),
            transaction,
        },
        editor::Request::Redo { lease, transaction } => Request::Redo {
            lease: lease_in(&lease),
            transaction,
        },
        editor::Request::Query { lease, transaction } => Request::Query {
            lease: lease_in(&lease),
            transaction,
        },
    }
}

const fn status_out(status: Status) -> editor::Status {
    match status {
        Status::Unavailable => editor::Status::Unavailable,
        Status::Ready => editor::Status::Ready,
        Status::Applied => editor::Status::Applied,
        Status::Undone => editor::Status::Undone,
        Status::Redone => editor::Status::Redone,
        Status::Stale => editor::Status::Stale,
        Status::Invalid => editor::Status::Invalid,
        Status::Unknown => editor::Status::Unknown,
        Status::Revoked => editor::Status::Revoked,
    }
}

/// Nanoseconds on the monotonic clock Terminal reads leases by.
fn monotonic_ns() -> u64 {
    let mut clock = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a properly sized output and a supported clock.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut clock) } != 0 {
        return 0;
    }
    u64::try_from(clock.tv_sec)
        .ok()
        .and_then(|secs| secs.checked_mul(1_000_000_000))
        .and_then(|ns| ns.checked_add(u64::try_from(clock.tv_nsec).ok()?))
        .unwrap_or(0)
}

impl SessionField {
    /// Serve `socket`, the capability Terminal holds the other end of,
    /// until it closes or sends something that is not a request.
    ///
    /// # Errors
    ///
    /// The wake pipe, the identity or the reading thread could not be made.
    pub fn serve(socket: Arc<editor::Socket>) -> io::Result<Arc<Self>> {
        let mut incarnation = [0_u8; 16];
        while incarnation == [0; 16] {
            getrandom::fill(&mut incarnation).map_err(io::Error::other)?;
        }
        let mut fds = [0; 2];
        // SAFETY: pipe2 writes two descriptors into the array we own.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the kernel just returned these two descriptors.
        let (wake_read, wake_write) =
            unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        socket.read_timeout(None)?;
        let field = Arc::new(Self {
            socket,
            incarnation,
            queue: Mutex::new(VecDeque::new()),
            listening: AtomicBool::new(false),
            wake_read,
            wake_write,
        });
        let reading = Arc::downgrade(&field);
        let socket = Arc::clone(&field.socket);
        std::thread::Builder::new()
            .name("ush-field".into())
            .spawn(move || read_requests(&socket, &reading))?;
        Ok(field)
    }

    /// The session's terminal as the line editor reads it: woken by a
    /// request while it listens.
    pub fn keys(self: &Arc<Self>, terminal: std::fs::File) -> Keys {
        Keys {
            terminal,
            field: Arc::clone(self),
        }
    }

    fn queue(&self) -> std::sync::MutexGuard<'_, VecDeque<(u64, Request)>> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wake(&self) {
        let byte = 1_u8;
        // SAFETY: one byte from a local into a non-blocking pipe we own.
        unsafe { libc::write(self.wake_write.as_raw_fd(), (&raw const byte).cast(), 1) };
    }
}

impl Drop for SessionField {
    fn drop(&mut self) {
        self.socket.retire();
    }
}

/// Read Terminal's requests until the capability closes. A malformed one
/// retires it, as an uncertain one would.
fn read_requests(socket: &editor::Socket, field: &Weak<SessionField>) {
    let mut bytes = [0_u8; editor::FRAME_BYTES];
    while let Ok(size) = socket.receive(&mut bytes) {
        let request = bytes
            .get(..size)
            .ok_or_else(|| io::Error::other("bound"))
            .and_then(editor::decode_request);
        let (Ok((id, request)), Some(field)) = (request, field.upgrade()) else {
            break;
        };
        field.queue().push_back((id, request_in(request)));
        if field.listening.load(Ordering::SeqCst) {
            field.wake();
        }
    }
    socket.retire();
}

impl Field for SessionField {
    fn incarnation(&self) -> [u8; 16] {
        self.incarnation
    }

    fn now_ns(&self) -> u64 {
        monotonic_ns()
    }

    fn listen(&self, on: bool) {
        self.listening.store(on, Ordering::SeqCst);
    }

    fn take(&self) -> Option<(u64, Request)> {
        self.queue().pop_front()
    }

    fn reply(&self, reply: &Reply) {
        let frame = editor::encode_reply(&editor::Reply {
            request: reply.request,
            status: status_out(reply.status),
            lease: reply.lease.map(lease_out),
            transaction: reply.transaction,
            can_undo: reply.can_undo,
            can_redo: reply.can_redo,
            text: reply.text.clone(),
        });
        // An answer that cannot be sent leaves the app uncertain: the
        // capability is retired rather than left half answered.
        if frame.and_then(|frame| self.socket.send(&frame)).is_err() {
            self.socket.retire();
        }
    }
}

/// The session's terminal, read by the line editor: a request from Terminal
/// interrupts a read while the editor listens for one.
pub struct Keys {
    terminal: std::fs::File,
    field: Arc<SessionField>,
}

impl Read for Keys {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut fds = [
                crate::broker::pollfd(self.terminal.as_raw_fd(), libc::POLLIN),
                crate::broker::pollfd(self.field.wake_read.as_raw_fd(), libc::POLLIN),
            ];
            crate::broker::poll_fds(&mut fds, None)?;
            let [terminal, woken] = fds;
            if woken.revents != 0 {
                super::drain(&self.field.wake_read);
                if self.field.listening.load(Ordering::SeqCst) {
                    return Err(io::ErrorKind::Interrupted.into());
                }
            }
            if terminal.revents != 0 {
                return self.terminal.read(buf);
            }
            if woken.revents == 0 {
                // A signal (Ctrl-C) ended the wait, as it ends a read.
                return Err(io::ErrorKind::Interrupted.into());
            }
        }
    }
}
