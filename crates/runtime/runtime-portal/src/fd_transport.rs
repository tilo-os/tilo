//! Unix descriptor transport shared by service clients and the Linux supervisor.
use std::io;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};

/// Send a payload with one borrowed descriptor.
///
/// # Errors
/// Returns the socket error or an error for an empty or partially sent payload.
pub fn send_with_fd(sock: RawFd, payload: &[u8], fd: RawFd) -> io::Result<()> {
    send_with_fds(sock, payload, &[fd])
}

/// Send one payload with a bounded descriptor bundle, preserving its order.
///
/// The extra descriptors carry explicitly negotiated capabilities (a session
/// lease, an editor socket); they are not accepted by the existing single-descriptor receive operation.
///
/// # Errors
/// Rejects empty payloads, empty or oversized bundles, and partial sends.
pub fn send_with_fds(sock: RawFd, payload: &[u8], fds: &[RawFd]) -> io::Result<()> {
    if payload.is_empty() || fds.is_empty() || fds.len() > 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "descriptor bundle needs a payload and one to three descriptors",
        ));
    }
    let mut iov = libc::iovec {
        iov_base: payload.as_ptr().cast_mut().cast(),
        iov_len: payload.len(),
    };
    // cmsghdr requires native alignment; a byte array does not provide it.
    let mut control = [0usize; 8];
    let rights_bytes = u32::try_from(std::mem::size_of_val(fds))
        .map_err(|_| io::Error::other("invalid descriptor bundle length"))?;
    // SAFETY: each pointer is aligned, initialized and live for sendmsg; CMSG_SPACE
    // for three descriptors fits the control buffer on supported Unix targets.
    let sent = unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &raw mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = libc::CMSG_SPACE(rights_bytes) as _;
        let header = libc::CMSG_FIRSTHDR(&raw const msg);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(rights_bytes) as _;
        std::ptr::copy_nonoverlapping(
            fds.as_ptr(),
            libc::CMSG_DATA(header).cast::<RawFd>(),
            fds.len(),
        );
        libc::sendmsg(sock, &raw const msg, 0)
    };
    if sent < 0 {
        return Err(io::Error::last_os_error());
    }
    if usize::try_from(sent).ok() != Some(payload.len()) {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "partial descriptor message",
        ));
    }
    Ok(())
}

/// Receive a payload and transfer ownership of its optional descriptor to the caller.
///
/// # Errors
/// Returns socket errors or rejects truncated messages and unexpected descriptor counts.
/// All descriptors received on an error path are closed.
pub fn recv_with_fd(sock: RawFd) -> io::Result<(Vec<u8>, Option<RawFd>)> {
    recv_with_owned_fd(sock).map(|(bytes, fd)| (bytes, fd.map(IntoRawFd::into_raw_fd)))
}

/// Owned counterpart of [`recv_with_fd`], for clients that validate the payload.
///
/// # Errors
/// Same errors as [`recv_with_fd`].
pub fn recv_with_owned_fd(sock: RawFd) -> io::Result<(Vec<u8>, Option<OwnedFd>)> {
    let mut buf = [0u8; 4096];
    recv_into(sock, &mut buf)
}

/// Receive an explicitly negotiated bundle of at most three owned descriptors.
///
/// The caller must validate the message's declared roles and exact count before
/// using any descriptor. Malformed/truncated messages close every received FD.
///
/// # Errors
/// Returns transport errors or rejects an oversized/truncated descriptor bundle.
pub fn recv_with_owned_fds(sock: RawFd) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    recv_bundle(sock, &mut [0u8; 4096], 3)
}

/// Receive bounded data, refusing and closing every ancillary descriptor.
/// # Errors
/// Rejects empty/oversized buffers, truncation and unexpected rights.
#[cfg(target_os = "linux")]
pub(crate) fn recv_data(sock: RawFd, buffer: &mut [u8]) -> io::Result<usize> {
    if buffer.is_empty() || buffer.len() > 16384 {
        return Err(io::Error::other("invalid data receive bound"));
    }
    recv_bundle(sock, buffer, 0).map(|(bytes, _)| bytes.len())
}

fn recv_into(sock: RawFd, buf: &mut [u8]) -> io::Result<(Vec<u8>, Option<OwnedFd>)> {
    recv_bundle(sock, buf, 1).map(|(bytes, mut fds)| (bytes, fds.pop()))
}

