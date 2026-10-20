//! Target-user custody bridge for encrypted installer preparation.
//! No window is launched, and no identity is activated by the helper.
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const PACKAGE: &str = "prepared-setup.sealed";
const RECEIPT: &str = "activated.json";
const MAX_PACKAGE: u64 = 40 * 1024 * 1024;
const MAX_RESULT: usize = 32 * 1024;

#[derive(PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u32,
    digest: [u8; 32],
    identity_public_key: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_identity: Option<tilo_identity_host::enrolled_transport::CurrentDeviceDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    portable_home: Option<PortableHome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    portable_flatpak: Option<PortableApplications>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    browser: Option<BrowserDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service_sessions: Option<ServiceSessions>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Imported {
    pub identity_public_key: [u8; 32],
    pub account_count: usize,
    #[serde(default)]
    pub current_identity: Option<tilo_identity_host::enrolled_transport::CurrentDeviceDeclaration>,
    #[serde(skip)]
    pub resolved_current: Option<tilo_identity_host::enrolled_transport::ResolvedCurrentIdentity>,
    #[serde(default)]
    pub device_label: String,
    #[serde(default)]
    pub portable_home: Option<PortableHome>,
    #[serde(default)]
    pub portable_flatpak: Option<PortableApplications>,
    #[serde(default)]
    pub browser: Option<BrowserDeclaration>,
    #[serde(default)]
    pub service_sessions: Option<ServiceSessions>,
}

