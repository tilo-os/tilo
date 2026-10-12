//! Linux seqpacket transport; no parser/device dependency and no ancillary authority.
use crate::FRAME_BYTES;
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    net::{
        AddressFamily, RecvAncillaryBuffer, RecvFlags, ReturnFlags, SendFlags, SocketAddrUnix,
        SocketFlags, SocketType, recvmsg, send as send_packet, socket_with, sockopt,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io::{self, IoSliceMut},
    mem::MaybeUninit,
    os::fd::{AsFd, OwnedFd},
    time::{Duration, Instant},
};
fn create() -> io::Result<OwnedFd> {
    Ok(socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )?)
}
/// Creates a socket bound to `path`; the caller sets ownership and listens.
///
/// # Errors
/// Refuses paths beyond the native sockaddr bound and failed binds.
pub fn bind(path: &str) -> io::Result<OwnedFd> {
    let fd = create()?;
    rustix::net::bind(&fd, &SocketAddrUnix::new(path)?)?;
    Ok(fd)
}
/// # Errors
/// Refuses paths beyond the native sockaddr bound and failed connects.
pub fn connect(path: &str) -> io::Result<OwnedFd> {
    let fd = create()?;
    rustix::net::connect(&fd, &SocketAddrUnix::new(path)?)?;
    Ok(fd)
}
/// Accepts one connection before `deadline`.
///
/// # Errors
/// Refuses interruption, expired deadlines and socket errors.
pub fn accept(listener: &OwnedFd, deadline: Instant) -> io::Result<OwnedFd> {
    ready(listener, PollFlags::IN, deadline)?;
    Ok(rustix::net::accept_with(
        listener,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
    )?)
}
/// # Errors
/// Refuses socket credentials that cannot be obtained from the kernel.
pub fn peer_uid(fd: &OwnedFd) -> io::Result<u32> {
    Ok(sockopt::socket_peercred(fd)?.uid.as_raw())
}
fn ready(fd: &OwnedFd, events: PollFlags, deadline: Instant) -> io::Result<()> {
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "IPC deadline"))?;
        let timeout =
            Timespec::try_from(left.min(Duration::from_secs(1))).map_err(io::Error::other)?;
        let mut fds = [PollFd::from_borrowed_fd(fd.as_fd(), events)];
        if poll(&mut fds, Some(&timeout))? != 0 {
            if fds[0].revents().intersects(events) {
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
    ready(fd, PollFlags::OUT, deadline)?;
    if send_packet(fd, &bytes, SendFlags::NOSIGNAL)? != bytes.len() {
        return Err(io::Error::other("short packet"));
    }
    Ok(())
}
/// # Errors
/// Rejects all ancillary messages, truncation, EOF and malformed messages.
pub fn receive<T: DeserializeOwned>(fd: &OwnedFd, deadline: Instant) -> io::Result<T> {
    ready(fd, PollFlags::IN, deadline)?;
    let mut bytes = vec![0u8; FRAME_BYTES];
    let mut space = [MaybeUninit::uninit(); 512];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let received = recvmsg(
        fd,
        &mut [IoSliceMut::new(&mut bytes)],
        &mut control,
        RecvFlags::CMSG_CLOEXEC,
    )?;
    // Draining closes any installed rights before rejecting; MSG_CTRUNC alone does not.
    let ancillary = control.drain().count() != 0;
    if received.bytes == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "broker connection closed",
        ));
    }
    if ancillary
        || received
            .flags
            .intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC)
    {
        return Err(io::Error::other("ancillary, truncated or closed IPC"));
    }
    bytes.truncate(received.bytes);
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
