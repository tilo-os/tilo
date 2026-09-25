//! One unprivileged, re-executed receiver per operation; no parent signal changes.
use super::super::{Admission, Code, Frame, MAX_FRAME, MAX_GUARDS, ReceiverReservation};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const FLAG: &str = "--qos-connected-lease-receiver";
const CHILD_FD: RawFd = 3;
const STARTUP: Duration = Duration::from_secs(2);
const ACK: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(250);
const SHUTDOWN: Duration = Duration::from_secs(1);
pub(super) const WORK: Duration = Duration::from_secs(30);
fn invalid() -> io::Error {
    io::Error::other("connected lease epoch revoked")
}
fn cvt(value: libc::c_int) -> io::Result<libc::c_int> {
    if value < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(value)
    }
}
fn lease(file: &File, command: libc::c_int, value: libc::c_int) -> io::Result<libc::c_int> {
    // SAFETY: live retained descriptor, integer-valued fcntl command.
    cvt(unsafe { libc::fcntl(file.as_raw_fd(), command, value) })
}
fn unlock(files: &[Arc<File>]) {
    for file in files {
        let _ = lease(file, libc::F_SETLEASE, libc::F_UNLCK);
    }
}
fn check_leases(files: &[Arc<File>]) -> io::Result<()> {
    for file in files {
        if lease(file, libc::F_GETLEASE, 0)? != libc::F_RDLCK {
            return Err(invalid());
        }
    }
    Ok(())
}
fn socket_pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: output has room for two descriptors.
    cvt(unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
            fds.as_mut_ptr(),
        )
    })?;
    // SAFETY: socketpair returned two independently owned descriptors.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}
fn poll(fd: RawFd, events: i16, deadline: Instant) -> io::Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "lease deadline"));
        }
        let mut item = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        // SAFETY: one valid pollfd output; the timeout is bounded by 250ms.
        let result = unsafe {
            libc::poll(
                &raw mut item,
                1,
                remaining.min(POLL).as_millis().max(1) as i32,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if item.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(invalid());
        }
        if item.revents & events != 0 {
            return Ok(());
        }
    }
}
fn send(socket: RawFd, frame: Frame, descriptor: Option<RawFd>) -> io::Result<()> {
    let mut bytes = frame.encode();
    let mut vector = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    // Native-word storage provides cmsghdr alignment. Only one fd is sent.
    let mut control = [0_usize; 8];
    // SAFETY: all-zero msghdr is valid before filling pointer/length pairs.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut vector;
    message.msg_iovlen = 1;
    if let Some(fd) = descriptor {
        message.msg_control = control.as_mut_ptr().cast();
        // SAFETY: space calculation for one native fd fits control.
        message.msg_controllen =
            unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) } as usize;
        // SAFETY: aligned live control buffer and correctly sized cmsg.
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&raw const message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as usize;
            std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), fd);
        }
    }
    // SAFETY: buffers and optional ancillary descriptor remain live for sendmsg.
    let sent = unsafe {
        libc::sendmsg(
            socket,
            &raw const message,
            libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
        )
    };
    if sent != bytes.len() as isize {
        return Err(if sent < 0 {
            io::Error::last_os_error()
        } else {
            invalid()
        });
    }
    Ok(())
}
fn receive(socket: RawFd) -> io::Result<(Frame, Vec<OwnedFd>)> {
    let mut bytes = [0_u8; MAX_FRAME];
    let mut control = [0_usize; 8];
    let mut vector = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    // SAFETY: zeroed structure populated with valid bounded buffers.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    // SAFETY: valid output buffers; MSG_CMSG_CLOEXEC closes inheritance races.
    let length = unsafe {
        libc::recvmsg(
            socket,
            &raw mut message,
            libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
        )
    };
    if length < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut descriptors = Vec::new();
    let mut bad = message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0;
    // SAFETY: kernel-populated ancillary buffer traversed using libc bounds helpers.
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&raw const message);
        while !header.is_null() {
            let base = libc::CMSG_LEN(0) as usize;
            if (*header).cmsg_len < base {
                bad = true;
                break;
            }
            let length = (*header).cmsg_len - base;
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                if !length.is_multiple_of(std::mem::size_of::<RawFd>()) {
                    bad = true;
                }
                for index in 0..length / std::mem::size_of::<RawFd>() {
                    let fd = std::ptr::read_unaligned(
                        libc::CMSG_DATA(header).cast::<RawFd>().add(index),
                    );
                    descriptors.push(OwnedFd::from_raw_fd(fd));
                }
            } else {
                bad = true;
            }
            header = libc::CMSG_NXTHDR(&raw const message, header);
        }
    }
    if bad || length == 0 {
        return Err(invalid());
    } // RAII closes every received fd.
    let frame =
        Frame::decode(bytes.get(..length as usize).ok_or_else(invalid)?).map_err(|()| invalid())?;
    Ok((frame, descriptors))
}

