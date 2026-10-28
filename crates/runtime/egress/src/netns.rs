//! A network namespace of the sandbox's own, and the sockets the supervisor
//! keeps inside it.
//!
//! The namespace belongs to a user namespace that maps exactly the caller's
//! uid and gid. The caller owns that user namespace, so the kernel grants it
//! `CAP_NET_ADMIN` over the network namespace from outside: it can join it
//! (to start the sandbox and `pasta` there) and program its filter. A
//! sandbox started inside creates its own, nested user namespace (bwrap
//! does), so whatever it runs has no capability over the network namespace
//! it sits in — not even one it could regain.
//!
//! The namespaces are made by util-linux `unshare`, which then becomes
//! `cat` holding them. The sockets kept inside are opened by a forked child
//! that joins them; it does only raw system calls between `fork` and
//! `_exit`, on memory prepared before the fork.

use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;

/// A network namespace inside a user namespace.
#[derive(Debug)]
pub struct Netns {
    /// A process in both namespaces: `/proc/<pid>/ns/…` names them for
    /// `pasta`, which does not take descriptors.
    pid: libc::pid_t,
    user: OwnedFd,
    net: OwnedFd,
    _holder: Option<Holder>,
}

/// The child that made the namespaces, kept alive (as `cat` reading a pipe)
/// so `/proc/<pid>/ns/…` keeps naming them. It exits when the pipe closes:
/// when the [`Netns`] drops, or the supervisor dies.
#[derive(Debug)]
struct Holder {
    pid: libc::pid_t,
    release: Option<OwnedFd>,
}

impl Drop for Holder {
    fn drop(&mut self) {
        drop(self.release.take());
        let mut status = 0;
        // SAFETY: `pid` is this process's child; `status` is a valid out
        // parameter.
        unsafe { libc::waitpid(self.pid, &raw mut status, 0) };
    }
}

/// What the supervisor keeps inside the namespace.
#[derive(Debug)]
pub struct Inside {
    /// The DNS stub's UDP socket, bound to `127.0.0.1:53`.
    pub dns_udp: OwnedFd,
    /// The DNS stub's TCP listener, on `127.0.0.1:53`.
    pub dns_tcp: OwnedFd,
    /// A `NETLINK_NETFILTER` socket, or why the kernel would not open one.
    pub netfilter: Result<OwnedFd, io::Error>,
}

/// Which step of the namespace setup failed, for the error.
const STEPS: &[&str] = &[
    "ok",
    "unshare(CLONE_NEWUSER|CLONE_NEWNET)",
    "writing /proc/self/setgroups",
    "writing /proc/self/uid_map",
    "writing /proc/self/gid_map",
    "joining the user namespace",
    "joining the network namespace",
    "opening a socket to configure lo",
    "reading lo's flags",
    "bringing lo up",
    "opening the DNS stub's UDP socket",
    "binding 127.0.0.1:53 (UDP)",
    "opening the DNS stub's TCP socket",
    "binding 127.0.0.1:53 (TCP)",
    "listening on 127.0.0.1:53 (TCP)",
    "handing the sockets back",
];

fn step_error(step: u32, errno: u32) -> io::Error {
    let what = usize::try_from(step)
        .ok()
        .and_then(|i| STEPS.get(i))
        .copied()
        .unwrap_or("setting up the network namespace");
    let os = io::Error::from_raw_os_error(i32::try_from(errno).unwrap_or(libc::EIO));
    io::Error::new(os.kind(), format!("{what}: {os}"))
}

/// How long `unshare` may take to make the namespaces.
const HOLDER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// util-linux `unshare`.
fn unshare_binary() -> &'static str {
    ["/usr/bin/unshare", "/bin/unshare"]
        .into_iter()
        .find(|p| std::path::Path::new(p).is_file())
        .unwrap_or("unshare")
}

