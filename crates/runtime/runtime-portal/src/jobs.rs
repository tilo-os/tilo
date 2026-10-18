//! Independent jobs: a process an app starts that lives a life of its own.
//!
//! A terminal tab or its shell, a build, a language server, an agent's tool
//! run: each can grow far beyond the app that started it, and when memory
//! runs out it should end alone, not take the app with it. The system gives
//! each such process a cgroup of its own below its app's
//! (`tilo-apps-<n>.scope/app-<id>-<k>/job-<j>/`, with `memory.oom.group=1`):
//! the kernel's OOM killer and the session's memory-pressure policy then end
//! that job, whole, and the app keeps running. Apps declare nothing; starting
//! a process through [`independent`] (for an app, the toolkit's
//! `tilo_process::job::independent`) is what makes it a job.
//!
//! How a process gets there: the session's app spawner (`tilo-launcher
//! app-spawner`, which owns the tree) makes the job's cgroup and hands back
//! an open descriptor of its `cgroup.procs`, a [`Slot`]. The process writes
//! `0` to it between fork and exec ([`Slot::place`]), which moves it there
//! before it runs a single instruction of its own program. The descriptor
//! carries the spawner's permission to move processes into that one cgroup
//! and nothing else: it names no other file, and the writer can only name
//! processes it can see.
//!
//! Who asks the spawner:
//!
//! - a sandboxed app, through its launcher's door: a socket the launcher
//!   hands every app as `TILO_SVC_JOBS_FD` ([`slot`]); the launcher asks the
//!   spawner on its behalf, naming itself as the app;
//! - a session service starting a process for an app (the `ush` supervisor,
//!   for a Terminal session), directly, naming the app by the process on the
//!   other end of the app's connection ([`slot_for_peer`]).
//!
//! Without a spawner (another desktop, a development host, a launcher that
//! had to ask the user manager for a scope of its own) there is no slot and
//! the process starts as before, in its app's cgroup. The module is Linux's:
//! cgroups are.

use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The service name of the launcher's door: the app reads its descriptor
/// from `TILO_SVC_JOBS_FD`.
pub const SERVICE: &str = "jobs";

/// Prefix of a delegated app tree's unit (`tilo-apps-<n>.scope`).
pub const TREE_PREFIX: &str = "tilo-apps-";

/// Where, below the person's runtime directory, each tree's spawner listens
/// for job requests.
pub const SOCKET_DIR: &str = "tilo/app-jobs";

/// The longest job label kept: what a notice names the job by.
pub const MAX_LABEL: usize = 48;

/// The largest message either side sends: a verb and a label, or an answer.
pub const MAX_MESSAGE: usize = 1024;

/// The most descriptors one message carries.
const MAX_FDS: usize = 3;

/// A job's cgroup, ready for one process to join.
#[derive(Debug)]
pub struct Slot {
    procs: OwnedFd,
    oom_score_adj: i32,
}

impl Slot {
    /// A slot over an open `cgroup.procs` descriptor, for a process to start
    /// with `oom_score_adj`.
    #[must_use]
    pub const fn new(procs: OwnedFd, oom_score_adj: i32) -> Self {
        Self {
            procs,
            oom_score_adj,
        }
    }

    /// The `oom_score_adj` the job's process starts with: its app's focus
    /// tier, a little more likely to be chosen than the app itself.
    #[must_use]
    pub const fn oom_score_adj(&self) -> i32 {
        self.oom_score_adj
    }

    /// The `cgroup.procs` descriptor of the job.
    #[must_use]
    pub fn procs(&self) -> BorrowedFd<'_> {
        self.procs.as_fd()
    }

    /// Make the process `command` starts join this job between fork and
    /// exec. A refused move (the job's cgroup went away) leaves the process
    /// in its app's cgroup, where it runs as it always did.
    pub fn place(self, command: &mut Command) -> &mut Command {
        use std::os::unix::process::CommandExt as _;
        let adj = AdjText::new(self.oom_score_adj);
        let procs = self.procs;
        // SAFETY: the closure makes async-signal-safe system calls only,
        // on memory prepared before the fork, and allocates nothing.
        unsafe {
            command.pre_exec(move || {
                join_and_adjust(procs.as_raw_fd(), &adj);
                Ok(())
            })
        }
    }
}

