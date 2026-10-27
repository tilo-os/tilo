//! Root-mounted target projection in a canonical owned private namespace.
//! This module does not admit caller paths, source credentials, or mount names.
//! The canonical signed runner must supply NativeTrust/NamespaceOwner hooks;
//! merely constructing this bootstrap does not launch a trusted service.
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{FileExt, MetadataExt, OpenOptionsExt},
            net::UnixStream,
        },
    },
    sync::Arc,
};
mod delayed_filter;
mod endpoint;
pub(crate) mod exposure;
mod observation;
mod selected_lookup;
mod selected_read;
mod shim;
mod workspace;
pub use endpoint::{
    LookedUpSelected, ObservedTargetObject, SelectedParentIssue, TargetProjectionEndpoint,
};
#[cfg(feature = "native-home-alias-qualification")]
pub use exposure::FixtureExposure;
pub use exposure::HeldViewExposure;
pub use shim::{AdmittedTargetFuse, MountedTargetBootstrap, RootTargetBootstrap, TargetFuseReady};

pub(crate) const TOKEN_FD: RawFd = 52;
pub(crate) const FUSE_FD: RawFd = 53;
pub(crate) const READY_FD: RawFd = 54;
pub const WORKER_ARG: &str = "--tilo-retained-target-projection";
pub const PACKAGE_ID: &str = "so.tilo.imager.home-export";
const MAX_RECORD: u64 = 16384;
const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
const NAMESPACES: [&str; 6] = ["mnt", "pid", "net", "ipc", "uts", "user"];
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn root() -> io::Result<()> {
    let mut r = 0;
    let mut e = 0;
    let mut s = 0;
    // SAFETY: getresuid fills three valid uid_t output pointers.
    if unsafe { libc::getresuid(&raw mut r, &raw mut e, &raw mut s) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if (r, e, s) != (0, 0, 0) || unsafe { libc::setfsuid(u32::MAX) } != 0 {
        return Err(refused("Target projection requires real root custody"));
    }
    Ok(())
}
/// Obtained from the live certified Shell `ExecutingPeer`, never NSS or source GID.
/// Root-only, nonserializable and no supplementary-source-group field.
pub struct TargetProjectionCredentials {
    uid: u32,
    gid: u32,
}
impl TargetProjectionCredentials {
    /// # Errors
    /// Fails unless the caller has real root custody and the IDs identify a
    /// non-root target user without sentinel user or group IDs.
    pub fn admitted(uid: u32, gid: u32) -> io::Result<Self> {
        root()?;
        validate_ids(uid, gid)?;
        Ok(Self { uid, gid })
    }
}
/// Fixed native presentation, sealed by the canonical root admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetProjectionMode {
    SelectedFile,
    HomeDirectory,
    SelectedParent,
}
pub trait TargetProjectionAuthority: super::ServiceAuthority {
    fn projection_mode(&self) -> TargetProjectionMode;
    /// Default deny. Only the setup-private adapter retaining the genuine root
    /// edit admission AND constructed filtered source supplies this live object.
    /// Mode or descriptive scope fields alone never establish source rights.
    ///
    /// # Errors
    /// Implementations fail if the live edit admission cannot be revalidated.
    fn selected_parent_edit(&self) -> io::Result<Option<&dyn SelectedParentEditAuthority>> {
        Ok(None)
    }
    /// # Errors
    /// Fails if the certified target identity cannot be admitted under root custody.
    fn target_credentials(&self) -> io::Result<TargetProjectionCredentials>;
    fn context_sha256(&self) -> [u8; 32];
    fn source_view_sha256(&self) -> [u8; 32];
    /// Actual selected held-file presentation component, preserving POSIX bytes.
    fn presentation_name(&self) -> io::Result<OsString>;
}
/// Nonserializable root policy bridge, implemented only by the setup-private
/// adapter around `RootSelectedEditAdmission` and its constructed source filter.
/// `check()` replays current original source and fresh target authorization.
pub trait SelectedParentEditAuthority: super::ServiceAuthority {
    /// # Errors
    /// Fails if the original selection or current target authorization cannot be
    /// revalidated to produce the sealed edit scope.
    fn scope_facts(&self) -> io::Result<SelectedParentScopeFacts>;
}
/// Descriptive binding only, with no FD, source credentials, backend methods,
/// serde implementation or grant constructor. The live authority is separate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedParentScopeFacts {
    pub request_id: String,
    pub resource_record: String,
    pub raw_leaf: Vec<u8>,
    pub source_parent_identity: [u64; 2],
    pub source_selected_identity: [u64; 2],
    pub source_context_sha256: [u8; 32],
    pub source_view_sha256: [u8; 32],
    pub target_context_sha256: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealedSelectedParentScope {
    // Closed selected + own create-new regular temps only. No directory,
    // sibling, unlink or replacement/rename authority is represented here.
    version: u32,
    max_owned_temporaries: u32,
    request_id: String,
    resource_record: String,
    raw_leaf: Vec<u8>,
    source_parent_identity: [u64; 2],
    source_selected_identity: [u64; 2],
    source_context_sha256: [u8; 32],
    source_view_sha256: [u8; 32],
    target_context_sha256: [u8; 32],
}
impl SealedSelectedParentScope {
    fn from_facts(facts: SelectedParentScopeFacts) -> Self {
        Self {
            version: 1,
            max_owned_temporaries: 64,
            request_id: facts.request_id,
            resource_record: facts.resource_record,
            raw_leaf: facts.raw_leaf,
            source_parent_identity: facts.source_parent_identity,
            source_selected_identity: facts.source_selected_identity,
            source_context_sha256: facts.source_context_sha256,
            source_view_sha256: facts.source_view_sha256,
            target_context_sha256: facts.target_context_sha256,
        }
    }
    fn validate(&self, record: &Record) -> io::Result<()> {
        validate_name(&self.raw_leaf)?;
        let id = uuid::Uuid::parse_str(&self.request_id)
            .map_err(|_| refused("Selected parent request differs"))?;
        if self.version != 1
            || self.max_owned_temporaries != 64
            || id.is_nil()
            || id.to_string() != self.request_id
            || self.resource_record
                != format!("selected-open-v2:edit-in-parent:{}", self.request_id)
            || self.source_parent_identity[1] == 0
            || self.source_selected_identity[1] == 0
            || self.source_parent_identity == self.source_selected_identity
            || self.source_context_sha256 == [0; 32]
            || self.source_view_sha256 != record.source_view_sha256
            || self.target_context_sha256 != record.context_sha256
            || self.raw_leaf != record.name
        {
            return Err(refused("Selected parent sealed scope differs"));
        }
        Ok(())
    }
}
fn selected_parent_scope(
    authority: &dyn TargetProjectionAuthority,
) -> io::Result<Option<SealedSelectedParentScope>> {
    authority.check()?;
    let edit = authority.selected_parent_edit()?;
    match (authority.projection_mode(), edit) {
        (TargetProjectionMode::SelectedParent, Some(edit)) => {
            edit.check()?;
            let facts = edit.scope_facts()?;
            edit.check()?;
            authority.check()?;
            Ok(Some(SealedSelectedParentScope::from_facts(facts)))
        }
        (TargetProjectionMode::SelectedParent, None) => {
            Err(refused("Selected parent lacks genuine live edit admission"))
        }
        (_, None) => Ok(None),
        (_, Some(_)) => Err(refused(
            "Ordinary projection cannot acquire selected parent edit scope",
        )),
    }
}
fn validate_ids(uid: u32, gid: u32) -> io::Result<()> {
    if uid == 0 || uid == u32::MAX || gid == u32::MAX {
        return Err(refused("Target credentials differ"));
    }
    Ok(())
}
fn validate_name(name: &[u8]) -> io::Result<()> {
    if name.is_empty()
        || name.len() > 255
        || name == b"."
        || name == b".."
        || name.contains(&0)
        || name.contains(&b'/')
    {
        return Err(refused("Target presentation component differs"));
    }
    Ok(())
}
fn namespaces() -> io::Result<Vec<(u64, u64)>> {
    NAMESPACES
        .iter()
        .map(|name| {
            let m = std::fs::metadata(format!("/proc/self/ns/{name}"))?;
            Ok((m.dev(), m.ino()))
        })
        .collect()
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FuseIdentity {
    dev: u64,
    ino: u64,
    rdev: u64,
    mode: u32,
    uid: u32,
    gid: u32,
}
impl FuseIdentity {
    fn observe(file: &File) -> io::Result<Self> {
        let m = file.metadata()?;
        if m.mode() & libc::S_IFMT != libc::S_IFCHR
            || libc::major(m.rdev()) != 10
            || libc::minor(m.rdev()) != 229
        {
            return Err(refused("Target FUSE descriptor is not /dev/fuse"));
        }
        let flags = descriptor_flags(file);
        if flags < 0 || flags & libc::O_ACCMODE != libc::O_RDWR {
            return Err(refused("Target FUSE descriptor mode differs"));
        }
        Ok(Self {
            dev: m.dev(),
            ino: m.ino(),
            rdev: m.rdev(),
            mode: m.mode(),
            uid: m.uid(),
            gid: m.gid(),
        })
    }
}
fn descriptor_flags(file: &File) -> i32 {
    // SAFETY: F_GETFL reads descriptor flags without pointer arguments.
    unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    mode: TargetProjectionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selected_parent: Option<SealedSelectedParentScope>,
    purpose: String,
    uid: u32,
    gid: u32,
    context_sha256: [u8; 32],
    source_view_sha256: [u8; 32],
    name: Vec<u8>,
    namespaces: Vec<(u64, u64)>,
    fuse: FuseIdentity,
    baseline: delayed_filter::Binding,
    mount_id: Option<u64>,
    clone_mount_id: Option<u64>,
}
impl Record {
    fn validate(&self, mounted: bool) -> io::Result<()> {
        validate_ids(self.uid, self.gid)?;
        validate_name(&self.name)?;
        if mounted {
            self.baseline.validate()?;
        }
        match (&self.mode, &self.selected_parent) {
            (TargetProjectionMode::SelectedParent, Some(scope)) => scope.validate(self)?,
            (TargetProjectionMode::SelectedParent, None) => {
                return Err(refused("Selected parent record lacks edit scope"));
            }
            (_, None) => {}
            (_, Some(_)) => return Err(refused("Ordinary projection contains edit scope")),
        }

        if self.schema != 4
            || self.purpose != "retained-target-projection"
            || self.context_sha256 == [0; 32]
            || self.source_view_sha256 == [0; 32]
            || self.namespaces.len() != NAMESPACES.len()
            || (mounted != self.mount_id.is_some())
            || self.mount_id == Some(0)
            || (mounted != self.clone_mount_id.is_some())
            || self.clone_mount_id == Some(0)
            || (mounted && self.clone_mount_id == self.mount_id)
        {
            return Err(refused("Target bootstrap record differs"));
        }
        Ok(())
    }
}
fn seal(record: &Record) -> io::Result<File> {
    root()?;
    let bytes = serde_json::to_vec(record)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_RECORD {
        return Err(refused("Target bootstrap bound exceeded"));
    }
    // SAFETY: literal name and fixed flags create a private sealable descriptor.
    let raw = unsafe {
        libc::memfd_create(
            c"tilo-target-projection".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(raw) };
    file.write_all(&bytes)?;
    if unsafe { libc::fcntl(raw, libc::F_ADD_SEALS, SEALS) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}
fn receive_record(file: &File) -> io::Result<Record> {
    let m = file.metadata()?;
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    let name = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    if !m.is_file()
        || m.uid() != 0
        || m.nlink() != 0
        || m.len() == 0
        || m.len() > MAX_RECORD
        || seals < 0
        || seals & SEALS != SEALS
        || !name
            .as_os_str()
            .as_bytes()
            .starts_with(b"/memfd:tilo-target-projection")
    {
        return Err(refused("Target bootstrap lacks sealed root custody"));
    }
    let mut bytes = vec![0; usize::try_from(m.len()).map_err(io::Error::other)?];
    file.read_exact_at(&mut bytes, 0)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
fn duplicate(fd: RawFd) -> io::Result<File> {
    let raw = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 128) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(raw) })
}
fn mount_id(file: &File) -> io::Result<u64> {
    let mut bytes = Vec::new();
    File::open(format!("/proc/self/fdinfo/{}", file.as_raw_fd()))?
        .take(16385)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16384 {
        return Err(refused("Target fdinfo bound exceeded"));
    }
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    let mut found = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("mnt_id:") {
            if found.is_some() {
                return Err(refused("Target mount identity duplicated"));
            }
            let value = value.trim();
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(refused("Target mount identity malformed"));
            }
            found = Some(value.parse::<u64>().map_err(io::Error::other)?);
        }
    }
    found
        .filter(|id| *id != 0)
        .ok_or_else(|| refused("Target mount identity unavailable"))
}
/// Canonical runner custody before namespace creation. Its descriptor pairs and
/// exact splice hooks are usable only after independent signed-native admission.
struct LaunchState {
    token: File,
    filter: delayed_filter::Filter,
    record: Record,
}
pub struct TargetProjectionAdmission {
    token: File,
    native: Option<super::CaptureAdmission>,
    native_installation: bool,
    workspace: std::sync::Mutex<Option<workspace::Workspace>>,
    launch: std::sync::Mutex<Option<LaunchState>>,
    fuse: File,
    ready: UnixStream,
    pub(super) receiver: std::sync::Mutex<Option<UnixStream>>,
    record: Record,
    pub(super) authority: Arc<dyn TargetProjectionAuthority>,
    exposure: Arc<exposure::ExposureCustody>,
}
impl TargetProjectionAdmission {
    /// # Errors
    /// Fails if root custody, live authority, offline policy, target identity,
    /// FUSE access, or sealed bootstrap preparation cannot be established.
    pub fn admit(authority: Arc<dyn TargetProjectionAuthority>) -> io::Result<Self> {
        root()?;
        authority.check()?;
        if !authority.network_hosts()?.is_empty() {
            return Err(refused("Target projection service must be offline"));
        }
        let selected_parent = selected_parent_scope(authority.as_ref())?;
        let c = authority.target_credentials()?;
        let fuse = File::options()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/dev/fuse")?;
        let record = Record {
            // Schema 3 requires the long-lived authenticated TOM1 observation channel.
            // Older workers only acknowledge readiness and cannot serve observations.
            schema: 4,
            mode: authority.projection_mode(),
            selected_parent,
            purpose: "retained-target-projection".into(),
            uid: c.uid,
            gid: c.gid,
            context_sha256: authority.context_sha256(),
            source_view_sha256: authority.source_view_sha256(),
            name: authority.presentation_name()?.into_vec(),
            namespaces: namespaces()?,
            fuse: FuseIdentity::observe(&fuse)?,
            baseline: delayed_filter::Binding {
                sha256: [0; 32],
                bytes: 0,
            },
            mount_id: None,
            clone_mount_id: None,
        };
        record.validate(false)?;
        let token = seal(&record)?;
        let (ready, receiver) = UnixStream::pair()?;
        let this = Self {
            token,
            fuse,
            ready,
            receiver: std::sync::Mutex::new(Some(receiver)),
            record,
            native: None,
            native_installation: false,
            workspace: std::sync::Mutex::new(None),
            launch: std::sync::Mutex::new(None),
            authority,
            exposure: Arc::new(exposure::ExposureCustody::default()),
        };
        this.check()?;
        Ok(this)
    }
    pub(crate) fn begin_exposure_stop(&self) -> bool {
        self.exposure.begin_stop()
    }
    pub(crate) fn check(&self) -> io::Result<()> {
        root()?;
        self.authority.check()?;
        let c = self.authority.target_credentials()?;
        if selected_parent_scope(self.authority.as_ref())? != self.record.selected_parent
            || self.authority.projection_mode() != self.record.mode
            || c.uid != self.record.uid
            || c.gid != self.record.gid
            || self.authority.context_sha256() != self.record.context_sha256
            || self.authority.source_view_sha256() != self.record.source_view_sha256
            || self.authority.presentation_name()?.into_vec() != self.record.name
            || FuseIdentity::observe(&self.fuse)? != self.record.fuse
            || receive_record(&self.token)?.namespaces != self.record.namespaces
        {
            return Err(refused("Target admission changed"));
        }
        if namespaces()? != self.record.namespaces {
            return Err(refused("Target parent namespaces changed"));
        }
        if let Some(native) = &self.native {
            native.check()?;
        }
        if let Some(workspace) = self
            .workspace
            .lock()
            .map_err(|_| refused("Target workspace poisoned"))?
            .as_ref()
        {
            workspace.check()?;
        }
        self.authority.check()
    }
    /// # Errors
    /// Fails if admission has changed, a runtime is already present, or the
    /// runtime directory and its signed publisher cannot be verified.
    pub fn with_native_runtime(
        mut self,
        runtime: &std::path::Path,
        publisher: &str,
    ) -> io::Result<Self> {
        self.check()?;
        if self.native.is_some() {
            return Err(refused("Target native runtime already admitted"));
        }
        let directory = File::options()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(runtime)?;
        self.native = Some(super::CaptureAdmission::verify(
            runtime, publisher, directory, 0,
        )?);
        self.check()?;
        Ok(self)
    }
    /// # Errors
    /// Fails if admission has changed, the package is not the projection worker,
    /// or the installation cannot be bound to the admitted signed runtime.
    pub fn with_installation(
        mut self,
        installed: package_store::InstalledPackage,
        publisher: &str,
    ) -> io::Result<Self> {
        self.check()?;
        if self.native_installation || installed.id != PACKAGE_ID {
            return Err(refused("Target native installation differs"));
        }
        self.native = Some(
            self.native
                .take()
                .ok_or_else(|| refused("Target native runtime missing"))?
                .with_installation(installed, publisher)?,
        );
        self.native_installation = true;
        self.check()?;
        Ok(self)
    }
    pub(crate) fn native(&self) -> io::Result<&super::CaptureAdmission> {
        self.check()?;
        if !self.native_installation {
            return Err(refused("Target native installation missing"));
        }
        self.native
            .as_ref()
            .ok_or_else(|| refused("Target native runtime missing"))
    }
    pub(crate) fn prepare_workspace(&self, path: &std::path::Path) -> io::Result<()> {
        self.check()?;
        let mut slot = self
            .workspace
            .lock()
            .map_err(|_| refused("Target workspace poisoned"))?;
        if slot.is_some() {
            return Err(refused("Target workspace already prepared"));
        }
        *slot = Some(workspace::prepare(path, &self.record)?);
        drop(slot);
        Ok(())
    }
    pub(crate) fn launch_dirs(&self) -> io::Result<(std::path::PathBuf, std::path::PathBuf)> {
        self.check()?;
        self.workspace
            .lock()
            .map_err(|_| refused("Target workspace poisoned"))?
            .as_ref()
            .ok_or_else(|| refused("Target workspace missing"))?
            .paths()
    }
    pub(crate) fn identity(&self) -> (u32, u32) {
        (self.record.uid, self.record.gid)
    }
    pub(crate) fn prepare_filter(&self, baseline: &OwnedFd) -> io::Result<()> {
        self.native()?;
        let mut slot = self
            .launch
            .lock()
            .map_err(|_| refused("Target filter custody poisoned"))?;
        if slot.is_some() {
            return Err(refused("Target baseline already prepared"));
        }
        let filter = delayed_filter::Filter::capture(baseline)?;
        let mut record = self.record.clone();
        record.baseline = filter.binding();
        record.baseline.validate()?;
        let token = seal(&record)?;
        *slot = Some(LaunchState {
            token,
            filter,
            record,
        });
        drop(slot);
        Ok(())
    }
    fn launch_record(&self) -> io::Result<Record> {
        self.launch
            .lock()
            .map_err(|_| refused("Target filter custody poisoned"))?
            .as_ref()
            .map(|s| s.record.clone())
            .ok_or_else(|| refused("Target filter missing"))
    }
    /// The fixed compatibility filter stays active; the normal compiler output
    /// is installed by the signed shim immediately after root mount admission.
    /// Canonical runner must separately require root native mode, host userns,
    /// fresh PID/net/IPC/UTS/mount namespaces and only the three transient caps.
    pub(crate) fn splice_baseline_argv(&self, argv: &mut Vec<String>) -> io::Result<()> {
        self.check()?;
        let expected = ["--add-seccomp-fd", "40", "--add-seccomp-fd", "41"];
        if argv.len() < 4 || argv.iter().take(4).map(String::as_str).ne(expected) {
            return Err(refused("Target canonical baseline prefix differs"));
        }
        argv.drain(2..4);
        Ok(())
    }
    pub(crate) fn pairs(&self) -> io::Result<Vec<(RawFd, RawFd)>> {
        let mut pairs = self.native()?.descriptor_pairs();
        pairs.retain(|(_, fd)| *fd != super::capture::SOURCE_FD);
        let guard = self
            .launch
            .lock()
            .map_err(|_| refused("Target filter custody poisoned"))?;
        let state = guard
            .as_ref()
            .ok_or_else(|| refused("Target baseline missing"))?;
        pairs.extend([
            (state.token.as_raw_fd(), TOKEN_FD),
            (self.fuse.as_raw_fd(), FUSE_FD),
            (self.ready.as_raw_fd(), READY_FD),
        ]);
        drop(guard);
        Ok(pairs)
    }
    pub(crate) fn replace_baseline(&self, pairs: &mut [(RawFd, RawFd)]) -> io::Result<()> {
        self.check()?;
        let guard = self
            .launch
            .lock()
            .map_err(|_| refused("Target filter custody poisoned"))?;
        let state = guard
            .as_ref()
            .ok_or_else(|| refused("Target baseline missing"))?;
        let target = crate::services::BASELINE_SECCOMP_CHILD_FD;
        if pairs.iter().filter(|(_, fd)| *fd == target).count() != 1 {
            return Err(refused("Target baseline descriptor multiplicity differs"));
        }
        pairs
            .iter_mut()
            .find(|(_, fd)| *fd == target)
            .ok_or(io::ErrorKind::InvalidData)?
            .0 = state.filter.descriptor();
        drop(guard);
        Ok(())
    }
    pub(crate) fn splice_credentials(&self, argv: &mut Vec<String>) -> io::Result<()> {
        self.native()?;
        if argv.iter().any(|v| {
            [
                "--share-net",
                "--unshare-user",
                "--uid",
                "--gid",
                "--original-source-fd",
                "--retained-io-fd",
                "--target-projection-fd",
            ]
            .contains(&v.as_str())
        }) {
            return Err(refused("Target namespace plan changed"));
        }
        let pos = argv
            .iter()
            .position(|v| v == "--unshare-all")
            .ok_or_else(|| refused("Target canonical namespace plan missing"))?;
        argv.splice(
            pos..=pos,
            [
                "--unshare-pid",
                "--unshare-net",
                "--unshare-ipc",
                "--unshare-uts",
                "--unshare-cgroup-try",
                "--cap-drop",
                "ALL",
                "--cap-add",
                "CAP_SETUID",
                "--cap-add",
                "CAP_SETGID",
                "--cap-add",
                "CAP_SYS_ADMIN",
            ]
            .map(String::from),
        );
        let shim = argv
            .iter()
            .position(|v| v == "__sandbox-exec")
            .ok_or_else(|| refused("Target signed shim missing"))?;
        let after_shim = shim.checked_add(1).ok_or(io::ErrorKind::InvalidData)?;
        argv.splice(
            after_shim..after_shim,
            ["--target-projection-fd".into(), TOKEN_FD.to_string()],
        );
        let mut traversal = Vec::new();
        for p in [
            "/run/tilo",
            "/etc",
            "/etc/alternatives",
            "/run/tilo-runtime",
            "/var",
            "/var/tilo-apps",
            "/usr/share",
            "/usr/share/X11",
        ] {
            traversal.extend(["--perms".into(), "0755".into(), "--dir".into(), p.into()]);
        }
        argv.splice(0..0, traversal);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn target_bootstrap_refuses_pre_observation_workers() {
        let mut record = Record {
            schema: 4,
            mode: TargetProjectionMode::HomeDirectory,
            selected_parent: None,
            purpose: "retained-target-projection".into(),
            uid: 1000,
            gid: 1000,
            context_sha256: [1; 32],
            source_view_sha256: [2; 32],
            name: b"Home".to_vec(),
            namespaces: namespaces().unwrap(),
            // Validation of the wire version does not mint or observe a FUSE endpoint.
            fuse: FuseIdentity {
                dev: 0,
                ino: 0,
                rdev: 0,
                mode: 0,
                uid: 0,
                gid: 0,
            },
            baseline: delayed_filter::Binding {
                sha256: [0; 32],
                bytes: 0,
            },
            mount_id: None,
            clone_mount_id: None,
        };
        record.validate(false).unwrap();
        record.mode = TargetProjectionMode::SelectedParent;
        assert_eq!(
            record.validate(false).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        record.mode = TargetProjectionMode::HomeDirectory;
        record.schema = 2;
        assert_eq!(
            record.validate(false).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        record.schema = 3;
        assert_eq!(
            record.validate(false).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        record.schema = 5;
        assert_eq!(
            record.validate(false).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        record.schema = 4;
        record.validate(false).unwrap();
    }
    struct ModeOnlyAuthority;
    impl super::super::ServiceAuthority for ModeOnlyAuthority {
        fn check(&self) -> io::Result<()> {
            Ok(())
        }
    }
    impl TargetProjectionAuthority for ModeOnlyAuthority {
        fn projection_mode(&self) -> TargetProjectionMode {
            TargetProjectionMode::SelectedParent
        }
        fn target_credentials(&self) -> io::Result<TargetProjectionCredentials> {
            Err(refused(
                "No authentic target credential in mode-only negative fixture",
            ))
        }
        fn context_sha256(&self) -> [u8; 32] {
            [0; 32]
        }
        fn source_view_sha256(&self) -> [u8; 32] {
            [0; 32]
        }
        fn presentation_name(&self) -> io::Result<OsString> {
            Err(refused("No source grant"))
        }
    }
    #[test]
    fn selected_parent_enum_and_descriptive_fields_cannot_upgrade_default_denied_authority() {
        // Calls the actual admission/endpoint scope path, before opening FUSE.
        // No positive fake root/source/worker grant is constructed here.
        assert_eq!(
            selected_parent_scope(&ModeOnlyAuthority)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }
    #[test]
    fn ordinary_and_unsealed_descriptors_cannot_mint_target_admission() {
        let ordinary = tempfile::tempfile().unwrap();
        assert!(receive_record(&ordinary).is_err());
        assert!(FuseIdentity::observe(&ordinary).is_err());
        let raw = unsafe {
            libc::memfd_create(
                c"tilo-target-projection".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        assert!(raw >= 0);
        let mut unsealed = unsafe { File::from_raw_fd(raw) };
        unsealed.write_all(b"{}").unwrap();
        assert!(receive_record(&unsealed).is_err());
        assert!(FuseIdentity::observe(&unsealed).is_err());
    }
    #[test]
    fn presentation_names_preserve_non_utf8_but_refuse_path_and_overlong_input() {
        validate_name(&[b'f', 0xff]).unwrap();
        for bad in [
            &b""[..],
            &b"."[..],
            &b".."[..],
            &b"a/b"[..],
            &b"a\0b"[..],
            &[b'a'; 256][..],
        ] {
            assert!(validate_name(bad).is_err());
        }
    }
    #[test]
    fn held_mount_identity_comes_from_actual_descriptor_not_namespace_inode() {
        let root = File::open("/").unwrap();
        assert_ne!(mount_id(&root).unwrap(), 0);
        let ns = File::open("/proc/self/ns/mnt").unwrap();
        assert!(FuseIdentity::observe(&ns).is_err());
    }
}
