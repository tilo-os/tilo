//! Who is on the other end of a Unix socket here.

use std::os::unix::net::UnixStream;

/// Whether the socket's peer is this user: its control sockets accept nobody else.
pub(crate) fn same_user(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd as _;
    let mut credentials = libc::ucred {
        pid: 0,
        uid: u32::MAX,
        gid: 0,
    };
    let Ok(mut length) = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()) else {
        return false;
    };
    // SAFETY: both pointers name initialised storage of `length` bytes.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut credentials).cast(),
            std::ptr::from_mut(&mut length),
        )
    };
    result == 0 && credentials.uid == rustix::process::getuid().as_raw()
}
