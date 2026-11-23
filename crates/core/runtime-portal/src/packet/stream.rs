//! Packets on a stream socket, where local sockets have no packet type (macOS: an `AF_UNIX`
//! socket there is a stream or a datagram, and a datagram pair never reads its peer's end). Each
//! packet is its length, four bytes little-endian, then its bytes, with the descriptors it carries
//! beside its first bytes. A packet is written whole under its socket's send lock and read whole
//! under its receive lock, so two threads on one link never interleave or split one.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd};
use std::sync::{Arc, Mutex, PoisonError};

use super::MAX_PACKET;
use crate::process_link::MAX_FDS;

/// The length a packet starts with.
const HEADER: usize = 4;

/// The locks of one socket, which every descriptor of it shares: a link's `try_clone` is the same
/// socket.
#[derive(Default)]
struct Locks {
    send: Mutex<()>,
    receive: Mutex<()>,
}

/// The locks of every socket a link of this process is on, by its device and inode.
type Sockets = HashMap<(i64, u64), Arc<Locks>>;

/// The locks of the socket behind `socket`, by its device and inode.
fn locks(socket: BorrowedFd<'_>) -> io::Result<Arc<Locks>> {
    static LOCKS: Mutex<Option<Sockets>> = Mutex::new(None);
    // SAFETY: an all-zero `stat` is a valid value of this plain C struct; `fstat` fills it.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `socket` is an open descriptor this call borrows, and `stat` is writable.
    if unsafe { libc::fstat(socket.as_raw_fd(), &raw mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let key = (i64::from(stat.st_dev), stat.st_ino);
    let mut held = LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
    let all = held.get_or_insert_with(HashMap::new);
    // A socket closed since leaves its locks unused: let them go as another socket comes.
    all.retain(|_, locks| Arc::strong_count(locks) > 1);
    let locks = Arc::clone(all.entry(key).or_default());
    drop(held);
    Ok(locks)
}

/// What each end of a link buffers: a few whole packets, as a Linux packet socket holds them, so
/// a packet is sent without waiting for its peer to read (a stream socket here buffers 8 KiB).
const BUFFERED: usize = 4 * MAX_PACKET;

/// A connected pair of stream sockets, each closed on exec, never signalling on a write to a peer
/// that has gone, and buffering a few whole packets.
pub(super) fn pair() -> io::Result<(OwnedFd, OwnedFd)> {
    use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};
    let (a, b) = socketpair(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::empty(),
        None,
    )?;
    let buffered = libc::c_int::try_from(BUFFERED).map_err(|_| io::ErrorKind::InvalidInput)?;
    for end in [&a, &b] {
        rustix::io::fcntl_setfd(end, rustix::io::FdFlags::CLOEXEC)?;
        for (option, value) in [
            (libc::SO_NOSIGPIPE, 1),
            (libc::SO_SNDBUF, buffered),
            (libc::SO_RCVBUF, buffered),
        ] {
            set_option(end.as_fd(), option, value)?;
        }
    }
    Ok((a, b))
}

/// Set the socket-level int option `option` of `socket` to `value`.
fn set_option(socket: BorrowedFd<'_>, option: libc::c_int, value: libc::c_int) -> io::Result<()> {
    // SAFETY: setting an int socket option on an open socket this call borrows, from an int that
    // outlives the call.
    let set = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            option,
            (&raw const value).cast(),
            libc::socklen_t::try_from(std::mem::size_of_val(&value)).unwrap_or(4),
        )
    };
    if set != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Write `packet` on `socket` whole, with `fds` beside its first bytes.
