//! Unix descriptor transport shared by service clients and the Linux supervisor.
use std::io::{self, IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags,
};

/// The most descriptors one bundle carries: a session's PTY master, its
/// private lease and, when negotiated, its editor capability.
const MAX_BUNDLE: usize = 3;

/// Send a payload with one borrowed descriptor.
pub fn send_with_fd(sock: impl AsFd, payload: &[u8], fd: impl AsFd) -> io::Result<()> {
    send_with_fds(sock, payload, &[fd.as_fd()])
}

/// Send one payload with a bounded descriptor bundle, preserving its order.
///
/// The extra descriptors carry explicitly negotiated capabilities (a session
/// lease, an editor socket); they are not accepted by the single-descriptor
/// receive operation.
///
/// # Errors
/// Rejects empty payloads, empty or oversized bundles, and partial sends.
pub fn send_with_fds(sock: impl AsFd, payload: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    if payload.is_empty() || fds.is_empty() || fds.len() > MAX_BUNDLE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "descriptor bundle needs a payload and one to three descriptors",
        ));
    }
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_BUNDLE))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !control.push(SendAncillaryMessage::ScmRights(fds)) {
        return Err(io::Error::other(
            "descriptor bundle exceeds the control buffer",
        ));
    }
    let sent = rustix::net::sendmsg(
        sock,
        &[IoSlice::new(payload)],
        &mut control,
        SendFlags::empty(),
    )?;
    if sent != payload.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "partial descriptor message",
        ));
    }
    Ok(())
}

/// Receive a payload and transfer ownership of its optional descriptor to the caller.
pub fn recv_with_fd(sock: impl AsFd) -> io::Result<(Vec<u8>, Option<OwnedFd>)> {
    recv_into(sock, &mut [0u8; 4096])
}

/// Receive an explicitly negotiated bundle of at most three owned descriptors.
///
/// The caller must validate the message's declared roles and exact count before
/// using any descriptor. Malformed/truncated messages close every received FD.
pub fn recv_with_fds(sock: impl AsFd) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    recv_bundle(sock, &mut [0u8; 4096], MAX_BUNDLE)
}

/// Receive bounded data, refusing and closing every ancillary descriptor.
/// # Errors
/// Rejects empty/oversized buffers, truncation and unexpected rights.
#[cfg(target_os = "linux")]
pub(crate) fn recv_data(sock: impl AsFd, buffer: &mut [u8]) -> io::Result<usize> {
    if buffer.is_empty() || buffer.len() > 16384 {
        return Err(io::Error::other("invalid data receive bound"));
    }
    recv_bundle(sock, buffer, 0).map(|(bytes, _)| bytes.len())
}

fn recv_into(sock: impl AsFd, buf: &mut [u8]) -> io::Result<(Vec<u8>, Option<OwnedFd>)> {
    recv_bundle(sock, buf, 1).map(|(bytes, mut fds)| (bytes, fds.pop()))
}

