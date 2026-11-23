//! Target-user ownership bridge for encrypted installer preparation.
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

pub use prepared_setup_model::{
    ApplicationsExpectation as PortableApplications, BrowserExpectation as BrowserPayload,
    BrowserHandoff as BrowserDeclaration, Limits as ApplicationLimits,
    PortableHomeDeclaration as PortableHome, SelectedSession,
    ServiceSessionsDeclaration as ServiceSessions, ThunderbirdImportReceipt,
};
/// Receipt emitted on stdout by `tilo-prepared-setup`.
pub type HelperReceipt = prepared_setup_model::PreparedSetupReceipt<
    tilo_identity::enrolled_transport::CurrentDeviceDeclaration,
>;

#[derive(PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u32,
    digest: [u8; 32],
    identity_public_key: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_identity: Option<tilo_identity::enrolled_transport::CurrentDeviceDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    portable_home: Option<PortableHome>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "prepared_setup_model::stored::deserialize_option_applications"
    )]
    portable_flatpak: Option<PortableApplications>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "prepared_setup_model::stored::deserialize_option_browser"
    )]
    browser: Option<BrowserDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service_sessions: Option<ServiceSessions>,
    /// What the helper actually imported from the selected mail profile
    /// artifacts. It carries opaque source IDs and counts only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mail: Vec<ThunderbirdImportReceipt>,
}