/// An `oom_score_adj` value as the bytes written to the file, prepared
/// before a fork.
#[derive(Clone, Copy)]
struct AdjText {
    bytes: [u8; 8],
    len: usize,
}

impl AdjText {
    fn new(value: i32) -> Self {
        let text = value.clamp(-1000, 1000).to_string();
        let mut bytes = [0_u8; 8];
        let len = text.len().min(bytes.len());
        if let (Some(target), Some(source)) = (bytes.get_mut(..len), text.as_bytes().get(..len)) {
            target.copy_from_slice(source);
        }
        Self { bytes, len }
    }

    fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or_default()
    }
}

/// In the child, between fork and exec: join the job through `procs` (`0`
/// names the writer), then take the job's `oom_score_adj`.
fn join_and_adjust(procs: std::os::fd::RawFd, adj: &AdjText) {
    const ADJ_PATH: &std::ffi::CStr = c"/proc/self/oom_score_adj";
    // SAFETY: plain system calls on a descriptor and buffers owned by the
    // caller; failures are ignored on purpose (see `Slot::place`).
    unsafe {
        libc::write(procs, b"0".as_ptr().cast(), 1);
        let fd = libc::open(ADJ_PATH.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
        if fd >= 0 {
            let text = adj.as_bytes();
            libc::write(fd, text.as_ptr().cast(), text.len());
            libc::close(fd);
        }
    }
}

/// `label` as a notice shows it: one line, no control characters, at most
/// [`MAX_LABEL`] characters. Empty stays empty.
#[must_use]
pub fn clean_label(label: &str) -> String {
    let words: Vec<&str> = label
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect();
    words.join(" ").chars().take(MAX_LABEL).collect()
}

/// A program's name as a job label: the last component of `program`.
#[must_use]
pub fn program_label(program: &std::ffi::OsStr) -> String {
    clean_label(
        &Path::new(program)
            .file_name()
            .unwrap_or(program)
            .to_string_lossy(),
    )
}

/// The delegated app tree a `/proc/<pid>/cgroup` text places a process in:
/// the `tilo-apps-<n>.scope` component of its unified-hierarchy path.
#[must_use]
pub fn tree_in(cgroup: &str) -> Option<&str> {
    let path = cgroup.lines().find_map(|line| line.strip_prefix("0::"))?;
    path.split('/').find(|part| {
        part.strip_prefix(TREE_PREFIX)
            .and_then(|rest| rest.strip_suffix(".scope"))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Where the spawner of `tree` listens, below `runtime_dir`.
#[must_use]
pub fn socket_path(runtime_dir: &Path, tree: &str) -> PathBuf {
    runtime_dir.join(SOCKET_DIR).join(format!("{tree}.sock"))
}

/// The person's runtime directory, where the spawners' sockets are.
#[must_use]
pub fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
}

// --- wire --------------------------------------------------------------------

/// A message is NUL-separated fields over a `SOCK_SEQPACKET` socket: nothing
/// it carries (a verb, a label, a path, a number) contains a NUL.
///
/// # Errors
///
/// A field that contains a NUL.
pub fn encode(fields: &[&[u8]]) -> io::Result<Vec<u8>> {
    if fields.iter().any(|field| field.contains(&0)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a message field contains a NUL byte",
        ));
    }
    Ok(fields.join(&0))
}

/// The fields of a message.
#[must_use]
pub fn decode(message: &[u8]) -> Vec<&[u8]> {
    message.split(|byte| *byte == 0).collect()
}

/// Send one message with `fds`.
///
/// # Errors
///
/// The socket's error, or a message that could not be sent whole.
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
    let sent = sendmsg(
        socket,
        &[io::IoSlice::new(&message)],
        &mut control,
        SendFlags::NOSIGNAL,
    )?;
    if sent == message.len() {
        Ok(())
    } else {
        Err(io::ErrorKind::WriteZero.into())
    }
}

