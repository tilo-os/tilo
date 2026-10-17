use super::*;
use std::{
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    time::{Duration, Instant},
};

impl Incarnation {
    pub fn capture(pid: u32, uid: u32) -> io::Result<Self> {
        let (start, _) = stat(pid)?;
        if std::fs::metadata(format!("/proc/{pid}"))?.uid() != uid || pid <= 1 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let value = Self {
            pid,
            start,
            uid,
            boot: boot_id()?,
        };
        value
            .open_current()?
            .ok_or_else(|| failure("capture process exited"))?;
        Ok(value)
    }

    fn open_current(&self) -> io::Result<Option<File>> {
        if self.boot != boot_id()? {
            return Ok(None);
        }
        let (start, _) = match stat(self.pid) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if start != self.start {
            return Ok(None);
        }
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, self.pid, 0) };
        if raw < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            return Err(error);
        }
        let file = unsafe { File::from_raw_fd(i32::try_from(raw).map_err(io::Error::other)?) };
        let (start, _) = match stat(self.pid) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if start != self.start {
            return Ok(None);
        }
        if std::fs::metadata(format!("/proc/{}", self.pid))?.uid() != self.uid {
            return Err(failure("capture process credentials changed"));
        }
        Ok(Some(file))
    }

    /// Hold the mount namespace of this exact authenticated host incarnation.
    /// A recycled PID or stopped host never grants privileged mount authority.
    pub(in crate::capture) fn mount_namespace(&self) -> io::Result<File> {
        let process = self
            .open_current()?
            .ok_or_else(|| failure("capture host exited before projection"))?;
        if exited(&process)? {
            return Err(failure("capture host stopped before projection"));
        }
        let namespace = File::open(format!("/proc/{}/ns/mnt", self.pid))?;
        let own = std::fs::metadata("/proc/self/ns/mnt")?;
        let held = namespace.metadata()?;
        if (own.dev(), own.ino()) == (held.dev(), held.ino())
            || self.open_current()?.is_none()
            || exited(&process)?
        {
            return Err(failure("capture host has no private live mount namespace"));
        }
        let current = std::fs::metadata(format!("/proc/{}/ns/mnt", self.pid))?;
        if (held.dev(), held.ino()) != (current.dev(), current.ino()) {
            return Err(failure("capture host mount namespace changed"));
        }
        Ok(namespace)
    }
}

impl Custody {
    pub fn active_namespace(&self) -> io::Result<bool> {
        if self.host.boot != boot_id()? || !self.launch_acknowledged {
            return Ok(false);
        }
        let Some(namespace) = &self.namespace else {
            return Ok(false);
        };
        let Some(host) = self.host.open_current()? else {
            return Ok(false);
        };
        if exited(&host)? {
            return Ok(false);
        }
        let init = Incarnation {
            pid: namespace.init_pid,
            start: namespace.start_ticks,
            uid: self.host.uid,
            boot: self.host.boot,
        };
        let Some(init) = init.open_current()? else {
            return Ok(false);
        };
        if exited(&init)? {
            return Ok(false);
        }
        verify_namespace(namespace)?;
        Ok(true)
    }
    pub fn admit_namespace(&mut self, namespace: Namespace) -> io::Result<()> {
        if !self.launch_acknowledged {
            return Err(failure("capture launch was not durably acknowledged"));
        }
        let init = Incarnation::capture(namespace.init_pid, self.host.uid)?;
        if init.start != namespace.start_ticks || !descendant(namespace.init_pid, self.host)? {
            return Err(failure("capture init is not this exact host descendant"));
        }
        verify_namespace(&namespace)?;
        self.namespace = Some(namespace);
        Ok(())
    }

    pub fn require_stopped(&self) -> io::Result<()> {
        self.require_stopped_inner()?;
        Ok(())
    }

    fn require_stopped_inner(&self) -> io::Result<()> {
        if self.host.boot != boot_id()? {
            return Ok(());
        }
        if !self.launch_acknowledged {
            if let Some(fd) = self.host.open_current()? {
                if !exited(&fd)? {
                    return Err(failure("unlaunched capture host is still active"));
                }
            }
            return Ok(());
        }
        let namespace = self
            .namespace
            .as_ref()
            .ok_or_else(|| failure("capture namespace creation window needs recovery evidence"))?;
        let init = Incarnation {
            pid: namespace.init_pid,
            start: namespace.start_ticks,
            uid: self.host.uid,
            boot: self.host.boot,
        };
        if let Some(fd) = init.open_current()? {
            if !exited(&fd)? {
                return Err(failure("capture namespace is still active"));
            }
        }
        if let Some(fd) = self.host.open_current()? {
            if !exited(&fd)? {
                return Err(failure("capture host is still active"));
            }
        }
        Ok(())
    }

