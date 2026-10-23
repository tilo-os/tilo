//! Exact root-bootstrap clone placement. No source path or permission constructor.
use super::{
    Arc, AsRawFd, File, FromRawFd, MetadataExt, OpenOptionsExt, OwnedFd, Read,
    TargetProjectionEndpoint, TargetProjectionMode, io, mount_id, refused, root,
};
use crate::ordinary_namespace::NamespaceWitness;
use getrandom::rand_core::TryRng as _;
use runtime_portal::process_binding::ExecutingPeer;
use std::{ffi::CString, os::unix::fs::DirBuilderExt, path::PathBuf};

/// This is lifetime custody only. It never makes failed authority checks pass.
/// Only actual root endpoint clone ownership increments it; no public setter or
/// caller boolean can defer shutdown of an unrelated service.
#[derive(Default)]
pub(super) struct ExposureCustody(std::sync::Mutex<ExposureState>);
#[derive(Default)]
struct ExposureState {
    holds: usize,
    stopping: bool,
}
pub(super) struct ExposureHold(Arc<ExposureCustody>);
impl ExposureCustody {
    pub(super) fn acquire(self: &Arc<Self>) -> io::Result<ExposureHold> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| refused("Exposure custody poisoned"))?;
        if state.stopping {
            return Err(io::ErrorKind::Interrupted.into());
        }
        state.holds = state
            .holds
            .checked_add(1)
            .ok_or(io::ErrorKind::InvalidData)?;
        drop(state);
        Ok(ExposureHold(Arc::clone(self)))
    }
    /// Linearizes admission against the first intentional cancellation signal.
    /// Poisoning refuses shutdown; it never discharges unknown custody.
    pub(super) fn begin_stop(&self) -> bool {
        let Ok(mut state) = self.0.lock() else {
            return false;
        };
        if state.holds != 0 {
            return false;
        }
        state.stopping = true;
        true
    }
}
impl Drop for ExposureHold {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.0.lock()
            && let Some(remaining) = state.holds.checked_sub(1)
        {
            state.holds = remaining;
        }
    }
}
const BASE: &str = "/run/tilo-retained-views";
const CLONE: u32 = 1;
const MOVE_EMPTY: u32 = 0x4 | 0x40;
#[repr(C)]
struct Attributes {
    set: u64,
    clear: u64,
    propagation: u64,
    userns: u64,
}
/// Called only in the independently admitted root bootstrap's owning namespace.
pub(super) fn clone_view(view: &File) -> io::Result<File> {
    root()?;
    let raw = unsafe {
        libc::syscall(
            libc::SYS_open_tree,
            view.as_raw_fd(),
            c"".as_ptr(),
            CLONE | libc::O_CLOEXEC as u32 | libc::AT_EMPTY_PATH as u32,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = i32::try_from(raw).map_err(io::Error::other)?;
    let clone = unsafe { File::from_raw_fd(descriptor) };
    let attrs = Attributes {
        set: 0x2 | 0x4 | 0x8,
        clear: 0,
        propagation: libc::MS_PRIVATE,
        userns: 0,
    };
    if unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            clone.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            &raw const attrs,
            std::mem::size_of::<Attributes>(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(clone)
}
fn namespace() -> io::Result<File> {
    File::open("/proc/self/ns/mnt")
}
fn identity(file: &File) -> io::Result<(u64, u64)> {
    let m = file.metadata()?;
    Ok((m.dev(), m.ino()))
}
fn protected_directory(path: &std::path::Path) -> io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let m = file.metadata()?;
    if !m.is_dir()
        || m.uid() != 0
        || m.gid() != 0
        || m.mode() & 0o022 != 0
        || m.mode() & 0o111 != 0o111
    {
        return Err(refused("Exposure ancestry not protected and searchable"));
    }
    Ok(file)
}
fn mount_entry(id: u64) -> io::Result<Option<(bool, bool)>> {
    let mut bytes = Vec::new();
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/proc/self/mountinfo")?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(refused("Exposure mountinfo bound exceeded"));
    }
    let mut found = None;
    for row in bytes.split(|b| *b == b'\n').filter(|r| !r.is_empty()) {
        let fields: Vec<&[u8]> = row.split(|b| *b == b' ').collect();
        let parse = |v: &[u8]| -> io::Result<u64> {
            if v.is_empty() || !v.iter().all(u8::is_ascii_digit) {
                return Err(refused("Exposure mount row malformed"));
            }
            std::str::from_utf8(v)
                .map_err(io::Error::other)?
                .parse()
                .map_err(io::Error::other)
        };
        if fields.len() < 10 {
            return Err(refused("Exposure mount row truncated"));
        }
        let [mount, parent, _, _, _, options, ..] = fields.as_slice() else {
            return Err(refused("Exposure mount row truncated"));
        };
        let rowid = parse(mount)?;
        parse(parent)?;
        let split = fields
            .iter()
            .position(|f| *f == b"-")
            .ok_or_else(|| refused("Exposure mount row separator missing"))?;
        if split < 6 || split.checked_add(4) != Some(fields.len()) {
            return Err(refused("Exposure mount row malformed"));
        }
        if rowid == id {
            if found.is_some() {
                return Err(refused("Exposure mount ID duplicated"));
            }
            found = Some((
                !fields
                    .get(6..split)
                    .ok_or_else(|| refused("Exposure mount row malformed"))?
                    .iter()
                    .any(|v| v.starts_with(b"shared:") || v.starts_with(b"master:")),
                [
                    b"nodev".as_slice(),
                    b"nosuid".as_slice(),
                    b"noexec".as_slice(),
                ]
                .iter()
                .all(|v| options.split(|b| *b == b',').any(|f| f == *v)),
            ));
        }
    }
    Ok(found)
}
fn mount_row(id: u64) -> io::Result<Option<bool>> {
    Ok(mount_entry(id)?.map(|(private, restricted)| private && restricted))
}
fn mounted_file(path: &std::path::Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}
fn held_inode(file: &File) -> io::Result<u64> {
    let mut bytes = Vec::new();
    File::open(format!("/proc/self/fdinfo/{}", file.as_raw_fd()))?
        .take(16385)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16384 {
        return Err(refused("Exposure fdinfo bound exceeded"));
    }
    let mut found = None;
    for row in bytes.split(|b| *b == b'\n') {
        if let Some(value) = row.strip_prefix(b"ino:") {
            if found.is_some() {
                return Err(refused("Exposure fdinfo inode duplicated"));
            }
            let value = value.trim_ascii();
            if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
                return Err(refused("Exposure fdinfo inode malformed"));
            }
            found = Some(
                std::str::from_utf8(value)
                    .map_err(io::Error::other)?
                    .parse()
                    .map_err(io::Error::other)?,
            );
        }
    }
    found
        .filter(|v| *v != 0)
        .ok_or_else(|| refused("Exposure fdinfo inode absent"))
}
fn exact_mode(file: &File, mode: u32) -> io::Result<()> {
    // O_PATH cannot fchmod; reopen only the root-owned staging directory itself.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    if identity(file)? != identity(&held)? {
        return Err(refused("Exposure chmod identity differs"));
    }
    if unsafe { libc::fchmod(held.as_raw_fd(), mode) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn readonly(file: &File) -> io::Result<()> {
    let attrs = Attributes {
        set: 1,
        clear: 0,
        propagation: 0,
        userns: 0,
    };
    if unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            &raw const attrs,
            std::mem::size_of::<Attributes>(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
/// Owned before placement and all fallible postchecks. Root session retains this
/// before any possible delivery, through observed consuming init stop. Unknown
/// stop blocks Drop rather than allowing the root process to lose mount custody.
pub struct HeldViewExposure {
    endpoint: Arc<TargetProjectionEndpoint>,
    hold: Option<ExposureHold>,
    peer: Option<Arc<ExecutingPeer>>,
    placement: Placement,
    facts: Option<super::observation::Facts>,
    readonly: bool,
    possible_delivery: bool,
    consumer: Option<OwnedFd>,
}
enum Placement {
    Local(MountPlacement),
    Remote(remote_placement::RemotePlacement),
}
impl HeldViewExposure {
    /// Prepare custody for a verified launcher in this mount namespace.
    pub fn prepare(
        endpoint: Arc<TargetProjectionEndpoint>,
        peer: Arc<ExecutingPeer>,
        binding: &crate::retained_view::ConsumerLaunchBinding,
    ) -> io::Result<Self> {
        root()?;
        endpoint.check()?;
        binding.verify_peer_program(&peer)?;
        if identity(&namespace()?)?
            != identity(&File::open(format!("/proc/{}/ns/mnt", peer.pid()))?)?
            || !peer.current()
            || (peer.uid(), peer.gid()) != (endpoint.record.uid, endpoint.record.gid)
        {
            return Err(refused("Exposure launcher mount namespace differs"));
        }
        Self::allocate(endpoint, Some(peer), !binding.writable)
    }
    fn allocate(
        endpoint: Arc<TargetProjectionEndpoint>,
        peer: Option<Arc<ExecutingPeer>>,
        requested_readonly: bool,
    ) -> io::Result<Self> {
        root()?;
        endpoint.check()?;
        let hold = endpoint.exposure_hold()?;
        let view = endpoint.take_detached_view()?;
        let id = endpoint
            .record
            .clone_mount_id
            .ok_or(io::ErrorKind::InvalidData)?;
        let readonly =
            requested_readonly || endpoint.record.mode == TargetProjectionMode::SelectedFile;
        let mut nonce = [0; 32];
        getrandom::SysRng
            .try_fill_bytes(&mut nonce)
            .map_err(io::Error::other)?;
        let placement = MountPlacement::allocate(view, id, readonly, nonce)?;
        Ok(Self {
            endpoint,
            hold: Some(hold),
            peer,
            placement: Placement::Local(placement),
            facts: None,
            readonly,
            possible_delivery: false,
            consumer: None,
        })
    }
    /// The original source owner stays in its own namespace. The actual managed
    /// consuming helper owns the separate staging namespace and launcher child.
    /// # Errors
    /// Returns an error if endpoint or launcher validation fails, the managed
    /// helper cannot establish private namespace custody, or its lock is poisoned.
    pub fn prepare_remote(
        endpoint: Arc<TargetProjectionEndpoint>,
        peer: Arc<ExecutingPeer>,
        binding: &crate::retained_view::ConsumerLaunchBinding,
        managed: Arc<std::sync::Mutex<crate::ordinary_child::ManagedConsumingBootstrap>>,
    ) -> io::Result<Self> {
        let mut preparation = ExposurePreparation::capture_remote(endpoint, peer, binding)?;
        let placement = remote_placement::RemotePlacement::prepare(&mut preparation, managed)?;
        Ok(Self {
            endpoint: Arc::clone(&preparation.endpoint),
            hold: preparation.hold.take(),
            peer: Some(Arc::clone(&preparation.peer)),
            readonly: preparation.record.readonly,
            placement: Placement::Remote(placement),
            facts: None,
            possible_delivery: false,
            consumer: None,
        })
    }
    /// Install in the persistent root owner before any control exchange.
    /// # Errors
    /// Returns an error if authority has expired, placement fails, or the measured
    /// mount identity, topology or security restrictions do not match.
    pub fn place(&mut self) -> io::Result<()> {
        self.endpoint.check()?;
        match &mut self.placement {
            Placement::Local(p) => p.place()?,
            Placement::Remote(p) => p.place()?,
        }
        let view = match &self.placement {
            Placement::Local(p) => p.file()?,
            Placement::Remote(p) => p.file()?,
        };
        let facts = self.endpoint.measure_exposure(view)?;
        let expected = libc::ST_NODEV
            | libc::ST_NOSUID
            | libc::ST_NOEXEC
            | if self.readonly { libc::ST_RDONLY } else { 0 };
        if facts.fs_flags & expected != expected
            || mount_id(view)? != facts.mount_id
            || held_inode(view)? != facts.ino
        {
            return Err(refused("Exposure measured object or restrictions differ"));
        }
        self.facts = Some(facts);
        self.check()
    }
    /// Validate live authority and exact placement custody.
    pub fn check(&self) -> io::Result<()> {
        self.endpoint.check()?;
        if let Some(peer) = &self.peer
            && !peer.current()
        {
            return Err(refused("Exposure launcher retired"));
        }
        match &self.placement {
            Placement::Local(p) => p.check()?,
            Placement::Remote(p) => p.check()?,
        }
        let facts = self.facts.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        let view = match &self.placement {
            Placement::Local(p) => p.file()?,
            Placement::Remote(p) => p.file()?,
        };
        if mount_id(view)? != facts.mount_id || held_inode(view)? != facts.ino {
            return Err(refused("Exposure object differs"));
        }
        Ok(())
    }
    /// Return the held view after validating custody.
    pub fn file(&self) -> io::Result<&File> {
        self.check()?;
        match &self.placement {
            Placement::Local(p) => p.file(),
            Placement::Remote(p) => p.file(),
        }
    }
    /// Return the validated placed mount identity.
    pub fn mount_id(&self) -> io::Result<u64> {
        self.check()?;
        Ok(self
            .facts
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?
            .mount_id)
    }
    /// Check that an observation belongs to this exposure's endpoint and inode.
    pub fn matches(&self, object: &super::ObservedTargetObject<'_>) -> io::Result<()> {
        self.check()?;
        object.check()?;
        if self.peer.is_none() {
            return Err(refused(
                "Fixture exposure cannot issue application authority",
            ));
        }
        let facts = self.facts.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        if !std::ptr::eq(Arc::as_ptr(&self.endpoint), object.endpoint)
            || (facts.dev, facts.ino) != (object.dev()?, object.ino()?)
        {
            return Err(refused("Exposure source observation differs"));
        }
        Ok(())
    }
    pub fn observe_failed_issue(&mut self, failure: &crate::retained_view::FailedViewIssue) {
        if !failure.view_may_have_been_delivered() {
            self.possible_delivery = false;
        }
    }
    /// Record possible delivery only while the exposure remains valid.
    pub fn mark_possible_delivery(&mut self) -> io::Result<()> {
        self.check()?;
        self.possible_delivery = true;
        Ok(())
    }
    /// Retain an independently verified consuming namespace witness.
    pub fn adopt_consumer(&mut self, witness: &NamespaceWitness) -> io::Result<()> {
        let peer = self
            .peer
            .as_ref()
            .ok_or_else(|| refused("Fixture cannot adopt production witness"))?;
        witness.validate_for_launcher(peer)?;
        if self.consumer.is_some() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        // Retain before any fallible remote W exchange.
        self.consumer = Some(witness.init_pidfd()?);
        if let Placement::Remote(p) = &mut self.placement {
            p.adopt_consumer(witness, peer)?;
        }
        Ok(())
    }
    /// Retire the mount only after any possible consumer has stopped.
    pub fn retire(&mut self) -> io::Result<()> {
        if self.possible_delivery {
            let fd = self.consumer.as_ref().ok_or(io::ErrorKind::WouldBlock)?;
            if !kernel_stopped(fd)? {
                return Err(io::ErrorKind::WouldBlock.into());
            }
        }
        match &mut self.placement {
            Placement::Local(p) => p.retire()?,
            Placement::Remote(p) => p.retire()?,
        }
        self.facts.take();
        self.consumer.take();
        self.possible_delivery = false;
        self.hold.take();
        Ok(())
    }
}
fn kernel_stopped(fd: &OwnedFd) -> io::Result<bool> {
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&raw mut p, 1, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if p.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(refused("Exposure pidfd observation failed"));
    }
    Ok(p.revents & libc::POLLIN != 0)
}
impl Drop for HeldViewExposure {
    fn drop(&mut self) {
        while self.hold.is_some() {
            if self.retire().is_ok() {
                break;
            }
            std::thread::park_timeout(std::time::Duration::from_millis(50));
        }
    }
}

/// Disposable boundary fixture only. No conversion to HeldViewExposure, Home,
/// SelectedParent, installed consent, signed app binding or Author grant exists.
#[cfg(feature = "native-home-alias-qualification")]
pub struct FixtureExposure {
    inner: HeldViewExposure,
    namespace: Option<File>,
}
#[cfg(feature = "native-home-alias-qualification")]
impl FixtureExposure {
    pub fn prepare(endpoint: Arc<TargetProjectionEndpoint>, writable: bool) -> io::Result<Self> {
        Ok(Self {
            inner: HeldViewExposure::allocate(endpoint, None, !writable)?,
            namespace: None,
        })
    }
    pub fn place(&mut self) -> io::Result<()> {
        self.inner.place()
    }
    pub fn file(&self) -> io::Result<&File> {
        self.inner.file()
    }
    pub fn mount_id(&self) -> io::Result<u64> {
        self.inner.mount_id()
    }
    pub fn target_observed_stopped(&self) -> io::Result<bool> {
        self.inner.endpoint.observed_stopped()
    }
    pub fn original_mount_id(&self) -> io::Result<u64> {
        self.inner.endpoint.observe_held_view()?.mount_id()
    }
    pub fn mark_possible_delivery(&mut self) -> io::Result<()> {
        self.inner.mark_possible_delivery()
    }
    /// Actual foreign attached mount refusal on the new-kernel boundary.
    pub fn verify_foreign_attached_clone_refused(&self) -> io::Result<()> {
        root()?;
        self.inner.endpoint.check()?;
        let original = self.inner.endpoint.held_view()?;
        match clone_view(&original) {
            Err(error) if error.raw_os_error() == Some(libc::EINVAL) => Ok(()),
            Err(error) => Err(error),
            Ok(_) => Err(refused("Kernel unexpectedly copied foreign attached view")),
        }
    }
    pub fn begin_next_consumer(&mut self) -> io::Result<()> {
        if !self.observed_stopped()? {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.inner.consumer.take();
        self.namespace.take();
        self.inner.possible_delivery = false;
        self.inner.check()
    }

    /// Untrusted info routing identifies only a candidate. Actual owned child,
    /// parent relations, held init pidfd/PID namespace and NSpid independently
    /// establish fixture custody. Never interpret outer process exit as stop.
    pub fn adopt_owned_child(
        &mut self,
        outer: &std::process::Child,
        info: &[u8],
    ) -> io::Result<()> {
        root()?;
        if !self.inner.possible_delivery || self.inner.consumer.is_some() || info.len() > 16384 {
            return Err(refused("Fixture consumer custody state differs"));
        }
        fn status(pid: u32) -> io::Result<String> {
            let mut text = String::new();
            File::open(format!("/proc/{pid}/status"))?
                .take(16385)
                .read_to_string(&mut text)?;
            if text.len() > 16384 {
                return Err(refused("Fixture status bound exceeded"));
            }
            Ok(text)
        }
        fn ppid(text: &str) -> io::Result<u32> {
            text.lines()
                .find_map(|l| l.strip_prefix("PPid:"))
                .ok_or(io::ErrorKind::InvalidData)?
                .trim()
                .parse()
                .map_err(io::Error::other)
        }
        let outer_status = status(outer.id())?;
        if ppid(&outer_status)? != std::process::id() {
            return Err(refused("Fixture bwrap not owned child"));
        }
        let info: serde_json::Value = serde_json::from_slice(info).map_err(io::Error::other)?;
        let pid: u32 = info
            .get("child-pid")
            .and_then(serde_json::Value::as_u64)
            .ok_or(io::ErrorKind::InvalidData)?
            .try_into()
            .map_err(io::Error::other)?;
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let held = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        let ns = File::open(format!("/proc/{pid}/ns/pid"))?;
        let st = status(pid)?;
        let nspid = st
            .lines()
            .find_map(|l| l.strip_prefix("NSpid:"))
            .ok_or(io::ErrorKind::InvalidData)?;
        if ppid(&st)? != outer.id()
            || nspid.split_whitespace().last() != Some("1")
            || identity(&ns)? == identity(&File::open("/proc/self/ns/pid")?)?
        {
            return Err(refused("Fixture actual owned namespace init differs"));
        }
        // Recheck after opening both kernel handles; never accept numeric wire
        // facts without actual held identity/ancestry.
        if ppid(&status(pid)?)? != outer.id() || ppid(&status(outer.id())?)? != std::process::id() {
            return Err(refused("Fixture ancestry changed"));
        }
        self.namespace = Some(ns);
        self.inner.consumer = Some(held);
        Ok(())
    }
    pub fn observed_stopped(&self) -> io::Result<bool> {
        let Some(fd) = &self.inner.consumer else {
            return Ok(false);
        };
        let mut p = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&raw mut p, 1, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(p.revents & libc::POLLIN != 0)
    }
    pub fn retire(&mut self) -> io::Result<()> {
        self.inner.retire()?;
        self.namespace.take();
        Ok(())
    }
}

#[cfg(test)]
mod custody_tests {
    use super::*;
    #[test]
    fn intentional_stop_and_acquisition_are_one_closed_transition() {
        let custody = Arc::new(ExposureCustody::default());
        let hold = custody.acquire().unwrap();
        assert!(!custody.begin_stop());
        drop(hold);
        assert!(custody.begin_stop());
        assert_eq!(
            custody.acquire().err().unwrap().kind(),
            io::ErrorKind::Interrupted
        );
    }
    #[test]
    fn simultaneous_stop_cannot_overlap_a_new_exposure_hold() {
        for _ in 0..32 {
            let custody = Arc::new(ExposureCustody::default());
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let worker_custody = Arc::clone(&custody);
            let worker_barrier = Arc::clone(&barrier);
            let worker = std::thread::spawn(move || {
                worker_barrier.wait();
                worker_custody.acquire()
            });
            barrier.wait();
            let stopping = custody.begin_stop();
            let acquired = worker.join().unwrap();
            assert_ne!(stopping, acquired.is_ok());
            if let Ok(hold) = acquired {
                assert!(!custody.begin_stop());
                drop(hold);
                assert!(custody.begin_stop());
            }
            assert!(custody.acquire().is_err());
        }
    }
}

mod mount_placement;
mod remote_placement;
pub(crate) use mount_placement::MountPlacement;
/// Preparation retains the real endpoint and target service custody. Its wire
/// description is never a grant; only a managed child can consume this object.
struct ExposurePreparation {
    endpoint: Arc<TargetProjectionEndpoint>,
    hold: Option<ExposureHold>,
    peer: Arc<ExecutingPeer>,
    binding: crate::retained_view::ConsumerLaunchBinding,
    record: crate::ordinary_child::exposure_control::PlacementRecord,
    clone: Option<File>,
}
impl ExposurePreparation {
    fn capture_remote(
        endpoint: Arc<TargetProjectionEndpoint>,
        peer: Arc<ExecutingPeer>,
        binding: &crate::retained_view::ConsumerLaunchBinding,
    ) -> io::Result<Self> {
        root()?;
        endpoint.check()?;
        binding.verify_peer_program(&peer)?;
        if !peer.current() || (peer.uid(), peer.gid()) != (endpoint.record.uid, endpoint.record.gid)
        {
            return Err(refused("Remote exposure target peer differs"));
        }
        let hold = endpoint.exposure_hold()?;
        let clone = endpoint.take_detached_view()?;
        let clone_id = endpoint
            .record
            .clone_mount_id
            .ok_or(io::ErrorKind::InvalidData)?;
        if mount_id(&clone)? != clone_id {
            return Err(refused("Remote detached clone differs"));
        }
        let readonly =
            !binding.writable || endpoint.record.mode == TargetProjectionMode::SelectedFile;
        if readonly {
            self::readonly(&clone)?;
        }
        let mut nonce = [0; 32];
        getrandom::SysRng
            .try_fill_bytes(&mut nonce)
            .map_err(io::Error::other)?;
        let record = crate::ordinary_child::exposure_control::PlacementRecord {
            schema: 1,
            nonce,
            context_sha256: endpoint.record.context_sha256,
            source_view_sha256: endpoint.record.source_view_sha256,
            target_uid: endpoint.record.uid,
            target_gid: endpoint.record.gid,
            clone_mount_id: clone_id,
            readonly,
        };
        record.validate()?;
        Ok(Self {
            endpoint,
            hold: Some(hold),
            peer,
            binding: binding.clone(),
            record,
            clone: Some(clone),
        })
    }
    pub(crate) fn target_peer(&self) -> &ExecutingPeer {
        &self.peer
    }
    pub(crate) fn check(&self) -> io::Result<()> {
        root()?;
        self.endpoint.check()?;
        self.binding.verify_peer_program(&self.peer)?;
        self.record.validate()?;
        if !self.peer.current()
            || self.record.context_sha256 != self.endpoint.record.context_sha256
            || self.record.source_view_sha256 != self.endpoint.record.source_view_sha256
            || Some(self.record.clone_mount_id) != self.endpoint.record.clone_mount_id
        {
            return Err(refused("Remote preparation retired"));
        }
        Ok(())
    }
}