impl Netns {
    /// A new network namespace in a new user namespace mapping exactly this
    /// process's uid and gid. Only `lo` exists in it, down until
    /// [`Netns::open_inside`] brings it up.
    ///
    /// # Errors
    /// Returns the step that failed — typically `unshare`, where unprivileged
    /// user namespaces are disabled.
    pub fn create() -> io::Result<Self> {
        // util-linux `unshare` makes the namespaces and becomes `cat`, which
        // reads the release pipe until it closes. A fresh image rather than
        // a fork of this process: a supervisor that loaded key material is
        // not dumpable, and a fork of it could neither write its own
        // `uid_map` nor have `/proc/<pid>/ns` opened by pasta.
        let (release_r, release_w) = pipe()?;
        let mut child = Command::new(unshare_binary())
            .args(["--user", "--map-current-user", "--net", "--", "cat"])
            .stdin(std::process::Stdio::from(release_r))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("starting unshare: {e}")))?;
        let pid =
            libc::pid_t::try_from(child.id()).map_err(|_| io::Error::other("pid out of range"))?;
        let holder = Holder {
            pid,
            release: Some(release_w),
        };
        let deadline = std::time::Instant::now().checked_add(HOLDER_TIMEOUT);
        // `cat` is running once `unshare` has made and mapped the namespaces.
        loop {
            if std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "cat")
            {
                break;
            }
            if let Ok(Some(status)) = child.try_wait() {
                let mut said = String::new();
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_string(&mut said);
                }
                return Err(io::Error::other(format!(
                    "unshare --user --net failed ({status}): {}",
                    said.trim()
                )));
            }
            if deadline.is_none_or(|d| std::time::Instant::now() > d) {
                let _ = child.kill();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "unshare --user --net did not finish",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        drop(child.stderr.take());
        let user = open_ns(pid, "user")?;
        let net = open_ns(pid, "net")?;
        Ok(Self {
            pid,
            user,
            net,
            _holder: Some(holder),
        })
    }

    /// The namespaces process `pid` is in — a sandbox that made its own
    /// (`bwrap --unshare-net`), held still with `--block-fd` while the
    /// supervisor attaches. The caller must own the process's user
    /// namespace (have created it, or its parent).
    pub fn of_process(pid: u32) -> io::Result<Self> {
        let pid = libc::pid_t::try_from(pid)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid out of range"))?;
        Ok(Self {
            pid,
            user: open_ns(pid, "user")?,
            net: open_ns(pid, "net")?,
            _holder: None,
        })
    }

    /// A process in the namespaces.
    pub fn pid(&self) -> libc::pid_t {
        self.pid
    }

    /// Make `cmd` start inside both namespaces. The process keeps its uid;
    /// it holds the namespace's capabilities only until it `exec`s, which
    /// drops them (it is not uid 0 there). `self` must outlive the spawn.
    pub fn enter(&self, cmd: &mut Command) {
        let user = self.user.as_raw_fd();
        let net = self.net.as_raw_fd();
        // SAFETY: `setns` is async-signal-safe; the descriptors are open in
        // the child because `self` outlives the spawn.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setns(user, libc::CLONE_NEWUSER) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::setns(net, libc::CLONE_NEWNET) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    /// Bring `lo` up and open the sockets the supervisor keeps inside: the
    /// DNS stub's, bound to `127.0.0.1:53`, and a `NETLINK_NETFILTER`
    /// socket. A forked child joins the namespaces (with their capabilities,
    /// since it does not `exec`), opens them and hands them back over a
    /// socket pair; the descriptors keep the namespace they were opened in.
    ///
    /// # Errors
    /// Returns the step that failed. A refused netfilter socket is not an
    /// error here: it is reported in [`Inside::netfilter`], since a policy
    /// that needs no filter does not need it.
    pub fn open_inside(&self) -> io::Result<Inside> {
        let (parent, child) = socketpair()?;
        let user = self.user.as_raw_fd();
        let net = self.net.as_raw_fd();
        // SAFETY: as in `create`.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            // SAFETY: as in `create`; this is the child.
            unsafe { inside_child(child.as_raw_fd(), user, net) }
        }
        drop(child);
        let received = recv_fds(&parent);
        let mut status = 0;
        // SAFETY: `pid` is this process's child.
        unsafe { libc::waitpid(pid, &raw mut status, 0) };
        let (words, mut fds) = received?;
        let [step, errno, netfilter_errno, _] = words;
        if step != 0 {
            return Err(step_error(step, errno));
        }
        let netfilter = if netfilter_errno == 0 && fds.len() == 3 {
            fds.pop()
                .ok_or_else(|| io::Error::other("no netfilter socket"))
        } else {
            Err(io::Error::from_raw_os_error(
                i32::try_from(netfilter_errno).unwrap_or(libc::EIO),
            ))
        };
        let mut fds = fds.into_iter();
        let (Some(dns_udp), Some(dns_tcp)) = (fds.next(), fds.next()) else {
            return Err(io::Error::other("the namespace child sent too few sockets"));
        };
        Ok(Inside {
            dns_udp,
            dns_tcp,
            netfilter,
        })
    }
}

