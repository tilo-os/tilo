use std::io::{self, IoSlice, IoSliceMut, Read as _};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, UCred,
};

fn refused(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

/// An exact kernel-attested task. Construction requires matching the actual
/// sender's credentials to a received connected socket's pinned creator.
/// It contains no application, role, workspace or filesystem authority.
pub struct ExecutingPeer {
    credentials: UCred,
    pidfd: OwnedFd,
}
impl AsFd for ExecutingPeer {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.pidfd.as_fd()
    }
}
impl ExecutingPeer {
    /// Kernel process ID, meaningful only while the retained pidfd is current.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.credentials.pid.as_raw_nonzero().get().cast_unsigned()
    }
    /// The task has not exited; a reused numeric PID cannot revive this proof.
    #[must_use]
    pub fn current(&self) -> bool {
        let mut poll = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd and an owned pidfd; nonblocking query.
        unsafe { libc::poll(&raw mut poll, 1, 0) == 0 }
    }
    /// Retain the kernel handle after the host has admitted its process facts.
    #[must_use]
    pub fn into_pidfd(self) -> OwnedFd {
        self.pidfd
    }

    /// Receive one bounded message only from this still-live executing task.
    ///
    /// The owner arms credentials before handing out the connected descriptor.
    /// A descendant inheriting the FD cannot impersonate its pinned producer.
    /// This proves process custody, not resource or application authority.
    ///
    /// # Errors
    /// Refuses a changed/dead sender, rights, truncation or an empty message.
    pub fn receive_packet(&self, socket: &impl AsFd, message: &mut [u8]) -> io::Result<usize> {
        if message.is_empty() || message.len() > 16384 {
            return Err(refused("native message bound is invalid"));
        }
        let chunk = receive_chunk(socket, message)?;
        if chunk.invalid
            || chunk.count == 0
            || chunk.count > message.len()
            || !chunk.rights.is_empty()
            || chunk.credentials != Some(self.credentials)
            || !self.current()
        {
            return Err(refused(
                "native message does not belong to its pinned producer",
            ));
        }
        Ok(chunk.count)
    }

    /// Confirm this pinned task belongs to this still-live launch instance.
    /// An authority owner must also check its own app/workspace launch record.
    #[must_use]
    pub fn belongs_to(&self, launcher_pid: u32, launcher_start: u64) -> bool {
        if !self.current() {
            return false;
        }
        let mut pid = self.pid();
        for _ in 0..32 {
            let Some((parent, start)) = process_stat(pid) else {
                return false;
            };
            if pid == launcher_pid {
                return start == launcher_start && self.current();
            }
            if parent == 0 || parent == pid {
                return false;
            }
            pid = parent;
        }
        false
    }
}

/// Kernel ancestry and start time, not an application-provided process claim.
fn process_stat(pid: u32) -> Option<(u32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<_> = stat.rsplit_once(") ")?.1.split_whitespace().collect();
    if fields.first() == Some(&"Z") {
        return None;
    }
    Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
}

/// Enable sender credentials before sending the host's one-time challenge.
/// # Errors
/// Returns the socket configuration error; there is no unverified fallback.
pub fn arm(socket: &impl AsFd) -> io::Result<()> {
    rustix::net::sockopt::set_socket_passcred(socket, true).map_err(io::Error::from)
}

/// Complete the executing-app handshake on its owned native service stream.
/// The caller runs this before any RPC, on its startup/transport worker.
/// # Errors
/// Refuses a missing/malformed challenge, unusable socket, or refused ready ack.
pub fn complete(socket: &mut UnixStream) -> io::Result<()> {
    socket.set_nonblocking(false)?;
    // No wall-clock deadline: the host answers as soon as it is scheduled,
    // and a host that is gone closes the socket, which ends every read here
    // with end-of-file. A fixed deadline only turned a busy machine into a
    // failed launch.
    complete_inner(socket)
}
fn complete_inner(socket: &mut UnixStream) -> io::Result<()> {
    let mut magic = [0; 4];
    socket.read_exact(&mut magic)?;
    if magic != super::MAGIC {
        return Err(refused("native process binding protocol mismatch"));
    }
    let mut challenge = [0; 32];
    socket.read_exact(&mut challenge)?;
    let (_owned, proof) = UnixStream::pair()?;
    send_answer(socket, &challenge, &proof)?;
    let mut ready = [0];
    socket.read_exact(&mut ready)?;
    if ready != [1] {
        return Err(refused("native executing process binding refused"));
    }
    Ok(())
}
fn send_answer(socket: &UnixStream, challenge: &[u8; 32], proof: &UnixStream) -> io::Result<()> {
    let rights = [proof.as_fd()];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    if !ancillary.push(SendAncillaryMessage::ScmRights(&rights)) {
        return Err(io::Error::other(
            "native proof ancillary buffer is too small",
        ));
    }
    let count = rustix::net::sendmsg(
        socket,
        &[IoSlice::new(challenge)],
        &mut ancillary,
        SendFlags::NOSIGNAL,
    )?;
    if count != challenge.len() {
        return Err(io::Error::other("incomplete native process proof"));
    }
    Ok(())
}

