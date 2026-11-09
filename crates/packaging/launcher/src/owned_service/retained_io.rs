//! Root-native retained-source I/O projection for one fixed offline service.
//! This is distinct from immutable migration capture. No launch option or wire
//! credential claim can mint an admission; the root actor retains actual custody.
use serde::{Deserialize, Serialize};

use std::{
    fs::File,
    io::{self, Read, Seek, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    sync::{Arc, Mutex},
};

pub const SOURCE_PATH: &str = "/run/tilo/retained/source";
mod removal;
use super::refused;
use super::workspace;
pub use removal::RetainedNativeOwner;
pub const REMOVAL_DIRECTORY_FD: i32 = removal::FD;
pub const REMOVAL_JOURNAL_FD: i32 = removal::JOURNAL_FD;
pub const SOURCE_DATA_FD: i32 = removal::SOURCE_DATA_FD;
pub const NATIVE_OWNER_FD: i32 = 59;

const SOURCE_FD: RawFd = 44;
pub(crate) const TOKEN_FD: RawFd = 49;
const MAX_TOKEN: u64 = 1024 * 1024;
const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
const NAMESPACES: [&str; 5] = ["mnt", "pid", "net", "ipc", "user"];
const WORKSPACE: workspace::Layout = workspace::Layout {
    label: "Retained source",
    person: workspace::Person::Empty,
    root,
};
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
    /// Obtain the independently admitted original source credentials.
    fn source_credentials(&self) -> io::Result<RetainedIoCredentials>;
    /// Obtain the canonical retained native owner's identity.
    fn native_owner_sha256(&self) -> io::Result<[u8; 32]> {
        Err(refused(
            "Canonical retained native owner was not independently admitted",
        ))
    }
    /// Clone the independently admitted root-protected source namespace.
    fn removal_namespace(&self) -> io::Result<File> {
        Err(refused(
            "Root protected source namespace not independently admitted",
        ))
    }
    /// Retain namespace custody for recovery after the worker stops.
    fn retain_native_owner(&self, _owner: RetainedNativeOwner) -> io::Result<()> {
        Ok(())
    }
    /// Confirm that native namespace obligations have been retired.
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
    /// Refuses non-root callers, invalid or read-only sources, and revoked
    /// authority; propagates I/O errors.
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
        *held = Some({
            self.facts.validate()?;
            workspace::prepare(
                WORKSPACE,
                path,
                workspace::Owner {
                    uid: self.facts.uid,
                    gid: self.facts.gid,
                },
            )?
        });
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
        super::splice_custody(
            argv,
            &super::CustodySplice {
                token_flag: "--retained-io-fd",
                token_fd: TOKEN_FD,
                extra_caps: &[],
                traversal: &[
                    "/run/tilo",
                    "/run/tilo/retained",
                    "/etc",
                    "/etc/alternatives",
                    "/run/tilo-runtime",
                    "/var",
                    "/var/tilo-apps",
                    "/usr/share",
                    "/usr/share/X11",
                ],
                forbidden: &[
                    "--share-net",
                    "--unshare-user",
                    "--uid",
                    "--gid",
                    "--original-source-fd",
                ],
                changed: "Retained namespace plan changed",
                no_namespaces: "Retained private namespace missing",
                no_shim: "Retained I/O signed shim missing",
            },
        )
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
    prepare_removal(&facts, source, keep)?;
    Ok(RetainedIoCredentials {
        uid: facts.uid,
        gid: facts.gid,
        groups: facts.groups,
    })
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
        let retained = [
            REMOVAL_DIRECTORY_FD,
            REMOVAL_JOURNAL_FD,
            SOURCE_DATA_FD,
            NATIVE_OWNER_FD,
        ];
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