fn open_ns(pid: libc::pid_t, kind: &str) -> io::Result<OwnedFd> {
    let path = format!("/proc/{pid}/ns/{kind}");
    std::fs::File::open(&path)
        .map(OwnedFd::from)
        .map_err(|e| io::Error::new(e.kind(), format!("{path}: {e}")))
}

pub(crate) fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` has room for the two descriptors.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let [r, w] = fds;
    // SAFETY: `pipe2` returned two new descriptors nothing else owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(r), OwnedFd::from_raw_fd(w)) })
}

fn socketpair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` has room for the two descriptors.
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let [a, b] = fds;
    // SAFETY: `socketpair` returned two new descriptors nothing else owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(a), OwnedFd::from_raw_fd(b)) })
}

/// The current `errno`, as reported to the parent.
unsafe fn errno() -> u32 {
    // SAFETY: `__errno_location` returns this thread's errno slot.
    unsafe { (*libc::__errno_location()).unsigned_abs() }
}

/// Close every descriptor but `keep` (ascending).
pub(crate) unsafe fn close_others(keep: [RawFd; 2]) {
    let [a, b] = if keep[0] <= keep[1] {
        keep
    } else {
        [keep[1], keep[0]]
    };
    let ranges = [
        (0, a.wrapping_sub(1)),
        (a.wrapping_add(1), b.wrapping_sub(1)),
        (b.wrapping_add(1), RawFd::MAX),
    ];
    for (from, to) in ranges {
        if from <= to && from >= 0 {
            // SAFETY: close_range only closes descriptors.
            unsafe {
                libc::syscall(
                    libc::SYS_close_range,
                    from.unsigned_abs(),
                    to.unsigned_abs(),
                    0,
                );
            }
        }
    }
}

unsafe fn send_and_exit(sock: RawFd, words: [u32; 4], fds: &[RawFd]) -> ! {
    #[repr(C, align(8))]
    struct Control([u8; 64]);
    // SAFETY: the message, its iovec and control buffer are locals valid for
    // the call; CMSG_* only compute offsets into `control`.
    unsafe {
        let mut words = words;
        let mut iov = libc::iovec {
            iov_base: words.as_mut_ptr().cast(),
            iov_len: std::mem::size_of::<[u32; 4]>(),
        };
        let mut control = Control([0; 64]);
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &raw mut iov;
        msg.msg_iovlen = 1;
        let bytes = u32::try_from(std::mem::size_of_val(fds)).unwrap_or(0);
        if bytes > 0 {
            msg.msg_control = control.0.as_mut_ptr().cast();
            msg.msg_controllen = libc::CMSG_SPACE(bytes) as _;
            let header = libc::CMSG_FIRSTHDR(&raw const msg);
            if !header.is_null() {
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN(bytes) as _;
                std::ptr::copy_nonoverlapping(
                    fds.as_ptr(),
                    libc::CMSG_DATA(header).cast::<RawFd>(),
                    fds.len(),
                );
            }
        }
        libc::sendmsg(sock, &raw const msg, 0);
        libc::_exit(0)
    }
}

unsafe fn report(sock: RawFd, step: u32) -> ! {
    // SAFETY: see `send_and_exit`.
    unsafe {
        let e = errno();
        send_and_exit(sock, [step, e, 0, 0], &[])
    }
}

