//! No ambient or keystroke fallback on hosts without the cooperative producer.
use super::{Completion, Failure, Request, Snapshot, Status};
use std::io;
use std::sync::Arc;
pub struct Socket;
impl Socket {
    #[cfg(unix)]
    /// Refuse an editor descriptor on an unsupported host.
    /// # Errors
    /// Always returns Unsupported and closes the supplied descriptor.
    pub fn adopt(_fd: std::os::fd::OwnedFd) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Native editor is unavailable on this host",
        ))
    }
}
pub struct Client;
impl Client {
    /// Refuse worker construction on an unsupported host.
    /// # Errors
    /// Always returns Unsupported.
    pub fn new(_socket: Socket, _wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Native editor is unavailable on this host",
        ))
    }
    pub fn snapshot(&self) -> Option<Snapshot> {
        None
    }
    pub fn epoch(&self) -> u64 {
        0
    }
    pub fn revoke(&self) {}
    pub fn submit(
        &self,
        _epoch: u64,
        _operation: Request,
        _expected: Status,
        answer: Box<dyn Completion>,
    ) {
        answer.finish(Err(Failure::Retired));
    }
}