/// One message and the descriptors that came with it, close-on-exec;
/// `None` at end of file.
///
/// # Errors
///
/// The socket's error, or a message cut short.
pub fn receive(
    socket: BorrowedFd<'_>,
    buffer: &mut [u8],
) -> io::Result<Option<(usize, Vec<OwnedFd>)>> {
    use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, recvmsg};
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let received = loop {
        match recvmsg(
            socket,
            &mut [io::IoSliceMut::new(buffer)],
            &mut control,
            RecvFlags::CMSG_CLOEXEC,
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

/// The request for a job named `label`: `job`, the label.
#[must_use]
pub fn request_fields(label: &str) -> [Vec<u8>; 2] {
    [
        b"job".to_vec(),
        clean_label(label).replace('\0', "").into_bytes(),
    ]
}

/// The label of a job request, or `None` for anything else.
#[must_use]
pub fn parse_request(fields: &[&[u8]]) -> Option<String> {
    match fields {
        [b"job", label] => Some(clean_label(&String::from_utf8_lossy(label))),
        _ => None,
    }
}

/// The answer granting a slot: `ok` and the job's `oom_score_adj`, with the
/// `cgroup.procs` descriptor.
#[must_use]
pub fn granted_fields(oom_score_adj: i32) -> [Vec<u8>; 2] {
    [b"ok".to_vec(), oom_score_adj.to_string().into_bytes()]
}

/// The answer refusing one: `failed` and why, for the log.
#[must_use]
pub fn refused_fields(reason: &str) -> [Vec<u8>; 2] {
    [b"failed".to_vec(), reason.replace('\0', " ").into_bytes()]
}

/// Read an answer into a slot.
///
/// # Errors
///
/// The refusal's reason, or a malformed answer.
pub fn parse_answer(fields: &[&[u8]], fds: Vec<OwnedFd>) -> io::Result<Slot> {
    match (fields, <[OwnedFd; 1]>::try_from(fds)) {
        ([b"ok", adj], Ok([procs])) => {
            let adj = std::str::from_utf8(adj)
                .ok()
                .and_then(|adj| adj.parse::<i32>().ok())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed answer"))?;
            Ok(Slot::new(procs, adj))
        }
        ([b"failed", reason], _) => Err(io::Error::other(
            String::from_utf8_lossy(reason).into_owned(),
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "malformed answer",
        )),
    }
}

/// Ask `door` (the launcher's, or a spawner's socket) for a job named
/// `label`. `member`, a pidfd of a process of the app the job belongs to,
/// goes with the request to a spawner; the launcher's door knows its app.
///
/// # Errors
///
/// The door's refusal or failure.
pub fn request(
    door: BorrowedFd<'_>,
    label: &str,
    member: Option<BorrowedFd<'_>>,
) -> io::Result<Slot> {
    let fields = request_fields(label);
    let fields: Vec<&[u8]> = fields.iter().map(Vec::as_slice).collect();
    let fds: Vec<BorrowedFd<'_>> = member.into_iter().collect();
    send(door, &fields, &fds)?;
    let mut buffer = vec![0_u8; MAX_MESSAGE];
    let Some((length, fds)) = receive(door, &mut buffer)? else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the job door closed",
        ));
    };
    let message = buffer.get(..length).unwrap_or_default();
    parse_answer(&decode(message), fds)
}

/// The app's door to its launcher, taken once and kept close-on-exec, so
/// the jobs it starts do not inherit it. Requests on it are one at a time.
fn door() -> Option<&'static std::sync::Mutex<OwnedFd>> {
    static DOOR: std::sync::OnceLock<Option<std::sync::Mutex<OwnedFd>>> =
        std::sync::OnceLock::new();
    DOOR.get_or_init(|| {
        // SAFETY: the door is this module's alone; nothing else in the
        // process takes the `jobs` service descriptor.
        let fd = unsafe { crate::take_service_fd(SERVICE) }?;
        rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC).ok()?;
        Some(std::sync::Mutex::new(fd))
    })
    .as_ref()
}

