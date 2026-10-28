//! The socket loop: one connection at a time, one request per connection.

use std::io::{BufRead, BufReader, Read};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{Backend, Caller, Service};
use crate::protocol::{Envelope, MAX_LINE_BYTES, PROTOCOL_VERSION, Response};

/// How long one client may take to send its request or read the answer.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

fn answer(stream: &UnixStream, response: &Response, fd: Option<std::os::fd::BorrowedFd<'_>>) {
    if let Ok(mut line) = serde_json::to_vec(response) {
        line.push(b'\n');
        let _ = crate::fdpass::write_line_with_fd(stream, &line, fd);
    }
}

/// Serve one connection: identify the caller before reading a byte of its
/// request, then answer it.
pub fn serve_one<B: Backend>(
    stream: &UnixStream,
    service: &mut Service<B>,
    identify: &mut impl FnMut(&UnixStream) -> Result<Caller, String>,
) {
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(CLIENT_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(CLIENT_TIMEOUT)).is_err()
    {
        return;
    }
    let caller = match identify(stream) {
        Ok(caller) => caller,
        Err(reason) => return answer(stream, &Response::Refused { reason }, None),
    };
    let mut line = String::new();
    if BufReader::new(stream)
        .take(MAX_LINE_BYTES)
        .read_line(&mut line)
        .is_err()
    {
        return;
    }
    let (response, fd) = match serde_json::from_str::<Envelope>(&line) {
        Ok(envelope) if envelope.protocol == PROTOCOL_VERSION => {
            service.handle(caller, envelope.request)
        }
        Ok(_) => (
            Response::Refused {
                reason: "unsupported protocol version".into(),
            },
            None,
        ),
        Err(_) => (
            Response::Refused {
                reason: "malformed request".into(),
            },
            None,
        ),
    };
    answer(stream, &response, fd.as_ref().map(AsFd::as_fd));
    // The descriptor drops here: the shell holds the only one now.
}

fn readable(listener: &UnixListener, timeout: Duration) -> bool {
    let mut poll = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: one valid pollfd for the duration of the call.
    unsafe { libc::poll(&raw mut poll, 1, millis) > 0 }
}

/// How the loop paces itself and when it ends.
#[derive(Clone, Copy, Debug)]
pub struct Pacing {
    /// How often the helper checks that what it set up still has a reason.
    pub housekeeping: Duration,
    /// Return after this long without a request while nothing is open; the
    /// socket unit starts the helper again on the next connection.
    pub idle_exit: Option<Duration>,
}

/// Serve `listener` until `stop` is set or the helper has been idle long
/// enough to exit.
pub fn serve<B: Backend>(
    listener: &UnixListener,
    service: &mut Service<B>,
    mut identify: impl FnMut(&UnixStream) -> Result<Caller, String>,
    pacing: Pacing,
    stop: &AtomicBool,
) {
    let mut tidied = Instant::now();
    let mut last_request = Instant::now();
    service.housekeeping();
    while !stop.load(Ordering::Acquire) {
        if readable(listener, pacing.housekeeping)
            && let Ok((stream, _)) = listener.accept()
        {
            serve_one(&stream, service, &mut identify);
            last_request = Instant::now();
        }
        if tidied.elapsed() >= pacing.housekeeping {
            service.housekeeping();
            tidied = Instant::now();
        }
        if pacing
            .idle_exit
            .is_some_and(|idle| service.is_idle() && last_request.elapsed() >= idle)
        {
            return;
        }
    }
}
