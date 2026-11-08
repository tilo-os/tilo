//! The messages that travel over a connected door.
//!
//! A door is a `SOCK_SEQPACKET` socket an app is handed as one of its
//! service descriptors (`runtime_portal::windows`, `registration_gate`,
//! `origin_connector`, `tilolang_host`): every request is one message of
//! NUL-separated fields, with the descriptors it carries beside it, and every
//! answer is one message back. Seqlpacket keeps message boundaries, so a verb
//! and its arguments arrive together and neither side buffers a stream.

use std::io;
#[cfg(unix)]
use std::os::fd::{BorrowedFd, OwnedFd};

/// The largest message either side sends: a verb, its fields and an answer.
pub const MAX_MESSAGE: usize = 1024;

/// The most descriptors one message carries: a tilolang instance's five
/// (`crate::tilolang_host::InstanceFds`).
#[cfg(unix)]
const MAX_FDS: usize = 5;

/// `fields` as one message: NUL-separated, so no field may contain a NUL.
pub fn encode(fields: &[&[u8]]) -> io::Result<Vec<u8>> {
    if fields.iter().any(|field| field.contains(&0)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a message field contains a NUL byte",
        ));
    }
    Ok(fields.join(&0))
}

/// The fields of `message`.
#[must_use]
pub fn decode(message: &[u8]) -> Vec<&[u8]> {
    message.split(|byte| *byte == 0).collect()
}

/// Send `fields`, with `fds` beside them, as one message over `socket`.
#[cfg(unix)]
pub fn send(socket: BorrowedFd<'_>, fields: &[&[u8]], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
    let message = encode(fields)?;
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if fds.len() > MAX_FDS
        || (!fds.is_empty() && !control.push(SendAncillaryMessage::ScmRights(fds)))
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // A peer that has gone must not signal the process. `MSG_NOSIGNAL` says
    // that where the platform has it; macOS has no such flag and its Rust
    // runtime already ignores `SIGPIPE`, so the send fails with `EPIPE`.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let flags = SendFlags::NOSIGNAL;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let flags = SendFlags::empty();
    let sent = sendmsg(socket, &[io::IoSlice::new(&message)], &mut control, flags)?;
    if sent == message.len() {
        Ok(())
    } else {
        Err(io::ErrorKind::WriteZero.into())
    }
}

/// One message and the descriptors that came with it, close-on-exec;
/// `None` at end of file.
#[cfg(unix)]
pub fn receive(
    socket: BorrowedFd<'_>,
    buffer: &mut [u8],
) -> io::Result<Option<(usize, Vec<OwnedFd>)>> {
    use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, recvmsg};
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    // `MSG_CMSG_CLOEXEC` marks the descriptors close-on-exec as they arrive.
    // A platform without it (macOS) marks them below instead.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let flags = RecvFlags::CMSG_CLOEXEC;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let flags = RecvFlags::empty();
    let received = loop {
        match recvmsg(
            socket,
            &mut [io::IoSliceMut::new(buffer)],
            &mut control,
            flags,
        ) {
            Err(rustix::io::Errno::INTR) => {}
            other => break other?,
        }
    };
    let mut fds = Vec::new();
    for message in control.drain() {
        if let RecvAncillaryMessage::ScmRights(rights) = message {
            fds.extend(rights);
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    for fd in &fds {
        rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::CLOEXEC)?;
    }
    if received
        .flags
        .intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a message was cut short",
        ));
    }
    if received.bytes == 0 && fds.is_empty() {
        return Ok(None);
    }
    Ok(Some((received.bytes, fds)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd as _;

    #[test]
    fn a_message_round_trips_with_its_descriptor() {
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let file = std::fs::File::open("/dev/null").unwrap();
        send(client.as_fd(), &[b"open", b"/a/b"], &[file.as_fd()]).unwrap();
        let mut buffer = vec![0_u8; MAX_MESSAGE];
        let (length, fds) = receive(server.as_fd(), &mut buffer).unwrap().unwrap();
        assert_eq!(decode(&buffer[..length]), [b"open".as_slice(), b"/a/b"]);
        assert_eq!(fds.len(), 1);
        assert_eq!(
            encode(&[b"a\0b"]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