/// A job named `label` for this app, from its launcher's door; `None` where
/// the system offers none (see the module docs).
#[must_use]
pub fn slot(label: &str) -> Option<Slot> {
    let answer = {
        let door = door()?
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        request(door.as_fd(), label, None)
    };
    match answer {
        Ok(slot) => Some(slot),
        Err(error) => {
            eprintln!("tilo-jobs: {label} runs in its app's cgroup: {error}");
            None
        }
    }
}

/// Start `command` as an independent job named `label` when the system
/// offers one: in a cgroup of its own below this app's, ended alone when
/// memory runs out. Elsewhere `command` is left as it was.
pub fn independent<'a>(command: &'a mut Command, label: &str) -> &'a mut Command {
    match slot(label) {
        Some(slot) => slot.place(command),
        None => command,
    }
}

/// For a session service starting a process on an app's behalf: a job named
/// `label` in the app on the other end of `stream`, the connection the app's
/// launcher opened. `None` (logged) when that is no app of a delegated tree.
#[must_use]
pub fn slot_for_peer(stream: &std::os::unix::net::UnixStream, label: &str) -> Option<Slot> {
    let member = match peer_pidfd(stream.as_fd()) {
        Ok(member) => member,
        Err(error) => {
            eprintln!("tilo-jobs: {label} gets no job: the peer: {error}");
            return None;
        }
    };
    match slot_for_member(member.as_fd(), label) {
        Ok(slot) => slot,
        Err(error) => {
            eprintln!("tilo-jobs: {label} gets no job: {error}");
            None
        }
    }
}

/// A job named `label` in the app the process behind the pidfd `member`
/// belongs to, asked of its tree's spawner. `Ok(None)` when it is in no
/// delegated tree.
///
/// # Errors
///
/// The spawner's refusal, or no spawner to ask.
pub fn slot_for_member(member: BorrowedFd<'_>, label: &str) -> io::Result<Option<Slot>> {
    let pid = pid_of_pidfd(member)?;
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
    let Some(tree) = tree_in(&cgroup) else {
        return Ok(None);
    };
    let runtime = runtime_dir().ok_or_else(|| io::Error::other("no XDG_RUNTIME_DIR"))?;
    let spawner = connect(&socket_path(&runtime, tree))?;
    request(spawner.as_fd(), label, Some(member)).map(Some)
}

/// Connect to a spawner's job socket.
///
/// # Errors
///
/// The socket's refusal.
pub fn connect(path: &Path) -> io::Result<OwnedFd> {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType, socket_with};
    let socket = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )?;
    rustix::net::connect(&socket, &SocketAddrUnix::new(path)?)?;
    Ok(socket)
}

/// A pidfd of the process that created the socket on the other end of
/// `stream` (`SO_PEERPIDFD`): it pins that process, so its pid cannot name
/// another one while it is held.
///
/// # Errors
///
/// The kernel's refusal.
pub fn peer_pidfd(stream: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    use std::os::fd::FromRawFd as _;
    let mut fd: libc::c_int = -1;
    let mut length =
        libc::socklen_t::try_from(std::mem::size_of_val(&fd)).map_err(io::Error::other)?;
    // SAFETY: a valid socket and a correctly sized output buffer.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
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
        return Err(io::Error::other("no peer pidfd"));
    }
    // SAFETY: the kernel just installed this descriptor for this process.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// A pidfd of this process.
