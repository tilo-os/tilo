//! One line over a Unix stream, with at most one file descriptor beside it.
//!
//! The helper hands the shell an opened TUN this way: the descriptor rides
//! as `SCM_RIGHTS` with the first byte of the answer line.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

/// Room for one descriptor's control message.
const CONTROL_BYTES: usize = 64;

/// Write `line` to `stream`, with `fd` attached to its first byte.
///
/// # Errors
///
/// The write fails.
pub fn write_line_with_fd(
    stream: &UnixStream,
    line: &[u8],
    fd: Option<BorrowedFd<'_>>,
) -> io::Result<()> {
    let Some(fd) = fd else {
        return write_all(stream, line);
    };
    let Some((first, rest)) = line.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a descriptor needs a byte to ride with",
        ));
    };
    let mut byte = [*first];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0_u8; CONTROL_BYTES];
    // SAFETY: a zeroed msghdr is a valid empty header; every pointer set
    // below outlives the sendmsg call.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    let fd_bytes = u32::try_from(size_of::<RawFd>()).map_err(io::Error::other)?;
    // SAFETY: CMSG_SPACE only computes a size.
    let space = unsafe { libc::CMSG_SPACE(fd_bytes) } as usize;
    if space > control.len() {
        return Err(io::Error::other("control buffer too small"));
    }
    message.msg_controllen = space as _;
    // SAFETY: the header points at `control`, which has room for one
    // descriptor's message, so the first header is in bounds.
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&raw const message);
        if header.is_null() {
            return Err(io::Error::other("no room for the descriptor"));
        }
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(fd_bytes) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), fd.as_raw_fd());
    }
    loop {
        // SAFETY: `message` is fully initialised and its buffers live.
        let sent =
            unsafe { libc::sendmsg(stream.as_raw_fd(), &raw const message, libc::MSG_NOSIGNAL) };
        if sent == 1 {
            break;
        }
        let error = io::Error::last_os_error();
        if sent < 0 && error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(if sent < 0 {
            error
        } else {
            io::Error::new(io::ErrorKind::WriteZero, "the descriptor was not sent")
        });
    }
    write_all(stream, rest)
}

fn write_all(mut stream: &UnixStream, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    stream.write_all(bytes)
}

/// Read one line (without its newline) of at most `max` bytes from `stream`,
/// and the first descriptor that came with it. Any further descriptors are
/// closed.
///
/// # Errors
///
/// The read fails, the line is longer than `max`, or the stream ends first.
pub fn read_line_with_fd(stream: &UnixStream, max: u64) -> io::Result<(Vec<u8>, Option<OwnedFd>)> {
    let max = usize::try_from(max).unwrap_or(usize::MAX);
    let mut line = Vec::new();
    let mut fd: Option<OwnedFd> = None;
    // One byte at a time until the newline, so nothing past this line is
    // taken from the stream.
    loop {
        let mut byte = [0_u8; 1];
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: 1,
        };
        let mut control = [0_u8; CONTROL_BYTES];
        // SAFETY: as above.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &raw mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control.len() as _;
        // SAFETY: `message` points at live buffers of the sizes it names.
        let read =
            unsafe { libc::recvmsg(stream.as_raw_fd(), &raw mut message, libc::MSG_CMSG_CLOEXEC) };
        if read < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        // SAFETY: the kernel filled `control` up to `msg_controllen`; the
        // CMSG macros walk only within it.
        unsafe {
            let mut header = libc::CMSG_FIRSTHDR(&raw const message);
            while !header.is_null() {
                if (*header).cmsg_level == libc::SOL_SOCKET
                    && (*header).cmsg_type == libc::SCM_RIGHTS
                {
                    let data = libc::CMSG_DATA(header);
                    let payload =
                        ((*header).cmsg_len as usize).saturating_sub(libc::CMSG_LEN(0) as usize);
                    for index in 0..payload.checked_div(size_of::<RawFd>()).unwrap_or(0) {
                        let raw = std::ptr::read_unaligned(data.cast::<RawFd>().add(index));
                        let owned = OwnedFd::from_raw_fd(raw);
                        if fd.is_none() {
                            fd = Some(owned);
                        }
                    }
                }
                header = libc::CMSG_NXTHDR(&raw const message, header);
            }
        }
        if message.msg_flags & libc::MSG_CTRUNC != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "more descriptors came than one",
            ));
        }
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the stream ended before the line did",
            ));
        }
        if byte[0] == b'\n' {
            return Ok((line, fd));
        }
        if line.len() >= max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the line is too long",
            ));
        }
        line.push(byte[0]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Seek as _, Write as _};

    #[test]
    fn a_descriptor_travels_with_its_line() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"tun").unwrap();
        file.rewind().unwrap();
        write_line_with_fd(
            &a,
            b"{\"result\":\"opened\"}\n",
            Some(std::os::fd::AsFd::as_fd(&file)),
        )
        .unwrap();
        drop(file);
        let (line, fd) = read_line_with_fd(&b, 1024).unwrap();
        assert_eq!(line, b"{\"result\":\"opened\"}");
        let mut received = std::fs::File::from(fd.expect("a descriptor"));
        let mut text = String::new();
        received.read_to_string(&mut text).unwrap();
        assert_eq!(text, "tun");
    }

    #[test]
    fn a_line_without_a_descriptor_and_a_long_line_are_told_apart() {
        let (a, b) = UnixStream::pair().unwrap();
        write_line_with_fd(&a, b"done\n", None).unwrap();
        let (line, fd) = read_line_with_fd(&b, 1024).unwrap();
        assert_eq!(line, b"done");
        assert!(fd.is_none());
        write_line_with_fd(&a, b"0123456789\n", None).unwrap();
        assert!(read_line_with_fd(&b, 4).is_err());
    }
}
