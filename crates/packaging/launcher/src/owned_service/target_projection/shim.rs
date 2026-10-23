//! Signed shim's transient root phase. Any error is fatal to that canonical
//! service; `NamespaceOwner` retains the child until actual namespace teardown.
use super::{
    AsRawFd, FUSE_FD, File, FromRawFd, FuseIdentity, MetadataExt, OpenOptionsExt, OsString,
    OsStringExt, OwnedFd, READY_FD, Read, Record, TOKEN_FD, TargetProjectionMode, UnixStream,
    delayed_filter, duplicate, io, mount_id, namespaces, receive_record, refused, root, seal,
    validate_ids,
};
use runtime_portal::process_binding;
use std::ffi::CString;
use std::os::fd::AsFd;

pub struct RootTargetBootstrap {
    record: Record,
    fuse: File,
    ready: UnixStream,
    filter: delayed_filter::Filter,
}
pub struct MountedTargetBootstrap {
    record: Record,
    readonly_view: Option<HeldReadView>,
}
struct HeldReadView {
    file: File,
    mount_id: u64,
    ino: u64,
}
impl HeldReadView {
    fn capture(file: File, expected_mount: u64) -> io::Result<Self> {
        let ino = held_inode(&file)?;
        let result = Self {
            file,
            mount_id: expected_mount,
            ino,
        };
        result.check(expected_mount)?;
        Ok(result)
    }
    fn check(&self, expected_mount: u64) -> io::Result<()> {
        let flags = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if flags & (libc::O_PATH | libc::O_DIRECTORY) != (libc::O_PATH | libc::O_DIRECTORY)
            || mount_id(&self.file)? != expected_mount
            || expected_mount != self.mount_id
            || held_inode(&self.file)? != self.ino
        {
            return Err(refused("Target held read view kernel identity differs"));
        }
        Ok(())
    }
}
fn held_inode(file: &File) -> io::Result<u64> {
    let mut bytes = Vec::new();
    File::open(format!("/proc/self/fdinfo/{}", file.as_raw_fd()))?
        .take(16385)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16384 {
        return Err(refused("Target held read fdinfo bound exceeded"));
    }
    let mut found = None;
    for line in std::str::from_utf8(&bytes)
        .map_err(io::Error::other)?
        .lines()
    {
        if let Some(value) = line.strip_prefix("ino:") {
            let value = value.trim();
            if found.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(refused("Target held read inode malformed"));
            }
            found = Some(value.parse::<u64>().map_err(io::Error::other)?);
        }
    }
    found
        .filter(|value| *value != 0)
        .ok_or_else(|| refused("Target held read inode unavailable"))
}
impl RootTargetBootstrap {
    /// Exact fixed descriptor admission, before mandatory Landlock. The caller
    /// is the independently verified canonical native shim, never an app mode.
    ///
    /// # Errors
    /// Fails if root credentials, the sealed bootstrap, namespace separation, inherited FUSE identity, or baseline filter custody cannot be verified.
    pub fn prepare() -> io::Result<Self> {
        root()?;
        let record = receive_record(&duplicate(TOKEN_FD)?)?;
        record.validate(false)?;
        let actual = namespaces()?;
        if actual.last() != record.namespaces.last()
            || actual
                .iter()
                .take(5)
                .zip(record.namespaces.iter().take(5))
                .any(|(a, b)| a == b)
        {
            return Err(refused("Target bootstrap namespace closure differs"));
        }
        let fuse = duplicate(FUSE_FD)?;
        if FuseIdentity::observe(&fuse)? != record.fuse {
            return Err(refused("Target FUSE admission differs"));
        }
        let ready: OwnedFd = duplicate(READY_FD)?.into();
        let ready = UnixStream::from(ready);
        let filter = delayed_filter::Filter::receive(&record.baseline)?;
        Ok(Self {
            record,
            fuse,
            ready,
            filter,
        })
    }
    /// Root kernel mount into root-private canonical namespace. No fuser
    /// Session is created as root: its Owner ACL must snapshot the target UID.
    ///
    /// # Errors
    /// Fails if private placement, mounting, receipt publication, descriptor handoff, or baseline filter installation fails. After mounting, the namespace owner must reclaim the namespace on failure.
    pub fn mount_and_publish(mut self) -> io::Result<MountedTargetBootstrap> {
        root()?;
        let parent = private_parent()?;
        let leaf = child_directory(&parent, c"view", 0o700)?;
        if std::fs::read_dir(format!("/proc/self/fd/{}", leaf.as_raw_fd()))?
            .next()
            .transpose()?
            .is_some()
        {
            return Err(refused("Target placement leaf is not empty"));
        }
        if mount_id(&leaf)? != mount_id(&parent)? {
            return Err(refused("Target leaf already has a mount"));
        }
        let target = CString::new(format!("/proc/self/fd/{}", leaf.as_raw_fd()))
            .map_err(io::Error::other)?;
        let options = CString::new(format!(
            "fd={},rootmode=40700,user_id={},group_id={},default_permissions",
            self.fuse.as_raw_fd(),
            self.record.uid,
            self.record.gid
        ))
        .map_err(io::Error::other)?;
        // SAFETY: literal filesystem/source, owned held target directory and
        // bounded NUL-terminated data remain live for the mount syscall.
        if unsafe {
            libc::mount(
                c"tilo-retained".as_ptr(),
                target.as_ptr(),
                c"fuse".as_ptr(),
                libc::MS_NODEV | libc::MS_NOSUID | libc::MS_NOEXEC,
                options.as_ptr().cast(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        // From successful mount onward errors are fatal; the canonical root
        // NamespaceOwner, not lexical umount/drop, owns namespace reclamation.
        let view = open_child(&parent, c"view")?;
        let mounted_id = mount_id(&view)?;
        if mounted_id == mount_id(&parent)? {
            return Err(refused("Target FUSE mount was not placed"));
        }
        self.record.namespaces = namespaces()?;
        self.record.mount_id = Some(mounted_id);
        // Clone while still root IN the owning mount namespace. A foreign
        // attached mount cannot be open_tree-cloned by the receiving root.
        let exposure = super::exposure::clone_view(&view)?;
        self.record.clone_mount_id = Some(mount_id(&exposure)?);
        self.record.validate(true)?;
        let receipt = seal(&self.record)?;
        let mnt = File::open("/proc/self/ns/mnt")?;
        process_binding::complete(&mut self.ready)?;
        bounded(&self.ready)?;
        for (index, fd) in [view.as_fd(), mnt.as_fd(), receipt.as_fd(), exposure.as_fd()]
            .into_iter()
            .enumerate()
        {
            runtime_portal::fd_transport::send_with_fd(
                self.ready.as_fd(),
                &[u8::try_from(index).map_err(io::Error::other)?],
                fd,
            )?;
            let mut ack = [0];
            self.ready.read_exact(&mut ack)?;
            if ack != [1] {
                return Err(refused("Root target mount custody refused"));
            }
        }
        // Publish the actual root-created observed receipt to fixed SDK52.
        if unsafe { libc::dup3(receipt.as_raw_fd(), TOKEN_FD, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // This exact compiled normal baseline closes the transient mount phase
        // BEFORE mandatory LL or any credential transition.
        self.filter.install()?;
        drop(self.ready);
        Ok(MountedTargetBootstrap {
            record: self.record,
            readonly_view: Some(HeldReadView::capture(view, mounted_id)?),
        })
    }
}
impl MountedTargetBootstrap {
    /// Consumes the exact root-opened mounted directory before ambient FD
    /// closure. No caller-selected path/descriptor can replace this private FD.
    pub(crate) fn confine(
        &mut self,
        spec: &sandbox_core::landlock::RulesetSpec,
        on_skip: impl FnMut(&std::path::Path, &landlock::PathFdError),
    ) -> Result<landlock::RestrictionStatus, sandbox_core::landlock::LandlockBuildError> {
        root()?;
        let view = self
            .readonly_view
            .take()
            .ok_or_else(|| refused("Target held read rule already consumed"))?;
        view.check(
            self.record
                .mount_id
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?,
        )?;
        let result = sandbox_core::landlock::restrict_self_with_readonly_directory(
            spec,
            view.file.as_fd(),
            on_skip,
        );
        // Must close before canonical close_ambient_fds, avoiding stale File
        // ownership after the ambient closer and no new exec-inherited FD.
        drop(view);
        result
    }
    /// Called exclusively after the canonical mandatory Landlock succeeds.
    /// It cannot recreate transient mount privilege or admit source groups.
    pub fn drop_after_confinement(self) -> io::Result<()> {
        if self.readonly_view.is_some() {
            return Err(refused("Target worker missing held read confinement"));
        }
        super::super::capture::original::credentials::drop_authenticated(
            self.record.uid,
            self.record.gid,
            &[],
        )?;
        Ok(())
    }
}
fn open_child(parent: &File, name: &std::ffi::CStr) -> io::Result<File> {
    let raw = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(raw) })
}
fn child_directory(parent: &File, name: &std::ffi::CStr, mode: u32) -> io::Result<File> {
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), mode) };
    if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
        return Err(io::Error::last_os_error());
    }
    let file = open_child(parent, name)?;
    let m = file.metadata()?;
    if !m.is_dir() || m.uid() != 0 || m.mode() & 0o777 != mode {
        return Err(refused("Target private placement differs"));
    }
    Ok(file)
}
fn private_parent() -> io::Result<File> {
    let m = std::fs::metadata("/")?;
    if m.uid() != 0 || m.mode() & 0o022 != 0 {
        return Err(refused("Target namespace root differs"));
    }
    let run = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open("/run")?;
    let m = run.metadata()?;
    if m.uid() != 0 || m.mode() & 0o022 != 0 {
        return Err(refused("Target /run parent differs"));
    }
    // The canonical sandbox already creates /run/tilo for app/person. Its root
    // must remain root-owned and nonwritable by the target service.
    let tilo = open_child(&run, c"tilo")?;
    let m = tilo.metadata()?;
    if m.uid() != 0 || m.mode() & 0o022 != 0 {
        return Err(refused("Target service placement parent differs"));
    }
    // Root-private parent remains hidden even through /proc/<worker>/root.
    // The target service needs only its inherited kernel channel, never this
    // pathname; normal app access comes from the separately held root endpoint.
    child_directory(&tilo, c"target-projection", 0o700)
}
/// Constructor-private post-confinement FD admission. No pathname or FD number
/// parameter permits a worker to select another mount/provider.
pub struct AdmittedTargetFuse {
    fuse: OwnedFd,
    ready: TargetFuseReady,
}
pub struct TargetFuseReady {
    stream: UnixStream,
    record: Record,
}
impl AdmittedTargetFuse {
    ///
    /// # Errors
    /// Fails if the sealed record, dropped credentials, namespaces, or inherited FUSE descriptor cannot be authenticated.
    pub fn receive() -> io::Result<Self> {
        let record = receive_record(&duplicate(TOKEN_FD)?)?;
        record.validate(true)?;
        verify_dropped(record.uid, record.gid)?;
        if namespaces()? != record.namespaces {
            return Err(refused("Target worker namespace differs"));
        }
        let fuse = duplicate(FUSE_FD)?;
        if FuseIdentity::observe(&fuse)? != record.fuse {
            return Err(refused("Target worker FUSE descriptor differs"));
        }
        let stream = UnixStream::from(OwnedFd::from(duplicate(READY_FD)?));
        Ok(Self {
            fuse: fuse.into(),
            ready: TargetFuseReady { stream, record },
        })
    }
    pub fn projection_mode(&self) -> TargetProjectionMode {
        self.ready.record.mode
    }
    pub fn credentials(&self) -> (u32, u32) {
        (self.ready.record.uid, self.ready.record.gid)
    }
    pub fn context_sha256(&self) -> [u8; 32] {
        self.ready.record.context_sha256
    }
    pub fn source_view_sha256(&self) -> [u8; 32] {
        self.ready.record.source_view_sha256
    }
    pub fn presentation_name(&self) -> OsString {
        OsString::from_vec(self.ready.record.name.clone())
    }
    pub fn into_parts(self) -> (OwnedFd, TargetFuseReady) {
        (self.fuse, self.ready)
    }
}
impl TargetFuseReady {
    /// Call only after actual `Session::from_fd` handshake. Root observes this
    /// exact target task, not an app-authored readiness label, then checks the
    /// held kernel endpoint with the connected source relay serving requests.
    pub fn serve_observations(mut self) -> io::Result<()> {
        verify_dropped(self.record.uid, self.record.gid)?;
        if namespaces()? != self.record.namespaces {
            return Err(refused("Target ready namespace differs"));
        }
        let cancellation = crate::owned_service::ServiceCancellation::default();
        crate::retained_view::qpe_exchange(
            &mut self.stream,
            &cancellation,
            std::time::Duration::from_secs(10),
            process_binding::complete,
        )?;
        super::observation::serve(self.stream, &self.record)
    }
}
pub(super) fn verify_dropped(uid: u32, gid: u32) -> io::Result<()> {
    #[repr(C)]
    struct Header {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    validate_ids(uid, gid)?;
    let (mut r, mut e, mut s) = (0, 0, 0);
    if unsafe { libc::getresuid(&raw mut r, &raw mut e, &raw mut s) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if (r, e, s) != (uid, uid, uid)
        || u32::from_ne_bytes(unsafe { libc::setfsuid(u32::MAX) }.to_ne_bytes()) != uid
    {
        return Err(refused("Target worker UID differs"));
    }
    if unsafe { libc::getresgid(&raw mut r, &raw mut e, &raw mut s) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if (r, e, s) != (gid, gid, gid)
        || u32::from_ne_bytes(unsafe { libc::setfsgid(u32::MAX) }.to_ne_bytes()) != gid
        || unsafe { libc::getgroups(0, std::ptr::null_mut()) } != 0
        || unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } != 1
    {
        return Err(refused("Target worker GID/groups/confinement differ"));
    }
    let header = Header {
        version: 0x2008_0522,
        pid: 0,
    };
    let mut caps = [Data {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    if unsafe { libc::syscall(libc::SYS_capget, &raw const header, caps.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if caps
        .iter()
        .any(|c| c.effective != 0 || c.permitted != 0 || c.inheritable != 0)
    {
        return Err(refused("Target worker retained capabilities"));
    }
    Ok(())
}
fn bounded(stream: &UnixStream) -> io::Result<()> {
    let timeout = Some(std::time::Duration::from_secs(10));
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)
}

#[cfg(test)]
mod held_read_view_tests {
    use super::*;
    #[test]
    fn actual_held_rule_view_survives_path_replacement_and_refuses_unrelated_fd() {
        use getrandom::rand_core::TryRng as _;
        let mut nonce = [0; 16];
        getrandom::SysRng.try_fill_bytes(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "tilo-held-read-{}-{}",
            std::process::id(),
            hex::encode(nonce)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(path.join("view")).unwrap();
        let held = File::options()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path.join("view"))
            .unwrap();
        let mount = mount_id(&held).unwrap();
        let mut view = HeldReadView::capture(held, mount).unwrap();
        std::fs::rename(path.join("view"), path.join("original")).unwrap();
        std::fs::create_dir(path.join("view")).unwrap();
        view.check(mount).unwrap();
        let unrelated = File::options()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path.join("view"))
            .unwrap();
        view.file = unrelated;
        assert_eq!(
            view.check(mount).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        drop(view);
        std::fs::remove_dir_all(path).unwrap();
    }
}