fn recv_bundle(sock: RawFd, buf: &mut [u8], maximum: usize) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    // XNU sockargs rejects MT_CONTROL above MCLBYTES (2048), before
    // unp_internalize. Receive the full bound: Darwin externalizes all rights
    // before truncating copyout, leaking descriptors omitted by a smaller buffer.
    // https://github.com/apple-oss-distributions/xnu/blob/ac9718fb1af618d5ce8678d0dc6e8a58f252216f/bsd/kern/uipc_syscalls.c#L3284
    let mut control = [0usize; 2048 / std::mem::size_of::<usize>()];
    let mut descriptors = Vec::new();
    // Linux can atomically set CLOEXEC at receipt. Darwin requires fcntl below.
    #[cfg(target_os = "linux")]
    let flags = libc::MSG_CMSG_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = 0;
    // SAFETY: recvmsg writes only to initialized, aligned local buffers. The kernel
    // supplies valid cmsg headers. Each SCM_RIGHTS fd is immediately owned, including
    // descriptors delivered with MSG_CTRUNC, so every rejection closes them.
    let (received, truncated) = unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &raw mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = 2048;
        let received = libc::recvmsg(sock, &raw mut msg, flags);
        if received < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut header = libc::CMSG_FIRSTHDR(&raw const msg);
        while !header.is_null() {
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let offset =
                    usize::try_from(libc::CMSG_DATA(header).offset_from(control.as_ptr().cast()))
                        .unwrap_or(usize::MAX);
                #[cfg(any(target_os = "linux", target_os = "android"))]
                let (control_len, header_len) = (msg.msg_controllen, (*header).cmsg_len);
                #[cfg(not(any(target_os = "linux", target_os = "android")))]
                let (control_len, header_len) = (
                    usize::try_from(msg.msg_controllen).unwrap_or(0),
                    usize::try_from((*header).cmsg_len).unwrap_or(0),
                );
                let available = control_len
                    .min(std::mem::size_of_val(&control))
                    .saturating_sub(offset);
                let size = header_len
                    .saturating_sub(libc::CMSG_LEN(0) as usize)
                    .min(available);
                for index in 0..size.checked_div(std::mem::size_of::<RawFd>()).unwrap_or(0) {
                    let raw = std::ptr::read_unaligned(
                        libc::CMSG_DATA(header).cast::<RawFd>().add(index),
                    );
                    descriptors.push(OwnedFd::from_raw_fd(raw));
                }
            }
            header = libc::CMSG_NXTHDR(&raw const msg, header);
        }
        (
            received,
            msg.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0,
        )
    };
    if truncated || descriptors.len() > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated or oversized descriptor message",
        ));
    }
    for fd in &descriptors {
        rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::CLOEXEC)?;
    }
    let length =
        usize::try_from(received).map_err(|_| io::Error::other("invalid receive length"))?;
    let bytes = buf
        .get(..length)
        .ok_or_else(|| io::Error::other("oversized receive length"))?;
    Ok((bytes.to_vec(), descriptors))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::net::{UnixDatagram, UnixStream};

    #[test]
    fn a_descriptor_survives_the_socket_and_arrives_close_on_exec() {
        // The descriptor is live on the far side.
        {
            let (a, b) = UnixStream::pair().unwrap();
            let (mut source, target) = UnixStream::pair().unwrap();
            send_with_fd(a.as_raw_fd(), b"session", target.as_raw_fd()).unwrap();
            let (payload, fd) = recv_with_fd(b.as_raw_fd()).unwrap();
            assert_eq!(payload, b"session");
            let mut received = unsafe { UnixStream::from_raw_fd(fd.unwrap()) };
            source.write_all(b"hello").unwrap();
            let mut bytes = [0; 5];
            received.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"hello");
        }
        // And it is close-on-exec.
        {
            let (a, b) = UnixStream::pair().unwrap();
            send_with_fd(a.as_raw_fd(), b"session", a.as_raw_fd()).unwrap();
            let (_, fd) = recv_with_fd(b.as_raw_fd()).unwrap();
            let received = unsafe { UnixStream::from_raw_fd(fd.unwrap()) };
            let flags = unsafe { libc::fcntl(received.as_raw_fd(), libc::F_GETFD) };
            assert_ne!(flags & libc::FD_CLOEXEC, 0);
        }
    }

    #[test]
    fn negotiated_bundle_preserves_roles_and_closes_on_exec() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let (mut first, first_fd) = UnixStream::pair().unwrap();
        let (mut second, second_fd) = UnixStream::pair().unwrap();
        send_with_fds(
            sender.as_raw_fd(),
            b"pty-and-editor-v1",
            &[first_fd.as_raw_fd(), second_fd.as_raw_fd()],
        )
        .unwrap();
        let (payload, fds) = recv_with_owned_fds(receiver.as_raw_fd()).unwrap();
        assert_eq!(payload, b"pty-and-editor-v1");
        assert_eq!(fds.len(), 2);
        first.write_all(b"P").unwrap();
        second.write_all(b"E").unwrap();
        for (fd, expected) in fds.into_iter().zip(*b"PE") {
            assert_ne!(
                unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
            let mut stream = UnixStream::from(fd);
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            assert_eq!(byte, [expected]);
        }
    }

    #[test]
    fn oversized_negotiated_bundle_closes_every_received_descriptor() {
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        let (mut reader, writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        send_many(&sender, b"invalid", &[writer.as_raw_fd(); 4]);
        drop(writer);
        assert!(recv_with_owned_fds(receiver.as_raw_fd()).is_err());
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
        let (a, b) = UnixDatagram::pair().unwrap();
        let (mut reader, writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        send_many(&a, &vec![1; bytes], &vec![writer.as_raw_fd(); count]);
        drop(writer);
        let received = recv_into(b.as_raw_fd(), &mut [0; 64]);
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
            // Excess descriptors previously truncated the control message.
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