fn recv_bundle(
    sock: impl AsFd,
    buf: &mut [u8],
    maximum: usize,
) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    // XNU sockargs rejects MT_CONTROL above MCLBYTES (2048), before
    // unp_internalize. Receive the full bound: Darwin externalizes all rights
    // before truncating copyout, leaking descriptors omitted by a smaller buffer.
    // https://github.com/apple-oss-distributions/xnu/blob/ac9718fb1af618d5ce8678d0dc6e8a58f252216f/bsd/kern/uipc_syscalls.c#L3284
    let mut space = [MaybeUninit::uninit(); 2048];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    // Linux can atomically set CLOEXEC at receipt. Darwin requires fcntl below.
    #[cfg(target_os = "linux")]
    let flags = RecvFlags::CMSG_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = RecvFlags::empty();
    let received = rustix::net::recvmsg(sock, &mut [IoSliceMut::new(buf)], &mut control, flags)?;
    // Every SCM_RIGHTS descriptor is owned immediately, including those
    // delivered with MSG_CTRUNC, so every rejection below closes them.
    let mut descriptors = Vec::new();
    for message in control.drain() {
        if let RecvAncillaryMessage::ScmRights(fds) = message {
            descriptors.extend(fds);
        }
    }
    if received
        .flags
        .intersects(ReturnFlags::CTRUNC | ReturnFlags::TRUNC)
        || descriptors.len() > maximum
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated or oversized descriptor message",
        ));
    }
    for fd in &descriptors {
        rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::CLOEXEC)?;
    }
    let bytes = buf
        .get(..received.bytes)
        .ok_or_else(|| io::Error::other("oversized receive length"))?;
    Ok((bytes.to_vec(), descriptors))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, RawFd};
    use std::os::unix::net::{UnixDatagram, UnixStream};

    #[test]
    fn a_descriptor_survives_the_socket_and_arrives_close_on_exec() {
        {
            let (a, b) = UnixStream::pair().unwrap();
            let (mut source, target) = UnixStream::pair().unwrap();
            send_with_fd(&a, b"session", &target).unwrap();
            let (payload, fd) = recv_with_fd(&b).unwrap();
            assert_eq!(payload, b"session");
            let mut received = UnixStream::from(fd.unwrap());
            source.write_all(b"hello").unwrap();
            let mut bytes = [0; 5];
            received.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"hello");
        }
        {
            let (a, b) = UnixStream::pair().unwrap();
            send_with_fd(&a, b"session", &a).unwrap();
            let (_, fd) = recv_with_fd(&b).unwrap();
            let flags = rustix::io::fcntl_getfd(fd.unwrap()).unwrap();
            assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
        }
    }

    #[test]
    fn negotiated_bundle_preserves_roles_and_closes_on_exec() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let (mut first, first_fd) = UnixStream::pair().unwrap();
        let (mut second, second_fd) = UnixStream::pair().unwrap();
        send_with_fds(
            &sender,
            b"pty-and-editor-v1",
            &[first_fd.as_fd(), second_fd.as_fd()],
        )
        .unwrap();
        let (payload, fds) = recv_with_fds(&receiver).unwrap();
        assert_eq!(payload, b"pty-and-editor-v1");
        assert_eq!(fds.len(), 2);
        first.write_all(b"P").unwrap();
        second.write_all(b"E").unwrap();
        for (fd, expected) in fds.into_iter().zip(*b"PE") {
            assert!(
                rustix::io::fcntl_getfd(&fd)
                    .unwrap()
                    .contains(rustix::io::FdFlags::CLOEXEC)
            );
            let mut stream = UnixStream::from(fd);
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            assert_eq!(byte, [expected]);
        }
    }

    #[test]
    fn oversized_negotiated_bundle_closes_every_received_descriptor() {
        let _quiet = crate::no_spawns();
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        let (mut reader, writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        send_many(&sender, b"invalid", &[writer.as_raw_fd(); MAX_BUNDLE + 1]);
        drop(writer);
        assert!(recv_with_fds(&receiver).is_err());
        assert_eq!(reader.read(&mut [0]).unwrap(), 0);
    }

    fn send_many(socket: &UnixDatagram, payload: &[u8], fds: &[RawFd]) {
        let size: libc::c_int = 16384;
        unsafe {
            assert_eq!(
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&raw const size).cast(),
                    std::mem::size_of_val(&size).try_into().unwrap()
                ),
                0
            );
        }
        let mut control = [0usize; 2048 / std::mem::size_of::<usize>()];
        let mut iov = libc::iovec {
            iov_base: payload.as_ptr().cast_mut().cast(),
            iov_len: payload.len(),
        };
        unsafe {
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = &raw mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            let size = u32::try_from(std::mem::size_of_val(fds)).unwrap();
            msg.msg_controllen = libc::CMSG_SPACE(size) as _;
            let header = libc::CMSG_FIRSTHDR(&raw const msg);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(size) as _;
            std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(header).cast(), fds.len());
            assert!(libc::sendmsg(socket.as_raw_fd(), &raw const msg, 0) > 0);
        }
    }

    fn rejected_message_closes_descriptors(count: usize, bytes: usize) {
        // No sibling test may have a child between its fork and its exec while
        // this waits for the last copy of `writer` to close.
        let _quiet = crate::no_spawns();
        let (a, b) = UnixDatagram::pair().unwrap();
        let (mut reader, writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        send_many(&a, &vec![1; bytes], &vec![writer.as_raw_fd(); count]);
        drop(writer);
        let received = recv_into(&b, &mut [0; 64]);
        assert!(
            received.is_err(),
            "unexpected accepted message: {received:?}"
        );
        let mut byte = [0];
        assert_eq!(
            reader.read(&mut byte).unwrap(),
            0,
            "a rejected descriptor leaked"
        );
    }

    #[test]
    fn rejected_messages_close_every_delivered_descriptor() {
        #[cfg(target_os = "macos")]
        let maximum_rights = 254;
        #[cfg(not(target_os = "macos"))]
        let maximum_rights = 253;
        // (case, descriptor count, payload bytes)
        let cases = [
            ("multiple descriptors are rejected and closed", 2, 1),
            ("excess descriptors that truncated control", 32, 1),
            ("maximum rights message", maximum_rights, 1),
            ("truncated payload closes the delivered descriptor", 1, 1024),
        ];
        for (case, count, bytes) in cases {
            eprintln!("case: {case}");
            rejected_message_closes_descriptors(count, bytes);
        }
    }
}
