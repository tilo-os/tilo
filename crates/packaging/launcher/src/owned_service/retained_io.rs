//! Root-native retained-source I/O projection for one fixed offline service.
//! This is distinct from immutable migration capture. No launch option or wire
//! credential claim can mint an admission; the root actor retains actual custody.
pub use runtime_portal::retained_replacement::SELECTED_REPLACEMENT_PERMIT_FD;
use runtime_portal::retained_replacement::{MAX_PERMIT_BYTES, SelectedReplacementPermitRecord};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use std::{
    fs::File,
    io::{self, Read, Seek, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::fs::{FileExt, MetadataExt, OpenOptionsExt},
    },
    sync::{Arc, Mutex},
};

pub const SOURCE_PATH: &str = "/run/tilo/retained/source";
mod removal;
pub use removal::RetainedNativeOwner;
mod workspace;
pub const REMOVAL_DIRECTORY_FD: i32 = removal::FD;
pub const REMOVAL_JOURNAL_FD: i32 = removal::JOURNAL_FD;
pub const SOURCE_DATA_FD: i32 = removal::SOURCE_DATA_FD;
pub const NATIVE_OWNER_FD: i32 = 59;
pub const NATIVE_HISTORY_VERIFIER_FD: i32 = 60;

const SOURCE_FD: RawFd = 44;
pub(crate) const TOKEN_FD: RawFd = 49;
const MAX_TOKEN: u64 = 1024 * 1024;
const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
const NAMESPACES: [&str; 5] = ["mnt", "pid", "net", "ipc", "user"];
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn root() -> io::Result<()> {
    super::capture::check_credentials(0)
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
/// Root actor's inseparable, nonserializable authority. Both the authenticated
/// source lease and live certified target session must remain retained here.
/// Implementations are native root adapters, never deserialized caller records.
pub trait RetainedIoAuthority: super::ServiceAuthority {
    /// Only the concrete current certified daemon/source adapter provides this
    /// held sealed root descriptor. Browsing has no verifier or raw fallback.
    ///
    /// # Errors
    /// Returns an error if the held verifier cannot be authenticated or cloned.
    fn native_history_verifier(&self) -> io::Result<Option<File>> {
        Ok(None)
    }

    /// Held root-sealed selected edit derivative, never caller supplied bytes.
    ///
    /// # Errors
    /// Returns an error if the held edit permit cannot be authenticated or cloned.
    fn selected_replacement_permit(&self) -> io::Result<Option<File>> {
        Ok(None)
    }

    /// Obtain the independently admitted original source credentials.
    ///
    /// # Errors
    /// Refuses unavailable or no longer authenticated source credentials.
    fn source_credentials(&self) -> io::Result<RetainedIoCredentials>;
    /// Obtain the canonical retained native owner's identity.
    ///
    /// # Errors
    /// Refuses absent or invalid independently admitted owner custody.
    fn native_owner_sha256(&self) -> io::Result<[u8; 32]> {
        Err(refused(
            "Canonical retained native owner was not independently admitted",
        ))
    }
    /// Clone the independently admitted root-protected source namespace.
    ///
    /// # Errors
    /// Refuses missing namespace custody and propagates descriptor clone errors.
    fn removal_namespace(&self) -> io::Result<File> {
        Err(refused(
            "Root protected source namespace not independently admitted",
        ))
    }
    /// Retain namespace custody for recovery after the worker stops.
    ///
    /// # Errors
    /// Returns an error if the adapter cannot retain this native owner.
    fn retain_native_owner(&self, _owner: RetainedNativeOwner) -> io::Result<()> {
        Ok(())
    }
    /// Confirm that native namespace obligations have been retired.
    ///
    /// # Errors
    /// Refuses unavailable retirement custody or outstanding recovery obligations.
    fn confirm_native_retirement(&self) -> io::Result<()> {
        Err(refused("Native source retirement custody is unavailable"))
    }
    fn target_uid(&self) -> u32;
    fn context_sha256(&self) -> [u8; 32];
}
/// Original facts admitted by a root actor from its protected source record.
/// Construction is root-only; ordinary app/launcher callers cannot choose groups.
pub struct RetainedIoCredentials {
    uid: u32,
    gid: u32,
    groups: Vec<u32>,
}
impl RetainedIoCredentials {
    /// Admit canonical source credentials from a root-only source record.
    ///
    /// # Errors
    /// Refuses non-root callers, root or sentinel identities, and oversized,
    /// unsorted or duplicate supplementary groups.
    pub fn admitted(uid: u32, gid: u32, groups: &[u32]) -> io::Result<Self> {
        root()?;
        validate_credentials(uid, gid, groups)?;
        Ok(Self {
            uid,
            gid,
            groups: groups.to_vec(),
        })
    }
}
fn validate_credentials(uid: u32, gid: u32, groups: &[u32]) -> io::Result<()> {
    if uid == 0
        || uid == u32::MAX
        || gid == u32::MAX
        || groups.len() > 65536
        || groups.contains(&u32::MAX)
        || groups.windows(2).any(|p| matches!(p, [a, b] if a >= b))
    {
        return Err(refused("Retained source credentials are not canonical"));
    }
    Ok(())
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Token {
    schema: u32,
    purpose: String,
    context_sha256: [u8; 32],
    uid: u32,
    gid: u32,
    groups: Vec<u32>,
    target_uid: u32,
    source: (u64, u64),
    directory: bool,
    namespaces: Vec<(u64, u64)>,
    removal: Option<removal::Facts>,
    native_owner_sha256: Option<[u8; 32]>,
    selected_replacement_sha256: Option<[u8; 32]>,
    native_history_verifier_sha256: Option<[u8; 32]>,
}
impl Token {
    fn validate(&self) -> io::Result<()> {
        validate_credentials(self.uid, self.gid, &self.groups)?;
        if self.schema != 2
            || self.purpose != "retained-source-io"
            || self.target_uid == 0
            || self.target_uid == u32::MAX
            || self.context_sha256 == [0; 32]
            || self.directory != self.removal.is_some()
            || self.directory != self.native_owner_sha256.is_some()
            || self.native_owner_sha256 == Some([0; 32])
            || self.selected_replacement_sha256 == Some([0; 32])
            || self.native_history_verifier_sha256 == Some([0; 32])
            || (self.native_history_verifier_sha256.is_some() && !self.directory)
            || (self.selected_replacement_sha256.is_some()
                && self.native_history_verifier_sha256.is_none())
            || (self.selected_replacement_sha256.is_some() && !self.directory)
            || self.namespaces.len() != NAMESPACES.len()
        {
            return Err(refused("Retained source token differs"));
        }
        Ok(())
    }
}
/// Holds the exact approved source descriptor and both authority lifetimes until
/// canonical `NamespaceOwner` observes quiescence. It is neither `Clone` nor serde.
pub struct RetainedIoAdmission {
    source: File,
    source_metadata: Option<File>,
    // Existing capture admission supplies only independently pinned runtime,
    // native package evidence and sealed executable closure here. Its internal
    // directory is the runtime root, never a projected source or capture grant.
    native: Option<super::CaptureAdmission>,
    native_installation: bool,
    pub(super) worker_program: Option<Arc<super::retained_worker::RetainedWorkerProgram>>,
    token: File,
    native_owner: Option<File>,
    selected_replacement: Option<File>,
    native_history_verifier: Option<File>,
    facts: Token,
    pub(super) authority: Arc<dyn RetainedIoAuthority>,
    workspace: Mutex<Option<workspace::Workspace>>,
    excluded: Mutex<Vec<File>>,
    removal: Option<removal::Removal>,
}
impl RetainedIoAdmission {
    /// Admit an editable source while retaining its authenticated root authority.
    ///
    /// # Errors
    /// Refuses non-root callers, invalid or read-only sources, inconsistent
    /// permits or verifier custody, and revoked authority; propagates I/O errors.
    pub fn admit(source: File, authority: Arc<dyn RetainedIoAuthority>) -> io::Result<Self> {
        root()?;
        authority.check()?;
        let c = authority.source_credentials()?;
        let m = source.metadata()?;
        if (!m.is_dir() && !m.is_file()) || (m.is_file() && m.nlink() != 1) {
            return Err(refused("Retained source is not a file or directory"));
        }
        let mut v: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatvfs(source.as_raw_fd(), &raw mut v) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if v.f_flag & libc::ST_RDONLY != 0 {
            return Err(refused("Retained editable source is read-only"));
        }
        let selected_replacement = authority.selected_replacement_permit()?;
        let selected_replacement_sha256 = selected_replacement
            .as_ref()
            .map(|permit| {
                replacement_digest(permit, (m.dev(), m.ino()), authority.native_owner_sha256()?)
            })
            .transpose()?;
        authority.check()?;
        let mut facts = Token {
            schema: 2,
            purpose: "retained-source-io".into(),
            context_sha256: authority.context_sha256(),
            uid: c.uid,
            gid: c.gid,
            groups: c.groups,
            target_uid: authority.target_uid(),
            source: (m.dev(), m.ino()),
            directory: m.is_dir(),
            namespaces: namespaces()?,
            removal: None,
            selected_replacement_sha256,
            native_history_verifier_sha256: None,
            native_owner_sha256: if m.is_dir() {
                Some(authority.native_owner_sha256()?)
            } else {
                None
            },
        };
        let removal = if facts.directory {
            Some(removal::Removal::create(
                &source,
                &facts,
                authority.clone(),
            )?)
        } else {
            None
        };
        facts.removal = removal.as_ref().map(removal::Removal::facts).transpose()?;
        let native_history_verifier = authority.native_history_verifier()?;
        if let Some(file) = &native_history_verifier {
            let claims = read_history_verifier(file)?;
            if !facts.directory || Some(claims.source_owner) != facts.native_owner_sha256 {
                return Err(refused("History verifier differs from retained source"));
            }
            facts.native_history_verifier_sha256 = Some(claims.sha256()?);
        }
        authority.check()?;
        facts.validate()?;
        let token = seal(&facts)?;
        let native_owner = facts
            .native_owner_sha256
            .as_ref()
            .map(|key| seal_bytes(key))
            .transpose()?;
        let source_metadata = source_metadata(&source, facts.directory)?;
        let this = Self {
            source,
            source_metadata,
            native: None,
            native_installation: false,
            worker_program: None,
            token,
            native_owner,
            selected_replacement,
            native_history_verifier,
            facts,
            authority,
            workspace: Mutex::new(None),
            excluded: Mutex::new(Vec::new()),
            removal,
        };
        this.check()?;
        Ok(this)
    }
    /// Admit the exact independently configured signed SDK publisher and pin
    /// its root/ABI plus the canonical shim/bwrap/loader executable closure.
    /// The root adapter obtains these inputs from `TrustedCaptureService`; no
    /// launch option, environment variable or publisher in an untrusted DTO
    /// supplies that policy. The runtime directory is internal metadata custody.
    ///
    /// # Errors
    /// Refuses revoked custody, duplicate runtime admission or an invalid signed
    /// runtime; propagates runtime directory and executable verification errors.
    pub fn with_native_runtime(
        mut self,
        runtime: &std::path::Path,
        publisher: &str,
    ) -> io::Result<Self> {
        self.check()?;
        if self.native.is_some() {
            return Err(refused("Native runtime already admitted"));
        }
        self.exclude_path(runtime)?;
        let internal_directory = File::options()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(runtime)?;
        self.native = Some(super::CaptureAdmission::verify(
            runtime,
            publisher,
            internal_directory,
            0,
        )?);
        self.check()?;
        Ok(self)
    }
    /// Bind exactly the actual sidecar installation already verified against
    /// the host application's embedded service identity, without registry lookup.
    ///
    /// # Errors
    /// Refuses revoked custody, duplicate installation admission or an invalid
    /// signed worker package; propagates path and installation verification errors.
    pub fn with_installation(
        mut self,
        installed: package_store::InstalledPackage,
        publisher: &str,
    ) -> io::Result<Self> {
        self.check()?;
        if self.native_installation {
            return Err(refused("Native installation already admitted"));
        }
        for path in [
            installed.payload_root.clone(),
            installed
                .manifest_path
                .parent()
                .ok_or(io::ErrorKind::InvalidInput)?
                .to_path_buf(),
            package_store::integrity::record_dir(&installed),
        ] {
            self.exclude_path(&path)?;
        }
        let worker_program = super::retained_worker::RetainedWorkerProgram::admit(&installed)?;
        self.native = Some(
            self.native
                .take()
                .ok_or_else(|| refused("Native runtime missing"))?
                .with_installation(installed, publisher)?,
        );
        self.worker_program = Some(Arc::new(worker_program));
        self.native_installation = true;
        self.check()?;
        Ok(self)
    }
    pub(crate) fn native(&self) -> io::Result<&super::CaptureAdmission> {
        self.check()?;
        if !self.native_installation {
            return Err(refused("Native installation missing"));
        }
        self.native
            .as_ref()
            .ok_or_else(|| refused("Native runtime missing"))
    }
    pub(crate) fn worker_argument(&self) -> &'static str {
        if self.facts.directory {
            super::RETAINED_HOME_IO_WORKER_ARGUMENT
        } else {
            super::RETAINED_IO_WORKER_ARGUMENT
        }
    }
    pub(crate) fn check(&self) -> io::Result<()> {
        root()?;
        self.authority.check()?;
        let c = self.authority.source_credentials()?;
        let m = self.source.metadata()?;
        if (m.dev(), m.ino()) != self.facts.source
            || m.is_dir() != self.facts.directory
            || (m.is_file() && m.nlink() != 1)
            || (c.uid, c.gid) != (self.facts.uid, self.facts.gid)
            || c.groups != self.facts.groups
            || self.authority.target_uid() != self.facts.target_uid
            || namespaces()? != self.facts.namespaces
            || self.authority.context_sha256() != self.facts.context_sha256
        {
            return Err(refused("Retained I/O authority changed"));
        }
        for input in self
            .excluded
            .lock()
            .map_err(|_| refused("Retained exclusions poisoned"))?
            .iter()
        {
            super::capture::inventory_scratch::refuse_retained_overlap(&self.source, input)?;
        }
        if let Some(workspace) = self
            .workspace
            .lock()
            .map_err(|_| refused("Worker custody poisoned"))?
            .as_ref()
        {
            workspace.check()?;
        }
        if let Some(native) = &self.native {
            native.check()?;
        }
        if self.native_installation {
            self.worker_program
                .as_ref()
                .ok_or_else(|| refused("Signed worker ELF missing"))?
                .check()?;
        }
        if let Some(removal) = &self.removal {
            removal.check()?;
        }
        match (
            &self.selected_replacement,
            self.facts.selected_replacement_sha256,
        ) {
            (Some(permit), Some(expected))
                if replacement_digest(
                    permit,
                    self.facts.source,
                    self.facts
                        .native_owner_sha256
                        .ok_or_else(|| refused("Native source owner absent"))?,
                )? == expected => {}
            (None, None) => {}
            _ => return Err(refused("Selected replacement permit differs")),
        }
        check_history_verifier(
            self.native_history_verifier.as_ref(),
            self.facts.native_history_verifier_sha256,
            self.facts.native_owner_sha256,
        )?;
        check_token(&self.token)
    }
    pub(crate) fn confirm_removal_retirement(&self) -> io::Result<()> {
        self.removal
            .as_ref()
            .map(removal::Removal::confirm_empty)
            .transpose()?;
        Ok(())
    }
    pub(crate) fn exclude_input(&self, input: File) -> io::Result<()> {
        self.check()?;
        super::capture::inventory_scratch::refuse_retained_overlap(&self.source, &input)?;
        self.excluded
            .lock()
            .map_err(|_| refused("Retained exclusions poisoned"))?
            .push(input);
        self.check()
    }
    pub(crate) fn exclude_path(&self, path: &std::path::Path) -> io::Result<()> {
        self.exclude_input(
            File::options()
                .read(true)
                .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)?,
        )
    }
    pub(crate) fn prepare_workspace(&self, path: &std::path::Path) -> io::Result<()> {
        self.check()?;
        self.exclude_path(path)?;
        let mut held = self
            .workspace
            .lock()
            .map_err(|_| refused("Worker custody poisoned"))?;
        if held.is_some() {
            return Err(refused("Worker already prepared"));
        }
        *held = Some(workspace::prepare(path, &self.facts)?);
        drop(held);
        Ok(())
    }
    pub(crate) fn launch_dirs(&self) -> io::Result<(std::path::PathBuf, std::path::PathBuf)> {
        self.check()?;
        self.workspace
            .lock()
            .map_err(|_| refused("Worker custody poisoned"))?
            .as_ref()
            .ok_or_else(|| refused("Worker workspace missing"))?
            .paths()
    }
    pub(crate) fn identity(&self) -> (u32, u32) {
        (self.facts.uid, self.facts.gid)
    }
    pub(crate) fn pairs(&self) -> io::Result<Vec<(RawFd, RawFd)>> {
        let mut pairs = self.native()?.descriptor_pairs();
        // Retained I/O owns FD44; capture's internal directory is NEVER exposed.
        pairs.retain(|(_, child)| *child != super::capture::SOURCE_FD);
        pairs.extend([
            (self.source.as_raw_fd(), SOURCE_FD),
            (self.token.as_raw_fd(), TOKEN_FD),
        ]);
        if let Some(removal) = &self.removal {
            pairs.push((removal.descriptor()?, REMOVAL_DIRECTORY_FD));
            pairs.push((removal.journal_descriptor()?, REMOVAL_JOURNAL_FD));
        }
        if let Some(owner) = &self.native_owner {
            pairs.push((owner.as_raw_fd(), NATIVE_OWNER_FD));
        }
        if let Some(verifier) = &self.native_history_verifier {
            pairs.push((verifier.as_raw_fd(), NATIVE_HISTORY_VERIFIER_FD));
        }
        if let Some(permit) = &self.selected_replacement {
            pairs.push((permit.as_raw_fd(), SELECTED_REPLACEMENT_PERMIT_FD));
        }
        if let Some(source) = &self.source_metadata {
            pairs.push((source.as_raw_fd(), SOURCE_DATA_FD));
        }
        Ok(pairs)
    }
    pub(crate) fn splice_source(&self, argv: &mut Vec<String>) -> io::Result<()> {
        self.check()?;
        argv.splice(
            0..0,
            [
                "--bind-fd".into(),
                SOURCE_FD.to_string(),
                SOURCE_PATH.into(),
            ],
        );
        let shim = argv
            .iter()
            .position(|v| v == "__sandbox-exec")
            .ok_or_else(|| refused("Retained I/O signed shim missing"))?;
        argv.splice(
            shim.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?..=shim,
            ["--rw-noexec".into(), SOURCE_PATH.into()],
        );
        if self.removal.is_some() {
            let shim = argv
                .iter()
                .position(|v| v == "__sandbox-exec")
                .ok_or_else(|| refused("Retained I/O signed shim missing"))?;
            argv.splice(
                shim.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?..=shim,
                [
                    "--rw-noexec".into(),
                    format!("/proc/self/fd/{REMOVAL_DIRECTORY_FD}"),
                    "--rw-noexec".into(),
                    format!("/proc/self/fd/{REMOVAL_JOURNAL_FD}"),
                ],
            );
        }
        Ok(())
    }
    pub(crate) fn splice_credentials(&self, argv: &mut Vec<String>) -> io::Result<()> {
        self.check()?;
        if argv.iter().any(|v| {
            [
                "--share-net",
                "--unshare-user",
                "--uid",
                "--gid",
                "--original-source-fd",
            ]
            .contains(&v.as_str())
        }) {
            return Err(refused("Retained namespace plan changed"));
        }
        let pos = argv
            .iter()
            .position(|v| v == "--unshare-all")
            .ok_or_else(|| refused("Retained private namespace missing"))?;
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
            ]
            .map(String::from),
        );
        let shim = argv
            .iter()
            .position(|v| v == "__sandbox-exec")
            .ok_or_else(|| refused("Retained I/O signed shim missing"))?;
        argv.splice(
            shim.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?..=shim,
            ["--retained-io-fd".into(), TOKEN_FD.to_string()],
        );
        let mut traversal = Vec::new();
        for p in [
            "/run/tilo",
            "/run/tilo/retained",
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
fn source_metadata(source: &File, directory: bool) -> io::Result<Option<File>> {
    if !directory {
        return Ok(None);
    }
    let file = rustix::fs::openat(
        source,
        c".",
        rustix::fs::OFlags::PATH
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    Ok(Some(file.into()))
}
fn check_token(token: &File) -> io::Result<()> {
    let m = token.metadata()?;
    let seals = unsafe { libc::fcntl(token.as_raw_fd(), libc::F_GET_SEALS) };
    if !m.is_file()
        || m.uid() != 0
        || m.nlink() != 0
        || m.len() > MAX_TOKEN
        || seals < 0
        || seals & SEALS != SEALS
    {
        return Err(refused("Retained token lacks immutable root custody"));
    }
    Ok(())
}
fn seal(facts: &Token) -> io::Result<File> {
    let bytes = serde_json::to_vec(facts)?;
    seal_bytes(&bytes)
}
fn seal_bytes(bytes: &[u8]) -> io::Result<File> {
    root()?;
    if bytes.len() as u64 > MAX_TOKEN {
        return Err(refused("Retained token too large"));
    }
    let raw = unsafe {
        libc::memfd_create(
            c"tilo-retained-io".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut f = unsafe { File::from_raw_fd(raw) };
    f.write_all(bytes)?;
    f.rewind()?;
    if unsafe { libc::fcntl(f.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(f)
}
/// Signed shim only: sealed root token, actual namespace/source identity and
/// ambient descriptor closure precede credential drop. Landlock must precede the final credential transition.
pub(crate) fn prepare(keep: &[i32], command: &[String]) -> io::Result<RetainedIoCredentials> {
    root()?;
    if keep.iter().any(|fd| {
        [
            TOKEN_FD,
            SOURCE_FD,
            REMOVAL_DIRECTORY_FD,
            REMOVAL_JOURNAL_FD,
            SOURCE_DATA_FD,
            NATIVE_OWNER_FD,
            SELECTED_REPLACEMENT_PERMIT_FD,
            NATIVE_HISTORY_VERIFIER_FD,
        ]
        .contains(fd)
    }) {
        return Err(refused("Retained authority descriptor cannot reach app"));
    }
    let raw = unsafe { libc::fcntl(TOKEN_FD, libc::F_DUPFD_CLOEXEC, 128) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut f = unsafe { File::from_raw_fd(raw) };
    check_token(&f)?;
    f.rewind()?;
    let mut bytes = Vec::new();
    f.take(MAX_TOKEN + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_TOKEN {
        return Err(refused("Retained token too large"));
    }
    let facts: Token = serde_json::from_slice(&bytes)?;
    facts.validate()?;
    let expected = if facts.directory {
        super::RETAINED_HOME_IO_WORKER_ARGUMENT
    } else {
        super::RETAINED_IO_WORKER_ARGUMENT
    };
    if command.len() != 2 || command.get(1).map(String::as_str) != Some(expected) {
        return Err(refused("Retained worker mode differs from sealed source"));
    }
    for ((name, expected), actual) in NAMESPACES.iter().zip(&facts.namespaces).zip(namespaces()?) {
        if (*name == "user" && actual != *expected) || (*name != "user" && actual == *expected) {
            return Err(refused("Retained private namespace differs"));
        }
    }
    let source = File::options()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(SOURCE_PATH)?;
    let m = source.metadata()?;
    if (m.dev(), m.ino()) != facts.source || m.is_dir() != facts.directory {
        return Err(refused("Retained projection differs"));
    }
    let verifier = if facts.native_history_verifier_sha256.is_some() {
        let raw = unsafe { libc::fcntl(NATIVE_HISTORY_VERIFIER_FD, libc::F_DUPFD_CLOEXEC, 128) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        Some(unsafe { File::from_raw_fd(raw) })
    } else {
        if unsafe { libc::fcntl(NATIVE_HISTORY_VERIFIER_FD, libc::F_GETFD) } >= 0
            || io::Error::last_os_error().raw_os_error() != Some(libc::EBADF)
        {
            return Err(refused("Unexpected native history descriptor"));
        }
        None
    };
    check_history_verifier(
        verifier.as_ref(),
        facts.native_history_verifier_sha256,
        facts.native_owner_sha256,
    )?;
    check_selected_replacement(&facts)?;
    prepare_removal(&facts, source, keep)?;
    Ok(RetainedIoCredentials {
        uid: facts.uid,
        gid: facts.gid,
        groups: facts.groups,
    })
}

fn check_selected_replacement(facts: &Token) -> io::Result<()> {
    match facts.selected_replacement_sha256 {
        Some(expected) => {
            let raw =
                unsafe { libc::fcntl(SELECTED_REPLACEMENT_PERMIT_FD, libc::F_DUPFD_CLOEXEC, 128) };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            let permit = unsafe { File::from_raw_fd(raw) };
            let owner = facts
                .native_owner_sha256
                .ok_or_else(|| refused("Native source owner absent"))?;
            if replacement_digest(&permit, facts.source, owner)? != expected {
                return Err(refused(
                    "Selected replacement permit differs from source token",
                ));
            }
        }
        None => {
            if unsafe { libc::fcntl(SELECTED_REPLACEMENT_PERMIT_FD, libc::F_GETFD) } >= 0
                || io::Error::last_os_error().raw_os_error() != Some(libc::EBADF)
            {
                return Err(refused("Unexpected selected replacement descriptor"));
            }
        }
    }
    Ok(())
}

fn prepare_removal(facts: &Token, source: File, keep: &[i32]) -> io::Result<()> {
    if let Some(removal) = &facts.removal {
        removal::prepare(removal)?;
        let raw = unsafe { libc::fcntl(NATIVE_OWNER_FD, libc::F_DUPFD_CLOEXEC, 128) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut owner = unsafe { File::from_raw_fd(raw) };
        check_token(&owner)?;
        owner.rewind()?;
        let mut key = Vec::new();
        owner.take(33).read_to_end(&mut key)?;
        if key.as_slice()
            != facts
                .native_owner_sha256
                .as_ref()
                .ok_or_else(|| refused("Native owner absent"))?
                .as_slice()
        {
            return Err(refused("Native owner differs from root-sealed source"));
        }
        let raw = unsafe { libc::fcntl(SOURCE_DATA_FD, libc::F_GETFL) };
        if raw < 0 || raw & libc::O_PATH == 0 {
            return Err(refused("Source derivative is not metadata-only"));
        }
        let raw = unsafe { libc::fcntl(SOURCE_DATA_FD, libc::F_DUPFD_CLOEXEC, 128) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let original = unsafe { File::from_raw_fd(raw) };
        let m = original.metadata()?;
        if (m.dev(), m.ino()) != facts.source || !m.is_dir() {
            return Err(refused("Source derivative differs"));
        }
        drop(original);
        drop(source);
        let mut retained = vec![
            REMOVAL_DIRECTORY_FD,
            REMOVAL_JOURNAL_FD,
            SOURCE_DATA_FD,
            NATIVE_OWNER_FD,
        ];
        if facts.selected_replacement_sha256.is_some() {
            retained.push(SELECTED_REPLACEMENT_PERMIT_FD);
        }
        if facts.native_history_verifier_sha256.is_some() {
            retained.push(NATIVE_HISTORY_VERIFIER_FD);
        }

        crate::landlock_shim::close_ambient_fds(&retained)?;
    } else {
        drop(source);
        crate::landlock_shim::close_ambient_fds(keep)?;
    }
    Ok(())
}

/// Called only after mandatory Landlock has been independently accepted.
pub(crate) fn drop_after_confinement(facts: RetainedIoCredentials) -> io::Result<()> {
    let RetainedIoCredentials { uid, gid, groups } = facts;
    super::capture::original::credentials::drop_authenticated(uid, gid, &groups)
}

/// Authenticate immutable anonymous root custody and exact source locus before
/// transferring this descriptor across credential drop. Parsing alone grants nothing.
fn replacement_digest(permit: &File, source: (u64, u64), owner: [u8; 32]) -> io::Result<[u8; 32]> {
    check_token(permit)?;
    let metadata = permit.metadata()?;
    if metadata.len() == 0 || metadata.len() > MAX_PERMIT_BYTES as u64 {
        return Err(refused("Selected replacement permit exceeds limit"));
    }
    // Positional reads leave the retained descriptor's offset untouched.
    let length = usize::try_from(metadata.len()).map_err(io::Error::other)?;
    let mut bytes = vec![0; length];
    permit.read_exact_at(&mut bytes, 0)?;
    let record = SelectedReplacementPermitRecord::decode(&bytes)?;
    if record.source_root != [source.0, source.1] || record.source_owner != owner {
        return Err(refused("Selected replacement source locus differs"));
    }
    Ok(Sha256::digest(bytes).into())
}

#[cfg(all(test, target_os = "linux"))]
mod selected_permit_tests {
    use super::*;
    #[test]
    fn source_bootstrap_fences_old_and_future_token_semantics() {
        let mut token = Token {
            schema: 2,
            purpose: "retained-source-io".into(),
            context_sha256: [1; 32],
            uid: 1000,
            gid: 1000,
            groups: vec![1000],
            target_uid: 1001,
            source: (2, 3),
            directory: false,
            namespaces: vec![(4, 5); NAMESPACES.len()],
            removal: None,
            native_owner_sha256: None,
            selected_replacement_sha256: None,
            native_history_verifier_sha256: None,
        };
        token.validate().unwrap();
        for schema in [0, 1, 3] {
            token.schema = schema;
            assert!(token.validate().is_err());
        }
        token.schema = 2;
        token.selected_replacement_sha256 = Some([3; 32]);
        assert!(
            token.validate().is_err(),
            "selected permit cannot accompany a plain leaf"
        );
    }

    #[test]
    fn actual_sealed_permit_digest_refuses_wrong_source_owner_and_nonsealed_file() {
        tilo_test_env::require_capability!(
            Chown,
            tilo_test_env::switch_identity(65534),
            "the sealed descriptor fixture is owned by root and another user"
        );
        let record = SelectedReplacementPermitRecord {
            schema: 2,
            purpose: runtime_portal::retained_replacement::SelectedReplacementPurpose::AtomicSave,
            source_owner: [7; 32],
            source_operation: "c62f9a00-cbf3-40d9-9f2a-1ead27b2a43f".into(),
            person: [8; 32],
            personal_room: "a7956913-f8dd-4e83-9b02-dd637c7cd522".into(),
            editor_request_id: "b5b95a69-7dbd-4c9c-b0e1-b5e79f3c0bc8".into(),
            editor_operation: "3fa5d30e-79b8-4bb4-839b-cfc18dfc2c24".into(),
            editor_context_sha256: [9; 32],
            package_sha256: "ab".repeat(32),
            source_root: [12, 34],
            parent: [12, 35],
            selected: [12, 36],
            components: vec![vec![0xff, b'x']],
        };
        let bytes = record.encode().unwrap();
        let sealed = seal_bytes(&bytes).unwrap();
        assert_eq!(
            replacement_digest(&sealed, (12, 34), [7; 32]).unwrap(),
            <[u8; 32]>::from(Sha256::digest(&bytes))
        );
        assert!(replacement_digest(&sealed, (12, 99), [7; 32]).is_err());
        assert!(replacement_digest(&sealed, (12, 34), [6; 32]).is_err());
        let mut unsealed = tempfile::tempfile().unwrap();
        unsealed.write_all(&bytes).unwrap();
        assert!(replacement_digest(&unsealed, (12, 34), [7; 32]).is_err());
        let bad = seal_bytes(b"{}").unwrap();
        assert!(replacement_digest(&bad, (12, 34), [7; 32]).is_err());
    }
}

fn read_history_verifier(
    file: &File,
) -> io::Result<runtime_portal::retained_role::NativeHistoryVerifierRecord> {
    check_token(file)?;
    if file.metadata()?.len() == 0 || file.metadata()?.len() > 4096 {
        return Err(refused("History verifier exceeds its closed bound"));
    }
    // Positioned reads leave the shared sealed descriptor offset untouched.
    let length = usize::try_from(file.metadata()?.len()).map_err(io::Error::other)?;
    let mut bytes = vec![0; length];
    file.read_exact_at(&mut bytes, 0)?;
    runtime_portal::retained_role::NativeHistoryVerifierRecord::decode(&bytes)
}
fn check_history_verifier(
    file: Option<&File>,
    expected: Option<[u8; 32]>,
    owner: Option<[u8; 32]>,
) -> io::Result<()> {
    match (file, expected) {
        (None, None) => Ok(()),
        (Some(file), Some(expected)) => {
            let claims = read_history_verifier(file)?;
            if Some(claims.source_owner) != owner || claims.sha256()? != expected {
                return Err(refused("History verifier differs from sealed admission"));
            }
            Ok(())
        }
        _ => Err(refused("History verifier presence changed")),
    }
}

#[cfg(test)]
mod history_verifier_tests {
    use super::*;

    #[test]
    fn history_verifier_rejects_mutable_named_storage() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"{}").unwrap();
        assert!(read_history_verifier(&file).is_err());
        assert!(check_history_verifier(Some(&file), None, None).is_err());
        assert!(check_history_verifier(None, Some([1; 32]), Some([2; 32])).is_err());
        assert!(check_history_verifier(None, None, None).is_ok());
    }

    #[cfg(all(target_os = "linux", feature = "managed-tree-test"))]
    #[test]
    fn root_sealed_verifier_binds_exact_source_and_certified_context() {
        root().expect("owned root-native qualification is required");
        let claims = runtime_portal::retained_role::NativeHistoryVerifierRecord {
            schema: 1,
            source_owner: [1; 32],
            source_operation: "ae76c85a-a998-482d-b108-9235f9bfd69c".into(),
            person: [2; 32],
            personal_room: "a18ad342-9994-47dc-ac96-6b93f28e2b99".into(),
            device: [3; 32],
        };
        let file = seal_bytes(&claims.encode().unwrap()).unwrap();
        let digest = claims.sha256().unwrap();
        assert!(check_history_verifier(Some(&file), Some(digest), Some([1; 32])).is_ok());
        assert!(check_history_verifier(Some(&file), Some(digest), Some([4; 32])).is_err());
        let mut other_room = claims.clone();
        other_room.personal_room = "a18ad342-9994-47dc-ac96-6b93f28e2b98".into();
        let other = seal_bytes(&other_room.encode().unwrap()).unwrap();
        assert!(check_history_verifier(Some(&other), Some(digest), Some([1; 32])).is_err());
        let mut other_device = claims.clone();
        other_device.device = [5; 32];
        let other = seal_bytes(&other_device.encode().unwrap()).unwrap();
        assert!(check_history_verifier(Some(&other), Some(digest), Some([1; 32])).is_err());
        let oversized = seal_bytes(&vec![b'x'; 4097]).unwrap();
        assert!(read_history_verifier(&oversized).is_err());
    }
}
