//! Canonical namespace supervision for ordinary signed GUI launches.
//!
//! This is kernel process custody, not a filesystem admission. The protected
//! retained owner must independently authenticate the launcher, package and
//! current session before accepting a witness or exporting a view.

use crate::{
    managed::EnrolledProcessObserver,
    owned_service::{NamespaceOwner, ServiceAuthority, ServiceCancellation},
};
use std::os::fd::AsFd;
use std::{
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, OwnedFd, RawFd},
    os::unix::{fs::MetadataExt, net::UnixStream},
    process::{Child, ExitStatus},
    sync::Arc,
    time::{Duration, Instant},
};

/// An init observed by the canonical owner after bubblewrap's actual info pipe
/// and kernel parent/PID namespace checks. There is no descriptor constructor.
/// Holding this witness does not grant access to a source or projected view.
pub struct NamespaceWitness {
    init: OwnedFd,
    pid_namespace: File,
    mount_namespace: File,
    pid: u32,
    outer: u32,
    launcher: u32,
}
impl NamespaceWitness {
    pub(crate) fn observed(
        init: OwnedFd,
        pid_namespace: File,
        mount_namespace: File,
        pid: u32,
        outer: u32,
    ) -> Self {
        Self {
            init,
            pid_namespace,
            mount_namespace,
            pid,
            outer,
            launcher: std::process::id(),
        }
    }
    #[must_use]
    pub fn init_pid(&self) -> u32 {
        self.pid
    }
    /// Clone the kernel task handle for protected lifetime supervision.
    /// # Errors
    /// Returns descriptor exhaustion; never falls back to the numeric PID.
    pub fn init_pidfd(&self) -> io::Result<OwnedFd> {
        self.init.try_clone()
    }
    /// # Errors
    /// Returns descriptor exhaustion.
    pub fn pid_namespace(&self) -> io::Result<File> {
        self.pid_namespace.try_clone()
    }
    /// # Errors
    /// Returns descriptor exhaustion.
    pub fn mount_namespace(&self) -> io::Result<File> {
        self.mount_namespace.try_clone()
    }
    /// Verify this opaque witness against the independently authenticated
    /// executing launcher. Package and session admission remain the root actor's
    /// responsibility; this establishes kernel ancestry and held namespaces.
    /// # Errors
    /// Refuses a stopped launcher/init, redirected handle or foreign namespace.
    pub fn validate_for_launcher(
        &self,
        peer: &runtime_portal::process_binding::ExecutingPeer,
    ) -> io::Result<()> {
        if !peer.current() || peer.pid() != self.launcher || self.observed_stopped()? {
            return Err(refused("ordinary namespace launcher is no longer current"));
        }
        let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", self.init.as_raw_fd()))?;
        let actual = info
            .lines()
            .find_map(|line| line.strip_prefix("Pid:"))
            .and_then(|value| value.trim().parse::<u32>().ok());
        if actual != Some(self.pid) || parent(self.outer)? != self.launcher {
            return Err(refused(
                "ordinary namespace pidfd or launcher ancestry differs",
            ));
        }
        crate::owned_service::verify_init(
            self.pid.try_into().map_err(io::Error::other)?,
            self.outer,
        )?;
        for (held, name) in [(&self.pid_namespace, "pid"), (&self.mount_namespace, "mnt")] {
            let current = std::fs::metadata(format!("/proc/{}/ns/{name}", self.pid))?;
            let actual = held.metadata()?;
            let launcher_namespace =
                std::fs::metadata(format!("/proc/{}/ns/{name}", self.launcher))?;
            if (current.dev(), current.ino())
                == (launcher_namespace.dev(), launcher_namespace.ino())
            {
                return Err(refused(
                    "ordinary namespace is not isolated from its launcher",
                ));
            }
            if (current.dev(), current.ino()) != (actual.dev(), actual.ino()) {
                return Err(refused("ordinary namespace descriptor differs"));
            }
        }
        if !peer.current() || self.observed_stopped()? {
            return Err(refused("ordinary namespace retired during review"));
        }
        Ok(())
    }
    /// Qualify a genuine executing task inside this actual consuming namespace.
    /// A PID label or a shared numeric UID cannot replace the retained peer.
    /// This proves process membership, never file or source authority.
    /// # Errors
    /// Refuses foreign namespaces, tasks outside the init ancestry or retirement.
    pub fn validate_member(
        &self,
        member: &runtime_portal::process_binding::ExecutingPeer,
        launcher: &runtime_portal::process_binding::ExecutingPeer,
    ) -> io::Result<()> {
        self.validate_for_launcher(launcher)?;
        if !member.current() {
            return Err(refused("consuming task exited"));
        }
        for (held, name) in [(&self.pid_namespace, "pid"), (&self.mount_namespace, "mnt")] {
            let actual = std::fs::metadata(format!("/proc/{}/ns/{name}", member.pid()))?;
            let expected = held.metadata()?;
            if (actual.dev(), actual.ino()) != (expected.dev(), expected.ino()) {
                return Err(refused("executing task belongs to another namespace"));
            }
        }
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", self.pid))?;
        let start_time = stat
            .rsplit_once(") ")
            .and_then(|(_, fields)| fields.split_whitespace().nth(19))
            .and_then(|value| value.parse().ok())
            .ok_or(io::ErrorKind::InvalidData)?;
        if !member.belongs_to(self.pid, start_time) {
            return Err(refused("executing task is outside consuming init"));
        }
        self.validate_for_launcher(launcher)?;
        if !member.current() {
            return Err(refused("consuming task retired during review"));
        }
        Ok(())
    }

