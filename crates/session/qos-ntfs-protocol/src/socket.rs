//! Linux seqpacket transport; no parser/device dependency and no ancillary authority.
use crate::FRAME_BYTES;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    ffi::CString,
    io, mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    time::Instant,
};
fn os(result: i32) -> io::Result<i32> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}
/// # Errors
/// Refuses unsupported socket creation.
pub fn create() -> io::Result<OwnedFd> {
    let fd = os(unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    })?;
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
/// # Errors
/// Refuses paths beyond the native sockaddr bound.
pub fn address(path: &str) -> io::Result<libc::sockaddr_un> {
    let name = CString::new(path).map_err(io::Error::other)?;
    let mut addr: libc::sockaddr_un = unsafe { mem::zeroed() };
    addr.sun_family = libc::sa_family_t::try_from(libc::AF_UNIX).map_err(io::Error::other)?;
    if name.as_bytes_with_nul().len() > addr.sun_path.len() {
        return Err(io::Error::other("socket locator limit"));
    }
    for (to, from) in addr.sun_path.iter_mut().zip(name.as_bytes_with_nul()) {
        *to = *from as libc::c_char;
    }
    Ok(addr)
}
/// # Errors
/// Refuses socket credentials that cannot be obtained from the kernel.
pub fn peer_uid(fd: &OwnedFd) -> io::Result<u32> {
    let mut cred: libc::ucred = unsafe { mem::zeroed() };
    let mut size =
        libc::socklen_t::try_from(mem::size_of::<libc::ucred>()).map_err(io::Error::other)?;
    os(unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &raw mut size,
        )
    })?;
    if size as usize != mem::size_of::<libc::ucred>() {
        return Err(io::Error::other("peer credentials size"));
    }
    Ok(cred.uid)
}
/// # Errors
/// Refuses interruption, expired deadlines and socket errors.
pub fn ready(fd: &OwnedFd, events: i16, deadline: Instant) -> io::Result<()> {
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "IPC deadline"))?;
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events,
            revents: 0,
        };
        let rc = os(unsafe {
            libc::poll(
                &raw mut poll,
                1,
                i32::try_from(left.as_millis().min(1000)).map_err(io::Error::other)?,
            )
        })?;
        if rc != 0 {
            if poll.revents & events != 0 {
                return Ok(());
            }
            return Err(io::Error::other("IPC closed"));
        }
    }
}
/// # Errors
/// Refuses oversized messages, stalled peers and short sends.
pub fn send<T: Serialize>(fd: &OwnedFd, value: &T, deadline: Instant) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > FRAME_BYTES {
        return Err(io::Error::other("IPC frame limit"));
    }
    ready(fd, libc::POLLOUT, deadline)?;
    let n = unsafe {
        libc::send(
            fd.as_raw_fd(),
            bytes.as_ptr().cast(),
            bytes.len(),
            libc::MSG_NOSIGNAL,
        )
    };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    if usize::try_from(n).map_err(io::Error::other)? != bytes.len() {
        return Err(io::Error::other("short packet"));
    }
    Ok(())
}
/// # Errors
/// Rejects all ancillary messages, truncation, EOF and malformed messages.
pub fn receive<T: DeserializeOwned>(fd: &OwnedFd, deadline: Instant) -> io::Result<T> {
    ready(fd, libc::POLLIN, deadline)?;
    let mut bytes = vec![0u8; FRAME_BYTES];
    let mut control = [0usize; 64];
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &raw mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = mem::size_of_val(&control);
    let n = unsafe { libc::recvmsg(fd.as_raw_fd(), &raw mut msg, libc::MSG_CMSG_CLOEXEC) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    // Close any installed rights before rejecting. MSG_CTRUNC alone does not close delivered descriptors.
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&raw const msg);
        while !header.is_null() {
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let size = (*header)
                    .cmsg_len
                    .saturating_sub(libc::CMSG_LEN(0) as usize);
                let data = std::slice::from_raw_parts(libc::CMSG_DATA(header), size);
                for chunk in data.chunks_exact(mem::size_of::<i32>()) {
                    if let Ok(bytes) = chunk.try_into() {
                        libc::close(i32::from_ne_bytes(bytes));
                    }
                }
            }
            header = libc::CMSG_NXTHDR(&raw const msg, header);
        }
    }
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "broker connection closed",
        ));
    }
    if msg.msg_controllen != 0 || msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
        return Err(io::Error::other("ancillary, truncated or closed IPC"));
    }
    bytes.truncate(usize::try_from(n).map_err(io::Error::other)?);
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
