//! The sockets this session serves, and the loop's wait on them.
//! Every listener the session serves is drained on its own pass: nothing of the desk is read from
//! another thread. What arrives must therefore end the loop's wait, or a client that connects to an
//! idle desk — every app window's first ask — would sit in the socket's backlog until something
//! else happened.

use std::io;
use std::os::fd::{AsFd, OwnedFd};

use tilo_compositor::{Compositor, ReadableWatch};

/// End the loop's wait when `listener` has a connection waiting, so the pass
/// that follows takes it.
///
/// The subscription reads nothing itself: the service that owns the socket
/// takes what is waiting on its own pass, as it takes it without this.
///
/// # Errors
/// The descriptor could not be duplicated for the loop, or the loop took no
/// more sources.
pub(crate) fn watch(
    compositor: &mut Compositor,
    listener: &impl AsFd,
) -> io::Result<ReadableWatch> {
    let fd: OwnedFd = listener.as_fd().try_clone_to_owned()?;
    compositor.watch_readable(fd).map_err(io::Error::other)
}
