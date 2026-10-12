//! Observes the actual installed receiving PID1, not an exit-status-only claim.
use super::*;
use std::os::{
    fd::AsRawFd as _,
    unix::fs::{MetadataExt as _, PermissionsExt as _},
};
use std::{
    os::{
        fd::{FromRawFd as _, OwnedFd},
        unix::net::{UnixListener, UnixStream},
    },
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
pub(super) fn credentials(stream: &UnixStream) -> io::Result<libc::ucred> {
    let mut value: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of_val(&value) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut value).cast(),
            &raw mut size,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if size as usize != std::mem::size_of_val(&value) {
        return Err(invalid());
    }
    Ok(value)
}
struct Init {
    pidfd: OwnedFd,
    namespace: File,
    mount: File,
    network: File,
}
impl Init {
    fn observe(stream: &UnixStream, outer: &Child, helper: &File) -> io::Result<Self> {
        let peer = credentials(stream)?;
        if peer.uid != 1000 || peer.gid != 1001 || peer.pid <= 1 {
            return Err(invalid());
        }
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, peer.pid, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let pidfd = unsafe { OwnedFd::from_raw_fd(i32::try_from(fd).map_err(|_| invalid())?) };
        let status = fs::read_to_string(format!("/proc/{}/status", peer.pid))?;
        let parent = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:"))
            .and_then(|v| v.trim().parse::<u32>().ok());
        let ids = status
            .lines()
            .find_map(|line| line.strip_prefix("NSpid:"))
            .map(|v| v.split_whitespace().collect::<Vec<_>>());
        if parent != Some(outer.id()) || !ids.is_some_and(|v| v.len() > 1 && v.last() == Some(&"1"))
        {
            return Err(invalid());
        }
        let running = fs::metadata(format!("/proc/{}/exe", peer.pid))?;
        let expected = helper.metadata()?;
        if (running.dev(), running.ino()) != (expected.dev(), expected.ino()) {
            return Err(invalid());
        }
        let mut namespaces = Vec::new();
        for name in ["pid", "mnt", "net"] {
            let namespace = File::open(format!("/proc/{}/ns/{name}", peer.pid))?;
            let value = namespace.metadata()?;
            let parent = fs::metadata(format!("/proc/self/ns/{name}"))?;
            if (value.dev(), value.ino()) == (parent.dev(), parent.ino()) {
                return Err(invalid());
            }
            namespaces.push(namespace);
        }
        let network = namespaces.pop().ok_or_else(invalid)?;
        let mount = namespaces.pop().ok_or_else(invalid)?;
        let namespace = namespaces.pop().ok_or_else(invalid)?;
        let observed = Self {
            pidfd,
            namespace,
            mount,
            network,
        };
        if observed.exited()? {
            return Err(invalid());
        }
        Ok(observed)
    }
    fn exited(&self) -> io::Result<bool> {
        let mut poll = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&raw mut poll, 1, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(invalid());
        }
        Ok(poll.revents & libc::POLLIN != 0)
    }
    fn kill(&self) {
        let _ = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
    }
}
struct Owned {
    child: Child,
    init: Option<Init>,
    reaped: bool,
}
impl Owned {
    fn terminate(&mut self) -> io::Result<()> {
        // Cleanup authority must not be lost if polling the init fails.
        if let Some(init) = &self.init {
            init.kill();
        }
        if !self.reaped {
            bounded_command::kill_tree(&mut self.child);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if !self.reaped && self.child.try_wait()?.is_some() {
                self.reaped = true;
            }
            let dead = self.init.as_ref().map_or(Ok(true), Init::exited)?;
            if self.reaped && dead {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(invalid());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}
fn exited_unreaped(child: &Child) -> io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            &raw mut info,
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}
fn collect(mut pipe: impl Read + std::os::fd::AsRawFd, stop: &AtomicBool) -> io::Result<Vec<u8>> {
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(bytes),
            Ok(n) => {
                if bytes.len() + n > BOUND {
                    return Err(invalid());
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if stop.load(std::sync::atomic::Ordering::Acquire) {
                    return Err(invalid());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
}
pub(super) fn run(helper: &File, cancel: &AtomicBool) -> io::Result<serde_json::Value> {
    let listener = UnixListener::bind(Path::new(ROOT).join("control"))?;
    fs::set_permissions(
        Path::new(ROOT).join("control"),
        fs::Permissions::from_mode(0o666),
    )?;
    listener.set_nonblocking(true)?;
    let mut command = Command::new("/usr/bin/unshare");
    command
        .args([
            "--pid",
            "--fork",
            "--kill-child=SIGKILL",
            "--mount",
            "--mount-proc",
            "--net",
            "--ipc",
            "--propagation",
            "private",
            "--",
            "/usr/bin/setpriv",
            "--reuid=1000",
            "--regid=1001",
            "--clear-groups",
            "--bounding-set=-all",
            "--inh-caps=-all",
            "--ambient-caps=-all",
            "--no-new-privs",
            "--",
            HELPER,
            "--applications-consumer-qualification-child",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    bounded_command::own_process_tree(&mut command);
    let parent = std::process::id();
    use std::os::unix::process::CommandExt as _;
    // Only async-signal-safe libc operations execute between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                || libc::getppid() as u32 != parent
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    let mut owned = Owned {
        child,
        init: None,
        reaped: false,
    };
    let stdout = owned.child.stdout.take().ok_or_else(invalid)?;
    let stderr = owned.child.stderr.take().ok_or_else(invalid)?;
    let deadline = Instant::now() + Duration::from_secs(300);
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let out = scope.spawn(|| collect(stdout, &stop));
        let err = scope.spawn(|| collect(stderr, &stop));
        let result = (|| {
            let handshake = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                if crate::setup::migration::check_cancel(cancel).is_err()
                    || exited_unreaped(&owned.child)?
                    || Instant::now() >= handshake
                {
                    return Err(invalid());
                }
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(e) => return Err(e),
                }
            };
            owned.init = Some(Init::observe(&stream, &owned.child, helper)?);
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            let mut ready = [0; 6];
            stream.read_exact(&mut ready)?;
            if ready != *b"ready\n" {
                return Err(invalid());
            }
            stream.write_all(&[1])?;
            drop(stream);
            loop {
                crate::setup::migration::check_cancel(cancel)?;
                if exited_unreaped(&owned.child)? {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(invalid());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let init = owned.init.as_ref().ok_or_else(invalid)?;
            // Linux's namespace init exit kills all remaining descendants,
            // including session-detached GPG/Flatpak helpers. Context/channel
            // retirement alone never satisfies this kernel condition.
            if !init.exited()? {
                return Err(invalid());
            }
            let coordinates = [
                init.namespace.metadata()?,
                init.mount.metadata()?,
                init.network.metadata()?,
            ];
            // Keep the unreaped process-group identity until exact cleanup.
            bounded_command::kill_tree(&mut owned.child);
            let status = owned.child.wait()?;
            owned.reaped = true;
            if !status.success() {
                return Err(invalid());
            }
            Ok(coordinates)
        })();
        let cleanup = owned.terminate();
        stop.store(true, std::sync::atomic::Ordering::Release);
        let stdout = out.join().map_err(|_| invalid())?;
        let stderr = err.join().map_err(|_| invalid())?;
        cleanup?;
        let coordinates = result?;
        let stdout = stdout?;
        if !stderr?.is_empty() {
            return Err(invalid());
        }
        let report: serde_json::Value = serde_json::from_slice(&stdout).map_err(|_| invalid())?;
        let keys = [
            "schema",
            "subset_protocol",
            "deployments",
            "install_commands",
            "commits",
            "selected_data_sha256",
        ];
        if !report
            .as_object()
            .is_some_and(|v| v.len() == keys.len() && keys.iter().all(|k| v.contains_key(*k)))
            || report["schema"] != 1
            || report["subset_protocol"] != 2
            || report["deployments"] != 2
            || report["install_commands"] != 2
        {
            return Err(invalid());
        }
        Ok(
            serde_json::json!({"result":report,"namespace":{"pid_dev":coordinates[0].dev(),"pid_ino":coordinates[0].ino(),"mount_dev":coordinates[1].dev(),"mount_ino":coordinates[1].ino(),"network_dev":coordinates[2].dev(),"network_ino":coordinates[2].ino()}}),
        )
    })
}
