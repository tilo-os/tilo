//! Host-owned tmpfs working image. No plaintext profile is staged on disk.
//! This retains the encrypted owner's outstanding lease; it cannot checkpoint
//! a running provider or mistake a child exit for writer quiescence.
use super::{Arc, GrantedSignalAuthority, NativeSignalAccount, SendError};
use std::{
    ffi::{CString, OsStr},
    fs::{File, OpenOptions},
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use tilo_messaging_protocol::signal_profile::{EncryptedSignalProfile, ProfileError, ProfileImage};

pub(super) const SERVICE_PACKAGE: &str = "so.tilo.signal-provider";
const MAX_DEPTH: usize = 36;
mod collection;
pub(super) mod provision;

/// One exact original account's private working lease. Keep this owner alive
/// until the service has stopped. Dropping it clears its RAM tree and leaves
/// the durable active marker: no previous crypto state can silently resume.
pub struct NativeSignalWorkingProfile {
    ram: RamDirectory,
    gate: Arc<GrantedSignalAuthority>,
    sealed: EncryptedSignalProfile,
    image: Option<ProfileImage>,
    lent: AtomicBool,
    launch_key: launcher::owned_service::ServiceLeaseKey,
}
impl NativeSignalWorkingProfile {
    /// Configure the normal launcher to give this service RAM-only app and
    /// person directories. The descriptor stays with this lease, never the UI.
    /// This neither starts a process nor changes account readiness.
    /// # Errors
    /// Refuses retired authority or a different service package/workspace.
    pub fn configure_launch(
        &self,
        options: &mut launcher::run::RunOptions,
    ) -> Result<(), SendError> {
        self.gate.check()?;
        if options.app_id.as_deref() != Some(SERVICE_PACKAGE)
            || options.workspace_id.is_some()
            || !matches!(options.mode, launcher::run::LaunchMode::Service { .. })
            || options.expected_package_hash.is_none()
            || !options.landlock_enforce
            || options.landlock_best_effort
            || options.allow_missing_runtime
            || options.allow_unsigned_runtime
            || options.capabilities_path.is_some()
            || options.wayland_socket.is_some()
            || options.render_node.is_some()
        {
            return Err(SendError::InvalidRequest);
        }
        ram_backing(&self.ram.root).map_err(|_| SendError::Unavailable)?;
        self.lent
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SendError::AlreadyReserved)?;
        options.data_home = Some(self.ram.source());
        options.identity = launcher::run::IdentitySource::None;
        options.no_services = true;
        Ok(())
    }
    /// Seal a lease whose mount source was never handed to a launcher. There
    /// cannot be a provider writer through this owner, so its unchanged image
    /// can be sealed even after original authority retirement. A refused
    /// cancellation retains this owner and its RAM tree; it cannot drop a
    /// potentially running provider's profile as a side effect of the error.
    /// # Errors
    /// Refuses any previously lent working source or failed durable checkpoint.
    pub fn cancel_before_launch(&mut self) -> Result<(), SendError> {
        if self.lent.load(Ordering::Acquire) {
            return Err(SendError::AlreadyReserved);
        }
        let image = self.image.take().ok_or(SendError::Unavailable)?;
        self.sealed
            .checkpoint(image)
            .map_err(|_| SendError::Unavailable)
    }
    pub(super) fn run_connected_and_checkpoint(
        mut self,
        mut launch: launcher::owned_service::ServiceLaunch,
        cancellation: launcher::owned_service::ServiceCancellation,
        network: Option<Arc<super::network::Consent>>,
        connected: Option<launcher::owned_service::ConnectedService>,
    ) -> Result<(), SendError> {
        self.configure_launch(&mut launch.options)?;
        let stopped = launcher::owned_service::run_connected(
            launch,
            Arc::new(ProfileServiceAuthority(self.gate.clone(), network)),
            cancellation,
            self.launch_key.clone(),
            connected,
        )
        .map_err(|_| SendError::Unavailable)?;
        self.checkpoint_stopped(&stopped)
    }
    fn checkpoint_stopped(
        &mut self,
        stopped: &launcher::owned_service::QuiescentExit,
    ) -> Result<(), SendError> {
        if !self.lent.load(Ordering::Acquire)
            || !stopped.matches(&self.launch_key, &self.ram.source(), SERVICE_PACKAGE)
        {
            return Err(SendError::InvalidRequest);
        }
        let files = self
            .ram
            .collect_profile()
            .map_err(|_| SendError::Unavailable)?;
        let image = self.image.take().ok_or(SendError::Unavailable)?;
        self.sealed
            .checkpoint_files(image, files)
            .map_err(|_| SendError::Unavailable)
    }
}
struct ProfileServiceAuthority(
    Arc<GrantedSignalAuthority>,
    Option<Arc<super::network::Consent>>,
);
impl launcher::owned_service::ServiceAuthority for ProfileServiceAuthority {
    fn network_hosts(&self) -> io::Result<Vec<String>> {
        self.1
            .as_ref()
            .map_or_else(|| Ok(Vec::new()), |consent| consent.hosts())
    }
    fn check(&self) -> io::Result<()> {
        if let Some(consent) = &self.1 {
            consent
                .check()
                .map_err(|_| io::ErrorKind::PermissionDenied)?;
        }
        self.0
            .check()
            .map_err(|_| io::ErrorKind::PermissionDenied.into())
    }
}
impl NativeSignalAccount {
    /// Materialize this account's existing sealed profile under the host's
    /// private tmpfs runtime directory. Disk, links and permissive roots refuse
    /// before beginning use or exposing any profile bytes.
    /// # Errors
    /// Refuses invalid RAM custody, original admission/grant, interrupted image
    /// or file creation failure. Failed materialization leaves use interrupted.
    pub fn materialize_profile(
        &self,
        encrypted_root: &Path,
        runtime_root: &Path,
    ) -> Result<NativeSignalWorkingProfile, SendError> {
        self.gate.check()?;
        let ram = RamDirectory::allocate(runtime_root).map_err(|_| SendError::Unavailable)?;
        let mut sealed = self.open_profile(encrypted_root)?;
        let image = sealed.begin_use().map_err(|_| SendError::Unavailable)?;
        image
            .visit_files(|path, bytes| {
                ram.write_profile(path, bytes)
                    .map_err(|_| ProfileError::Unavailable)
            })
            .map_err(|_| SendError::Unavailable)?;
        self.gate.check()?;
        Ok(NativeSignalWorkingProfile {
            ram,
            gate: self.gate.clone(),
            sealed,
            image: Some(image),
            lent: AtomicBool::new(false),
            launch_key: launcher::owned_service::ServiceLeaseKey::default(),
        })
    }
}