/// Public selection metadata; credential bytes remain in imported private custody.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceSessions {
    pub schema: u32,
    pub sessions: Vec<SelectedSession>,
    pub sealed_bytes: u64,
    pub ciphertext_sha256: [u8; 32],
}
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SelectedSession {
    pub id: String,
    pub service: browser_continuity::sessions::Service,
}
impl ServiceSessions {
    fn valid(&self) -> bool {
        if self.schema != 1
            || self.sessions.is_empty()
            || self.sessions.len() > 2
            || self.sealed_bytes == 0
            || self.sealed_bytes > 8 * 1024 * 1024 + 1024
        {
            return false;
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut services = std::collections::BTreeSet::new();
        self.sessions.iter().all(|session| {
            browser_continuity::sessions::valid_id(&session.id)
                && ids.insert(&session.id)
                && services.insert(session.service.as_str())
        })
    }
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PortableHome {
    pub schema: u32,
    pub setup_binding: String,
    pub selection: String,
    #[serde(default)]
    pub applications: bool,
}
impl PortableHome {
    pub(crate) fn valid(&self) -> bool {
        self.schema == 1
            && self.selection == "full_home"
            && !self.setup_binding.is_empty()
            && self.setup_binding.len() <= 256
            && !self.setup_binding.chars().any(char::is_control)
    }
}

/// The bounded public declaration authenticated by the canonical setup importer.
/// Ciphertext is not imported, trusted or activated merely by storing this value.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PortableApplications {
    pub schema: u32,
    pub operation: String,
    pub purpose: String,
    pub filename: String,
    pub setup_binding: String,
    pub sealed_bytes: u64,
    pub plaintext_bytes: u64,
    pub entries: usize,
    pub ciphertext_sha256: [u8; 32],
    pub manifest_sha256: [u8; 32],
    pub limits: ApplicationLimits,
}
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplicationLimits {
    pub plaintext_bytes: u64,
    pub entries: usize,
    pub metadata_bytes: usize,
}
impl PortableApplications {
    pub(crate) fn valid(&self) -> bool {
        self.schema == 1
            && self.purpose == "flatpak_applications"
            && uuid::Uuid::parse_str(&self.operation).is_ok()
            && self.filename == format!("applications-{}.sealed", self.operation)
            && !self.setup_binding.is_empty()
            && self.setup_binding.len() <= 256
            && !self.setup_binding.chars().any(char::is_control)
            && self.sealed_bytes != 0
            && self.plaintext_bytes != 0
            && self.plaintext_bytes <= self.limits.plaintext_bytes
            && self.entries != 0
            && self.entries <= self.limits.entries
            && self.limits.entries <= 1_000_000
            && self.limits.metadata_bytes != 0
            && self.limits.metadata_bytes <= 512 * 1024 * 1024
    }
}

/// Authenticated, separately selected browser domains; grants no home access.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserDeclaration {
    pub plan: browser_continuity::Plan,
    pub payload: BrowserPayload,
}
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserPayload {
    pub schema: u32,
    pub operation: String,
    pub purpose: String,
    pub filename: String,
    pub setup_binding: String,
    pub sealed_bytes: u64,
    pub plaintext_bytes: u64,
    pub entries: usize,
    pub ciphertext_sha256: [u8; 32],
    pub plan_sha256: [u8; 32],
    pub limits: ApplicationLimits,
}
impl BrowserDeclaration {
    pub(crate) fn valid(&self) -> bool {
        let payload = &self.payload;
        self.plan.encode().is_ok_and(|bytes| {
            !self.plan.profiles.is_empty()
                && self.plan.setup_binding == payload.setup_binding
                && <[u8; 32]>::from(Sha256::digest(bytes)) == payload.plan_sha256
        }) && payload.schema == 1
            && payload.purpose == "browser_profiles"
            && uuid::Uuid::parse_str(&payload.operation)
                .is_ok_and(|id| id.to_string() == payload.operation)
            && payload.filename == format!("browser-{}.sealed", payload.operation)
            && payload.sealed_bytes > 0
            && payload.plaintext_bytes > 0
            && payload.plaintext_bytes <= payload.limits.plaintext_bytes
            && payload.limits.plaintext_bytes <= 129 * 1024 * 1024
            && payload.entries > 0
            && payload.entries <= payload.limits.entries
            && payload.limits.entries <= 128
            && payload.limits.metadata_bytes > 0
            && payload.limits.metadata_bytes <= 1024 * 1024
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TransportReceipt {
    schema: u32,
    sealed_bytes: u64,
    ciphertext_sha256: String,
}

pub(crate) struct Pending {
    home: PathBuf,
    directory: PathBuf,
    pin: File,
    digest: [u8; 32],
}
pub(crate) struct Worker {
    command: bounded_command::AsyncCommand,
    _quarantine: tempfile::TempDir,
    identity_directory: PathBuf,
}
fn current_system_root() -> io::Result<PathBuf> {
    let paths = crate::firstboot::collaboration_identity_paths();
    paths
        .device_cert
        .parent()
        .and_then(Path::parent)
        .map(Path::to_owned)
        .ok_or_else(refusal)
}
fn refusal() -> io::Error {
    io::Error::other("Prepared setup is unavailable or changed")
}
fn current_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}
fn child(directory: &File, name: &str, flags: i32, mode: u32) -> io::Result<File> {
    let name = std::ffi::CString::new(name).map_err(|_| refusal())?;
    // SAFETY: name is terminated, directory is a live directory descriptor.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            mode,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returned a newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn private(file: &File, directory: bool) -> io::Result<()> {
    let m = file.metadata()?;
    if m.uid() != current_uid()
        || m.mode() & 0o077 != 0
        || (directory && !m.is_dir())
        || (!directory && !m.is_file())
    {
        return Err(refusal());
    }
    Ok(())
}
fn hash(directory: &File) -> io::Result<[u8; 32]> {
    let file = child(directory, PACKAGE, libc::O_RDONLY, 0)?;
    private(&file, false)?;
    let before = file.metadata()?;
    if before.len() > MAX_PACKAGE {
        return Err(refusal());
    }
    let mut hasher = Sha256::new();
    let mut input = (&file).take(MAX_PACKAGE + 1);
    let mut copied = 0u64;
    let mut buffer = [0u8; 8192];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(buffer.get(..count).ok_or_else(refusal)?);
        copied = copied
            .checked_add(u64::try_from(count).map_err(|_| refusal())?)
            .ok_or_else(refusal)?;
    }
    let after = file.metadata()?;
    if copied != before.len()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(refusal());
    }
    Ok(hasher.finalize().into())
}
fn state_root(home: &Path) -> PathBuf {
    home.join(".local/state/tilo/prepared-setup")
}
/// A bad object is pending too: detection cannot erase a failed preparation.
pub(crate) fn pending_at(home: &Path) -> bool {
    match std::fs::symlink_metadata(state_root(home)) {
        Ok(_) => true,
        Err(e) => e.kind() != io::ErrorKind::NotFound,
    }
}
pub(crate) fn session_pending(identity_dir: &Path) -> bool {
    let Some(home) = session_home() else {
        return false;
    };
    if !pending_at(&home) {
        return false;
    }
    Pending::open(&home)
        .and_then(|pending| {
            if !pending.completed(identity_dir)? {
                return Ok(true);
            }
            let Some(declaration) = pending.browser(identity_dir)? else {
                return Ok(false);
            };
            let browser = crate::onboarding_browser::Pending::open(
                home.clone(),
                identity_dir.to_path_buf(),
                declaration,
            )?;
            crate::shell::launcher::set_browser_continuity(
                browser.verified_projection().map(Path::to_owned),
            );
            Ok(browser.pending())
        })
        .unwrap_or(true)
}
pub(crate) fn session_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}
impl Pending {
    pub(crate) fn open(home: &Path) -> io::Result<Self> {
        let home = home.canonicalize()?;
        let mut pin = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&home)?;
        if pin.metadata()?.uid() != current_uid() {
            return Err(refusal());
        }
        tilo_files::native::atomic::StorageDirectory::from_file(pin.try_clone()?)?
            .validate_namespace_control()?;
        for segment in [".local", "state", "tilo", "prepared-setup"] {
            pin = child(&pin, segment, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
            if segment == "prepared-setup" {
                private(&pin, true)?;
            } else {
                tilo_files::native::atomic::StorageDirectory::from_file(pin.try_clone()?)?
                    .validate_namespace_control()?;
            }
        }
        let digest = hash(&pin)?;
        let file = child(&pin, "receipt.json", libc::O_RDONLY, 0)?;
        private(&file, false)?;
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err(refusal());
        }
        let receipt: TransportReceipt = serde_json::from_slice(&bytes).map_err(|_| refusal())?;
        let package = child(&pin, PACKAGE, libc::O_RDONLY, 0)?;
        if receipt.schema != 1
            || receipt.sealed_bytes != package.metadata()?.len()
            || receipt.ciphertext_sha256 != hex::encode(digest)
        {
            return Err(refusal());
        }
        Ok(Self {
            home: home.clone(),
            directory: state_root(&home),
            pin,
            digest,
        })
    }
    pub(crate) fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub(crate) fn validate(&self) -> io::Result<()> {
        let fresh = Self::open(&self.home)?;
        let a = fresh.pin.metadata()?;
        let b = self.pin.metadata()?;
        let pinned_identity = (b.dev(), b.ino());
        if (a.dev(), a.ino()) != pinned_identity || fresh.digest != self.digest {
            return Err(refusal());
        }
        Ok(())
    }
    pub(crate) fn completed(&self, identity_dir: &Path) -> io::Result<bool> {
        let file = match child(&self.pin, RECEIPT, libc::O_RDONLY, 0) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        private(&file, false)?;
        let mut bytes = Vec::new();
        file.take(32769).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_RESULT {
            return Err(refusal());
        }
        let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|_| refusal())?;
        self.validate()?;
        let signer = if let Some(claim) = &receipt.current_identity {
            claim
                .resolve_current(&current_system_root()?, identity_dir)?
                .load()?
        } else {
            let active =
                runtime_identity::resolve_active_identity(identity_dir).ok_or_else(refusal)?;
            runtime_identity::load_from_path(active).map_err(|_| refusal())?
        };
        if receipt.version != 1
            || receipt.digest != self.digest
            || signer.group_public_key() != receipt.identity_public_key
            || receipt
                .service_sessions
                .as_ref()
                .is_some_and(|value| !value.valid())
        {
            return Err(refusal());
        }
        Ok(true)
    }
    pub(crate) fn prepare_targets(&self, identity_dir: &Path) -> io::Result<()> {
        self.validate()?;
        if !identity_dir.is_absolute() {
            return Err(refusal());
        }
        let mut pin = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open("/")?;
        let mut parent = None;
        for component in identity_dir.components() {
            let std::path::Component::Normal(name) = component else {
                if component == std::path::Component::RootDir {
                    continue;
                }
                return Err(refusal());
            };
            let name = name.to_str().ok_or_else(refusal)?;
            let next = match child(&pin, name, libc::O_RDONLY | libc::O_DIRECTORY, 0) {
                Ok(next) => next,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let encoded = std::ffi::CString::new(name).map_err(|_| refusal())?;
                    // SAFETY: descriptor is live and name terminated; mkdirat creates one child.
                    if unsafe { libc::mkdirat(pin.as_raw_fd(), encoded.as_ptr(), 0o700) } != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    pin.sync_all()?;
                    child(&pin, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?
                }
                Err(error) => return Err(error),
            };
            parent = Some(pin);
            pin = next;
        }
        private(&pin, true)?;
        private(&parent.ok_or_else(refusal)?, true)?;
        Ok(())
    }

    pub(crate) fn start(
        &self,
        identity_dir: &Path,
        password: &[u8],
        host_desktop: bool,
    ) -> io::Result<Worker> {
        if password.is_empty() || password.len() > 4096 {
            return Err(refusal());
        }
        self.validate()?;
        let scratch = tempfile::Builder::new()
            .prefix(".import-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(&self.directory)?;
        let config = identity_dir.parent().ok_or_else(refusal)?;
        let executable = match std::fs::symlink_metadata("/etc/tilo/prepared-setup") {
            Ok(_) => Path::new("/etc/tilo/prepared-setup"),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Path::new("/usr/libexec/tilo-prepared-setup")
            }
            Err(_) => return Err(refusal()),
        };
        let helper = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(executable)?;
        let metadata = helper.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
            || metadata.mode() & 0o111 == 0
        {
            return Err(refusal());
        }
        let mut command = Command::new(executable);
        command
            .arg("--package")
            .arg(self.directory.join(PACKAGE))
            .arg("--quarantine")
            .arg(scratch.path().join("contents"))
            .arg("--identity-dir")
            .arg(identity_dir)
            .arg("--config-root")
            .arg(config);
        if host_desktop {
            command
                .arg("--host-device-root")
                .arg(current_system_root()?);
        }
        let command = bounded_command::spawn_prepared(
            command,
            Some(password),
            MAX_RESULT,
            Duration::from_secs(120),
        )
        .map_err(|_| refusal())?;
        Ok(Worker {
            command,
            _quarantine: scratch,
            identity_directory: identity_dir.to_owned(),
        })
    }
    pub(crate) fn portable_home(&self, identity_dir: &Path) -> io::Result<Option<PortableHome>> {
        if !self.completed(identity_dir)? {
            return Err(refusal());
        }
        let file = child(&self.pin, RECEIPT, libc::O_RDONLY, 0)?;
        private(&file, false)?;
        let mut bytes = Vec::new();
        file.take(32769).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_RESULT {
            return Err(refusal());
        }
        let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|_| refusal())?;
        if receipt
            .portable_home
            .as_ref()
            .is_some_and(|home| !home.valid())
        {
            return Err(refusal());
        }
        Ok(receipt.portable_home)
    }
    pub(crate) fn portable_applications(
        &self,
        identity_dir: &Path,
    ) -> io::Result<Option<PortableApplications>> {
        if !self.completed(identity_dir)? {
            return Err(refusal());
        }
        let file = child(&self.pin, RECEIPT, libc::O_RDONLY, 0)?;
        private(&file, false)?;
        let mut bytes = Vec::new();
        file.take(32769).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_RESULT {
            return Err(refusal());
        }
        let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|_| refusal())?;
        if receipt
            .portable_flatpak
            .as_ref()
            .is_some_and(|apps| !apps.valid())
        {
            return Err(refusal());
        }
        Ok(receipt.portable_flatpak)
    }
    pub(crate) fn browser(&self, identity_dir: &Path) -> io::Result<Option<BrowserDeclaration>> {
        if !self.completed(identity_dir)? {
            return Err(refusal());
        }
        let file = child(&self.pin, RECEIPT, libc::O_RDONLY, 0)?;
        private(&file, false)?;
        let mut bytes = Vec::new();
        file.take(32769).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_RESULT {
            return Err(refusal());
        }
        let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|_| refusal())?;
        if receipt.browser.as_ref().is_some_and(|browser| {
            !browser.valid() || browser.plan.identity_public_key != receipt.identity_public_key
        }) {
            return Err(refusal());
        }
        Ok(receipt.browser)
    }
    pub(crate) fn acknowledge(
        &self,
        public_key: [u8; 32],
        portable_home: Option<PortableHome>,
        portable_flatpak: Option<PortableApplications>,
        browser: Option<BrowserDeclaration>,
        service_sessions: Option<ServiceSessions>,
        current_identity: Option<tilo_identity_host::enrolled_transport::CurrentDeviceDeclaration>,
    ) -> io::Result<()> {
        self.validate()?;
        if service_sessions
            .as_ref()
            .is_some_and(|value| !value.valid())
            || portable_home.as_ref().is_some_and(|home| !home.valid())
            || portable_flatpak.as_ref().is_some_and(|apps| !apps.valid())
            || browser
                .as_ref()
                .is_some_and(|value| !value.valid() || value.plan.identity_public_key != public_key)
        {
            return Err(refusal());
        }
        let receipt = Receipt {
            version: 1,
            digest: self.digest,
            identity_public_key: public_key,
            portable_home,
            portable_flatpak,
            browser,
            service_sessions,
            current_identity,
        };
        let bytes = serde_json::to_vec(&receipt).map_err(|_| refusal())?;
        if bytes.len() > MAX_RESULT {
            return Err(refusal());
        }
        let receipt_path = self.directory.join(RECEIPT);
        match child(&self.pin, RECEIPT, libc::O_RDONLY, 0) {
            Ok(file) => {
                private(&file, false)?;
                let mut existing = Vec::new();
                file.take(32769).read_to_end(&mut existing)?;
                let saved: Receipt = serde_json::from_slice(&existing).map_err(|_| refusal())?;
                if saved != receipt {
                    return Err(refusal());
                }
                return self.pin.sync_all();
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        let mut staged = tempfile::NamedTempFile::new_in(&self.directory)?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        self.validate()?;
        staged
            .persist_noclobber(receipt_path)
            .map_err(|e| e.error)?;
        self.pin.sync_all()?;
        Ok(())
    }
}
impl Imported {
    pub(crate) fn admit(bytes: &[u8], identity_directory: &Path) -> io::Result<Self> {
        let mut imported = Self::decode(bytes)?;
        if let Some(claim) = &imported.current_identity {
            if claim.public_key() != imported.identity_public_key {
                return Err(refusal());
            }
            imported.resolved_current =
                Some(claim.resolve_current(&current_system_root()?, identity_directory)?);
        }
        Ok(imported)
    }
    fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > MAX_RESULT {
            return Err(refusal());
        }
        let value: Imported = serde_json::from_slice(bytes).map_err(|_| refusal())?;
        if value
            .service_sessions
            .as_ref()
            .is_some_and(|sessions| !sessions.valid())
            || value.browser.as_ref().is_some_and(|browser| {
                !browser.valid() || browser.plan.identity_public_key != value.identity_public_key
            })
            || value.account_count > 64
            || value
                .portable_flatpak
                .as_ref()
                .is_some_and(|apps| !apps.valid())
            || value
                .portable_home
                .as_ref()
                .is_some_and(|home| !home.valid())
        {
            return Err(refusal());
        }
        Ok(value)
    }
}
impl Worker {
    pub(crate) fn poll(&mut self) -> Option<io::Result<Imported>> {
        self.command.try_wait().map(|result| {
            let output = result.map_err(|_| refusal())?;
            if !output.status.success() {
                return Err(refusal());
            }
            Imported::admit(&output.stdout, &self.identity_directory)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{
        ffi::OsStrExt,
        fs::{PermissionsExt, symlink},
    };
    fn fixture() -> tempfile::TempDir {
        let home = tempfile::tempdir().expect("home");
        let root = state_root(home.path());
        std::fs::create_dir_all(&root).expect("state");
        for path in [
            home.path().join(".local"),
            home.path().join(".local/state"),
            home.path().join(".local/state/tilo"),
            root.clone(),
        ] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .expect("private");
        }
        let package = root.join(PACKAGE);
        std::fs::write(&package, b"fixture ciphertext").expect("ciphertext");
        std::fs::set_permissions(&package, std::fs::Permissions::from_mode(0o600))
            .expect("private package");
        let receipt = serde_json::json!({"schema":1,"sealed_bytes":18,"ciphertext_sha256":hex::encode(Sha256::digest(b"fixture ciphertext"))});
        let path = root.join("receipt.json");
        std::fs::write(&path, serde_json::to_vec(&receipt).expect("receipt"))
            .expect("receipt file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("private receipt");
        home
    }
    fn selected_sessions() -> ServiceSessions {
        ServiceSessions {
            schema: 1,
            sessions: vec![SelectedSession {
                id: "selected-session".into(),
                service: browser_continuity::sessions::Service::Slack,
            }],
            sealed_bytes: 1024,
            ciphertext_sha256: [3; 32],
        }
    }
    #[test]
    fn helper_service_sessions_accept_legacy_and_refuse_malformed_public_selection() {
        let key = [7u8; 32];
        let mut value = serde_json::json!({"identity_public_key": key, "account_count": 1});
        assert!(
            Imported::decode(&serde_json::to_vec(&value).unwrap())
                .unwrap()
                .service_sessions
                .is_none()
        );
        let selected = selected_sessions();
        value["service_sessions"] = serde_json::to_value(&selected).unwrap();
        assert!(
            Imported::decode(&serde_json::to_vec(&value).unwrap())
                .unwrap()
                .service_sessions
                .is_some_and(|sessions| sessions == selected)
        );
        for invalid in [
            serde_json::json!({"schema": 2, "sessions": selected.sessions, "sealed_bytes": 1024, "ciphertext_sha256": key}),
            serde_json::json!({"schema": 1, "sessions": [{"id":"selected-session","service":"spotify"}], "sealed_bytes": 1024, "ciphertext_sha256": key}),
            serde_json::json!({"schema": 1, "sessions": [], "sealed_bytes": 1024, "ciphertext_sha256": key}),
        ] {
            value["service_sessions"] = invalid;
            assert!(Imported::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        let mut duplicate = selected_sessions();
        duplicate.sessions.push(SelectedSession {
            id: "other-session".into(),
            service: browser_continuity::sessions::Service::Slack,
        });
        assert!(!duplicate.valid());
        duplicate.sessions[1].service = browser_continuity::sessions::Service::Discord;
        duplicate.sessions[1].id = duplicate.sessions[0].id.clone();
        assert!(!duplicate.valid());
        let mut oversized = selected_sessions();
        oversized.sealed_bytes = 8 * 1024 * 1024 + 1025;
        assert!(!oversized.valid());
    }
    #[test]
    fn acknowledge_keeps_service_session_selection_and_refuses_changed_retry() {
        let home = fixture();
        let pending = Pending::open(home.path()).unwrap();
        let selected = selected_sessions();
        pending
            .acknowledge([7; 32], None, None, None, Some(selected.clone()), None)
            .unwrap();
        pending
            .acknowledge([7; 32], None, None, None, Some(selected.clone()), None)
            .unwrap();
        let bytes = std::fs::read(state_root(home.path()).join(RECEIPT)).unwrap();
        let stored: Receipt = serde_json::from_slice(&bytes).unwrap();
        assert!(
            stored
                .service_sessions
                .is_some_and(|sessions| sessions == selected)
        );
        assert!(
            pending
                .acknowledge([7; 32], None, None, None, None, None)
                .is_err()
        );
        assert_eq!(
            std::fs::read(state_root(home.path()).join(RECEIPT)).unwrap(),
            bytes
        );
    }

    #[test]
    fn prepared_grant_accepts_owned_readable_ancestors_but_keeps_carrier_private() {
        let home = fixture();
        let ancestors = [
            home.path().join(".local"),
            home.path().join(".local/state"),
            home.path().join(".local/state/tilo"),
        ];
        for path in &ancestors {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let pending = Pending::open(home.path()).unwrap();
        pending.validate().unwrap();
        for path in &ancestors {
            assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o777, 0o755);
        }
        std::fs::set_permissions(&ancestors[0], std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(Pending::open(home.path()).is_err());
        std::fs::set_permissions(&ancestors[0], std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(
            state_root(home.path()),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(Pending::open(home.path()).is_err());
    }
    #[test]
    fn prepared_grant_binds_exact_delivered_bytes_and_rejects_parent_links() {
        let home = fixture();
        let pending = Pending::open(home.path()).expect("bound delivery");
        pending.validate().expect("unchanged");
        std::fs::write(state_root(home.path()).join(PACKAGE), b"changed ciphertext")
            .expect("mutation");
        assert!(pending.validate().is_err());
        assert!(pending_at(home.path()));
        let other = fixture();
        let actual = other.path().join("moved");
        std::fs::rename(other.path().join(".local"), &actual).expect("move");
        symlink(&actual, other.path().join(".local")).expect("link");
        assert!(Pending::open(other.path()).is_err());
    }
    #[test]
    fn missing_and_fifo_packages_stay_pending_and_never_block() {
        let home = fixture();
        let package = state_root(home.path()).join(PACKAGE);
        std::fs::remove_file(&package).expect("remove");
        assert!(pending_at(home.path()));
        assert!(Pending::open(home.path()).is_err());
        let name = std::ffi::CString::new(package.as_os_str().as_bytes()).expect("name");
        // SAFETY: name is terminated; this only creates a FIFO in the fixture.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(Pending::open(home.path()).is_err());
    }
    #[test]
    fn target_grant_creates_private_directories_and_refuses_public_existing_config() {
        let home = fixture();
        let pending = Pending::open(home.path()).expect("delivery");
        let identity = home.path().join(".config/tilo/identity");
        pending
            .prepare_targets(&identity)
            .expect("private new custody");
        assert_eq!(
            std::fs::metadata(&identity).expect("identity").mode() & 0o777,
            0o700
        );
        std::fs::set_permissions(
            identity.parent().expect("parent"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("public existing");
        assert!(pending.prepare_targets(&identity).is_err());
        assert_eq!(
            std::fs::metadata(identity.parent().expect("parent"))
                .expect("unchanged")
                .mode()
                & 0o777,
            0o755
        );
    }
}