pub(super) fn send(
    socket: BorrowedFd<'_>,
    packet: &[u8],
    fds: &[BorrowedFd<'_>],
) -> io::Result<()> {
    use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
    let length = u32::try_from(packet.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    if fds.len() > MAX_FDS {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut framed = Vec::with_capacity(HEADER.saturating_add(packet.len()));
    framed.extend_from_slice(&length.to_le_bytes());
    framed.extend_from_slice(packet);
    let locks = locks(socket)?;
    let _sending = locks.send.lock().unwrap_or_else(PoisonError::into_inner);
    let mut sent = 0;
    while sent < framed.len() {
        let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        if sent == 0 && !fds.is_empty() && !control.push(SendAncillaryMessage::ScmRights(fds)) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let rest = framed.get(sent..).unwrap_or_default();
        match sendmsg(
            socket,
            &[io::IoSlice::new(rest)],
            &mut control,
            SendFlags::empty(),
        ) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => sent = sent.saturating_add(written),
            Err(rustix::io::Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// The next packet on `socket` and the descriptors beside it; `None` once the peer is gone. When
/// `wait` is false, [`io::ErrorKind::WouldBlock`] at once if none has begun to arrive.
pub(super) fn receive(
    socket: BorrowedFd<'_>,
    wait: bool,
) -> io::Result<Option<(Vec<u8>, Vec<OwnedFd>)>> {
    let locks = locks(socket)?;
    let _receiving = if wait {
        locks.receive.lock().unwrap_or_else(PoisonError::into_inner)
    } else {
        match locks.receive.try_lock() {
            Ok(held) => held,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            // Another thread is reading this link: nothing is waiting for this one.
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err(io::ErrorKind::WouldBlock.into());
            }
        }
    };
    if !wait && !readable(socket)? {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    let mut fds = Vec::new();
    let mut header = [0_u8; HEADER];
    if !read_whole(socket, &mut header, &mut fds)? {
        return Ok(None);
    }
    let length = usize::try_from(u32::from_le_bytes(header)).unwrap_or(usize::MAX);
    if length > MAX_PACKET {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a packet is longer than any packet",
        ));
    }
    let mut body = vec![0_u8; length];
    if !read_whole(socket, &mut body, &mut fds)? {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the link ended inside a packet",
        ));
    }
    Ok(Some((body, fds)))
}

/// Whether `socket` has something to read, or its peer is gone: never waits.
pub(super) fn readable(socket: BorrowedFd<'_>) -> io::Result<bool> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let mut polled = [PollFd::new(&socket, PollFlags::IN)];
    let now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    poll(&mut polled, Some(&now))?;
    Ok(polled[0]
        .revents()
        .intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR))
}

/// Whether the peer of `socket` closed its end: nothing is waiting to be read and the link ended.
/// Never waits.
pub(super) fn hung_up(socket: BorrowedFd<'_>) -> bool {
    use rustix::net::{RecvFlags, recv};
    if !readable(socket).unwrap_or(true) {
        return false;
    }
    let mut byte = [0_u8; 1];
    match recv(socket, &mut byte, RecvFlags::PEEK | RecvFlags::DONTWAIT) {
        Ok((0, _)) => true,
        Ok(_) | Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => false,
        Err(_) => true,
    }
}

/// Fill `buffer` from `socket`, keeping the descriptors that arrive beside its bytes. `false` when
/// the link ended before its first byte; an end after it is an error.
fn read_whole(
    socket: BorrowedFd<'_>,
    buffer: &mut [u8],
    fds: &mut Vec<OwnedFd>,
) -> io::Result<bool> {
    use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, recvmsg};
    let mut read = 0;
    while read < buffer.len() {
        let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let rest = buffer.get_mut(read..).unwrap_or_default();
        let received = match recvmsg(
            socket,
            &mut [io::IoSliceMut::new(rest)],
            &mut control,
            RecvFlags::empty(),
        ) {
            Err(rustix::io::Errno::INTR) => continue,
            other => other?,
        };
        for message in control.drain() {
            if let RecvAncillaryMessage::ScmRights(rights) = message {
                for fd in rights {
                    rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)?;
                    fds.push(fd);
                }
            }
        }
        if received.flags.intersects(ReturnFlags::CTRUNC) || fds.len() > MAX_FDS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a packet carries more descriptors than any packet",
            ));
        }
        if received.bytes == 0 {
            if read == 0 && fds.is_empty() {
                return Ok(false);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the link ended inside a packet",
            ));
        }
        read = read.saturating_add(received.bytes);
    }
    Ok(true)
}
