//! The socket loop: one connection at a time, one request per connection.
//!
//! Serving connections one by one keeps the service single-threaded, which
//! is what lets it set up each confined helper between `fork` and `exec`
//! without another thread holding a lock the child would inherit.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{Backend, Caller, Service};
use crate::protocol::{Envelope, MAX_LINE_BYTES, PROTOCOL_VERSION, Response};

/// How long one client may take to send its request or read the answer.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

fn answer(stream: &mut UnixStream, response: &Response) {
    if let Ok(mut line) = serde_json::to_vec(response) {
        line.push(b'\n');
        let _ = stream.write_all(&line);
    }
}

fn serve_one<B: Backend>(
    mut stream: UnixStream,
    service: &mut Service<B>,
    identify: &mut impl FnMut(&UnixStream) -> Result<Caller, String>,
) {
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(CLIENT_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(CLIENT_TIMEOUT)).is_err()
    {
        return;
    }
    let caller = match identify(&stream) {
        Ok(caller) => caller,
        Err(reason) => return answer(&mut stream, &Response::Refused { reason }),
    };
    let mut line = String::new();
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    if BufReader::new(clone)
        .take(MAX_LINE_BYTES)
        .read_line(&mut line)
        .is_err()
    {
        return;
    }
    let response = match serde_json::from_str::<Envelope>(&line) {
        Ok(envelope) if envelope.protocol == PROTOCOL_VERSION => {
            service.handle(caller, envelope.request)
        }
        Ok(_) => Response::Refused {
            reason: "unsupported protocol version".into(),
        },
        Err(_) => Response::Refused {
            reason: "malformed request".into(),
        },
    };
    answer(&mut stream, &response);
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
    /// How often open drives are checked (unplugged, owner gone).
    pub housekeeping: Duration,
    /// Return after this long without a request while no drive is open; the
    /// socket unit starts the service again on the next connection.
    pub idle_exit: Option<Duration>,
}

/// Serve `listener` until `stop` is set or the service has been idle long
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
    while !stop.load(Ordering::Acquire) {
        if readable(
            listener,
            pacing.housekeeping.min(Duration::from_millis(500)),
        ) && let Ok((stream, _)) = listener.accept()
        {
            serve_one(stream, service, &mut identify);
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