    /// Transfer kernel witnesses over the already authenticated private lease
    /// endpoint. Each descriptor is acknowledged to avoid stream coalescing.
    /// # Errors
    /// Returns a transport failure; receipt alone never admits file authority.
    pub fn send(
        &self,
        stream: &mut UnixStream,
        cancellation: &ServiceCancellation,
    ) -> io::Result<()> {
        let mut io = Handoff::new(stream, cancellation, Duration::from_secs(10))?;
        for value in [self.pid, self.outer, self.launcher] {
            io.write_all(&value.to_be_bytes())?;
        }
        for (index, fd) in [
            self.init.as_fd(),
            self.pid_namespace.as_fd(),
            self.mount_namespace.as_fd(),
        ]
        .into_iter()
        .enumerate()
        {
            let tag = u8::try_from(index).map_err(io::Error::other)?;
            io.retry(libc::POLLOUT, |stream| {
                runtime_portal::fd_transport::send_with_fd(stream.as_fd(), &[tag], fd)
            })?;
            let mut ack = [0];
            io.read_exact(&mut ack)?;
            if ack != [1] {
                return Err(refused("ordinary namespace descriptor refused"));
            }
        }
        Ok(())
    }
    /// Receive only from the actor's independently authenticated actual
    /// launcher peer. Wire process numbers are checked against every held
    /// kernel descriptor and the actual live launcher before construction.
    /// # Errors
    /// Refuses mismatched task handles, namespace files, ancestry or framing.
    pub fn receive(
        stream: &mut UnixStream,
        peer: &runtime_portal::process_binding::ExecutingPeer,
        cancellation: &ServiceCancellation,
    ) -> io::Result<Self> {
        let mut io = Handoff::new(stream, cancellation, Duration::from_secs(10))?;
        let mut values = [0_u32; 3];
        for value in &mut values {
            let mut bytes = [0; 4];
            io.read_exact(&mut bytes)?;
            *value = u32::from_be_bytes(bytes);
        }
        let mut descriptors = Vec::new();
        for index in 0..3_u8 {
            let (bytes, fd) = io.retry(libc::POLLIN, |stream| {
                runtime_portal::fd_transport::recv_with_fd(stream.as_fd())
            })?;
            if bytes != [index] || !peer.current() {
                return Err(refused("ordinary namespace descriptor framing differs"));
            }
            descriptors.push(fd.ok_or_else(|| refused("ordinary namespace descriptor missing"))?);
            io.write_all(&[1])?;
        }
        let mount_namespace = File::from(descriptors.pop().ok_or(io::ErrorKind::InvalidData)?);
        let pid_namespace = File::from(descriptors.pop().ok_or(io::ErrorKind::InvalidData)?);
        let init = descriptors.pop().ok_or(io::ErrorKind::InvalidData)?;
        let [pid, outer, launcher] = values;
        let witness = Self {
            init,
            pid_namespace,
            mount_namespace,
            pid,
            outer,
            launcher,
        };
        witness.validate_for_launcher(peer)?;
        Ok(witness)
    }
    /// Kill only the exact held namespace init and wait for kernel exit.
    /// A timeout leaves this witness intact and cannot release a source lease.
    /// # Errors
    /// Returns an actual signal/poll error or an incomplete-stop timeout.
    pub fn cancel_and_wait(&self, budget: Duration) -> io::Result<()> {
        if self.observed_stopped()? {
            return Ok(());
        }
        if unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.init.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        } != 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        let deadline = Instant::now()
            .checked_add(budget)
            .ok_or(io::ErrorKind::InvalidInput)?;
        loop {
            if self.observed_stopped()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    /// Observe exact task exit, even after admission or session revocation.
    /// This does not establish a successful application exit.
    /// # Errors
    /// Returns a kernel poll failure rather than assuming the task stopped.
    pub fn observed_stopped(&self) -> io::Result<bool> {
        let mut fd = libc::pollfd {
            fd: self.init.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&raw mut fd, 1, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if fd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(fd.revents & libc::POLLIN != 0)
    }
}

/// Fixed bounded registration transport, not an arbitrary callback. It sends
/// exact descriptors to the root lease owner without replacing their origin.
pub struct ConnectedNamespace {
    stream: UnixStream,
}
impl ConnectedNamespace {
    /// Own the private registration transport after the launcher has verified
    /// its protected root helper. This transport carries kernel observations;
    /// constructing it grants no retained view or filesystem authority.
    #[must_use]
    pub fn new(stream: UnixStream) -> Self {
        Self { stream }
    }
    pub(crate) fn send(
        mut self,
        witness: NamespaceWitness,
        cancellation: &ServiceCancellation,
    ) -> io::Result<()> {
        let result = witness.send(&mut self.stream, cancellation);
        drop(witness);
        result
    }
}

/// Uses the existing owned-service namespace observer and teardown unchanged,
/// while leaving ordinary GUI network decisions in the normal pipeline.
/// This owner contains no retained view, FD grant or source path constructor.
pub struct OrdinaryNamespaceOwner {
    inner: NamespaceOwner,
    retained: Option<Arc<crate::retained_view::RetainedViewAdmission>>,
    selected_parent: Option<Arc<crate::retained_view::SelectedParentAdmission>>,
}
impl OrdinaryNamespaceOwner {
    pub(crate) fn with_selected_parent(
        authority: Arc<dyn ServiceAuthority>,
        cancellation: ServiceCancellation,
        connected: ConnectedNamespace,
        parent: Arc<crate::retained_view::SelectedParentAdmission>,
    ) -> io::Result<Self> {
        authority.check()?;
        Ok(Self {
            inner: NamespaceOwner::for_ordinary(authority, cancellation, connected)?,
            retained: None,
            selected_parent: Some(parent),
        })
    }
    pub(crate) fn with_retained(
        authority: Arc<dyn ServiceAuthority>,
        cancellation: ServiceCancellation,
        connected: ConnectedNamespace,
        retained: Arc<crate::retained_view::RetainedViewAdmission>,
    ) -> io::Result<Self> {
        authority.check()?;
        Ok(Self {
            inner: NamespaceOwner::for_ordinary(authority, cancellation, connected)?,
            retained: Some(retained),
            selected_parent: None,
        })
    }
    /// # Errors
    /// Refuses a retired authority or failure to create the canonical info pipe.
    pub fn new(
        authority: Arc<dyn ServiceAuthority>,
        cancellation: ServiceCancellation,
        connected: ConnectedNamespace,
    ) -> io::Result<Self> {
        authority.check()?;
        Ok(Self {
            inner: NamespaceOwner::for_ordinary(authority, cancellation, connected)?,
            retained: None,
            selected_parent: None,
        })
    }
}
impl EnrolledProcessObserver for OrdinaryNamespaceOwner {
    fn selected_parent(&self) -> Option<&crate::retained_view::SelectedParentAdmission> {
        self.selected_parent.as_deref()
    }
    fn retained_view(&self) -> Option<&crate::retained_view::RetainedViewAdmission> {
        self.retained.as_deref()
    }
    // Default reviewed_network_hosts(None) preserves the manifest/workspace
    // decision. Delegating ServiceAuthority.network_hosts would block or alter
    // ordinary app networking and is intentionally not part of this wrapper.
    fn namespace_init_fd(&self) -> Option<RawFd> {
        self.inner.namespace_init_fd()
    }
    fn owns_process_group(&self) -> bool {
        self.inner.owns_process_group()
    }
    fn before_spawn(&self) -> io::Result<()> {
        self.inner.before_spawn()
    }
    fn child_created(&self) {
        self.inner.child_created();
    }
    fn supervise(&self, child: Child) -> io::Result<ExitStatus> {
        self.inner.supervise(child)
    }
}

fn refused(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn parent(pid: u32) -> io::Result<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .and_then(|value| value.trim().parse().ok())
        .ok_or_else(|| refused("ordinary namespace parent unavailable"))
}

// One absolute deadline covers the entire transfer. Nonblocking syscalls and
// short poll slices ensure owner cancellation is observed while awaiting an
// authenticated but stalled peer; neither partial progress nor EINTR renews it.
pub(crate) struct Handoff<'a> {
    stream: &'a mut UnixStream,
    cancellation: &'a ServiceCancellation,
    deadline: Instant,
    was_nonblocking: bool,
}
impl<'a> Handoff<'a> {
    pub(crate) fn new(
        stream: &'a mut UnixStream,
        cancellation: &'a ServiceCancellation,
        budget: Duration,
    ) -> io::Result<Self> {
        let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let deadline = Instant::now()
            .checked_add(budget)
            .ok_or(io::ErrorKind::InvalidInput)?;
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            cancellation,
            deadline,
            was_nonblocking: flags & libc::O_NONBLOCK != 0,
        })
    }
    pub(crate) fn retry<T>(
        &mut self,
        events: i16,
        mut operation: impl FnMut(&mut UnixStream) -> io::Result<T>,
    ) -> io::Result<T> {
        loop {
            if self.cancellation.is_cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let remaining = self
                .deadline
                .checked_duration_since(Instant::now())
                .ok_or(io::ErrorKind::TimedOut)?;
            if remaining.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match operation(self.stream) {
                Ok(value) => return Ok(value),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
            let millis =
                i32::try_from(remaining.as_millis().clamp(1, 50)).map_err(io::Error::other)?;
            let mut fd = libc::pollfd {
                fd: self.stream.as_raw_fd(),
                events,
                revents: 0,
            };
            if unsafe { libc::poll(&raw mut fd, 1, millis) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }
    pub(crate) fn read_exact(&mut self, mut bytes: &mut [u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let count = self.retry(libc::POLLIN, |stream| stream.read(bytes))?;
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            bytes = bytes.get_mut(count..).ok_or(io::ErrorKind::InvalidData)?;
        }
        Ok(())
    }
    pub(crate) fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let count = self.retry(libc::POLLOUT, |stream| stream.write(bytes))?;
            if count == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            bytes = bytes.get(count..).ok_or(io::ErrorKind::InvalidData)?;
        }
        Ok(())
    }
}
impl Drop for Handoff<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.stream.set_nonblocking(self.was_nonblocking) {
            tracing::error!(%error, "ordinary namespace transport mode restoration failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd;

    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn stalled_handoff_uses_one_total_deadline_and_restores_socket_mode() {
        let (mut stream, _stalled_peer) = UnixStream::pair().unwrap();
        let cancellation = ServiceCancellation::default();
        {
            let mut io =
                Handoff::new(&mut stream, &cancellation, Duration::from_millis(60)).unwrap();
            let error = io.read_exact(&mut [0; 4]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        }
        let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(flags & libc::O_NONBLOCK, 0);
    }

    #[test]
    fn receive_refuses_actual_live_unisolated_child_even_with_authentic_launcher_peer() {
        // Genuine kernel process and pidfd, not a mocked authority or forged
        // ExecutingPeer. This proves the receive boundary refuses ancestry
        // claims; it does not pretend an ordinary child is a namespace init.
        let mut child = OwnedChild(
            std::process::Command::new("/usr/bin/sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        );
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, child.0.id(), 0) };
        assert!(
            raw >= 0,
            "required actual pidfd_open failed: {}",
            io::Error::last_os_error()
        );
        let init = unsafe { OwnedFd::from_raw_fd(i32::try_from(raw).unwrap()) };
        let pid_namespace = File::open(format!("/proc/{}/ns/pid", child.0.id())).unwrap();
        let mount_namespace = File::open(format!("/proc/{}/ns/mnt", child.0.id())).unwrap();
        let candidate = Arc::new(NamespaceWitness {
            init,
            pid_namespace,
            mount_namespace,
            pid: child.0.id(),
            outer: std::process::id(),
            launcher: std::process::id(),
        });
        let (mut receiver, mut sender) = UnixStream::pair().unwrap();
        for stream in [&receiver, &sender] {
            stream
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            stream
                .set_write_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
        }
        let sent_candidate = Arc::clone(&candidate);
        let worker = std::thread::spawn(move || {
            runtime_portal::process_binding::complete(&mut sender).unwrap();
            sent_candidate
                .send(&mut sender, &ServiceCancellation::default())
                .unwrap();
        });
        runtime_portal::process_binding::arm(&receiver).unwrap();
        let challenge = [29; 32];
        receiver
            .write_all(&runtime_portal::process_binding::MAGIC)
            .unwrap();
        receiver.write_all(&challenge).unwrap();
        let peer = runtime_portal::process_binding::receive(
            &receiver,
            &challenge,
            rustix::process::geteuid().as_raw(),
        )
        .unwrap();
        receiver.write_all(&[1]).unwrap();
        assert!(peer.current());
        assert_eq!(
            candidate.validate_member(&peer, &peer).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied,
            "genuine same-UID QPE cannot make a live unisolated child a consuming namespace"
        );
        let error =
            NamespaceWitness::receive(&mut receiver, &peer, &ServiceCancellation::default())
                .err()
                .expect("unisolated child must be refused");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        worker.join().unwrap();
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "refusing an unowned task must not signal it"
        );
    }
}