///
/// # Errors
///
/// The kernel's refusal.
pub fn own_pidfd() -> io::Result<OwnedFd> {
    use std::os::fd::FromRawFd as _;
    // SAFETY: pidfd_open takes scalars and returns a new descriptor or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, std::process::id(), 0) };
    let fd = i32::try_from(fd).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the kernel just installed this descriptor for this process.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The pid, in this process's pid namespace, of the process a pidfd refers
/// to (`Pid:` in its fdinfo).
///
/// # Errors
///
/// The process has ended, or is not visible here.
pub fn pid_of_pidfd(pidfd: BorrowedFd<'_>) -> io::Result<u32> {
    let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", pidfd.as_raw_fd()))?;
    pid_in_fdinfo(&info).ok_or_else(|| io::Error::other("the process has ended"))
}

/// The `Pid:` line of a pidfd's fdinfo; `None` for an ended process (`-1`)
/// or one outside this pid namespace (`0`).
#[must_use]
pub fn pid_in_fdinfo(info: &str) -> Option<u32> {
    let pid: i64 = info
        .lines()
        .find_map(|line| line.strip_prefix("Pid:"))?
        .trim()
        .parse()
        .ok()?;
    u32::try_from(pid).ok().filter(|pid| *pid > 0)
}

/// Whether the process behind `pidfd` is still running.
#[must_use]
pub fn alive(pidfd: BorrowedFd<'_>) -> bool {
    // SAFETY: signal 0 only checks; no pointers.
    unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            0,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        ) == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_request_and_its_answers_round_trip_and_refuse_what_they_cannot_carry() {
        let fields = request_fields("rust-analyzer\n--stdio\u{7}");
        let refs: Vec<&[u8]> = fields.iter().map(Vec::as_slice).collect();
        let wire = encode(&refs).unwrap();
        assert_eq!(
            parse_request(&decode(&wire)).as_deref(),
            Some("rust-analyzer --stdio")
        );
        assert_eq!(parse_request(&[b"kill", b"x"]), None);
        assert!(encode(&[b"job", b"a\0b"]).is_err());

        let refused = refused_fields("no app\0here");
        let refs: Vec<&[u8]> = refused.iter().map(Vec::as_slice).collect();
        let error = parse_answer(&refs, Vec::new()).unwrap_err();
        assert_eq!(error.to_string(), "no app here");
        // A grant without its descriptor is malformed, not a slot.
        let granted = granted_fields(600);
        let refs: Vec<&[u8]> = granted.iter().map(Vec::as_slice).collect();
        assert!(parse_answer(&refs, Vec::new()).is_err());
        let procs = OwnedFd::from(std::fs::File::open("/dev/null").unwrap());
        let slot = parse_answer(&refs, vec![procs]).unwrap();
        assert_eq!(slot.oom_score_adj(), 600);
    }

    #[test]
    fn labels_are_one_short_line() {
        assert_eq!(
            clean_label("  cargo\tbuild \n --release "),
            "cargo build --release"
        );
        assert_eq!(clean_label(&"x".repeat(200)).chars().count(), MAX_LABEL);
        assert_eq!(clean_label(""), "");
        assert_eq!(
            program_label(std::ffi::OsStr::new("/usr/bin/rust-analyzer")),
            "rust-analyzer"
        );
    }

    #[test]
    fn the_tree_and_its_socket_are_found_from_a_process_cgroup() {
        let tree = "/user.slice/user-1000.slice/user@1000.service/app.slice/tilo-apps-41.scope";
        assert_eq!(
            tree_in(&format!("0::{tree}/app-so.tilo.terminal-3/main\n")),
            Some("tilo-apps-41.scope")
        );
        assert_eq!(
            tree_in(
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/tilo-ush.service\n"
            ),
            None
        );
        assert_eq!(tree_in("0::/app.slice/tilo-apps-x.scope/app-a-1\n"), None);
        assert_eq!(
            socket_path(Path::new("/run/user/1000"), "tilo-apps-41.scope"),
            Path::new("/run/user/1000/tilo/app-jobs/tilo-apps-41.scope.sock")
        );
        assert_eq!(
            pid_in_fdinfo("pos:\t0\nflags:\t02\nPid:\t4242\n"),
            Some(4242)
        );
        assert_eq!(pid_in_fdinfo("Pid:\t-1\n"), None);
        assert_eq!(pid_in_fdinfo("Pid:\t0\n"), None);
    }

    #[test]
    fn the_oom_score_text_is_prepared_before_the_fork() {
        assert_eq!(AdjText::new(600).as_bytes(), b"600");
        assert_eq!(AdjText::new(-1000).as_bytes(), b"-1000");
        assert_eq!(AdjText::new(5000).as_bytes(), b"1000");
    }
}