    pub fn stop_and_observe(&self) -> io::Result<()> {
        if self.host.boot != boot_id()? {
            return self.require_stopped();
        }
        if !self.launch_acknowledged {
            if let Some(fd) = self.host.open_current()? {
                if !exited(&fd)? {
                    terminate(&fd)?;
                }
            }
            return self.require_stopped();
        }
        let namespace = self
            .namespace
            .as_ref()
            .ok_or_else(|| failure("unacknowledged capture namespace cannot be guessed"))?;
        let init = Incarnation {
            pid: namespace.init_pid,
            start: namespace.start_ticks,
            uid: self.host.uid,
            boot: self.host.boot,
        };
        if let Some(fd) = init.open_current()? {
            if !exited(&fd)? {
                verify_namespace(namespace)?;
                terminate(&fd)?;
            }
        }
        if let Some(fd) = self.host.open_current()? {
            if !exited(&fd)? {
                terminate(&fd)?;
            }
        }
        self.require_stopped()
    }
}

fn verify_namespace(namespace: &Namespace) -> io::Result<()> {
    let facts = std::fs::metadata(format!("/proc/{}/ns/pid", namespace.init_pid))?;
    let status = std::fs::read_to_string(format!("/proc/{}/status", namespace.init_pid))?;
    let ids = status
        .lines()
        .find_map(|line| line.strip_prefix("NSpid:"))
        .ok_or_else(|| failure("capture init namespace is unobservable"))?;
    if facts.dev() != namespace.namespace_dev
        || facts.ino() != namespace.namespace_ino
        || ids.split_whitespace().count() < 2
        || ids.split_whitespace().last() != Some("1")
    {
        return Err(failure("capture init namespace identity changed"));
    }
    Ok(())
}

fn descendant(mut pid: u32, host: Incarnation) -> io::Result<bool> {
    for _ in 0..128 {
        let (start, parent) = stat(pid)?;
        if pid == host.pid {
            return Ok(start == host.start && host.open_current()?.is_some());
        }
        if parent <= 1 || parent == pid {
            return Ok(false);
        }
        pid = parent;
    }
    Err(failure("capture process ancestry exceeds bound"))
}

fn stat(pid: u32) -> io::Result<(u64, u32)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let (_, fields) = text
        .rsplit_once(") ")
        .ok_or_else(|| failure("malformed native process stat"))?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    let start = fields
        .get(19)
        .ok_or_else(|| failure("process start unavailable"))?
        .parse()
        .map_err(io::Error::other)?;
    let parent = fields
        .get(1)
        .ok_or_else(|| failure("process parent unavailable"))?
        .parse()
        .map_err(io::Error::other)?;
    Ok((start, parent))
}

fn boot_id() -> io::Result<[u8; 16]> {
    let text = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    if text.len() > 64 {
        return Err(failure("native boot identity exceeds bound"));
    }
    Ok(*uuid::Uuid::parse_str(text.trim())
        .map_err(io::Error::other)?
        .as_bytes())
}

fn exited(fd: &File) -> io::Result<bool> {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let count = unsafe { libc::poll(&raw mut poll, 1, 0) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(count == 1 && poll.revents & libc::POLLIN != 0)
}

fn terminate(fd: &File) -> io::Result<()> {
    if unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            libc::SIGKILL,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    } < 0
    {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !exited(fd)? {
        if Instant::now() >= deadline {
            return Err(failure("capture process shutdown was not observed"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Owned(std::process::Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn foreign_boot_custody_never_signals_a_matching_live_pid() {
        let mut child = Owned(
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        );
        let uid = rustix::process::getuid().as_raw();
        let mut host = Incarnation::capture(child.0.id(), uid).unwrap();
        assert!(host.open_current().unwrap().is_some());
        host.boot[0] ^= 1;
        let custody = Custody {
            host,
            launch_acknowledged: true,
            namespace: Some(Namespace {
                init_pid: host.pid,
                start_ticks: host.start,
                namespace_dev: 0,
                namespace_ino: 0,
            }),
        };
        custody.stop_and_observe().unwrap();
        assert!(child.0.try_wait().unwrap().is_none());
    }

    #[test]
    fn a_reused_start_value_does_not_pin_the_live_process() {
        let mut child = Owned(
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        );
        let mut identity =
            Incarnation::capture(child.0.id(), rustix::process::getuid().as_raw()).unwrap();
        identity.start = identity.start.wrapping_add(1);
        assert!(identity.open_current().unwrap().is_none());
        assert!(child.0.try_wait().unwrap().is_none());
    }
}