pub struct Imported {
    pub identity_public_key: [u8; 32],
    pub account_count: usize,
    pub current_identity: Option<tilo_identity::enrolled_transport::CurrentDeviceDeclaration>,
    pub resolved_current: Option<tilo_identity::enrolled_transport::ResolvedCurrentIdentity>,
    pub device_label: String,
    pub portable_home: Option<PortableHome>,
    pub portable_flatpak: Option<PortableApplications>,
    pub browser: Option<BrowserDeclaration>,
    pub service_sessions: Option<ServiceSessions>,
    pub mail: Vec<ThunderbirdImportReceipt>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TransportReceipt {
    schema: u32,
    sealed_bytes: u64,
    ciphertext_sha256: String,
}

pub struct Pending {
    home: PathBuf,
    directory: PathBuf,
    pin: File,
    digest: [u8; 32],
}
pub struct Worker {
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
fn forwarded_system_root(host_desktop: bool) -> io::Result<Option<PathBuf>> {
    host_desktop.then(current_system_root).transpose()
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
pub fn pending_at(home: &Path) -> bool {
    match std::fs::symlink_metadata(state_root(home)) {
        Ok(_) => true,
        Err(e) => e.kind() != io::ErrorKind::NotFound,
    }
}
pub fn session_pending(identity_dir: &Path) -> bool {
    let Some(home) = crate::identity::session_home() else {
        return false;
    };
    // A same-computer migration publishes its signed first-launch projection
    // beside the ordinary browser-continuity records. Attach it before the
    // prepared-setup check, which this route does not have.
    crate::browser::attach_deferred_continuity();
    #[cfg(target_os = "linux")]
    if crate::peer::Peer::pending_at(&home) {
        return true;
    }
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
            let browser = crate::browser::Pending::open(
                home.clone(),
                identity_dir.to_path_buf(),
                declaration,
            )?;
            crate::services::set_browser_continuity(
                browser.verified_projection().map(Path::to_owned),
            );
            Ok(browser.pending())
        })
        .unwrap_or(true)
}
/// Consume the first-boot unseal of a same-computer TPM handoff. The file is
/// private to this account in tmpfs and is removed whether or not the import
/// accepts it, so it cannot be replayed after first use.
pub fn take_tpm_secret() -> Option<zeroize::Zeroizing<Vec<u8>>> {
    use std::os::unix::fs::MetadataExt;
    let path = PathBuf::from(format!("/run/tilo-prepared-setup/{}.secret", current_uid()));
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.uid() != current_uid() || metadata.mode() & 0o077 != 0 {
        return None;
    }
    let secret = std::fs::read(&path).ok().map(zeroize::Zeroizing::new);
    let _ = std::fs::remove_file(&path);
    secret.filter(|value| !value.is_empty() && value.len() <= 4096)
}

impl Pending {
    pub fn open(home: &Path) -> io::Result<Self> {
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
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub fn validate(&self) -> io::Result<()> {
        let fresh = Self::open(&self.home)?;
        let a = fresh.pin.metadata()?;
        let b = self.pin.metadata()?;
        let pinned_identity = (b.dev(), b.ino());
        if (a.dev(), a.ino()) != pinned_identity || fresh.digest != self.digest {
            return Err(refusal());
        }
        Ok(())
    }
    pub fn completed(&self, identity_dir: &Path) -> io::Result<bool> {
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
                tilo_identity::store::resolve_active_identity(identity_dir).ok_or_else(refusal)?;
            tilo_identity::store::load_from_path(active).map_err(|_| refusal())?
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
    pub fn prepare_targets(&self, identity_dir: &Path) -> io::Result<()> {
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

    pub fn start(
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
            .arg(config)
            .arg("--target-home")
            .arg(&self.home);
        if let Some(root) = forwarded_system_root(host_desktop)? {
            command.arg("--host-device-root").arg(root);
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
    pub fn portable_home(&self, identity_dir: &Path) -> io::Result<Option<PortableHome>> {
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
    pub fn portable_applications(
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
    pub fn browser(&self, identity_dir: &Path) -> io::Result<Option<BrowserDeclaration>> {
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
    pub fn acknowledge(
        &self,
        public_key: [u8; 32],
        portable_home: Option<PortableHome>,
        portable_flatpak: Option<PortableApplications>,
        browser: Option<BrowserDeclaration>,
        service_sessions: Option<ServiceSessions>,
        current_identity: Option<tilo_identity::enrolled_transport::CurrentDeviceDeclaration>,
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
            mail: Vec::new(),
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
    pub fn admit(bytes: &[u8], identity_directory: &Path) -> io::Result<Self> {
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
        let receipt: HelperReceipt = serde_json::from_slice(bytes).map_err(|_| refusal())?;
        receipt.validate().map_err(|_| refusal())?;
        Ok(Self {
            identity_public_key: receipt.identity_public_key,
            account_count: receipt.account_count,
            current_identity: receipt.current_identity,
            resolved_current: None,
            device_label: receipt.device_label,
            portable_home: receipt.portable_home,
            portable_flatpak: receipt.portable_flatpak,
            browser: receipt.browser,
            service_sessions: receipt.service_sessions,
            mail: receipt.mail,
        })
    }
}
impl Worker {
    pub fn poll(&mut self) -> Option<io::Result<Imported>> {
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
            consumer_protocol: 1,
            sessions: vec![SelectedSession {
                id: "selected-session".into(),
                service: browser_continuity::sessions::Service::Slack,
            }],
            sealed_bytes: 1024,
            ciphertext_sha256: [3; 32],
        }
    }
    tilo_test_env::scenario! {
        fn a_prepared_setup_package_is_bound_to_its_private_delivery_and_acknowledged_once;

        phase helper_service_sessions_accept_legacy_and_refuse_malformed_public_selection;
        {
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

        phase acknowledge_keeps_service_session_selection_and_refuses_changed_retry;
        {
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

        phase prepared_grant_accepts_owned_readable_ancestors_but_keeps_carrier_private;
        {
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

        phase prepared_grant_binds_exact_delivered_bytes_and_rejects_parent_links;
        {
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

        phase missing_and_fifo_packages_stay_pending_and_never_block;
        {
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

        phase a_host_desktop_forwards_the_configured_system_root;
        {
            let root = tempfile::tempdir().unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_IDENTITY_ROOT", root.path());
            assert_eq!(
                forwarded_system_root(true).unwrap().as_deref(),
                Some(root.path())
            );
            assert_eq!(forwarded_system_root(false).unwrap(), None);
        }

        phase target_grant_creates_private_directories_and_refuses_public_existing_config;
        {
            let home = fixture();
            let pending = Pending::open(home.path()).expect("delivery");
            let identity = home.path().join(".config/tilo/identity");
            pending
                .prepare_targets(&identity)
                .expect("private new ownership");
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

    fn producer_wire_parts(
        key: [u8; 32],
    ) -> (
        [u8; 32],
        serde_json::Value,
        serde_json::Value,
        serde_json::Value,
        serde_json::Value,
    ) {
        let plan = browser_continuity::Plan::new(
            "setup-binding".into(),
            key,
            vec![
                browser_continuity::Profile::new(
                    "a".repeat(64),
                    browser_continuity::Provider::Chromium,
                    vec!["browser".into(), "Default".into()],
                    true,
                    false,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let plan_sha256: [u8; 32] = Sha256::digest(plan.encode().unwrap()).into();
        let apps_ciphertext = [4u8; 32];
        let apps_manifest = [5u8; 32];
        let browser_ciphertext = [6u8; 32];
        let sessions_ciphertext = [3u8; 32];
        let applications = serde_json::json!({
            "operation": "11111111-1111-4111-8111-111111111111",
            "filename": "applications-11111111-1111-4111-8111-111111111111.sealed",
            "setup_binding": "setup-binding",
            "sealed_bytes": 512,
            "plaintext_bytes": 256,
            "entries": 4,
            "ciphertext_sha256": apps_ciphertext,
            "limits": {
                "plaintext_bytes": 1_048_576,
                "entries": 128,
                "metadata_bytes": 65_536
            },
            "kind": {
                "consumer_protocol": 2,
                "manifest_sha256": apps_manifest
            }
        });
        let browser = serde_json::json!({
            "plan": plan,
            "payload": {
                "operation": "22222222-2222-4222-8222-222222222222",
                "filename": "browser-22222222-2222-4222-8222-222222222222.sealed",
                "setup_binding": "setup-binding",
                "sealed_bytes": 512,
                "plaintext_bytes": 256,
                "entries": 2,
                "ciphertext_sha256": browser_ciphertext,
                "limits": {
                    "plaintext_bytes": 1_048_576,
                    "entries": 64,
                    "metadata_bytes": 65_536
                },
                "kind": {
                    "plan_sha256": plan_sha256
                }
            }
        });
        let service_sessions = serde_json::json!({
            "schema": 1,
            "consumer_protocol": 2,
            "sessions": [{
                "id": "discord-session",
                "service": "discord"
            }],
            "sealed_bytes": 1024,
            "ciphertext_sha256": sessions_ciphertext
        });
        let full = serde_json::json!({
            "identity_public_key": key,
            "account_count": 1,
            "service_sessions": service_sessions,
            "device_label": "Ada's laptop",
            "portable_home": {
                "schema": 1,
                "setup_binding": "setup-binding",
                "selection": "full_home",
                "applications": true
            },
            "portable_flatpak": applications,
            "browser": browser,
            "mail": [{
                "accounts": [{
                    "source_id": "account-1",
                    "created": true,
                    "needs_reconnect": false,
                    "folders": [{
                        "source_id": "folder-1",
                        "messages": 12,
                        "bytes": 4096,
                        "local_only": false
                    }]
                }],
                "address_books": 1,
                "calendars": 2
            }]
        });
        (plan_sha256, applications, browser, service_sessions, full)
    }

    #[test]
    fn producer_receipt_with_portable_applications_browser_and_protocol_2_sessions_parses() {
        let key = [7u8; 32];
        let (plan_sha256, applications, browser, service_sessions, full) = producer_wire_parts(key);
        let apps_only = serde_json::json!({
            "identity_public_key": key,
            "account_count": 1,
            "portable_flatpak": applications,
        });
        let browser_only = serde_json::json!({
            "identity_public_key": key,
            "account_count": 1,
            "browser": browser,
        });
        let sessions_only = serde_json::json!({
            "identity_public_key": key,
            "account_count": 1,
            "service_sessions": service_sessions,
        });
        let apps_result = Imported::decode(&serde_json::to_vec(&apps_only).unwrap());
        let browser_result = Imported::decode(&serde_json::to_vec(&browser_only).unwrap());
        let sessions_result = Imported::decode(&serde_json::to_vec(&sessions_only).unwrap());
        let full_result = Imported::decode(&serde_json::to_vec(&full).unwrap());
        assert!(
            apps_result.is_ok()
                && browser_result.is_ok()
                && sessions_result.is_ok()
                && full_result.is_ok(),
            "apps={:?} browser={:?} sessions={:?} full={:?}",
            apps_result.err(),
            browser_result.err(),
            sessions_result.err(),
            full_result.err(),
        );
        let parsed = full_result.unwrap();
        assert_eq!(parsed.identity_public_key, key);
        assert_eq!(parsed.account_count, 1);
        assert_eq!(parsed.device_label, "Ada's laptop");
        assert_eq!(
            parsed
                .service_sessions
                .as_ref()
                .map(|sessions| sessions.consumer_protocol),
            Some(2)
        );
        assert_eq!(
            parsed
                .portable_flatpak
                .as_ref()
                .map(|apps| apps.kind.consumer_protocol),
            Some(2)
        );
        assert_eq!(
            parsed
                .browser
                .as_ref()
                .map(|handoff| handoff.payload.kind.plan_sha256),
            Some(plan_sha256)
        );
        assert_eq!(parsed.mail.len(), 1);
        assert_eq!(parsed.mail[0].accounts[0].source_id, "account-1");
    }

    #[test]
    fn producer_receipt_round_trips_and_refuses_unknown_or_flat_fields() {
        let key = [7u8; 32];
        let (_, _, _, _, full) = producer_wire_parts(key);
        let parsed = Imported::decode(&serde_json::to_vec(&full).unwrap()).unwrap();
        let home = fixture();
        let pending = Pending::open(home.path()).unwrap();
        pending
            .acknowledge(
                parsed.identity_public_key,
                parsed.portable_home.clone(),
                parsed.portable_flatpak.clone(),
                parsed.browser.clone(),
                parsed.service_sessions.clone(),
                parsed.current_identity.clone(),
            )
            .unwrap();
        let receipt = HelperReceipt {
            identity_public_key: parsed.identity_public_key,
            account_count: parsed.account_count,
            current_identity: parsed.current_identity,
            service_sessions: parsed.service_sessions,
            device_label: parsed.device_label,
            portable_home: parsed.portable_home,
            portable_flatpak: parsed.portable_flatpak,
            browser: parsed.browser,
            mail: parsed.mail,
        };
        let round_tripped = Imported::decode(&serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert_eq!(round_tripped.service_sessions, receipt.service_sessions);
        assert_eq!(round_tripped.portable_home, receipt.portable_home);
        assert_eq!(round_tripped.portable_flatpak, receipt.portable_flatpak);
        assert_eq!(round_tripped.browser, receipt.browser);
        assert_eq!(round_tripped.mail, receipt.mail);

        let mut with_unknown = full.clone();
        with_unknown["unexpected"] = serde_json::json!(true);
        assert!(Imported::decode(&serde_json::to_vec(&with_unknown).unwrap()).is_err());

        let mut bad_nested = full;
        bad_nested["portable_flatpak"]["kind"]["extra"] = serde_json::json!(1);
        assert!(Imported::decode(&serde_json::to_vec(&bad_nested).unwrap()).is_err());

        let apps_ciphertext = [4u8; 32];
        let apps_manifest = [5u8; 32];
        let flat_apps = serde_json::json!({
            "identity_public_key": key,
            "account_count": 1,
            "portable_flatpak": {
                "schema": 1,
                "operation": "11111111-1111-4111-8111-111111111111",
                "purpose": "flatpak_applications",
                "filename": "applications-11111111-1111-4111-8111-111111111111.sealed",
                "setup_binding": "setup-binding",
                "sealed_bytes": 512,
                "plaintext_bytes": 256,
                "entries": 4,
                "ciphertext_sha256": apps_ciphertext,
                "manifest_sha256": apps_manifest,
                "limits": {
                    "plaintext_bytes": 1_048_576,
                    "entries": 128,
                    "metadata_bytes": 65_536
                }
            }
        });
        assert!(Imported::decode(&serde_json::to_vec(&flat_apps).unwrap()).is_err());
    }

    fn write_private_file(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn fixture_with_active_identity() -> (tempfile::TempDir, PathBuf, [u8; 32], [u8; 32]) {
        let home = fixture();
        let identity = home.path().join(".config/tilo/identity");
        std::fs::create_dir_all(&identity).unwrap();
        for part in [".config", ".config/tilo", ".config/tilo/identity"] {
            std::fs::set_permissions(
                home.path().join(part),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        tilo_identity::provisioning::create_primary_identity(
            &identity,
            "Ada",
            [10, 40, 60],
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        tilo_identity::store::set_active_identity(&identity, "primary").unwrap();
        let signer = tilo_identity::store::load_from_path(identity.join("primary.json")).unwrap();
        let digest = Pending::open(home.path()).unwrap().digest();
        (home, identity, signer.group_public_key(), digest)
    }

    #[test]
    fn stored_mine_tip_flat_activated_json_loads_and_sets_session_pending() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(tilo_identity::store::FILE_SECRETS_ENV, "1");
        let (home, identity, key, digest) = fixture_with_active_identity();
        env.set("HOME", home.path());

        // Exact wire layout written by `mine-tip` (2026-10-27) in
        // `crates/session/shell/src/onboarding_prepared.rs` (`Receipt` +
        // `PortableApplications`, fields serialized in declaration order).
        let digest_json = serde_json::to_string(&digest).unwrap();
        let key_json = serde_json::to_string(&key).unwrap();
        let apps_cipher_json = serde_json::to_string(&[4u8; 32]).unwrap();
        let apps_manifest_json = serde_json::to_string(&[5u8; 32]).unwrap();
        let flat_activated = format!(
            concat!(
                r#"{{"version":1,"digest":{digest},"identity_public_key":{key},"#,
                r#""portable_flatpak":{{"schema":1,"#,
                r#""operation":"11111111-1111-4111-8111-111111111111","#,
                r#""purpose":"flatpak_applications","#,
                r#""filename":"applications-11111111-1111-4111-8111-111111111111.sealed","#,
                r#""setup_binding":"mine-tip-binding","#,
                r#""sealed_bytes":18,"plaintext_bytes":16,"entries":2,"#,
                r#""ciphertext_sha256":{cipher},"manifest_sha256":{manifest},"#,
                r#""limits":{{"plaintext_bytes":1048576,"entries":128,"metadata_bytes":65536}}}}}}"#
            ),
            digest = digest_json,
            key = key_json,
            cipher = apps_cipher_json,
            manifest = apps_manifest_json,
        );
        let activated_path = state_root(home.path()).join(RECEIPT);
        write_private_file(&activated_path, flat_activated.as_bytes());

        assert!(
            !session_pending(&identity),
            "a device with a 2026-10-27 flat activated.json must not re-enter onboarding"
        );
        let pending = Pending::open(home.path()).unwrap();
        assert!(pending.completed(&identity).unwrap());
        let loaded = pending.portable_applications(&identity).unwrap().unwrap();
        assert_eq!(loaded.kind.consumer_protocol, 1);
        assert_eq!(loaded.kind.manifest_sha256, [5; 32]);
        assert_eq!(loaded.operation, "11111111-1111-4111-8111-111111111111");

        // Idempotent retry against the flat stored copy succeeds.
        pending
            .acknowledge(key, None, Some(loaded.clone()), None, None, None)
            .unwrap();

        // Newly written activated.json uses the present nested `kind` shape.
        std::fs::remove_file(&activated_path).unwrap();
        pending
            .acknowledge(key, None, Some(loaded), None, None, None)
            .unwrap();
        let rewritten: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&activated_path).unwrap()).unwrap();
        assert!(rewritten["portable_flatpak"].get("schema").is_none());
        assert!(rewritten["portable_flatpak"].get("purpose").is_none());
        assert_eq!(
            rewritten["portable_flatpak"]["kind"]["consumer_protocol"],
            1
        );
        assert!(!session_pending(&identity));

        // Unknown fields on a flat stored copy still fail closed.
        let bad_flat =
            flat_activated.replacen(r#""schema":1,"#, r#""schema":1,"unexpected":true,"#, 1);
        write_private_file(&activated_path, bad_flat.as_bytes());
        assert!(pending.completed(&identity).is_err());
        assert!(session_pending(&identity));
    }

    #[test]
    fn stored_mine_tip_flat_browser_activated_json_loads_and_tracks_browser_pending() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(tilo_identity::store::FILE_SECRETS_ENV, "1");
        let (home, identity, key, digest) = fixture_with_active_identity();
        env.set("HOME", home.path());

        let plan = browser_continuity::Plan::new(
            "mine-tip-browser-binding".into(),
            key,
            vec![
                browser_continuity::Profile::new(
                    "a".repeat(64),
                    browser_continuity::Provider::Chromium,
                    vec!["browser".into(), "Default".into()],
                    true,
                    false,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let plan_sha256: [u8; 32] = Sha256::digest(plan.encode().unwrap()).into();
        let flat_activated = format!(
            concat!(
                r#"{{"version":1,"digest":{digest},"identity_public_key":{key},"#,
                r#""browser":{{"plan":{plan},"payload":{{"schema":1,"#,
                r#""operation":"22222222-2222-4222-8222-222222222222","#,
                r#""purpose":"browser_profiles","#,
                r#""filename":"browser-22222222-2222-4222-8222-222222222222.sealed","#,
                r#""setup_binding":"mine-tip-browser-binding","#,
                r#""sealed_bytes":18,"plaintext_bytes":16,"entries":2,"#,
                r#""ciphertext_sha256":{cipher},"plan_sha256":{plan_hash},"#,
                r#""limits":{{"plaintext_bytes":1048576,"entries":64,"metadata_bytes":65536}}}}}}}}"#
            ),
            digest = serde_json::to_string(&digest).unwrap(),
            key = serde_json::to_string(&key).unwrap(),
            plan = serde_json::to_string(&plan).unwrap(),
            cipher = serde_json::to_string(&[6u8; 32]).unwrap(),
            plan_hash = serde_json::to_string(&plan_sha256).unwrap(),
        );
        let activated_path = state_root(home.path()).join(RECEIPT);
        write_private_file(&activated_path, flat_activated.as_bytes());

        let pending = Pending::open(home.path()).unwrap();
        assert!(pending.completed(&identity).unwrap());
        let loaded = pending.browser(&identity).unwrap().unwrap();
        assert_eq!(loaded.plan, plan);
        assert_eq!(loaded.payload.kind.plan_sha256, plan_sha256);
        // Browser continuation is still unfinished, so session_pending stays true.
        assert!(session_pending(&identity));
    }

    fn sample_helper_expectations(
        binding: &str,
        key: [u8; 32],
    ) -> (
        imager_backend::setup::portable_home_media::Expectation,
        PortableApplications,
        BrowserDeclaration,
    ) {
        use imager_backend::setup::{
            browser_transfer, flatpak_transfer::portable as flatpak_portable, portable_home_media,
            portable_transfer::Limits, prepared_setup::BrowserHandoff,
        };

        let limits = Limits {
            plaintext_bytes: 1_048_576,
            entries: 64,
            metadata_bytes: 65_536,
        };
        let home_op = "10101010-1010-4010-8010-101010101010".to_owned();
        let home_exp = portable_home_media::Expectation {
            filename: format!("home-{home_op}.sealed"),
            operation: home_op,
            setup_binding: binding.to_owned(),
            sealed_bytes: 512,
            plaintext_bytes: 256,
            entries: 4,
            ciphertext_sha256: [1; 32],
            limits,
            kind: portable_home_media::Home { runtime_sockets: 0 },
        };
        let app_op = "11111111-1111-4111-8111-111111111111".to_owned();
        let apps = flatpak_portable::Expectation {
            filename: format!("applications-{app_op}.sealed"),
            operation: app_op,
            setup_binding: binding.to_owned(),
            sealed_bytes: 512,
            plaintext_bytes: 256,
            entries: 4,
            ciphertext_sha256: [4; 32],
            limits,
            kind: flatpak_portable::Applications {
                consumer_protocol: 2,
                manifest_sha256: [5; 32],
            },
        };
        let plan = browser_continuity::Plan::new(
            binding.to_owned(),
            key,
            vec![
                browser_continuity::Profile::new(
                    "a".repeat(64),
                    browser_continuity::Provider::Chromium,
                    vec!["browser".into(), "Default".into()],
                    true,
                    false,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let plan_sha256: [u8; 32] = Sha256::digest(plan.encode().unwrap()).into();
        let browser_op = "22222222-2222-4222-8222-222222222222".to_owned();
        let browser = BrowserHandoff {
            plan,
            payload: browser_transfer::Expectation {
                filename: format!("browser-{browser_op}.sealed"),
                operation: browser_op,
                setup_binding: binding.to_owned(),
                sealed_bytes: 512,
                plaintext_bytes: 256,
                entries: 2,
                ciphertext_sha256: [6; 32],
                limits,
                kind: browser_transfer::Browser { plan_sha256 },
            },
        };
        (home_exp, apps, browser)
    }

    fn build_real_helper_package(
        root: &Path,
        password: &[u8],
    ) -> (
        PathBuf,
        [u8; 32],
        PortableApplications,
        BrowserDeclaration,
        ServiceSessions,
    ) {
        use imager_backend::{
            imager_model::InstallProfile,
            setup::{
                portable_transfer::{ExportReview, Purpose},
                prepared_identity::PreparedIdentity,
                prepared_setup::{
                    ApplicationsCarriage, HomeCarriage, IDENTITY_FILE, LIMITS, Manifest,
                    PortableHomePlan, PortableHomeSelection, PublicInstallProfile,
                    SERVICE_SESSIONS_FILE, Unit,
                },
                service_sessions::PreparedSessions,
            },
        };
        use std::sync::atomic::AtomicBool;
        use zeroize::Zeroizing;

        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let staging = root.join("staging");
        std::fs::create_dir(&staging).unwrap();
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700)).unwrap();
        let identity = PreparedIdentity::create(
            &root.join("identity-source"),
            "Ada",
            [1, 2, 3],
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        identity
            .export(
                &staging.join(IDENTITY_FILE),
                Zeroizing::new(String::from_utf8(password.to_vec()).unwrap()),
            )
            .unwrap();
        let binding = "helper-end-to-end-binding".to_owned();
        let (artifact, sessions_decl) = PreparedSessions::from_protocol_2_origin_fixture(
            identity.public_key(),
            binding.clone(),
        )
        .unwrap()
        .seal(password)
        .unwrap();
        assert_eq!(sessions_decl.consumer_protocol, 2);
        write_private_file(&staging.join(SERVICE_SESSIONS_FILE), artifact.as_bytes());

        let (home_exp, apps, browser) = sample_helper_expectations(&binding, identity.public_key());
        let mut manifest = Manifest::new(binding, identity.public_key(), Vec::new()).unwrap();
        let draft = InstallProfile {
            device_label: "Ada's laptop".into(),
            ..Default::default()
        };
        manifest.install_profile = Some(PublicInstallProfile::from_draft(&draft).unwrap());
        manifest.units.plan_home(PortableHomePlan {
            schema: 1,
            selection: PortableHomeSelection::FullHome,
            applications: true,
        });
        manifest
            .units
            .carry_home(HomeCarriage::Sealed(home_exp))
            .unwrap();
        manifest
            .units
            .insert(Unit::MediumApplications(ApplicationsCarriage::Sealed {
                expectation: apps.clone(),
                alone: false,
            }));
        manifest.units.insert(Unit::Browser(browser.clone()));
        manifest
            .units
            .insert(Unit::ServiceSessions(sessions_decl.clone()));
        manifest.write(&staging).unwrap();

        let package = root.join("setup.sealed");
        ExportReview::review(&staging, &package, Purpose::PreparedSetup, LIMITS)
            .unwrap()
            .export(Zeroizing::new(password.to_vec()), &AtomicBool::new(false))
            .unwrap();
        (package, identity.public_key(), apps, browser, sessions_decl)
    }

    #[test]
    fn helper_run_output_with_applications_browser_and_protocol_2_sessions_decodes_in_session() {
        use imager_backend::setup::prepared_setup_helper::{HelperAdmissions, run_with_admissions};
        use std::ffi::OsString;

        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(tilo_identity::store::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let password = b"several unrelated installation words";
        let (package, key, apps, browser, sessions_decl) =
            build_real_helper_package(&root, password);

        let args: Vec<OsString> = [
            ("--package", package),
            ("--quarantine", root.join("quarantine")),
            ("--identity-dir", root.join("identities")),
            ("--config-root", root.join("tilo")),
        ]
        .into_iter()
        .flat_map(|(flag, path)| [OsString::from(flag), path.into_os_string()])
        .collect();
        let mut saw_service_protocol = None;
        let mut saw_home = false;
        let mut saw_apps = false;
        let mut output = Vec::new();
        run_with_admissions(
            args,
            &mut &password[..],
            &mut output,
            HelperAdmissions {
                home: || {
                    saw_home = true;
                    Ok(())
                },
                service_protocol: |protocol| {
                    saw_service_protocol = Some(protocol);
                    Ok(())
                },
                applications: || {
                    saw_apps = true;
                    Ok(())
                },
            },
        )
        .unwrap();
        assert!(saw_home && saw_apps && saw_service_protocol == Some(2));
        assert!(output.ends_with(b"\n"));

        // Confirm the producer's wire shape is unchanged before handing its exact bytes to decode.
        let wire: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert!(wire["portable_flatpak"].get("schema").is_none());
        assert!(wire["portable_flatpak"].get("purpose").is_none());
        assert_eq!(wire["portable_flatpak"]["kind"]["consumer_protocol"], 2);
        assert!(wire["browser"]["payload"].get("schema").is_none());
        assert!(wire["browser"]["payload"].get("purpose").is_none());
        assert_eq!(wire["service_sessions"]["consumer_protocol"], 2);

        let decoded = Imported::decode(&output).unwrap();
        assert_eq!(decoded.identity_public_key, key);
        assert_eq!(decoded.device_label, "Ada's laptop");
        assert_eq!(decoded.portable_flatpak.as_ref(), Some(&apps));
        assert_eq!(decoded.browser.as_ref(), Some(&browser));
        assert_eq!(decoded.service_sessions.as_ref(), Some(&sessions_decl));
        assert_eq!(
            decoded
                .portable_home
                .as_ref()
                .map(|home| (&home.selection, home.applications)),
            Some((&prepared_setup_model::PortableHomeSelection::FullHome, true))
        );
    }
}
