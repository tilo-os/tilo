//! The socket loop: one connection at a time, one request per connection.

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::AtomicBool;

pub use peer_facts::server::Pacing;

use super::{Backend, Caller, Service};
use crate::protocol::{MAX_LINE_BYTES, PROTOCOL_VERSION, Request, Response};

impl<B: Backend> peer_facts::server::Handler for Service<B> {
    type Caller = Caller;
    type Request = Request;
    type Response = Response;
    type Attachment = OwnedFd;

    const PROTOCOL_VERSION: u32 = PROTOCOL_VERSION;
    const MAX_LINE_BYTES: u64 = MAX_LINE_BYTES;

    fn refused(reason: String) -> Response {
        Response::Refused { reason }
    }

    fn handle(&mut self, caller: Caller, request: Request) -> (Response, Option<OwnedFd>) {
        Self::handle(self, caller, request)
    }

    fn housekeeping(&mut self) {
        Self::housekeeping(self);
    }

    fn is_idle(&self) -> bool {
        Self::is_idle(self)
    }

    fn write_reply(stream: &UnixStream, line: &[u8], fd: Option<OwnedFd>) -> io::Result<()> {
        crate::fdpass::write_line_with_fd(stream, line, fd.as_ref().map(AsFd::as_fd))
    }
}

/// Serve one connection: identify the caller before reading a byte of its
/// request, then answer it.
pub fn serve_one<B: Backend>(
    stream: &UnixStream,
    service: &mut Service<B>,
    identify: &mut impl FnMut(&UnixStream) -> Result<Caller, String>,
) {
    peer_facts::server::serve_one(stream, service, identify);
}

/// Serve `listener` until `stop` is set or the helper has been idle long
/// enough to exit.
pub fn serve<B: Backend>(
    listener: &UnixListener,
    service: &mut Service<B>,
    identify: impl FnMut(&UnixStream) -> Result<Caller, String>,
    pacing: Pacing,
    stop: &AtomicBool,
) {
    peer_facts::server::serve(listener, service, identify, pacing, stop);
}