struct RamDirectory {
    parent: File,
    name: CString,
    root: File,
}
impl RamDirectory {
    fn allocate(parent_path: &Path) -> io::Result<Self> {
        if !parent_path.is_absolute() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let parent = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent_path)?;
        validate_directory(&parent)?;
        ram_backing(&parent)?;
        let name = component(OsStr::new(&format!("tilo-signal-{}", uuid::Uuid::new_v4())))?;
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let root = match open_directory(&parent, &name) {
            Ok(root) => root,
            Err(error) => {
                unsafe {
                    libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR);
                }
                return Err(error);
            }
        };
        Ok(Self { parent, name, root })
    }
    fn source(&self) -> PathBuf {
        fd_path(&self.root)
    }
    fn write_profile(&self, path: &str, bytes: &[u8]) -> io::Result<()> {
        let mut components = path
            .split('/')
            .map(|part| component(OsStr::new(part)))
            .collect::<io::Result<Vec<_>>>()?;
        if components.is_empty() || components.len() > 32 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let leaf = components.pop().ok_or(io::ErrorKind::InvalidInput)?;
        let mut directory = self.root.try_clone()?;
        for part in [
            launcher::appdir::ROOMS_DIR,
            launcher::appdir::PERSONAL_ROOM,
            SERVICE_PACKAGE,
        ] {
            directory = child_directory(&directory, &component(OsStr::new(part))?)?;
        }
        for part in components {
            directory = child_directory(&directory, &part)?;
        }
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                leaf.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || !tilo_files::native::private::is_owner_only(&metadata)
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        file.write_all(bytes)
    }
}
impl Drop for RamDirectory {
    fn drop(&mut self) {
        // Descriptor-relative cleanup cannot follow a provider-created link or
        // remove a replacement tree. Failure never retires the encrypted marker.
        if clear_directory(&self.root, 0).is_err() {
            return;
        }
        if let Ok(current) = open_directory(&self.parent, &self.name)
            && let (Ok(original), Ok(now)) = (self.root.metadata(), current.metadata())
            && original.dev() == now.dev()
            && original.ino() == now.ino()
        {
            unsafe {
                libc::unlinkat(
                    self.parent.as_raw_fd(),
                    self.name.as_ptr(),
                    libc::AT_REMOVEDIR,
                );
            }
        }
    }
}
fn component(name: &OsStr) -> io::Result<CString> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || matches!(bytes, b"." | b"..")
        || bytes.contains(&b'/')
        || bytes.contains(&b'\\')
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    CString::new(bytes).map_err(io::Error::other)
}
fn fd_path(file: &File) -> PathBuf {
    PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        file.as_raw_fd()
    ))
}
fn validate_directory(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_dir()
        || !tilo_files::native::private::owned_by_me(&metadata)
        || !tilo_files::native::private::is_owner_only(&metadata)
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::fstatfs(file.as_raw_fd(), stats.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { stats.assume_init() }.f_type != libc::TMPFS_MAGIC {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
fn ram_backing(directory: &File) -> io::Result<()> {
    // The mount id comes from the retained opened inode, not path-prefix
    // guesses. An explicitly noswap tmpfs remains RAM-only on disk-swap hosts.
    let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", directory.as_raw_fd()))?;
    let mount = info
        .lines()
        .find_map(|line| line.strip_prefix("mnt_id:"))
        .ok_or(io::ErrorKind::InvalidData)?
        .trim()
        .parse::<u64>()
        .map_err(io::Error::other)?;
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")?;
    let options = mounts
        .lines()
        .find_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            if left.split_whitespace().next()?.parse::<u64>().ok()? != mount {
                return None;
            }
            let mut fields = right.split_whitespace();
            if fields.next()? != "tmpfs" {
                return None;
            }
            fields.next()?;
            fields.next()
        })
        .ok_or(io::ErrorKind::InvalidData)?;
    if options.split(',').any(|option| option == "noswap") {
        return Ok(());
    }
    ram_only_swap()
}
fn ram_only_swap() -> io::Result<()> {
    // tmpfs pages may enter swap. Permit only an absent swap set or zram
    // devices with no writeback backing device; do not alter host policy.
    let swaps = std::fs::read_to_string("/proc/swaps")?;
    let mut lines = swaps.lines();
    if !lines
        .next()
        .is_some_and(|line| line.starts_with("Filename"))
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    for line in lines {
        let device = line
            .split_whitespace()
            .next()
            .ok_or(io::ErrorKind::InvalidData)?;
        let name = device
            .strip_prefix("/dev/zram")
            .filter(|suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or(io::ErrorKind::PermissionDenied)?;
        let backing = std::fs::read_to_string(format!("/sys/class/block/zram{name}/backing_dev"))?;
        if backing.trim() != "none" {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
    }
    Ok(())
}
fn open_directory(parent: &File, name: &CString) -> io::Result<File> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    validate_directory(&file)?;
    Ok(file)
}
fn child_directory(parent: &File, name: &CString) -> io::Result<File> {
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0
        && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
    {
        return Err(io::Error::last_os_error());
    }
    open_directory(parent, name)
}
fn clear_directory(directory: &File, depth: usize) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(io::ErrorKind::InvalidData.into());
    }
    for entry in std::fs::read_dir(fd_path(directory))? {
        let entry = entry?;
        let name = component(&entry.file_name())?;
        let kind = entry.file_type()?;
        let flags = if kind.is_dir() {
            clear_directory(&open_directory(directory, &name)?, depth.saturating_add(1))?;
            libc::AT_REMOVEDIR
        } else {
            0
        };
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), flags) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    /// Re-execute the actual test in a private mount namespace. The production
    /// RAM check remains unchanged; unavailable namespace/mount capability is
    /// a failed qualification, never a skip or a fake tmpfs policy override.
    #[cfg(feature = "signal-profile-native-test")]
    pub(crate) fn in_noswap_namespace(test: &str) -> bool {
        const MARKER: &str = "TILO_SIGNAL_NOSWAP_TEST_NAMESPACE";
        if std::env::var(MARKER).as_deref() == Ok("1") {
            return false;
        }
        let existing = runtime();
        let directory = File::open(existing.path()).unwrap();
        if ram_backing(&directory).is_ok() {
            return false;
        }
        let test = test.split_once("::").unwrap().1;
        let output = std::process::Command::new("/usr/bin/unshare")
            .args(["--user", "--map-root-user", "--mount", "--fork", "/bin/sh", "-c",
                "set -eu; /usr/bin/mount -t tmpfs -o noswap,size=128M,mode=1777 tmpfs /dev/shm; exec \"$@\"",
                "tilo-signal-noswap-test"])
            .arg(std::env::current_exe().unwrap())
            .args([test, "--exact", "--nocapture"])
            .env(MARKER, "1")
            .output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success()
                && stdout.contains("running 1 test")
                && stdout.contains("test result: ok. 1 passed"),
            "actual noswap custody qualification failed: {}\n{stdout}\n{stderr}",
            output.status
        );

        true
    }

    fn runtime() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("tilo-signal-ram-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/dev/shm")
            .unwrap()
    }

    #[test]
    fn working_profile_requires_private_opened_ram_directory() {
        let runtime = runtime();
        let link = runtime.path().join("link");
        symlink(runtime.path(), &link).unwrap();
        assert!(RamDirectory::allocate(&link).is_err());
        let public = runtime.path().join("public");
        std::fs::create_dir(&public).unwrap();
        std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(RamDirectory::allocate(&public).is_err());
        assert_eq!(std::fs::read_dir(&public).unwrap().count(), 0);
        let disk = tempfile::Builder::new()
            .prefix("tilo-signal-disk-refusal-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/var/tmp")
            .unwrap();
        let fd = File::open(disk.path()).unwrap();
        let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
        assert_eq!(
            unsafe { libc::fstatfs(fd.as_raw_fd(), stats.as_mut_ptr()) },
            0
        );
        assert_ne!(
            unsafe { stats.assume_init() }.f_type,
            libc::TMPFS_MAGIC,
            "fixture must observe a disk filesystem to prove disk refusal"
        );
        assert!(tilo_files::native::private::is_owner_only(
            &fd.metadata().unwrap()
        ));
        assert!(validate_directory(&fd).is_err());
        assert_eq!(std::fs::read_dir(disk.path()).unwrap().count(), 0);
    }

    #[cfg(feature = "signal-profile-native-test")]
    #[test]
    fn working_profile_stages_private_bytes_and_never_follows_provider_links() {
        if in_noswap_namespace(concat!(
            module_path!(),
            "::working_profile_stages_private_bytes_and_never_follows_provider_links"
        )) {
            return;
        }
        let runtime = runtime();
        let outside = tempfile::tempdir().unwrap();
        let ram = RamDirectory::allocate(runtime.path()).unwrap();
        ram.write_profile("data/account.db", b"fixture-profile-state")
            .unwrap();
        let profile = launcher::appdir::host_app_dir(
            &ram.source(),
            launcher::appdir::PERSONAL_ROOM,
            SERVICE_PACKAGE,
        );
        let file = profile.join("data/account.db");
        assert_eq!(std::fs::read(&file).unwrap(), b"fixture-profile-state");
        assert!(tilo_files::native::private::is_owner_only(
            &std::fs::metadata(&file).unwrap()
        ));
        assert!(ram.write_profile("../escaped", b"forbidden").is_err());
        assert!(ram.write_profile("data/account.db", b"overwrite").is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"fixture-profile-state");
        std::fs::rename(profile.join("data"), profile.join("original-data")).unwrap();
        symlink(outside.path(), profile.join("data")).unwrap();
        assert!(ram.write_profile("data/escaped", b"forbidden").is_err());
        assert!(!outside.path().join("escaped").exists());
        drop(ram);
        assert_eq!(std::fs::read_dir(runtime.path()).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[cfg(feature = "signal-profile-native-test")]
    #[test]
    fn working_profile_cleanup_never_removes_a_replacement_directory() {
        if in_noswap_namespace(concat!(
            module_path!(),
            "::working_profile_cleanup_never_removes_a_replacement_directory"
        )) {
            return;
        }
        let runtime = runtime();
        let ram = RamDirectory::allocate(runtime.path()).unwrap();
        ram.write_profile("account.db", b"fixture-profile-state")
            .unwrap();
        let original = runtime.path().join(OsStr::from_bytes(ram.name.as_bytes()));
        let moved = runtime.path().join("moved-owner");
        std::fs::rename(&original, &moved).unwrap();
        std::fs::create_dir(&original).unwrap();
        std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(original.join("belongs-to-replacement"), b"preserve").unwrap();
        drop(ram);
        assert_eq!(
            std::fs::read(original.join("belongs-to-replacement")).unwrap(),
            b"preserve"
        );
        assert_eq!(std::fs::read_dir(moved).unwrap().count(), 0);
    }
}
