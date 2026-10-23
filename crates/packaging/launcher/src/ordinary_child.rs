//! Kernel custody of the fixed ordinary launcher owned by the installed root helper.
//! This transports ownership only; registration, package/session policy and the
//! inner ordinary namespace witness remain independent mandatory admissions.
use crate::{owned_service::ServiceCancellation, retained_view::ConsumerCommand};
use runtime_portal::{fd_transport, process_binding::ExecutingPeer};
use sha2::{Digest, Sha256};
use std::os::fd::AsFd;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            fs::{MetadataExt, OpenOptionsExt},
            net::UnixStream,
            process::{CommandExt, ExitStatusExt},
        },
    },
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
pub(crate) mod exposure_control;
pub use exposure_control::{ChildExposureOwner, ExposureRequest};

const MAGIC: &[u8; 4] = b"TOC1";
const WAIT_ENV: &str = "TILO_INTERNAL_LAUNCH_REGISTRATION_FD";
fn denied() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn digest(command: &ConsumerCommand) -> io::Result<[u8; 32]> {
    command.validate()?;
    if command.program != "/usr/libexec/tilo/tilo-launcher"
        || command.env.iter().any(|(key, _)| key == WAIT_ENV)
    {
        return Err(denied());
    }
    if command.env.iter().any(|(key, _)| {
        key.starts_with("LD_") || key.starts_with("DYLD_") || key == "GLIBC_TUNABLES"
    }) {
        return Err(denied());
    }
    Ok(Sha256::digest(serde_json::to_vec(command).map_err(io::Error::other)?).into())
}
fn namespace(pid: u32) -> io::Result<File> {
    File::open(format!("/proc/{pid}/ns/mnt"))
}
fn same(a: &File, b: &File) -> io::Result<bool> {
    let a = a.metadata()?;
    let b = b.metadata()?;
    Ok((a.dev(), a.ino()) == (b.dev(), b.ino()))
}
fn stopped(fd: &impl AsRawFd) -> io::Result<bool> {
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&raw mut p, 1, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if p.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(denied());
    }
    Ok(p.revents & libc::POLLIN != 0)
}
fn kernel_pid(fd: &OwnedFd) -> io::Result<u32> {
    let mut bytes = Vec::new();
    File::open(format!("/proc/self/fdinfo/{}", fd.as_raw_fd()))?
        .take(4097)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(denied());
    }
    let mut found = None;
    for line in std::str::from_utf8(&bytes)
        .map_err(io::Error::other)?
        .lines()
    {
        if let Some(value) = line.strip_prefix("Pid:") {
            let value = value.trim();
            if found.is_some() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(denied());
            }
            found = Some(value.parse::<u32>().map_err(io::Error::other)?);
        }
    }
    found.filter(|pid| *pid > 1).ok_or_else(denied)
}
fn tagged_send(stream: &mut UnixStream, tag: u8, fd: impl std::os::fd::AsFd) -> io::Result<()> {
    fd_transport::send_with_fd(stream.as_fd(), &[tag], fd)?;
    let mut ack = [0];
    stream.read_exact(&mut ack)?;
    if ack != [tag] {
        return Err(denied());
    }
    Ok(())
}
fn tagged_receive(stream: &mut UnixStream, tag: u8) -> io::Result<OwnedFd> {
    let (bytes, fd) = fd_transport::recv_with_fd(stream.as_fd())?;
    if bytes != [tag] {
        return Err(denied());
    }
    let fd = fd.ok_or_else(denied)?;
    stream.write_all(&[tag])?;
    Ok(fd)
}
fn same_root_socket(socket: &UnixStream, root: &ExecutingPeer) -> io::Result<()> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length =
        libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).map_err(io::Error::other)?;
    if unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>()
        || credentials.uid != 0
        || credentials.pid <= 0
        || credentials.pid.cast_unsigned() != root.pid()
        || !root.current()
    {
        return Err(denied());
    }
    Ok(())
}
fn target_environment(peer: &ExecutingPeer) -> io::Result<Vec<(String, String)>> {
    if !peer.current() {
        return Err(denied());
    }
    let mut bytes = Vec::new();
    File::open(format!("/proc/{}/environ", peer.pid()))?
        .take(262_145)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 262_144 || !peer.current() {
        return Err(denied());
    }
    let allowed = [
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "PATH",
        "LANG",
        "LANGUAGE",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
        "XDG_RUNTIME_DIR",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "XDG_DATA_DIRS",
        "XDG_CONFIG_DIRS",
        "XDG_SESSION_TYPE",
        "XDG_SESSION_ID",
        "XDG_CURRENT_DESKTOP",
        "DBUS_SESSION_BUS_ADDRESS",
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "XAUTHORITY",
    ];
    let mut result = Vec::new();
    for field in bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
    {
        let Some(equal) = field.iter().position(|byte| *byte == b'=') else {
            return Err(denied());
        };
        let key =
            std::str::from_utf8(field.get(..equal).ok_or_else(denied)?).map_err(|_| denied())?;
        if allowed.contains(&key) {
            let value_start = equal.checked_add(1).ok_or_else(denied)?;
            let value = std::str::from_utf8(field.get(value_start..).ok_or_else(denied)?)
                .map_err(|_| denied())?;
            if result.iter().any(|(prior, _)| prior == key) {
                return Err(denied());
            }
            result.push((key.to_owned(), value.to_owned()));
        }
    }
    if !result
        .iter()
        .any(|(key, value)| key == "HOME" && value.starts_with('/'))
        || !result
            .iter()
            .any(|(key, value)| key == "XDG_RUNTIME_DIR" && value.starts_with('/'))
        || !peer.current()
    {
        return Err(denied());
    }
    Ok(result)
}
fn protected_launcher() -> io::Result<File> {
    for path in ["/", "/usr", "/usr/libexec", "/usr/libexec/tilo"] {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path)?;
        let facts = file.metadata()?;
        if facts.uid() != 0 || facts.mode() & 0o022 != 0 {
            return Err(denied());
        }
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open("/usr/libexec/tilo/tilo-launcher")?;
    let facts = file.metadata()?;
    if !facts.is_file()
        || facts.uid() != 0
        || facts.nlink() != 1
        || facts.mode() & 0o7022 != 0
        || facts.mode() & 0o111 == 0
    {
        return Err(denied());
    }
    let mut magic = [0; 4];
    std::os::unix::fs::FileExt::read_exact_at(&file, &mut magic, 0)?;
    if &magic != b"\x7fELF" {
        return Err(denied());
    }
    Ok(file)
}
/// Root alone retains and reaps this actual Child. It grants no source access.
/// The caller must already own its private mount namespace and signed context.
pub struct OwnedOrdinaryChild {
    child: Child,
    pidfd: OwnedFd,
    namespace: File,
    gate: Option<UnixStream>,
    control: UnixStream,
    remote: Option<UnixStream>,
    digest: [u8; 32],
    cancel: ServiceCancellation,
    executable: File,
}
// Only async-signal-safe credential/descriptor syscalls run after fork.
fn prepare_launcher_credentials(
    cmd: &mut Command,
    wait: UnixStream,
    groups: Vec<u32>,
    uid: u32,
    gid: u32,
) {
    unsafe {
        cmd.pre_exec(move || {
            if libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) != 0
            {
                return Err(io::Error::last_os_error());
            }
            let fd = wait.as_raw_fd();
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0
                || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                || libc::prctl(libc::PR_SET_KEEPCAPS, 0, 0, 0, 0) != 0
                || libc::prctl(
                    libc::PR_CAP_AMBIENT,
                    libc::PR_CAP_AMBIENT_CLEAR_ALL,
                    0,
                    0,
                    0,
                ) != 0
                || libc::setgroups(groups.len(), groups.as_ptr()) != 0
                || libc::setresgid(gid, gid, gid) != 0
                || libc::setresuid(uid, uid, uid) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}
fn child_cancellation(cancel: ServiceCancellation) -> ServiceCancellation {
    let child = cancel.child();
    drop(cancel);
    child
}
impl OwnedOrdinaryChild {
    pub fn spawn(
        command: &ConsumerCommand,
        target: &ExecutingPeer,
        cancel: ServiceCancellation,
    ) -> io::Result<Self> {
        if rustix::process::getuid().as_raw() != 0
            || rustix::process::geteuid().as_raw() != 0
            || !target.current()
            || target.uid() == 0
            || cancel.is_cancelled()
        {
            return Err(denied());
        }
        let cancel = child_cancellation(cancel);
        let digest = digest(command)?;
        let groups = target.supplementary_groups()?.to_vec();
        let uid = target.uid();
        let gid = target.gid();
        let environment = target_environment(target)?;
        let executable = protected_launcher()?;
        let own_namespace = namespace(std::process::id())?;
        if same(&own_namespace, &namespace(target.pid())?)? {
            return Err(denied());
        }
        let (gate, wait) = UnixStream::pair()?;
        let (control, remote) = UnixStream::pair()?;
        let mut cmd = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
        cmd.arg0(&command.program)
            .args(&command.args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        for (key, value) in &environment {
            cmd.env(key, value);
        }
        for (key, value) in &command.env {
            cmd.env(key, value);
        }
        cmd.env(WAIT_ENV, wait.as_raw_fd().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        prepare_launcher_credentials(&mut cmd, wait, groups, uid, gid);
        let mut child = cmd.spawn()?;
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0u32) };
        if fd < 0 {
            let error = io::Error::last_os_error();
            let _ = child.kill();
            while child.wait().is_err() {
                let _ = child.kill();
                std::thread::park_timeout(Duration::from_millis(50));
            }
            return Err(error);
        }
        let pidfd = unsafe { OwnedFd::from_raw_fd(i32::try_from(fd).map_err(io::Error::other)?) };
        let child_namespace = match namespace(child.id()) {
            Ok(ns) => ns,
            Err(error) => {
                let _ = child.kill();
                while child.wait().is_err() {
                    let _ = child.kill();
                    std::thread::park_timeout(Duration::from_millis(50));
                }
                return Err(error);
            }
        };
        if !same(&own_namespace, &child_namespace).unwrap_or(false)
            || !target.current()
            || !protected_launcher()
                .and_then(|current| same(&executable, &current))
                .unwrap_or(false)
        {
            let _ = child.kill();
            while child.wait().is_err() {
                let _ = child.kill();
                std::thread::park_timeout(Duration::from_millis(50));
            }
            return Err(denied());
        }
        Ok(Self {
            child,
            pidfd,
            namespace: child_namespace,
            gate: Some(gate),
            control,
            remote: Some(remote),
            digest,
            cancel,
            executable,
        })
    }
    /// Start the actual root owner. A panic/observation error cannot mint stop.
    pub fn start_supervision(self) -> io::Result<OrdinaryChildSupervisor> {
        let launcher_pid = self.child.id();
        let launcher_pidfd = self.pidfd.try_clone()?;
        let launcher_namespace = self.namespace.try_clone()?;
        let cancel = self.cancel.clone();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let custody = Arc::new(Mutex::new(Some(self)));
        let owned = custody.clone();
        let observed = stop_flag.clone();
        let worker = std::thread::Builder::new()
            .name("root-ordinary-launcher-owner".into())
            .spawn(move || {
                let mut locked = owned
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(child) = locked.as_mut() else { return };
                let _ = child.serve();
                // Even protocol failure requires actual root kill/wait and kernel stop.
                let _ = child.child.kill();
                if child.child.wait().is_ok() && stopped(&child.pidfd).unwrap_or(false) {
                    locked.take();
                    drop(locked);
                    observed.store(true, Ordering::Release);
                }
            })?;
        Ok(OrdinaryChildSupervisor {
            launcher_pid,
            launcher_pidfd,
            launcher_namespace,
            custody,
            cancel,
            stopped: stop_flag,
            worker: Some(worker),
        })
    }
    /// Existing authenticated bootstrap only. ACKs mean transport, not authority.
    pub fn handoff(&mut self, stream: &mut UnixStream) -> io::Result<()> {
        if self.cancel.is_cancelled()
            || stopped(&self.pidfd)?
            || !same(&self.executable, &protected_launcher()?)?
        {
            return Err(denied());
        }
        let root_namespace = namespace(std::process::id())?;
        if !same(&self.namespace, &root_namespace)? {
            return Err(denied());
        }
        stream.write_all(MAGIC)?;
        stream.write_all(&self.digest)?;
        tagged_send(stream, 90, self.pidfd.as_fd())?;
        tagged_send(stream, 91, self.namespace.as_fd())?;
        tagged_send(stream, 92, self.gate.as_ref().ok_or_else(denied)?.as_fd())?;
        tagged_send(
            stream,
            93,
            self.child.stdout.as_ref().ok_or_else(denied)?.as_fd(),
        )?;
        tagged_send(
            stream,
            94,
            self.child.stderr.as_ref().ok_or_else(denied)?.as_fd(),
        )?;
        tagged_send(stream, 95, self.remote.as_ref().ok_or_else(denied)?.as_fd())?;
        tagged_send(stream, 96, root_namespace.as_fd())?;
        self.gate.take();
        self.child.stdout.take();
        self.child.stderr.take();
        self.remote.take();
        Ok(())
    }
    /// Run on the existing root owner worker. Reap evidence is an actual wait
    /// result. Caller still retains source and inner-namespace retirement duties.
    pub fn serve(&mut self) -> io::Result<()> {
        self.control
            .set_read_timeout(Some(Duration::from_millis(50)))?;
        self.control
            .set_write_timeout(Some(Duration::from_secs(10)))?;
        loop {
            if self.cancel.is_cancelled() {
                let _ = self.child.kill();
                return Err(io::ErrorKind::Interrupted.into());
            }
            let mut command = [0];
            match self.control.read_exact(&mut command) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(error) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return Err(error);
                }
                Ok(()) => {}
            }
            match command[0] {
                b'T' => {}
                b'K' => {
                    if self.child.try_wait()?.is_none() {
                        self.child.kill()?;
                    }
                }
                _ => return Err(denied()),
            }
            match self.child.try_wait()? {
                Some(status) => {
                    self.control.write_all(b"S")?;
                    self.control.write_all(&status.into_raw().to_be_bytes())?;
                    return Ok(());
                }
                None => self.control.write_all(b"P")?,
            }
        }
    }
}
impl Drop for OwnedOrdinaryChild {
    fn drop(&mut self) {
        // Handoff and owner-start failure retain real child custody until both
        // reap and the independently held kernel handle observe termination.
        loop {
            let _ = self.child.kill();
            if self.child.wait().is_ok() && stopped(&self.pidfd).unwrap_or(false) {
                break;
            }
            std::thread::park_timeout(Duration::from_millis(50));
        }
    }
}
/// Opaque kernel observations bound to the root actor, not a wire PID grant.
pub struct PreparedOrdinaryChild {
    pidfd: OwnedFd,
    root_pidfd: OwnedFd,
    root_pid: u32,
    parent_pidfd: Option<OwnedFd>,
    delegated_helper: Option<File>,
    namespace: File,
    pid: u32,
    gate: Option<UnixStream>,
    stdout: Option<File>,
    stderr: Option<File>,
    control: UnixStream,
    reaped: Option<ExitStatus>,
}
impl PreparedOrdinaryChild {
    pub fn receive(
        stream: &mut UnixStream,
        root: &ExecutingPeer,
        command: &ConsumerCommand,
    ) -> io::Result<Self> {
        if root.uid() != 0 || !root.current() {
            return Err(denied());
        }
        let mut magic = [0; 4];
        let mut expected = [0; 32];
        stream.read_exact(&mut magic)?;
        stream.read_exact(&mut expected)?;
        if &magic != MAGIC || expected != digest(command)? {
            return Err(denied());
        }
        let pidfd = tagged_receive(stream, 90)?;
        let namespace = File::from(tagged_receive(stream, 91)?);
        let gate = UnixStream::from(tagged_receive(stream, 92)?);
        let stdout = File::from(tagged_receive(stream, 93)?);
        let stderr = File::from(tagged_receive(stream, 94)?);
        let control = UnixStream::from(tagged_receive(stream, 95)?);
        let root_namespace = File::from(tagged_receive(stream, 96)?);
        mount_namespace(&root_namespace)?;
        let pid = kernel_pid(&pidfd)?;
        for pipe in [&stdout, &stderr] {
            let facts = pipe.metadata()?;
            let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
            if facts.mode() & libc::S_IFMT != libc::S_IFIFO
                || flags < 0
                || flags & libc::O_ACCMODE != libc::O_RDONLY
            {
                return Err(denied());
            }
        }
        gate.peer_addr()?;
        control.peer_addr()?;
        same_root_socket(&gate, root)?;
        same_root_socket(&control, root)?;
        let mut status = String::new();
        File::open(format!("/proc/{pid}/status"))?
            .take(65537)
            .read_to_string(&mut status)?;
        if status.len() > 65536
            || !status.lines().any(|line| {
                line.strip_prefix("PPid:")
                    .is_some_and(|value| value.trim() == root.pid().to_string())
            })
        {
            return Err(denied());
        }

        if stopped(&pidfd)?
            || !same(&namespace, &self::namespace(pid)?)?
            || !same(&namespace, &root_namespace)?
            || same(&namespace, &self::namespace(std::process::id())?)?
            || !root.current()
        {
            return Err(denied());
        }
        control.set_read_timeout(Some(Duration::from_secs(10)))?;
        control.set_write_timeout(Some(Duration::from_secs(10)))?;
        Ok(Self {
            pidfd,
            root_pidfd: root.pidfd().try_clone_to_owned()?,
            root_pid: root.pid(),
            parent_pidfd: None,
            delegated_helper: None,
            namespace,
            pid,
            gate: Some(gate),
            stdout: Some(stdout),
            stderr: Some(stderr),
            control,
            reaped: None,
        })
    }
    pub fn id(&self) -> u32 {
        self.pid
    }
    /// # Errors
    /// Refuses a gate that has already been taken.
    pub fn gate(&mut self) -> io::Result<UnixStream> {
        self.gate.take().ok_or_else(denied)
    }
    pub fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take()
    }
    pub fn take_stderr(&mut self) -> Option<File> {
        self.stderr.take()
    }
    /// # Errors
    /// Refuses stale root custody or a peer outside the held launcher namespace.
    pub fn check_peer(&self, peer: &ExecutingPeer) -> io::Result<()> {
        self.check_root()?;
        peer.matches_transferred_pidfd(std::os::fd::AsFd::as_fd(&self.pidfd))?;
        if peer.pid() != self.pid || !same(&self.namespace, &namespace(peer.pid())?)? {
            return Err(denied());
        }
        Ok(())
    }
    fn check_root(&self) -> io::Result<()> {
        if stopped(&self.root_pidfd)? {
            return Err(denied());
        }
        if let Some(parent) = &self.parent_pidfd
            && stopped(parent)?
        {
            return Err(denied());
        }
        if let Some(helper) = &self.delegated_helper
            && (!same(helper, &protected_consuming_helper()?)?
                || !same(helper, &File::open(format!("/proc/{}/exe", self.root_pid))?)?)
        {
            return Err(denied());
        }
        Ok(())
    }
    fn request(&mut self, command: u8) -> io::Result<Option<ExitStatus>> {
        self.check_root()?;
        if stopped(&self.root_pidfd)? {
            return Err(denied());
        }
        self.control.write_all(&[command])?;
        let mut state = [0];
        self.control.read_exact(&mut state)?;
        self.check_root()?;
        if stopped(&self.root_pidfd)? {
            return Err(denied());
        }
        match state[0] {
            b'P' => Ok(None),
            b'S' => {
                let mut bytes = [0; 4];
                self.control.read_exact(&mut bytes)?;
                let raw = i32::from_be_bytes(bytes);
                if !stopped(&self.pidfd)? || !(libc::WIFEXITED(raw) || libc::WIFSIGNALED(raw)) {
                    return Err(denied());
                }
                self.reaped = Some(ExitStatus::from_raw(raw));
                Ok(Some(ExitStatus::from_raw(raw)))
            }
            _ => Err(denied()),
        }
    }
    pub fn kill(&mut self) -> io::Result<()> {
        if self.reaped.is_some() {
            return Ok(());
        }
        let _ = self.request(b'K')?;
        Ok(())
    }
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.reaped {
            return Ok(status);
        }
        loop {
            if let Some(status) = self.request(b'T')? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
impl Drop for PreparedOrdinaryChild {
    fn drop(&mut self) {
        if self.reaped.is_none() {
            let _ = self.control.write_all(b"K");
            let _ = self.control.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Parent root driver retains this through child reap AND its separate source/
/// inner-namespace cleanup. Thread completion alone is never stop evidence.
pub struct OrdinaryChildSupervisor {
    launcher_pid: u32,
    launcher_pidfd: OwnedFd,
    launcher_namespace: File,
    custody: Arc<Mutex<Option<OwnedOrdinaryChild>>>,
    cancel: ServiceCancellation,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl OrdinaryChildSupervisor {
    /// Verify the actual still-owned launcher; transferred labels never suffice.
    pub(crate) fn verify_launcher_pidfd(&self, received: &OwnedFd) -> io::Result<()> {
        if self.observed_stopped() || stopped(&self.launcher_pidfd)? || stopped(received)? {
            return Err(denied());
        }
        let pid = kernel_pid(received)?;
        if pid != self.launcher_pid
            || kernel_pid(&self.launcher_pidfd)? != pid
            || !same(&self.launcher_namespace, &namespace(pid)?)?
        {
            return Err(denied());
        }
        if stopped(received)? || stopped(&self.launcher_pidfd)? {
            return Err(denied());
        }
        Ok(())
    }
    pub fn observed_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    pub fn retire_before_exit(&mut self) {
        self.cancel.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // Recover actual custody after an owner panic or failed observation;
        // never replace this with a thread-completion or cancellation claim.
        while !self.observed_stopped() {
            let mut locked = self
                .custody
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(child) = locked.as_mut() {
                let _ = child.child.kill();
                if child.child.wait().is_ok() && stopped(&child.pidfd).unwrap_or(false) {
                    locked.take();
                    self.stopped.store(true, Ordering::Release);
                }
            }
            drop(locked);
            if !self.observed_stopped() {
                std::thread::park_timeout(Duration::from_millis(50));
            }
        }
    }
}
impl Drop for OrdinaryChildSupervisor {
    fn drop(&mut self) {
        self.retire_before_exit();
        let _ = &self.custody;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These are ownership failure tests of a real controlled process. They
    // do not construct installed source, person, package or namespace grants.
    fn controlled_child() -> (OwnedOrdinaryChild, UnixStream, OwnedFd) {
        let child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("controlled child");
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0u32) };
        assert!(raw >= 0, "actual pidfd required");
        let pidfd = unsafe {
            OwnedFd::from_raw_fd(i32::try_from(raw).expect("pidfd fits a file descriptor"))
        };
        let observed = pidfd.try_clone().unwrap();
        let (control, remote) = UnixStream::pair().unwrap();
        let (gate, _wait) = UnixStream::pair().unwrap();
        let namespace = namespace(child.id()).unwrap();
        (
            OwnedOrdinaryChild {
                child,
                pidfd,
                namespace,
                gate: Some(gate),
                control,
                remote: None,
                digest: [0; 32],
                cancel: ServiceCancellation::default(),
                executable: File::open("/proc/self/exe").unwrap(),
            },
            remote,
            observed,
        )
    }

    #[test]
    fn live_launcher_witness_uses_immutable_observation_while_owner_serves() {
        let (child, _remote, observed) = controlled_child();
        let mut owner = child.start_supervision().unwrap();
        owner.verify_launcher_pidfd(&observed).unwrap();
        let unrelated: OwnedFd = tempfile::tempfile().unwrap().into();
        assert!(owner.verify_launcher_pidfd(&unrelated).is_err());
        owner.retire_before_exit();
        assert!(owner.verify_launcher_pidfd(&observed).is_err());
        assert!(stopped(&observed).unwrap());
    }

    #[test]
    fn malformed_status_request_retires_actual_child_without_positive_protocol_reply() {
        let (child, mut remote, observed) = controlled_child();
        let mut owner = child.start_supervision().unwrap();
        remote.write_all(b"?").unwrap();
        tilo_test_env::wait_until("the owner to process the refusal", || {
            owner.worker.as_ref().unwrap().is_finished()
        });
        owner.retire_before_exit();
        assert!(owner.observed_stopped());
        assert!(stopped(&observed).unwrap());
        let mut response = [0];
        assert_eq!(remote.read(&mut response).unwrap(), 0);
    }

    #[test]
    fn panicked_worker_retains_real_child_until_owner_recovers_poisoned_custody() {
        let (child, _remote, observed) = controlled_child();
        let launcher_pid = child.child.id();
        let launcher_pidfd = child.pidfd.try_clone().unwrap();
        let launcher_namespace = child.namespace.try_clone().unwrap();
        let cancel = child.cancel.clone();
        let custody = Arc::new(Mutex::new(Some(child)));
        let worker_custody = custody.clone();
        let worker = std::thread::spawn(move || {
            let _held = worker_custody.lock().unwrap();
            panic!("controlled owner failure before stop observation");
        });
        let mut owner = OrdinaryChildSupervisor {
            launcher_pid,
            launcher_pidfd,
            launcher_namespace,
            custody,
            cancel,
            stopped: Arc::new(AtomicBool::new(false)),
            worker: Some(worker),
        };
        assert!(!owner.observed_stopped());
        assert!(!stopped(&observed).unwrap());
        owner.retire_before_exit();
        assert!(owner.observed_stopped());
        assert!(stopped(&observed).unwrap());
        assert!(
            owner
                .custody
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none()
        );
    }

    #[test]
    fn disconnected_control_drop_reaps_actual_process() {
        let (mut child, remote, observed) = controlled_child();
        drop(remote);
        // A dropped transport cannot be treated as an exit observation.
        assert!(!stopped(&observed).unwrap());
        assert!(child.serve().is_err());
        drop(child);
        assert!(stopped(&observed).unwrap());
    }
}

const BOOTSTRAP_MAGIC: &[u8; 4] = b"TCB1";
const BOOTSTRAP_REFUSED: &[u8; 4] = b"TCB0";
const HELPER_PROGRAM: &str = "/usr/libexec/tilo-prepared-setup";
fn protected_consuming_helper() -> io::Result<File> {
    for path in ["/", "/usr", "/usr/libexec"] {
        let held = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path)?;
        let facts = held.metadata()?;
        if facts.uid() != 0 || facts.mode() & 0o022 != 0 {
            return Err(denied());
        }
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(HELPER_PROGRAM)?;
    let facts = file.metadata()?;
    if !facts.is_file()
        || facts.uid() != 0
        || facts.nlink() != 1
        || facts.mode() & 0o7022 != 0
        || facts.mode() & 0o111 == 0
    {
        return Err(denied());
    }
    let mut magic = [0; 4];
    std::os::unix::fs::FileExt::read_exact_at(&file, &mut magic, 0)?;
    if &magic != b"\x7fELF" {
        return Err(denied());
    }
    Ok(file)
}
fn exact_parent(pid: u32, parent: u32) -> io::Result<()> {
    let mut bytes = Vec::new();
    File::open(format!("/proc/{pid}/status"))?
        .take(65537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(denied());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| denied())?;
    let mut found = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("PPid:") {
            let value = value.trim();
            if found.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(denied());
            }
            found = Some(value.parse::<u32>().map_err(|_| denied())?);
        }
    }
    if found != Some(parent) {
        return Err(denied());
    }
    Ok(())
}
/// Deliver only an actual owned root child already independently authenticated
/// by the original helper. No command, UID, source or namespace grant is sent.
/// The proof endpoint must have been created by that exact consuming child.
/// # Errors
/// Refuses foreign child/parent, dead peer, replaced installed image or endpoint.
pub fn send_consuming_bootstrap(
    stream: &mut UnixStream,
    child: &mut Child,
    peer: &ExecutingPeer,
    endpoint: &UnixStream,
) -> io::Result<()> {
    let transfer = review_bootstrap(child, peer, endpoint)?;
    transfer.send(stream, peer, endpoint)
}
struct BootstrapTransfer {
    namespaces: ParentObservedNamespaces,
    helper: File,
    start: u64,
}
fn review_bootstrap(
    child: &mut Child,
    peer: &ExecutingPeer,
    endpoint: &UnixStream,
) -> io::Result<BootstrapTransfer> {
    if rustix::process::getuid().as_raw() != 0
        || rustix::process::geteuid().as_raw() != 0
        || peer.uid() != 0
        || peer.pid() != child.id()
        || !peer.current()
        || child.try_wait()?.is_some()
    {
        return Err(denied());
    }
    exact_parent(peer.pid(), std::process::id())?;
    consuming_mode(peer.pid())?;
    same_root_socket(endpoint, peer)?;
    let helper = protected_consuming_helper()?;
    if !same(&helper, &File::open(format!("/proc/{}/exe", peer.pid()))?)? {
        return Err(denied());
    }
    let start = peer.start_time()?;
    let namespaces = ParentObservedNamespaces {
        child: namespace(peer.pid())?,
        parent: namespace(std::process::id())?,
    };
    namespaces.validate()?;
    Ok(BootstrapTransfer {
        namespaces,
        helper,
        start,
    })
}
impl BootstrapTransfer {
    fn send(
        self,
        stream: &mut UnixStream,
        peer: &ExecutingPeer,
        endpoint: &UnixStream,
    ) -> io::Result<()> {
        let Self {
            helper,
            start,
            namespaces,
        } = self;
        stream.write_all(BOOTSTRAP_MAGIC)?;
        stream.write_all(&start.to_be_bytes())?;
        tagged_send(stream, 86, peer.pidfd())?;
        tagged_send(stream, 87, helper.as_fd())?;
        tagged_send(stream, 88, endpoint.as_fd())?;
        tagged_send(stream, 97, namespaces.child.as_fd())?;
        tagged_send(stream, 98, namespaces.parent.as_fd())?;
        if !peer.current()
            || peer.start_time()? != start
            || !same(&namespaces.child, &namespace(peer.pid())?)?
            || !same(&namespaces.parent, &namespace(std::process::id())?)?
            || !same(&helper, &protected_consuming_helper()?)?
        {
            return Err(denied());
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum BootstrapKind {
    Refused,
    Delegated,
}
fn bootstrap_kind(magic: [u8; 4]) -> io::Result<BootstrapKind> {
    match &magic {
        value if value == BOOTSTRAP_MAGIC => Ok(BootstrapKind::Delegated),
        value if value == BOOTSTRAP_REFUSED => Ok(BootstrapKind::Refused),
        _ => Err(denied()),
    }
}
fn read_bootstrap_kind(stream: &mut UnixStream) -> io::Result<BootstrapKind> {
    let mut magic = [0; 4];
    stream.read_exact(&mut magic)?;
    bootstrap_kind(magic)
}
/// Custody framing outcome only, never view admission or successful launch.
pub enum DelegationOutcome {
    Delegated,
    Refused,
}
/// Emit the sole complete negative handoff at the existing delegation phase.
/// # Errors
/// An incomplete write leaves the bootstrap unusable and must retire it.
pub fn refuse_consuming_bootstrap(stream: &mut UnixStream, deadline: Instant) -> io::Result<()> {
    let fence = SocketDeadline::start(stream, ServiceCancellation::default(), deadline)?;
    stream.write_all(BOOTSTRAP_REFUSED)?;
    fence.check()
}

/// A fresh direct QPE to the exact root child delegated by the original helper.
/// This owns process observations only; the original authenticated source and
/// session custody remain mandatory and are never replaced by this value.
pub struct DelegatedConsumingBootstrap {
    namespace: File,
    parent_pidfd: OwnedFd,
    peer: ExecutingPeer,
    helper: File,
    endpoint: UnixStream,
}
impl DelegatedConsumingBootstrap {
    /// Receive on the existing original-root transport, then authenticate both
    /// directions directly on the consuming child's private proof endpoint.
    /// # Errors
    /// Refuses any missing parent, current image, exact pidfd or fresh QPE.
    pub fn receive(stream: &mut UnixStream, parent: &ExecutingPeer) -> io::Result<Self> {
        Self::receive_until(
            stream,
            parent,
            Instant::now()
                .checked_add(Duration::from_secs(10))
                .ok_or(io::ErrorKind::InvalidInput)?,
        )
    }
    /// # Errors
    /// Refuses the exact shared frame or direct private child proof before the
    /// caller's absolute deadline, without renewing it on partial progress.
    pub fn receive_until(
        stream: &mut UnixStream,
        parent: &ExecutingPeer,
        deadline: Instant,
    ) -> io::Result<Self> {
        Self::receive_selected(stream, parent, deadline)??.ok_or_else(denied)
    }
    /// Complete shared framing first. A closed refusal or failure on the
    /// separately authenticated child endpoint is an operation outcome; malformed
    /// or truncated shared framing remains a bootstrap transport error.
    /// # Errors
    /// Refuses stale parent custody, malformed framing or an expired deadline; inner errors report child authentication failure.
    pub fn receive_selected(
        stream: &mut UnixStream,
        parent: &ExecutingPeer,
        deadline: Instant,
    ) -> io::Result<io::Result<Option<Self>>> {
        if parent.uid() != 0 || !parent.current() {
            return Err(denied());
        }
        let shared_fence = SocketDeadline::start(stream, ServiceCancellation::default(), deadline)?;
        let mut start = [0; 8];
        if read_bootstrap_kind(stream)? == BootstrapKind::Refused {
            shared_fence.check()?;
            return Ok(Ok(None));
        }
        stream.read_exact(&mut start)?;
        let pidfd = tagged_receive(stream, 86)?;
        let helper = File::from(tagged_receive(stream, 87)?);
        let endpoint = UnixStream::from(tagged_receive(stream, 88)?);
        let namespaces = ParentObservedNamespaces {
            child: File::from(tagged_receive(stream, 97)?),
            parent: File::from(tagged_receive(stream, 98)?),
        };
        shared_fence.check()?;
        drop(shared_fence);
        Ok(Self::authenticate_received(
            parent, start, pidfd, helper, endpoint, namespaces, deadline,
        )
        .map(Some))
    }
    fn authenticate_received(
        parent: &ExecutingPeer,
        start: [u8; 8],
        pidfd: OwnedFd,
        helper: File,
        mut endpoint: UnixStream,
        namespaces: ParentObservedNamespaces,
        deadline: Instant,
    ) -> io::Result<Self> {
        let fence = SocketDeadline::start(&endpoint, ServiceCancellation::default(), deadline)?;
        let pid = kernel_pid(&pidfd)?;
        if pid == parent.pid()
            || stopped(&pidfd)?
            || !same(&helper, &protected_consuming_helper()?)?
        {
            return Err(denied());
        }
        exact_parent(pid, parent.pid())?;
        consuming_mode(pid)?;
        namespaces.validate()?;
        if same(&namespaces.child, &namespace(std::process::id())?)? {
            return Err(denied());
        }
        endpoint.set_read_timeout(Some(Duration::from_secs(10)))?;
        endpoint.set_write_timeout(Some(Duration::from_secs(10)))?;
        // Consuming root first authenticates this actual target Shell, just as
        // the original bootstrap does; then Shell authenticates the delegate.
        runtime_portal::process_binding::complete(&mut endpoint)?;
        endpoint.set_read_timeout(Some(Duration::from_secs(10)))?;
        endpoint.set_write_timeout(Some(Duration::from_secs(10)))?;
        runtime_portal::process_binding::arm(&endpoint)?;
        let challenge = fresh_challenge();
        endpoint.write_all(&runtime_portal::process_binding::MAGIC)?;
        endpoint.write_all(&challenge)?;
        let peer = runtime_portal::process_binding::receive(&endpoint, &challenge, 0)?;
        peer.matches_transferred_pidfd(std::os::fd::AsFd::as_fd(&pidfd))?;
        drop(pidfd);
        same_root_socket(&endpoint, &peer)?;
        if peer.pid() != pid
            || peer.start_time()? != u64::from_be_bytes(start)
            || !parent.current()
            || !peer.current()
            || !same(&helper, &protected_consuming_helper()?)?
        {
            return Err(denied());
        }
        exact_parent(pid, parent.pid())?;
        consuming_mode(pid)?;
        endpoint.write_all(&[1])?;
        fence.check()?;
        Ok(Self {
            namespace: namespaces.child,
            parent_pidfd: parent.pidfd().try_clone_to_owned()?,
            peer,
            helper,
            endpoint,
        })
    }
    /// # Errors
    /// Refuses the private ordinary-child handoff under the same total deadline.
    pub fn receive_ordinary_until(
        mut self,
        command: &ConsumerCommand,
        deadline: Instant,
    ) -> io::Result<PreparedOrdinaryChild> {
        let fence =
            SocketDeadline::start(&self.endpoint, ServiceCancellation::default(), deadline)?;
        if stopped(&self.parent_pidfd)?
            || !self.peer.current()
            || !same(&self.helper, &protected_consuming_helper()?)?
        {
            return Err(denied());
        }
        let mut child = PreparedOrdinaryChild::receive(&mut self.endpoint, &self.peer, command)?;
        if stopped(&self.parent_pidfd)?
            || !self.peer.current()
            || !same(&self.namespace, &child.namespace)?
        {
            return Err(denied());
        }
        child.parent_pidfd = Some(self.parent_pidfd);
        child.delegated_helper = Some(self.helper);
        fence.check()?;
        Ok(child)
    }
}

/// Only internal fixed-helper entry; arguments carry one inherited private
/// control descriptor, never paths, credentials, commands or source authority.
pub const CONSUMING_BOOTSTRAP_MODE: &str = "--retained-consumer-bootstrap";

fn consuming_mode(pid: u32) -> io::Result<()> {
    let mut bytes = Vec::new();
    File::open(format!("/proc/{pid}/cmdline"))?
        .take(4097)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 4096 || !bytes.ends_with(&[0]) {
        return Err(denied());
    }
    let args = bytes
        .strip_suffix(&[0])
        .ok_or_else(denied)?
        .split(|byte| *byte == 0)
        .collect::<Vec<_>>();
    let [program, mode, descriptor] = args.as_slice() else {
        return Err(denied());
    };
    if *program != HELPER_PROGRAM.as_bytes() || *mode != CONSUMING_BOOTSTRAP_MODE.as_bytes() {
        return Err(denied());
    }
    let descriptor = std::str::from_utf8(descriptor).map_err(|_| denied())?;
    let fd = descriptor.parse::<i32>().map_err(|_| denied())?;
    if fd < 3 || fd.to_string() != descriptor {
        return Err(denied());
    }
    Ok(())
}

/// Root ownership of the actual fixed consuming bootstrap. Its stop certifies
/// this outer task only. Source/target namespace and exposure retirement must
/// still be observed independently by their existing canonical owners.
pub struct ManagedConsumingBootstrap {
    child: Child,
    peer: ExecutingPeer,
    namespace: File,
    helper: File,
    control: UnixStream,
    shell_endpoint: Option<UnixStream>,
    cancel: ServiceCancellation,
    stop_requested: bool,
    command_sent: bool,
}
impl ManagedConsumingBootstrap {
    /// Spawn only the fixed protected internal helper from an actual installed
    /// root helper, then authenticate its current kernel identity both ways.
    /// The child entry must isolate its own mount namespace before QPE.
    /// # Errors
    /// Refuses root/program/child/QPE/namespace evidence or cancelled custody.
    pub fn spawn(cancel: ServiceCancellation) -> io::Result<Self> {
        if rustix::process::getuid().as_raw() != 0
            || rustix::process::geteuid().as_raw() != 0
            || cancel.is_cancelled()
        {
            return Err(denied());
        }
        let cancel = child_cancellation(cancel);
        let helper = protected_consuming_helper()?;
        if !same(&helper, &File::open("/proc/self/exe")?)? {
            return Err(denied());
        }
        let (mut control, inherited) = UnixStream::pair()?;
        control.set_read_timeout(Some(Duration::from_secs(10)))?;
        control.set_write_timeout(Some(Duration::from_secs(10)))?;
        let mut command = Command::new(format!("/proc/self/fd/{}", helper.as_raw_fd()));
        command
            .arg0(HELPER_PROGRAM)
            .arg(CONSUMING_BOOTSTRAP_MODE)
            .arg(inherited.as_raw_fd().to_string())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || {
                if libc::syscall(
                    libc::SYS_close_range,
                    3u32,
                    u32::MAX,
                    libc::CLOSE_RANGE_CLOEXEC,
                ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                let fd = inherited.as_raw_fd();
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        // Until successful mutual authentication, no target or source may be
        // created in the child. Failure therefore reaps this actual bootstrap.
        let admitted = (|| {
            runtime_portal::process_binding::complete(&mut control)?;
            control.set_read_timeout(Some(Duration::from_secs(10)))?;
            control.set_write_timeout(Some(Duration::from_secs(10)))?;
            runtime_portal::process_binding::arm(&control)?;
            let challenge = fresh_challenge();
            control.write_all(&runtime_portal::process_binding::MAGIC)?;
            control.write_all(&challenge)?;
            let peer = runtime_portal::process_binding::receive(&control, &challenge, 0)?;
            if peer.pid() != child.id()
                || !peer.current()
                || child.try_wait()?.is_some()
                || !same(&helper, &File::open(format!("/proc/{}/exe", peer.pid()))?)?
                || !same(&helper, &protected_consuming_helper()?)?
            {
                return Err(denied());
            }
            exact_parent(peer.pid(), std::process::id())?;
            let namespace = namespace(peer.pid())?;
            if same(&namespace, &self::namespace(std::process::id())?)? {
                return Err(denied());
            }
            control.write_all(&[1])?;
            let endpoint = UnixStream::from(tagged_receive(&mut control, 89)?);
            same_root_socket(&endpoint, &peer)?;
            if !peer.current() || cancel.is_cancelled() {
                return Err(denied());
            }
            Ok((peer, namespace, endpoint))
        })();
        let (peer, namespace, endpoint) = match admitted {
            Ok(admitted) => admitted,
            Err(error) => {
                let _ = child.kill();
                while child.wait().is_err() {
                    let _ = child.kill();
                    std::thread::park_timeout(Duration::from_millis(50));
                }
                return Err(error);
            }
        };
        Ok(Self {
            child,
            peer,
            namespace,
            helper,
            control,
            shell_endpoint: Some(endpoint),
            cancel,
            stop_requested: false,
            command_sent: false,
        })
    }
    fn check(&self) -> io::Result<()> {
        if self.cancel.is_cancelled()
            || self.stop_requested
            || !self.peer.current()
            || !same(&self.namespace, &namespace(self.peer.pid())?)?
            || !same(&self.helper, &protected_consuming_helper()?)?
            || !same(
                &self.helper,
                &File::open(format!("/proc/{}/exe", self.peer.pid()))?,
            )?
        {
            return Err(denied());
        }
        exact_parent(self.peer.pid(), std::process::id())
    }
    /// Send the single child-created direct Shell proof endpoint on the existing
    /// authenticated root bootstrap transport. No listener is created.
    /// # Errors
    /// Refuses repeated delegation or lost actual child/program custody.
    pub fn delegate_shell(&mut self, stream: &mut UnixStream) -> io::Result<()> {
        self.check()?;
        let endpoint = self.shell_endpoint.as_ref().ok_or_else(denied)?;
        send_consuming_bootstrap(stream, &mut self.child, &self.peer, endpoint)?;
        self.shell_endpoint.take();
        self.check()
    }
    /// Queue the frozen command before shared delegation. The child still waits
    /// for genuine direct Shell QPE before reading or executing this command.
    /// A local refusal emits exactly TCB0; only attempted shared framing errors
    /// escape as errors and require global bootstrap retirement.
    /// # Errors
    /// Refuses incomplete shared framing or an elapsed absolute request budget.
    pub fn delegate_shell_command(
        &mut self,
        stream: &mut UnixStream,
        command: &ConsumerCommand,
        target: &ExecutingPeer,
        deadline: Instant,
    ) -> io::Result<DelegationOutcome> {
        let prepared = (|| {
            self.check()?;
            let fence = SocketDeadline::start(&self.control, self.cancel.clone(), deadline)?;
            self.send_command(command, target)?;
            fence.check()?;
            let endpoint = self.shell_endpoint.as_ref().ok_or_else(denied)?;
            review_bootstrap(&mut self.child, &self.peer, endpoint)
        })();
        let Ok(transfer) = prepared else {
            refuse_consuming_bootstrap(stream, deadline)?;
            return Ok(DelegationOutcome::Refused);
        };
        let fence = SocketDeadline::start(stream, ServiceCancellation::default(), deadline)?;
        transfer.send(
            stream,
            &self.peer,
            self.shell_endpoint.as_ref().ok_or_else(denied)?,
        )?;
        self.shell_endpoint.take();
        fence.check()?;
        Ok(DelegationOutcome::Delegated)
    }
    /// Request cancellation of the fixed child driver, which must first retire
    /// its ordinary child and exposure owners before exiting. This never
    /// certifies inner namespace or source cleanup.
    pub fn retire_before_exit(&mut self) {
        if !self.stop_requested {
            self.stop_requested = true;
            self.cancel.cancel();
            let _ = self.control.write_all(b"X");
            let _ = self.control.shutdown(std::net::Shutdown::Write);
        }
        // Keep actual owned Child and held namespace until root-child reaping.
        // Parent/child controller failure cannot turn into a successful detach.
        while self.child.wait().is_err() {
            std::thread::park_timeout(Duration::from_millis(50));
        }
        while !stopped(&self.peer.pidfd()).unwrap_or(false) {
            std::thread::park_timeout(Duration::from_millis(50));
        }
    }
}
impl Drop for ManagedConsumingBootstrap {
    fn drop(&mut self) {
        self.retire_before_exit();
    }
}

/// A bounded descriptive command on the already authenticated parent channel.
/// It is not installed source, person or package authority.
pub enum ConsumingBootstrapCommand {
    Launch(ObservedCommand),
    Exposure(ExposureRequest),
    Retire,
}
impl ManagedConsumingBootstrap {
    /// Deliver the exact frozen target command once, over the same private
    /// parent channel. The independently authenticated Shell checks its digest.
    /// # Errors
    /// Refuses repeated command, current custody loss or invalid fixed program.
    pub fn send_command(
        &mut self,
        command: &ConsumerCommand,
        target: &ExecutingPeer,
    ) -> io::Result<()> {
        self.check()?;
        if self.command_sent {
            return Err(denied());
        }
        digest(command)?;
        let routing = TargetKernelRouting::observe(target)?;
        let bytes = serde_json::to_vec(&RoutedCommandWire {
            command: command.clone(),
            routing,
        })
        .map_err(io::Error::other)?;
        if bytes.is_empty() || bytes.len() > 64 * 1024 {
            return Err(denied());
        }
        self.control.write_all(b"C")?;
        self.control.write_all(
            &u32::try_from(bytes.len())
                .map_err(|_| denied())?
                .to_be_bytes(),
        )?;
        self.control.write_all(&bytes)?;
        self.command_sent = true;
        if !target.current() {
            return Err(denied());
        }
        self.check()
    }
}

/// The internal fixed helper's current parent and private channels. This grants
/// no source access; Imager independently keeps its actual source/session owner
/// in the original parent and checks observed Shell against that certified input.
pub struct ConsumingBootstrapEntry {
    parent: ExecutingPeer,
    helper: File,
    control: UnixStream,
    shell: UnixStream,
    command_received: bool,
    shell_authenticated: bool,
    target_routing: Option<TargetKernelRouting>,
}
impl ConsumingBootstrapEntry {
    /// Call after isolating this helper's own mount namespace. The inherited
    /// descriptor alone is insufficient: actual root-parent QPE/image are needed.
    /// # Errors
    /// Refuses wrong mode, root, parent, installed image or mount namespace.
    pub fn authenticate(mut control: UnixStream) -> io::Result<Self> {
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(denied());
        }
        consuming_mode(std::process::id())?;
        let helper = protected_consuming_helper()?;
        if !same(&helper, &File::open("/proc/self/exe")?)? {
            return Err(denied());
        }
        let parent_pid = unsafe { libc::getppid() }.cast_unsigned();
        if parent_pid <= 1 || same(&namespace(std::process::id())?, &namespace(parent_pid)?)? {
            return Err(denied());
        }
        control.set_read_timeout(Some(Duration::from_secs(10)))?;
        control.set_write_timeout(Some(Duration::from_secs(10)))?;
        runtime_portal::process_binding::arm(&control)?;
        let challenge = fresh_challenge();
        control.write_all(&runtime_portal::process_binding::MAGIC)?;
        control.write_all(&challenge)?;
        let parent = runtime_portal::process_binding::receive(&control, &challenge, 0)?;
        if parent.pid() != parent_pid
            || !parent.current()
            || !same(&helper, &File::open(format!("/proc/{parent_pid}/exe"))?)?
        {
            return Err(denied());
        }
        exact_parent(std::process::id(), parent_pid)?;
        control.write_all(&[1])?;
        runtime_portal::process_binding::complete(&mut control)?;
        // complete resets timeouts; restore the bounded controller contract.
        control.set_read_timeout(Some(Duration::from_secs(10)))?;
        control.set_write_timeout(Some(Duration::from_secs(10)))?;
        let (shell, endpoint) = UnixStream::pair()?;
        tagged_send(&mut control, 89, endpoint.as_fd())?;
        if !parent.current() || !same(&helper, &protected_consuming_helper()?)? {
            return Err(denied());
        }
        Ok(Self {
            parent,
            helper,
            control,
            shell,
            command_received: false,
            shell_authenticated: false,
            target_routing: None,
        })
    }
    fn check(&self) -> io::Result<()> {
        if !self.parent.current()
            || !same(&self.helper, &protected_consuming_helper()?)?
            || !same(
                &self.helper,
                &File::open(format!("/proc/{}/exe", self.parent.pid()))?,
            )?
            || !same(&self.helper, &File::open("/proc/self/exe")?)?
        {
            return Err(denied());
        }
        exact_parent(std::process::id(), self.parent.pid())
    }
    /// Observe the actual target Shell over the child-created private endpoint.
    /// Caller must match it to original parent's certified input before spawn.
    /// # Errors
    /// Refuses repeated use, root sender, wrong parent or incomplete mutual QPE.
    pub fn authenticate_shell(&mut self) -> io::Result<ExecutingPeer> {
        self.check()?;
        if self.shell_authenticated {
            return Err(denied());
        }
        runtime_portal::process_binding::arm(&self.shell)?;
        self.shell.set_read_timeout(Some(Duration::from_secs(10)))?;
        self.shell
            .set_write_timeout(Some(Duration::from_secs(10)))?;
        let challenge = fresh_challenge();
        self.shell
            .write_all(&runtime_portal::process_binding::MAGIC)?;
        self.shell.write_all(&challenge)?;
        let shell = runtime_portal::process_binding::receive_nonroot_for_authentication(
            &self.shell,
            &challenge,
        )?;
        self.check()?;
        self.target_routing
            .as_ref()
            .ok_or_else(denied)?
            .check(&shell)?;
        self.shell.write_all(&[1])?;
        runtime_portal::process_binding::complete(&mut self.shell)?;
        self.shell.set_read_timeout(Some(Duration::from_secs(10)))?;
        self.shell
            .set_write_timeout(Some(Duration::from_secs(10)))?;
        self.check()?;
        self.shell_authenticated = true;
        Ok(shell)
    }
    /// The sole bounded parent-control reader. Exposure control will extend its
    /// finite vocabulary, preserving one owner of this stream.
    /// # Errors
    /// Refuses unknown opcodes, duplicate commands, invalid bounds or lost parent.
    pub fn next_command(&mut self) -> io::Result<ConsumingBootstrapCommand> {
        self.check()?;
        self.control
            .set_read_timeout(Some(Duration::from_millis(50)))?;
        let mut opcode = [0];
        self.control.read_exact(&mut opcode)?;
        self.control
            .set_read_timeout(Some(Duration::from_secs(10)))?;
        match opcode[0] {
            b'X' => Ok(ConsumingBootstrapCommand::Retire),
            opcode @ (b'E' | b'R' | b'W') => self
                .read_exposure(opcode)
                .map(ConsumingBootstrapCommand::Exposure),
            b'C' => {
                if self.command_received {
                    return Err(denied());
                }
                let mut length = [0; 4];
                self.control.read_exact(&mut length)?;
                let length = u32::from_be_bytes(length) as usize;
                if length == 0 || length > 64 * 1024 {
                    return Err(denied());
                }
                let mut bytes = vec![0; length];
                self.control.read_exact(&mut bytes)?;
                let wire: RoutedCommandWire =
                    serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                digest(&wire.command)?;
                wire.routing.validate()?;
                self.check()?;
                self.target_routing = Some(wire.routing.clone());
                self.command_received = true;
                Ok(ConsumingBootstrapCommand::Launch(ObservedCommand {
                    command: wire.command,
                    routing: wire.routing,
                }))
            }
            _ => Err(denied()),
        }
    }
    /// Deliver actual ordinary child custody on the independently authenticated
    /// direct Shell stream; caller retains the returned root supervisor.
    /// # Errors
    /// Refuses missing mutual QPE or current parent/child/command evidence.
    pub fn handoff_ordinary(&mut self, child: &mut OwnedOrdinaryChild) -> io::Result<()> {
        self.check()?;
        if !self.shell_authenticated || !self.command_received {
            return Err(denied());
        }
        child.handoff(&mut self.shell)?;
        self.check()
    }
    /// Exact currently authenticated original source helper observation only.
    #[must_use]
    pub fn parent(&self) -> &ExecutingPeer {
        &self.parent
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetKernelRouting {
    pid: u32,
    start: u64,
    uid: u32,
    gid: u32,
    groups: Vec<u32>,
}
impl TargetKernelRouting {
    fn observe(target: &ExecutingPeer) -> io::Result<Self> {
        if !target.current() {
            return Err(denied());
        }
        let value = Self {
            pid: target.pid(),
            start: target.start_time()?,
            uid: target.uid(),
            gid: target.gid(),
            groups: target.supplementary_groups()?.to_vec(),
        };
        value.validate()?;
        if !target.current() {
            return Err(denied());
        }
        Ok(value)
    }
    fn validate(&self) -> io::Result<()> {
        if self.pid <= 1 || self.start == 0 || self.uid == 0 || self.groups.len() > 256 {
            return Err(denied());
        }
        Ok(())
    }
    fn check(&self, target: &ExecutingPeer) -> io::Result<()> {
        self.validate()?;
        if !target.current()
            || target.pid() != self.pid
            || target.start_time()? != self.start
            || target.uid() != self.uid
            || target.gid() != self.gid
            || target.supplementary_groups()? != self.groups.as_slice()
            || !target.current()
        {
            return Err(denied());
        }
        Ok(())
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutedCommandWire {
    command: ConsumerCommand,
    routing: TargetKernelRouting,
}
/// The sole descriptive command decoded from the actual authenticated parent.
/// Its routing must match independently acquired direct Shell kernel evidence.
/// No public constructor or serialization can turn caller labels into this value.
pub struct ObservedCommand {
    command: ConsumerCommand,
    routing: TargetKernelRouting,
}
impl ObservedCommand {
    #[must_use]
    pub fn command(&self) -> &ConsumerCommand {
        &self.command
    }
    /// # Errors
    /// Refuses every PID/start/UID/GID/groups difference or unavailable live peer.
    pub fn check_target(&self, peer: &ExecutingPeer) -> io::Result<()> {
        self.routing.check(peer)
    }
}
impl ManagedConsumingBootstrap {
    pub(crate) fn check_current(&self) -> io::Result<()> {
        if !self.peer.current()
            || !same(&self.namespace, &namespace(self.peer.pid())?)?
            || !same(&self.helper, &protected_consuming_helper()?)?
            || !same(
                &self.helper,
                &File::open(format!("/proc/{}/exe", self.peer.pid()))?,
            )?
        {
            return Err(denied());
        }
        exact_parent(self.peer.pid(), std::process::id())
    }
    /// Read only this actual owned child's mount table, with a finite byte cap.
    pub(crate) fn mountinfo(&self) -> io::Result<File> {
        self.check_current()?;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(format!("/proc/{}/mountinfo", self.peer.pid()))?;
        bounded_mountinfo(file.try_clone()?)?;
        file.seek(SeekFrom::Start(0))?;
        self.check_current()?;
        Ok(file)
    }
    pub(crate) fn clone_namespace(&self) -> io::Result<File> {
        self.check_current()?;
        let held = self.namespace.try_clone()?;
        self.check_current()?;
        Ok(held)
    }
    pub(crate) fn verify_launcher(&self, launcher: &ExecutingPeer) -> io::Result<()> {
        self.check_current()?;
        if !launcher.current() || !same(&self.namespace, &namespace(launcher.pid())?)? {
            return Err(denied());
        }
        exact_parent(launcher.pid(), self.peer.pid())?;
        self.check_current()
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;
    fn command() -> ConsumerCommand {
        ConsumerCommand {
            program: "/usr/libexec/tilo/tilo-launcher".into(),
            args: vec!["run".into()],
            env: Vec::new(),
        }
    }
    #[test]
    fn actual_factory_command_refuses_loader_and_registration_environment() {
        let original = command();
        assert!(digest(&original).is_ok());
        for key in [
            WAIT_ENV,
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "GLIBC_TUNABLES",
            "DYLD_INSERT_LIBRARIES",
        ] {
            let mut candidate = original.clone();
            candidate.env.push((key.into(), "/attacker".into()));
            assert_eq!(
                digest(&candidate).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        let mut foreign = original;
        foreign.program = "/usr/bin/bash".into();
        assert_eq!(
            digest(&foreign).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
    #[test]
    fn private_target_routing_codec_refuses_unknown_authority_and_unbounded_groups() {
        let wire = RoutedCommandWire {
            command: command(),
            routing: TargetKernelRouting {
                pid: 2,
                start: 1,
                uid: 1000,
                gid: 1000,
                groups: vec![1000],
            },
        };
        let mut encoded = serde_json::to_value(&wire).unwrap();
        encoded["routing"]["source_groups"] = serde_json::json!([0]);
        assert!(serde_json::from_value::<RoutedCommandWire>(encoded).is_err());
        let mut oversized = wire.routing;
        oversized.groups = vec![1000; 257];
        assert!(oversized.validate().is_err());
    }
}

fn fresh_challenge() -> [u8; 32] {
    let mut challenge = [0; 32];
    for (chunk, random) in challenge
        .chunks_exact_mut(16)
        .zip([uuid::Uuid::new_v4(), uuid::Uuid::new_v4()])
    {
        chunk.copy_from_slice(random.as_bytes());
    }
    challenge
}

fn bounded_mountinfo(file: File) -> io::Result<Vec<u8>> {
    const LIMIT: u64 = 4 * 1024 * 1024;
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT || bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod exposure_adapter_tests {
    use super::*;
    use std::io::{Seek, SeekFrom};
    #[test]
    fn actual_mountinfo_acquisition_preserves_raw_bytes_and_refuses_oversize() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"1 0 0:1 / /raw-\xff rw - tmpfs tmpfs rw\n")
            .unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(
            bounded_mountinfo(file).unwrap(),
            b"1 0 0:1 / /raw-\xff rw - tmpfs tmpfs rw\n"
        );
        let mut oversized = tempfile::tempfile().unwrap();
        oversized.set_len(4 * 1024 * 1024 + 1).unwrap();
        oversized.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(
            bounded_mountinfo(oversized).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}

struct SocketDeadline {
    done: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    cancel: ServiceCancellation,
    deadline: Instant,
}
impl SocketDeadline {
    fn start(
        stream: &UnixStream,
        cancel: ServiceCancellation,
        deadline: Instant,
    ) -> io::Result<Self> {
        if cancel.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let exact = stream.try_clone()?;
        let done = Arc::new(AtomicBool::new(false));
        let finished = Arc::clone(&done);
        let cancellation = cancel.clone();
        let worker = std::thread::Builder::new()
            .name("ordinary-bootstrap-deadline".into())
            .spawn(move || {
                while !finished.load(Ordering::Acquire) {
                    if cancellation.is_cancelled() || Instant::now() >= deadline {
                        let _ = exact.shutdown(std::net::Shutdown::Both);
                        break;
                    }
                    std::thread::park_timeout(Duration::from_millis(10));
                }
            })?;
        Ok(Self {
            done,
            worker: Some(worker),
            cancel,
            deadline,
        })
    }
    fn check(&self) -> io::Result<()> {
        if self.cancel.is_cancelled() {
            Err(io::ErrorKind::Interrupted.into())
        } else if Instant::now() >= self.deadline {
            Err(io::ErrorKind::TimedOut.into())
        } else {
            Ok(())
        }
    }
}
impl Drop for SocketDeadline {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod bootstrap_refusal_tests {
    use super::*;
    #[test]
    fn closed_refusal_consumes_exact_phase_and_preserves_next_handoff() {
        let (mut root, mut shell) = UnixStream::pair().unwrap();
        refuse_consuming_bootstrap(&mut root, tilo_test_env::hang_deadline()).unwrap();
        root.write_all(BOOTSTRAP_MAGIC).unwrap();
        assert_eq!(
            read_bootstrap_kind(&mut shell).unwrap(),
            BootstrapKind::Refused
        );
        assert_eq!(
            read_bootstrap_kind(&mut shell).unwrap(),
            BootstrapKind::Delegated
        );
        for bytes in [*b"TCB2", *b"TC00", *b"\0\0\0\0"] {
            assert!(bootstrap_kind(bytes).is_err());
        }
    }
    #[test]
    fn partial_shared_handoff_obeys_original_absolute_deadline() {
        let (mut root, mut shell) = UnixStream::pair().unwrap();
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(60))
            .unwrap();
        let fence =
            SocketDeadline::start(&shell, ServiceCancellation::default(), deadline).unwrap();
        // The root stays open with a partial frame: only the fence at the
        // deadline ends the read.
        root.write_all(b"TCB").unwrap();
        assert!(read_bootstrap_kind(&mut shell).is_err());
        assert!(fence.check().is_err());
        drop(fence);
    }
    #[test]
    fn cancellation_interrupts_exact_private_control_without_touching_shared_stream() {
        let (mut child, mut control) = UnixStream::pair().unwrap();
        let (mut root, mut shell) = UnixStream::pair().unwrap();
        let cancel = ServiceCancellation::default();
        let fence = SocketDeadline::start(&control, cancel.clone(), tilo_test_env::hang_deadline())
            .unwrap();
        child.write_all(b"TC").unwrap();
        cancel.cancel();
        assert!(read_bootstrap_kind(&mut control).is_err());
        assert!(fence.check().is_err());
        drop(fence);
        refuse_consuming_bootstrap(&mut root, tilo_test_env::hang_deadline()).unwrap();
        assert_eq!(
            read_bootstrap_kind(&mut shell).unwrap(),
            BootstrapKind::Refused
        );
    }
}

// These handles arrive only on the already authenticated original-parent channel.
// The original parent independently brackets its actual owned child; these are
// process/namespace custody, never source or person authority.
struct ParentObservedNamespaces {
    child: File,
    parent: File,
}
impl ParentObservedNamespaces {
    fn validate(&self) -> io::Result<()> {
        mount_namespace(&self.child)?;
        mount_namespace(&self.parent)?;
        if same(&self.child, &self.parent)? {
            return Err(denied());
        }
        Ok(())
    }
}
fn mount_namespace(file: &File) -> io::Result<()> {
    const NS_GET_NSTYPE: libc::c_ulong = 0xb703;
    if unsafe { libc::ioctl(file.as_raw_fd(), NS_GET_NSTYPE) } != libc::CLONE_NEWNS {
        return Err(denied());
    }
    Ok(())
}