unsafe fn loopback_up(sock: RawFd) {
    // SAFETY: raw system calls on a zeroed local `ifreq`.
    unsafe {
        let s = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if s < 0 {
            report(sock, 7);
        }
        let mut ifr: libc::ifreq = std::mem::zeroed();
        ifr.ifr_name[0] = libc::c_char::try_from(b'l').unwrap_or_default();
        ifr.ifr_name[1] = libc::c_char::try_from(b'o').unwrap_or_default();
        if libc::ioctl(s, libc::SIOCGIFFLAGS, &raw mut ifr) != 0 {
            report(sock, 8);
        }
        ifr.ifr_ifru.ifru_flags |= libc::c_short::try_from(libc::IFF_UP).unwrap_or(1);
        if libc::ioctl(s, libc::SIOCSIFFLAGS, &raw const ifr) != 0 {
            report(sock, 9);
        }
        libc::close(s);
    }
}

fn loopback_53() -> libc::sockaddr_in {
    // SAFETY: an all-zero sockaddr_in is valid.
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    addr.sin_family = libc::sa_family_t::try_from(libc::AF_INET).unwrap_or_default();
    addr.sin_port = 53_u16.to_be();
    addr.sin_addr.s_addr = u32::from_ne_bytes([127, 0, 0, 1]);
    addr
}

unsafe fn inside_child(sock: RawFd, user: RawFd, net: RawFd) -> ! {
    // SAFETY: raw system calls on prepared memory; see `Netns::open_inside`.
    unsafe {
        if libc::setns(user, libc::CLONE_NEWUSER) != 0 {
            report(sock, 5);
        }
        if libc::setns(net, libc::CLONE_NEWNET) != 0 {
            report(sock, 6);
        }
        loopback_up(sock);
        let addr = loopback_53();
        let len = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_in>()).unwrap_or(16);

        let udp = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if udp < 0 {
            report(sock, 10);
        }
        if libc::bind(udp, (&raw const addr).cast(), len) != 0 {
            report(sock, 11);
        }
        let tcp = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        if tcp < 0 {
            report(sock, 12);
        }
        let one: libc::c_int = 1;
        libc::setsockopt(
            tcp,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            (&raw const one).cast(),
            libc::socklen_t::try_from(std::mem::size_of::<libc::c_int>()).unwrap_or(4),
        );
        if libc::bind(tcp, (&raw const addr).cast(), len) != 0 {
            report(sock, 13);
        }
        if libc::listen(tcp, 64) != 0 {
            report(sock, 14);
        }
        let nl = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            crate::nft::Netfilter::PROTOCOL,
        );
        if nl < 0 {
            let e = errno();
            send_and_exit(sock, [0, 0, e.max(1), 2], &[udp, tcp]);
        }
        send_and_exit(sock, [0, 0, 0, 3], &[udp, tcp, nl])
    }
}

/// Receive the four status words and up to three descriptors.
fn recv_fds(sock: &OwnedFd) -> io::Result<([u32; 4], Vec<OwnedFd>)> {
    let mut words = [0_u8; 16];
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(3))];
    let mut control = rustix::net::RecvAncillaryBuffer::new(&mut space);
    let received = rustix::net::recvmsg(
        sock,
        &mut [io::IoSliceMut::new(&mut words)],
        &mut control,
        rustix::net::RecvFlags::CMSG_CLOEXEC,
    )?;
    if received.bytes != words.len() {
        return Err(io::Error::other(
            "the namespace child exited without an answer",
        ));
    }
    let mut fds = Vec::new();
    for message in control.drain() {
        if let rustix::net::RecvAncillaryMessage::ScmRights(received) = message {
            fds.extend(received);
        }
    }
    let mut status = [0_u32; 4];
    for (word, bytes) in status.iter_mut().zip(words.chunks_exact(4)) {
        *word = u32::from_ne_bytes(bytes.try_into().unwrap_or_default());
    }
    Ok((status, fds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_step_is_named_in_the_error() {
        let e = step_error(11, u32::try_from(libc::EACCES).unwrap());
        assert!(e.to_string().contains("binding 127.0.0.1:53 (UDP)"), "{e}");
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    }
}