static RECEIVER_SLOT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static DEFERRED_REAP: std::sync::Mutex<Option<(Child, ReceiverReservation)>> =
    std::sync::Mutex::new(None);
fn reap_deferred() {
    let mut deferred = DEFERRED_REAP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if deferred
        .as_mut()
        .is_some_and(|(child, _)| matches!(child.try_wait(), Ok(Some(_))))
    {
        deferred.take();
    }
}
pub(super) struct Receiver {
    reservation: Option<ReceiverReservation>,
    socket: OwnedFd,
    child: Option<Child>,
    state: Admission,
    files: Vec<Arc<File>>,
    deadline: Instant,
}
impl Receiver {
    pub(super) fn start(epoch: [u8; 16]) -> io::Result<Self> {
        #[cfg(test)]
        let executable = std::env::var_os("QOS_LEASE_HELPER")
            .map(std::path::PathBuf::from)
            .ok_or_else(invalid)?;
        #[cfg(not(test))]
        let executable = std::env::current_exe()?;
        Self::start_command(epoch, Command::new(executable))
    }
    fn start_command(epoch: [u8; 16], mut command: Command) -> io::Result<Self> {
        reap_deferred();
        let reservation = ReceiverReservation::acquire(&RECEIVER_SLOT).map_err(|()| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "previous lease receiver still owned",
            )
        })?;
        let (parent, child_socket) = socket_pair()?;
        let child_fd = child_socket.as_raw_fd();
        command
            .arg(FLAG)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: pre_exec performs only async-signal-safe fd syscalls then returns
        // to exec. No allocation, signal handler, lease or Rust receiver loop here.
        unsafe {
            command.pre_exec(move || {
                if child_fd != CHILD_FD {
                    cvt(libc::dup2(child_fd, CHILD_FD))?;
                }
                cvt(libc::fcntl(CHILD_FD, libc::F_SETFD, 0))?;
                // CLOEXEC all unrelated descriptors, including ambient non-CLOEXEC
                // handles. Unsupported close_range refuses spawn; no weaker fallback.
                cvt(libc::syscall(libc::SYS_close_range, 4_u32, u32::MAX, 4_u32) as i32)?;
                Ok(())
            });
        }
        let child = command.spawn()?;
        drop(child_socket);
        let mut receiver = Self {
            reservation: Some(reservation),
            socket: parent,
            child: Some(child),
            state: Admission::new(epoch),
            files: Vec::new(),
            deadline: Instant::now() + WORK,
        };
        let hello = Frame {
            epoch,
            request: 0,
            guard: 0,
            code: Code::Hello,
        };
        let result = (|| {
            send(receiver.socket.as_raw_fd(), hello, None)?;
            poll(
                receiver.socket.as_raw_fd(),
                libc::POLLIN,
                Instant::now() + STARTUP,
            )?;
            let (reply, fds) = receive(receiver.socket.as_raw_fd())?;
            if reply != hello || !fds.is_empty() {
                return Err(invalid());
            }
            Ok(())
        })();
        if result.is_err() {
            receiver.revoke();
        }
        result?;
        Ok(receiver)
    }
    pub(super) fn revoke(&mut self) {
        self.state.revoke();
        unlock(&self.files);
    }
    pub(super) fn acquire(&mut self, file: File) -> io::Result<Arc<File>> {
        let file = Arc::new(file);
        // Retain before transport: even a lost Acquired ack may have installed a
        // lease on the shared description and requires explicit parent unlock.
        if self.files.len() >= MAX_GUARDS as usize {
            self.revoke();
            return Err(invalid());
        }
        self.files.push(Arc::clone(&file));
        self.control(Code::Acquire, Some(file.as_raw_fd()))?;
        if let Err(error) = check_leases(&self.files) {
            self.revoke();
            return Err(error);
        }
        Ok(file)
    }
    pub(super) fn check(&mut self) -> io::Result<()> {
        if let Err(error) = check_leases(&self.files) {
            self.revoke();
            return Err(error);
        }
        self.control(Code::Check, None)?;
        if let Err(error) = check_leases(&self.files) {
            self.revoke();
            return Err(error);
        }
        Ok(())
    }
    fn control(&mut self, code: Code, descriptor: Option<RawFd>) -> io::Result<()> {
        let result = (|| {
            if Instant::now() >= self.deadline {
                return Err(invalid());
            }
            // An unsolicited reply, EOF or revocation can never be reused as the
            // next request's acknowledgment (including numeric fd reuse).
            match receive(self.socket.as_raw_fd()) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                _ => return Err(invalid()),
            }
            let request = self.state.request(code).map_err(|()| invalid())?;
            send(self.socket.as_raw_fd(), request, descriptor)?;
            poll(
                self.socket.as_raw_fd(),
                libc::POLLIN,
                (Instant::now() + ACK).min(self.deadline),
            )?;
            let (reply, fds) = receive(self.socket.as_raw_fd())?;
            if !fds.is_empty() {
                return Err(invalid());
            }
            self.state.accept(reply).map_err(|()| invalid())
        })();
        if result.is_err() {
            self.revoke();
        }
        result
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.revoke();
        // EOF instructs the receiver to unlock its duplicates, without depending
        // on an acknowledgement or installing any parent SIGPIPE disposition.
        // SAFETY: owned connected socket.
        unsafe {
            libc::shutdown(self.socket.as_raw_fd(), libc::SHUT_RDWR);
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        let deadline = Instant::now() + SHUTDOWN;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => {}
                Err(_) => break,
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        // A kernel-stalled child cannot be promised a hard reap deadline. Keep an
        // owned reap task rather than blocking the caller indefinitely.
        let Some(reservation) = self.reservation.take() else {
            return;
        };
        let pending = Arc::new(std::sync::Mutex::new(Some((child, reservation))));
        let owned = Arc::clone(&pending);
        let launched = std::thread::Builder::new()
            .name("connected-lease-reap".into())
            .spawn(move || {
                let work = owned
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if let Some((mut child, reservation)) = work {
                    if child.wait().is_err() {
                        *DEFERRED_REAP
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some((child, reservation));
                    } else {
                        drop(reservation);
                    }
                }
            });
        if launched.is_err() {
            // Thread creation failure must not drop the child/reap ownership.
            // The retained slot blocks new receivers; future admissions retry
            // nonblocking waitpid without an unbounded queue or UI-thread wait.
            let work = pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            *DEFERRED_REAP
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = work;
        }
    }
}
fn signal_fd() -> io::Result<OwnedFd> {
    // SAFETY: local initialized signal structures, in the isolated early child.
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        cvt(libc::sigemptyset(&raw mut mask))?;
        cvt(libc::sigaddset(&raw mut mask, libc::SIGIO))?;
        cvt(libc::sigprocmask(
            libc::SIG_BLOCK,
            &raw const mask,
            std::ptr::null_mut(),
        ))?;
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        cvt(libc::sigemptyset(&raw mut action.sa_mask))?;
        cvt(libc::sigaction(
            libc::SIGIO,
            &raw const action,
            std::ptr::null_mut(),
        ))?;
        let fd = cvt(libc::signalfd(
            -1,
            &raw const mask,
            libc::SFD_CLOEXEC | libc::SFD_NONBLOCK,
        ))?;
        Ok(OwnedFd::from_raw_fd(fd))
    }
}
fn no_signal(fd: RawFd) -> io::Result<()> {
    let mut info = std::mem::MaybeUninit::<libc::signalfd_siginfo>::uninit();
    // SAFETY: sized output buffer. Any signal, short read or EOF revokes.
    let count = unsafe {
        libc::read(
            fd,
            info.as_mut_ptr().cast(),
            std::mem::size_of::<libc::signalfd_siginfo>(),
        )
    };
    if count < 0 && io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock {
        Ok(())
    } else {
        Err(invalid())
    }
}
fn valid_channel(fd: RawFd) -> io::Result<()> {
    let mut kind: i32 = 0;
    let mut len = std::mem::size_of_val(&kind) as libc::socklen_t;
    // SAFETY: correctly sized getsockopt output.
    cvt(unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut kind).cast(),
            &raw mut len,
        )
    })?;
    if kind != libc::SOCK_SEQPACKET {
        return Err(invalid());
    }
    // SAFETY: zeroed credential output populated by kernel.
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    len = std::mem::size_of_val(&credentials) as libc::socklen_t;
    // SAFETY: correctly sized credential output; peer must be the spawning parent.
    cvt(unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut len,
        )
    })?;
    // SAFETY: identity queries have no preconditions.
    if unsafe {
        credentials.pid != libc::getppid()
            || credentials.uid != libc::geteuid()
            || libc::getuid() != libc::geteuid()
    } {
        return Err(invalid());
    }
    // SAFETY: does not acquire privilege; forbids subsequent exec privilege gains.
    cvt(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) })?;
    Ok(())
}
// Keep the epoch's receive/validate/revoke ordering together for review.
#[allow(
    clippy::too_many_lines,
    reason = "single bounded receiver state machine and one cleanup exit"
)]
fn run_receiver(socket: &OwnedFd) -> io::Result<()> {
    valid_channel(socket.as_raw_fd())?;
    let signals = signal_fd()?; // before any descriptor/lease can be accepted.
    let startup = Instant::now() + STARTUP;
    poll(socket.as_raw_fd(), libc::POLLIN, startup)?;
    let (hello, fds) = receive(socket.as_raw_fd())?;
    if hello.code != Code::Hello
        || hello.request != 0
        || hello.guard != 0
        || !fds.is_empty()
        || hello.epoch == [0; 16]
    {
        return Err(invalid());
    }
    no_signal(signals.as_raw_fd())?;
    send(socket.as_raw_fd(), hello, None)?;
    let mut state = Admission::new(hello.epoch);
    let mut files: Vec<Arc<File>> = Vec::new();
    let deadline = Instant::now() + WORK;
    let result = (|| {
        loop {
            no_signal(signals.as_raw_fd())?;
            check_leases(&files)?;
            if Instant::now() >= deadline {
                return Err(invalid());
            }
            let mut polls = [
                libc::pollfd {
                    fd: signals.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: socket.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: two initialized descriptors and bounded timeout.
            let status = unsafe { libc::poll(polls.as_mut_ptr(), 2, POLL.as_millis() as i32) };
            if status < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(io::Error::last_os_error());
            }
            if polls[0].revents != 0 {
                return Err(invalid());
            }
            if polls[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                return Err(invalid());
            }
            if polls[1].revents & libc::POLLIN == 0 {
                continue;
            }
            let (request, mut descriptors) = receive(socket.as_raw_fd())?;
            let expected = state.request(request.code).map_err(|()| invalid())?;
            if request != expected {
                return Err(invalid());
            }
            no_signal(signals.as_raw_fd())?;
            if request.code == Code::Acquire {
                if descriptors.len() != 1 {
                    return Err(invalid());
                }
                let fd = descriptors.pop().ok_or_else(invalid)?;
                let file = Arc::new(File::from(fd));
                if !file.metadata()?.is_file()
                    || lease(&file, libc::F_GETFL, 0)? & libc::O_ACCMODE != libc::O_RDONLY
                {
                    return Err(invalid());
                }
                files.push(Arc::clone(&file)); // cleanup even after partial setup.
                // SAFETY: getpid query; the description is exclusively owned by
                // this epoch and F_SETOWN targets only this receiver process.
                lease(&file, libc::F_SETOWN, unsafe { libc::getpid() })?;
                // F_SETSIG is Linux UAPI command 10; libc does not export it on aarch64.
                lease(&file, 10, 0)?;
                lease(&file, libc::F_SETLEASE, libc::F_RDLCK)?;
            } else if !descriptors.is_empty() {
                return Err(invalid());
            }
            no_signal(signals.as_raw_fd())?;
            check_leases(&files)?;
            let reply = Frame {
                code: if request.code == Code::Acquire {
                    Code::Acquired
                } else {
                    Code::Healthy
                },
                ..request
            };
            state.accept(reply).map_err(|()| invalid())?;
            send(socket.as_raw_fd(), reply, None)?; // backpressure revokes, no queue.
            if request.code == Code::Shutdown {
                return Ok(());
            }
        }
    })();
    state.revoke();
    unlock(&files);
    if result.is_err() {
        let _ = send(
            socket.as_raw_fd(),
            Frame {
                code: Code::Revoked,
                ..hello
            },
            None,
        );
    }
    result
}
pub(super) fn maybe_run(args: &[String]) -> Option<i32> {
    if args.get(1).map(String::as_str) != Some(FLAG) {
        return None;
    }
    if args.len() != 2 {
        return Some(2);
    }
    // SAFETY: this exclusive early helper mode takes ownership of fixed fd 3.
    // Invalid or unrelated descriptors are rejected without filesystem access.
    if unsafe { libc::fcntl(CHILD_FD, libc::F_GETFD) } < 0 {
        return Some(2);
    }
    let socket = unsafe { OwnedFd::from_raw_fd(CHILD_FD) };
    Some(if run_receiver(&socket).is_ok() { 0 } else { 2 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::OpenOptionsExt;
    // All signal disposition experiments are re-executed in isolated processes.
    // Tests intentionally do not skip unsupported leases: refusal is a failing
    // native qualification result, not an apparent pass.
    const TEST: &str = "connection::transaction::native::linux::lease_process::tests::native_isolated_receiver_regressions";
    static PARENT_SIGNALS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    extern "C" fn handler(_: i32) {
        PARENT_SIGNALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn tree() -> tempfile::TempDir {
        tempfile::tempdir_in(std::env::var_os("QOS_ENDPOINT_FIXTURE").unwrap()).unwrap()
    }
    fn start() -> Receiver {
        Receiver::start([31; 16]).unwrap()
    }
    fn writer(path: &std::path::Path) -> io::Result<File> {
        std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
    }
    #[test]
    #[ignore = "isolated subprocesses on authorized Linux native volume required"]
    #[allow(
        clippy::too_many_lines,
        reason = "isolated subprocess qualification matrix; no shared-runner signal changes"
    )]
    fn native_isolated_receiver_regressions() {
        let Some(case) = std::env::var_os("QOS_LEASE_CASE") else {
            for case in [
                "ignore",
                "handler",
                "old-writer",
                "mapping",
                "kill-receiver",
                "coalesced",
                "backpressure",
                "ancillary",
                "parent-death",
            ] {
                assert!(
                    Command::new(std::env::current_exe().unwrap())
                        .args(["--exact", TEST, "--ignored", "--nocapture"])
                        .env("QOS_LEASE_CASE", case)
                        .status()
                        .unwrap()
                        .success(),
                    "case {case}"
                );
            }
            return;
        };
        let tree = tree();
        let path = tree.path().join("file");
        std::fs::write(&path, b"preserve").unwrap();
        match case.to_str().unwrap() {
            "ignore" | "handler" => {
                // SAFETY: disposable test subprocess only, never the shared runner.
                unsafe {
                    let mut action: libc::sigaction = std::mem::zeroed();
                    action.sa_sigaction = if case == "ignore" {
                        libc::SIG_IGN
                    } else {
                        handler as *const () as usize
                    };
                    libc::sigemptyset(&raw mut action.sa_mask);
                    assert_eq!(
                        libc::sigaction(libc::SIGIO, &raw const action, std::ptr::null_mut()),
                        0
                    );
                    let mut before: libc::sigset_t = std::mem::zeroed();
                    assert_eq!(
                        libc::sigprocmask(libc::SIG_SETMASK, std::ptr::null(), &raw mut before),
                        0
                    );
                    let mut receiver = start();
                    let file = receiver.acquire(File::open(&path).unwrap()).unwrap();
                    assert!(writer(&path).is_err());
                    let deadline = Instant::now() + ACK;
                    while receiver
                        .child
                        .as_mut()
                        .unwrap()
                        .try_wait()
                        .unwrap()
                        .is_none()
                    {
                        assert!(
                            Instant::now() < deadline,
                            "receiver did not revoke independently"
                        );
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    assert_eq!(PARENT_SIGNALS.load(std::sync::atomic::Ordering::Relaxed), 0);
                    assert!(receiver.check().is_err());
                    let mut after: libc::sigaction = std::mem::zeroed();
                    assert_eq!(
                        libc::sigaction(libc::SIGIO, std::ptr::null(), &raw mut after),
                        0
                    );
                    assert_eq!(after.sa_sigaction, action.sa_sigaction);
                    let mut mask: libc::sigset_t = std::mem::zeroed();
                    assert_eq!(
                        libc::sigprocmask(libc::SIG_SETMASK, std::ptr::null(), &raw mut mask),
                        0
                    );
                    for signal in 1..65 {
                        assert_eq!(
                            libc::sigismember(&raw const before, signal),
                            libc::sigismember(&raw const mask, signal)
                        );
                    }
                    drop(receiver);
                    assert_eq!(lease(&file, libc::F_GETLEASE, 0).unwrap(), libc::F_UNLCK);
                    assert!(writer(&path).is_ok());
                }
            }
            "old-writer" => {
                let held = writer(&path).unwrap();
                let mut receiver = start();
                assert!(receiver.acquire(File::open(&path).unwrap()).is_err());
                assert_eq!(std::fs::read(&path).unwrap(), b"preserve");
                drop(held);
            }
            "mapping" => {
                let held = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .unwrap();
                // SAFETY: real writable shared mapping of the disposable regular file.
                let mapping = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        8,
                        libc::PROT_READ | libc::PROT_WRITE,
                        libc::MAP_SHARED,
                        held.as_raw_fd(),
                        0,
                    )
                };
                assert_ne!(mapping, libc::MAP_FAILED);
                drop(held);
                let mut receiver = start();
                assert!(receiver.acquire(File::open(&path).unwrap()).is_err());
                // SAFETY: mapping remains live after original fd close.
                unsafe {
                    assert_eq!(libc::munmap(mapping, 8), 0);
                }
            }
            "kill-receiver" => {
                let mut receiver = start();
                let file = receiver.acquire(File::open(&path).unwrap()).unwrap();
                receiver.child.as_mut().unwrap().kill().unwrap();
                receiver.child.as_mut().unwrap().wait().unwrap();
                // Parent still owns the exact shared description: check revokes
                // and explicitly unlocks it, independently of child destruction.
                assert!(receiver.check().is_err());
                assert_eq!(lease(&file, libc::F_GETLEASE, 0).unwrap(), libc::F_UNLCK);
                assert!(writer(&path).is_ok());
            }
            "coalesced" => {
                let second = tree.path().join("second");
                std::fs::write(&second, b"other").unwrap();
                let mut receiver = start();
                receiver.acquire(File::open(&path).unwrap()).unwrap();
                receiver.acquire(File::open(&second).unwrap()).unwrap();
                // SAFETY: stop only the owned receiver so both real opens produce
                // pending standard SIGIO before it can consume a notification.
                let pid = receiver.child.as_ref().unwrap().id() as i32;
                unsafe {
                    assert_eq!(libc::kill(pid, libc::SIGSTOP), 0);
                }
                assert!(writer(&path).is_err());
                assert!(writer(&second).is_err());
                unsafe {
                    assert_eq!(libc::kill(pid, libc::SIGCONT), 0);
                }
                assert!(receiver.check().is_err());
                assert!(receiver.check().is_err());
            }
            "backpressure" => {
                let (a, b) = socket_pair().unwrap();
                let frame = Frame {
                    epoch: [1; 16],
                    request: 1,
                    guard: 0,
                    code: Code::Check,
                };
                let mut full = false;
                for _ in 0..65536 {
                    if send(a.as_raw_fd(), frame, None).is_err() {
                        full = true;
                        break;
                    }
                }
                assert!(full);
                drop(b);
                assert!(send(a.as_raw_fd(), frame, None).is_err());
                // Exercise the actual receiver's full reply queue while a real
                // lease is held; it must unlock rather than queue indefinitely.
                let mut receiver = start();
                let held = receiver.acquire(File::open(&path).unwrap()).unwrap();
                let deadline = Instant::now() + STARTUP + ACK;
                let mut sequence = 2;
                loop {
                    if lease(&held, libc::F_GETLEASE, 0).unwrap() == libc::F_UNLCK {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "receiver retained lease under reply backpressure"
                    );
                    let request = Frame {
                        epoch: [31; 16],
                        request: sequence,
                        guard: 0,
                        code: Code::Check,
                    };
                    match send(receiver.socket.as_raw_fd(), request, None) {
                        Ok(()) => sequence += 1,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                    }
                }
                assert!(receiver.check().is_err());
            }
            "ancillary" => {
                let (a, b) = socket_pair().unwrap();
                let file = File::open(&path).unwrap();
                let frame = Frame {
                    epoch: [1; 16],
                    request: 1,
                    guard: 0,
                    code: Code::Check,
                };
                // A descriptor attached to Check is rejected by the real receiver.
                let mut receiver = start();
                let request = receiver.state.request(Code::Check).unwrap();
                send(receiver.socket.as_raw_fd(), request, Some(file.as_raw_fd())).unwrap();
                let _ = poll(
                    receiver.socket.as_raw_fd(),
                    libc::POLLIN,
                    Instant::now() + ACK,
                );
                assert!(receiver.check().is_err());
                // Oversized seqpacket must fail whole-frame decoding, not truncate
                // into a smaller syntactically valid request.
                let mut bytes = vec![0_u8; MAX_FRAME + 1];
                bytes[..40].copy_from_slice(&frame.encode());
                unsafe {
                    assert_eq!(
                        libc::send(
                            a.as_raw_fd(),
                            bytes.as_ptr().cast(),
                            bytes.len(),
                            libc::MSG_NOSIGNAL
                        ),
                        bytes.len() as isize
                    );
                }
                assert!(receive(b.as_raw_fd()).is_err());
                // Force ancillary truncation with more rights than the bounded
                // receiver storage, and ensure it refuses the complete packet.
                let mut rights = [0_usize; 32];
                let mut wire = frame.encode();
                let mut vector = libc::iovec {
                    iov_base: wire.as_mut_ptr().cast(),
                    iov_len: wire.len(),
                };
                unsafe {
                    let mut message: libc::msghdr = std::mem::zeroed();
                    message.msg_iov = &raw mut vector;
                    message.msg_iovlen = 1;
                    message.msg_control = rights.as_mut_ptr().cast();
                    message.msg_controllen =
                        libc::CMSG_SPACE(16 * std::mem::size_of::<RawFd>() as u32) as usize;
                    let header = libc::CMSG_FIRSTHDR(&raw const message);
                    (*header).cmsg_level = libc::SOL_SOCKET;
                    (*header).cmsg_type = libc::SCM_RIGHTS;
                    (*header).cmsg_len =
                        libc::CMSG_LEN(16 * std::mem::size_of::<RawFd>() as u32) as usize;
                    for index in 0..16 {
                        std::ptr::write_unaligned(
                            libc::CMSG_DATA(header).cast::<RawFd>().add(index),
                            file.as_raw_fd(),
                        );
                    }
                    assert_eq!(
                        libc::sendmsg(a.as_raw_fd(), &raw const message, libc::MSG_NOSIGNAL),
                        wire.len() as isize
                    );
                }
                assert!(receive(b.as_raw_fd()).is_err());
            }
            "parent-death" => {
                parent_death_checks_eof(&path, &tree.path().join("marker"));
            }
            "orphan-holder" => {
                let mut receiver = start();
                receiver
                    .acquire(File::open(std::env::var_os("QOS_LEASE_FILE").unwrap()).unwrap())
                    .unwrap();
                // Publish the actual receiver PID atomically after acquisition.
                // The observer opens a pidfd before killing this test parent.
                let marker =
                    std::path::PathBuf::from(std::env::var_os("QOS_LEASE_MARKER").unwrap());
                let pending = marker.with_extension("pending");
                std::fs::write(
                    &pending,
                    receiver.child.as_ref().unwrap().id().to_le_bytes(),
                )
                .unwrap();
                std::fs::rename(pending, marker).unwrap();
                std::thread::sleep(Duration::from_secs(10));
                panic!("test parent was not terminated");
            }
            _ => panic!("unknown isolated case"),
        }
    }
    // Test-only coordination: no signal disposition changes, subreaper setting,
    // production environment override, or conflicting file open is needed.
    struct TestParent(Child);
    impl Drop for TestParent {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn receiver_exited(pidfd: &OwnedFd, deadline: Instant) -> bool {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let mut item = libc::pollfd {
                fd: pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one initialized pidfd poll entry; bounded timeout. A pidfd
            // becomes readable on exit even if the orphan has not been reaped.
            let status = unsafe {
                libc::poll(
                    &raw mut item,
                    1,
                    remaining.min(POLL).as_millis().max(1) as i32,
                )
            };
            if status < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return false;
            }
            if item.revents & libc::POLLIN != 0 {
                return true;
            }
            if item.revents != 0 {
                return false;
            }
        }
    }
    fn parent_death_checks_eof(path: &std::path::Path, marker: &std::path::Path) {
        let mut parent = TestParent(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--ignored", "--nocapture"])
                .env("QOS_LEASE_CASE", "orphan-holder")
                .env("QOS_LEASE_FILE", path)
                .env("QOS_LEASE_MARKER", marker)
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + STARTUP + ACK;
        while !marker.exists() {
            assert!(
                Instant::now() < deadline,
                "test parent did not acquire a lease"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let bytes: [u8; 4] = std::fs::read(marker).unwrap().try_into().unwrap();
        let receiver_pid = i32::try_from(u32::from_le_bytes(bytes)).unwrap();
        // SAFETY: PID comes from our isolated test parent's real owned receiver.
        // Retain pidfd before terminating that parent to avoid PID reuse races.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, receiver_pid, 0_u32) };
        assert!(fd >= 0, "pidfd_open: {}", io::Error::last_os_error());
        // SAFETY: successful pidfd_open returned an independently owned fd.
        let pidfd = unsafe { OwnedFd::from_raw_fd(i32::try_from(fd).unwrap()) };
        let mut before = libc::pollfd {
            fd: pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: initialized poll entry; zero timeout verifies it is still alive.
        assert_eq!(
            unsafe { libc::poll(&raw mut before, 1, 0) },
            0,
            "receiver exited before its parent"
        );
        parent.0.kill().unwrap();
        parent.0.wait().unwrap();
        // No writer(), truncation or signal can trigger lease revocation here.
        // Ignoring EOF with working SIGIO handling must fail this independent
        // exit observation well before the receiver's 30-second work deadline.
        let exited_on_eof = receiver_exited(&pidfd, Instant::now() + SHUTDOWN + ACK);
        if !exited_on_eof {
            // Cleanup is not evidence: retain the failed observation before
            // terminating only this owned receiver through its retained pidfd.
            // SAFETY: retained pidfd identifies exactly our test receiver.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    pidfd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0_u32,
                );
            }
            let _ = receiver_exited(&pidfd, Instant::now() + SHUTDOWN);
        }
        assert!(
            exited_on_eof,
            "receiver did not terminate independently on parent EOF"
        );
        // Only after independent termination evidence may this request a writer.
        assert!(writer(path).is_ok());
    }
    #[test]
    #[ignore = "real kernel forced lease timeout; authorized Linux fixture required"]
    fn native_stalled_receiver_forced_break_revokes() {
        let tree = tree();
        let path = tree.path().join("file");
        std::fs::write(&path, b"preserve").unwrap();
        let mut receiver = start();
        let file = receiver.acquire(File::open(&path).unwrap()).unwrap();
        let pid = receiver.child.as_ref().unwrap().id() as i32;
        unsafe {
            assert_eq!(libc::kill(pid, libc::SIGSTOP), 0);
        }
        // Read policy only; never change the system-wide lease timeout.
        let timeout: u64 = std::fs::read_to_string("/proc/sys/fs/lease-break-time")
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(timeout <= 120, "native qualification budget");
        assert!(writer(&path).is_err());
        let deadline = Instant::now() + Duration::from_secs(timeout + 3);
        loop {
            if writer(&path).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(lease(&file, libc::F_GETLEASE, 0).unwrap(), libc::F_UNLCK);
        unsafe {
            assert_eq!(libc::kill(pid, libc::SIGCONT), 0);
        }
        assert!(receiver.check().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"preserve");
    }
}