struct Received {
    count: usize,
    credentials: Option<UCred>,
    rights: Vec<OwnedFd>,
    invalid: bool,
}
fn receive_chunk(socket: &impl AsFd, answer: &mut [u8]) -> io::Result<Received> {
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(4), ScmCredentials(1))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut space);
    let read = rustix::net::recvmsg(
        socket,
        &mut [IoSliceMut::new(answer)],
        &mut ancillary,
        RecvFlags::CMSG_CLOEXEC,
    )?;
    let mut result = Received {
        count: read.bytes,
        credentials: None,
        rights: Vec::new(),
        invalid: read
            .flags
            .intersects(ReturnFlags::CTRUNC | ReturnFlags::TRUNC),
    };
    // Rustix owns every installed descriptor, including unread/truncated or
    // rejected messages. Always drain before validating to close all rights.
    for message in ancillary.drain() {
        match message {
            RecvAncillaryMessage::ScmRights(rights) => result.rights.extend(rights),
            RecvAncillaryMessage::ScmCredentials(credentials) => {
                if result.credentials.replace(credentials).is_some() {
                    result.invalid = true;
                }
            }
            _ => result.invalid = true,
        }
    }
    Ok(result)
}

/// Receive the challenge answer with kernel sender credentials and one proof
/// socket. All other rights are closed; the returned pidfd pins the sender.
/// # Errors
/// Refuses wrong/truncated data, rights, user, creator, or a dead process.
pub fn receive(socket: &impl AsFd, challenge: &[u8; 32], uid: u32) -> io::Result<ExecutingPeer> {
    let mut receiver = Receiver::new(*challenge, uid);
    loop {
        if let Some(peer) = receiver.receive(socket)? {
            return Ok(peer);
        }
    }
}

/// Bounded incremental receiver, suitable for a reactor-owned nonblocking socket.
/// An incomplete frame retains only its own proof descriptor until dropped.
pub struct Receiver {
    challenge: [u8; 32],
    uid: u32,
    answer: [u8; 32],
    offset: usize,
    sender: Option<UCred>,
    proof: Option<OwnedFd>,
}
impl Receiver {
    /// Initialize one answer to a host-selected challenge.
    #[must_use]
    pub fn new(challenge: [u8; 32], uid: u32) -> Self {
        Self {
            challenge,
            uid,
            answer: [0; 32],
            offset: 0,
            sender: None,
            proof: None,
        }
    }
    /// Read one bounded chunk; `None` means an incomplete frame.
    /// # Errors
    /// Refuses malformed rights, credentials or challenge; propagates `WouldBlock`.
    pub fn receive(&mut self, socket: &impl AsFd) -> io::Result<Option<ExecutingPeer>> {
        let tail = self
            .answer
            .get_mut(self.offset..)
            .filter(|tail| !tail.is_empty())
            .ok_or_else(|| refused("native proof exceeded its frame"))?;
        let mut chunk = receive_chunk(socket, tail)?;
        if chunk.count == 0 && !chunk.invalid && chunk.rights.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the peer closed the connection before answering",
            ));
        }
        if chunk.invalid || chunk.count == 0 || chunk.count > tail.len() {
            return Err(refused("invalid native proof ancillary frame"));
        }
        let credentials = chunk
            .credentials
            .ok_or_else(|| refused("native proof has no kernel sender"))?;
        if credentials.uid.as_raw() != self.uid
            || self.sender.is_some_and(|sender| sender != credentials)
        {
            return Err(refused(
                "native proof sender changed or belongs to another user",
            ));
        }
        self.sender = Some(credentials);
        if !chunk.rights.is_empty() {
            if self.proof.is_some() || chunk.rights.len() != 1 {
                return Err(refused("native proof must contain exactly one socket"));
            }
            self.proof = chunk.rights.pop();
        }
        self.offset = self
            .offset
            .checked_add(chunk.count)
            .ok_or_else(|| refused("native proof length overflow"))?;
        if self.offset < self.answer.len() {
            return Ok(None);
        }
        if self.answer != self.challenge {
            return Err(refused("native process challenge mismatch"));
        }
        let sender = self
            .sender
            .take()
            .ok_or_else(|| refused("native proof sender is absent"))?;
        let proof = self
            .proof
            .take()
            .ok_or_else(|| refused("native proof socket is absent"))?;
        pin_sender(proof, sender).map(Some)
    }
}
fn pin_sender(proof: OwnedFd, sender: UCred) -> io::Result<ExecutingPeer> {
    let creator = rustix::net::sockopt::socket_peercred(&proof)?;
    if creator != sender {
        return Err(refused(
            "native proof socket does not belong to its actual sender",
        ));
    }
    let mut fd: libc::c_int = -1;
    let mut length =
        libc::socklen_t::try_from(std::mem::size_of_val(&fd)).map_err(io::Error::other)?;
    // SAFETY: an owned received descriptor, correctly sized writable output.
    // SO_PEERPIDFD pins the actual socket creator, not a wire-supplied PID.
    let result = unsafe {
        libc::getsockopt(
            proof.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERPIDFD,
            (&raw mut fd).cast(),
            &raw mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if fd < 0 {
        return Err(refused("native peer pidfd is absent"));
    }
    // SAFETY: successful SO_PEERPIDFD transfers this fresh descriptor.
    let pidfd = unsafe { OwnedFd::from_raw_fd(fd) };
    // The kernel task is pinned now; the temporary proof is no longer needed.
    drop(proof);
    if length as usize != std::mem::size_of::<libc::c_int>() {
        return Err(refused("native peer pidfd length mismatch"));
    }
    let peer = ExecutingPeer {
        credentials: sender,
        pidfd,
    };
    if !peer.current() {
        return Err(refused("native executing process exited before binding"));
    }
    Ok(peer)
}

#[cfg(test)]
mod tests;
