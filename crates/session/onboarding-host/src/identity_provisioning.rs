//! Shell-side host for the identity ceremony: persistence
//! (`~/.config/tilo/identity/primary.json`, `0600`) and the first-run
//! trigger.
//!
//! New ceremonies emit schema-1 JSON matching
//! [`tilo_identity_ceremony::IdentityRecord`] in memory, then the runtime
//! lifecycle store atomically imports it as public schema-2 metadata backed by
//! Secret Service or a host-only broker file before the ceremony returns.
//!
//! Intentionally thin: cryptographic logic stays in
//! `threshold-identity`, visual logic stays in `ui`.

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::fs;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use tilo_identity_ceremony::{IdentityFlowError, IdentityRecord, IdentityStore};

/// Resolve the user-owned device-identity root used by a Tilo Desktop session
/// installed on another Linux distribution.
///
/// XDG's data directory is the portable contract across Ubuntu, Fedora, Arch
/// and other hosts. Relative values are rejected: turning one into an
/// identity path using the compositor's working directory would make key
/// placement depend on the display manager.
pub fn host_identity_root(
    xdg_data_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> io::Result<PathBuf> {
    let base = if let Some(value) = xdg_data_home.filter(|value| !value.is_empty()) {
        PathBuf::from(value)
    } else {
        let home = home.filter(|value| !value.is_empty()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "neither XDG_DATA_HOME nor HOME is set for this login",
            )
        })?;
        PathBuf::from(home).join(".local/share")
    };
    if !base.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the host session data directory must be absolute",
        ));
    }
    Ok(base.join("tilo"))
}

/// Paths shared by the host Files service and its native clients.
fn host_files_environment(
    home: Option<std::ffi::OsString>,
    state_home: Option<std::ffi::OsString>,
    runtime_dir: Option<std::ffi::OsString>,
) -> io::Result<Vec<(&'static str, PathBuf)>> {
    let state = match state_home.filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => PathBuf::from(
            home.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is unavailable"))?,
        )
        .join(".local/state"),
    };
    let runtime = PathBuf::from(runtime_dir.ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is unavailable")
    })?);
    if !state.is_absolute() || !runtime.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "host Files directories must be absolute",
        ));
    }
    let cache = state.join("tilo/files");
    let storage = runtime.join("tilo/files-storage");
    let runtime = runtime.join("tilo/files-workspace");
    Ok(vec![
        ("TILO_FS_CONTENT_PEER_CACHE_DIR", cache.clone()),
        ("TILO_WORKSPACE_ROOTS", cache.join("mount")),
        ("TILO_AUTHORITY_SOCKET_DIR", runtime.clone()),
        ("TILO_FS_BLOB_SOCKET", runtime.join("fs-blobs.sock")),
        ("TILO_FS_STORAGE_DIR", storage),
        (
            "TILO_FS_AUTHORITY_PUBLISHER",
            PathBuf::from("/usr/libexec/tilo/tilo-relay-admin"),
        ),
    ])
}

/// Select the per-user identity tree for a host-installed desktop session.
///
/// Must run while process startup is still single-threaded. The environment
/// variable is also inherited by launcher/broker children, keeping every
/// identity consumer on the same tree. An explicit existing override remains
/// available for hermetic development and tests.
///
/// # Errors
///
/// Returns an error if the configured identity root is relative or its
/// user-private directory tree cannot be safely prepared.
#[allow(
    clippy::disallowed_methods,
    reason = "called from `main` before the warm-up thread or any other thread is spawned; the variable must be inherited by launcher and broker children"
)]
pub fn configure_host_identity_root() -> io::Result<PathBuf> {
    let root = match std::env::var_os("TILO_IDENTITY_ROOT").filter(|value| !value.is_empty()) {
        Some(root) => PathBuf::from(root),
        None => host_identity_root(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))?,
    };
    if !root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TILO_IDENTITY_ROOT must be absolute",
        ));
    }
    let files_environment = host_files_environment(
        std::env::var_os("HOME"),
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("XDG_RUNTIME_DIR"),
    )?;
    for (name, value) in files_environment {
        if std::env::var_os(name).is_none_or(|existing| existing.is_empty()) {
            // SAFETY: same single-threaded startup contract as the identity root.
            unsafe { std::env::set_var(name, value) };
        }
    }
    prepare_private_host_identity_tree(&root)?;
    // SAFETY: `main` calls this before it starts the warm-up thread or any
    // compositor/runtime worker. No other thread can read the environment.
    unsafe { std::env::set_var("TILO_IDENTITY_ROOT", &root) };
    Ok(root)
}

/// Create the host-session identity tree as a user-private trust boundary.
/// The full Tilo image intentionally shares its device key with a dedicated
/// system group; a distro session has no such group and must not inherit a
/// host's broad primary-group membership instead.
#[cfg(unix)]
fn prepare_private_host_identity_tree(root: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let parent = root.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "identity root has no parent")
    })?;
    std::fs::create_dir_all(parent)?;
    let parent_meta = std::fs::metadata(parent)?;
    if !parent_meta.is_dir()
        || !tilo_files::native::private::owned_by_me(&parent_meta)
        || parent_meta.mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "host-session identity parent must be user-owned and not group/world writable",
        ));
    }

    for directory in [
        root.to_path_buf(),
        root.join("user-identity"),
        root.join("device"),
    ] {
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) => {
                if !metadata.file_type().is_dir()
                    || !tilo_files::native::private::owned_by_me(&metadata)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "host-session identity directory is not a user-owned real directory: {}",
                            directory.display()
                        ),
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut builder = std::fs::DirBuilder::new();
                builder.mode(0o700).create(&directory)?;
            }
            Err(error) => return Err(error),
        }
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn prepare_private_host_identity_tree(_root: &std::path::Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "host desktop sessions require Unix filesystem permissions",
    ))
}

/// Writes public metadata to `$XDG_CONFIG_HOME/tilo/identity/primary.json`
/// (falling back to `$HOME/.config/...`) and imports its private share into
/// the runtime identity provider before returning.
pub struct DiskIdentityStore {
    path: PathBuf,
}

/// Environment marker used only within the shell process to carry the result
/// of the pre-compositor Secret Service probe into native compositor startup.
/// It is deliberately one-way: an unavailable existing key can request the
/// recovery surface, but no environment value can bypass identity checks.
pub const IDENTITY_RECOVERY_ENV: &str = "TILO_IDENTITY_RECOVERY";

/// What startup found when the existing identity's certificate was refused
/// for the clock's reading alone: the identity is intact and the session
/// waits for the clock rather than offering recovery. Set once, before the
/// compositor starts, by the same probe that decides recovery.
static IDENTITY_CLOCK_REFUSAL: std::sync::Mutex<Option<tilo_user_identity::CertificateClockError>> =
    std::sync::Mutex::new(None);

/// Record that the existing identity waits for the clock (see
/// [`identity_clock_refusal`]).
pub fn await_clock_for_identity(refusal: tilo_user_identity::CertificateClockError) {
    *IDENTITY_CLOCK_REFUSAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(refusal);
}

/// The clock refusal startup recorded for the existing identity, if any.
pub fn identity_clock_refusal() -> Option<tilo_user_identity::CertificateClockError> {
    IDENTITY_CLOCK_REFUSAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Whether an existing schema-2 identity should be recovered interactively.
///
/// A locked/missing Secret Service is recoverable only through the trusted
/// onboarding surface (unlock the host keyring or restore an encrypted copy).
/// Do not broaden this to every startup error: malformed metadata, permission
/// failures, and broken device certificates must continue to fail closed.
pub fn is_secret_service_unavailable(error: &io::Error) -> bool {
    runtime_identity::is_secret_service_unavailable_io(error)
}

impl DiskIdentityStore {
    /// Uses the canonical runtime resolver shared by Settings, first boot,
    /// and the launcher, so the boot chain cannot disagree because of a
    /// relative XDG path.
    ///
    /// # Errors
    ///
    /// Returns an error if the identity directory can't be resolved or
    /// created.
    pub fn from_env() -> Result<Self, io::Error> {
        let base = runtime_identity::identity_dir_from_env()?;
        // Imported identities keep a fingerprinted filename. Resolve the
        // active runtime identity first so every consumer shares one
        // canonical selection and a successful import remains ready on the
        // next login.
        let path = runtime_identity::resolve_active_identity(&base)
            .unwrap_or_else(|| base.join("primary.json"));
        Ok(Self { path })
    }

    /// Used by tests to point at a tempdir without `$HOME` shimming.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn identity_present(&self) -> bool {
        self.path.exists()
    }
}

impl IdentityStore for DiskIdentityStore {
    fn load(&self) -> Result<Option<IdentityRecord>, IdentityFlowError> {
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(IdentityFlowError::Io(error)),
        };
        let metadata = file.metadata().map_err(IdentityFlowError::Io)?;
        if !metadata.file_type().is_file()
            || !tilo_files::native::private::private_to_me(&metadata)
            || metadata.len() > 1024 * 1024
        {
            return Err(IdentityFlowError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity record must be a private user-owned regular file",
            )));
        }
        // `metadata.len()` was already checked <= 1 MiB above; this cast is a
        // capacity hint, so a lossy fallback on exotic 32-bit targets is harmless.
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        (&mut file)
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(IdentityFlowError::Io)?;
        if bytes.len() > 1024 * 1024 {
            return Err(IdentityFlowError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "identity record exceeds the size limit",
            )));
        }
        Ok(Some(IdentityRecord::from_json_bytes(&bytes)?))
    }

    fn save(&self, record: &IdentityRecord) -> Result<(), IdentityFlowError> {
        let bytes = record.to_json_bytes()?;
        let parent = self.path.parent().ok_or_else(|| {
            IdentityFlowError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "identity path has no parent",
            ))
        })?;
        let id = self
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                IdentityFlowError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "identity path has no valid id",
                ))
            })?;
        import_identity_record(parent, id, &bytes).map_err(IdentityFlowError::Io)?;
        Ok(())
    }
}

fn import_identity_record(parent: &Path, id: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    #[cfg(test)]
    {
        let provider = runtime_identity::BrokerFileProvider::new(parent);
        runtime_identity::import_identity_with_provider(parent, id, bytes, &provider)
    }
    #[cfg(not(test))]
    {
        runtime_identity::import_identity(parent, id, bytes)
    }
}

/// A free function, not a method on the store, so the boot path can
/// call it without owning the store first.
/// Complete this session's side of a device the person enrolled without
/// their root (a node's session, certified when its node was installed), so
/// it holds that person as its identity like any joined device
/// ([`tilo_identity_host::provisioning::complete_enrolled_device`]).
///
/// # Errors
///
/// Returns an error if the identity directory cannot be resolved or the
/// enrolled device cannot be completed.
pub fn complete_enrolled_device() -> io::Result<bool> {
    let store = tilo_identity_host::registry::IdentityRegistryStore::from_env()?;
    tilo_identity_host::provisioning::complete_enrolled_device(
        store.identity_dir(),
        &crate::firstboot::collaboration_identity_paths(),
        tilo_clock::unix_now_ns(),
    )
}

pub fn needs_first_run_flow(store: &impl IdentityStore) -> bool {
    match store.load() {
        Ok(None) => true,
        Ok(Some(_)) => false,
        Err(e) => {
            // A corrupt, unsafe, or unreadable record is not the same as no
            // record. Keep trusted setup in front of the desktop so the user
            // can connect a valid backup; the creation path still validates
            // the existing record and refuses to overwrite it.
            tracing::error!("identity store load failed; refusing to replace it: {e}");
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use getrandom::{SysRng, rand_core::UnwrapErr};
    use std::os::unix::fs::PermissionsExt;
    use std::time::SystemTime;
    use threshold_identity::ThresholdIdentity;
    use tilo_identity_ceremony::MemoryIdentityStore;

    fn fixture_record() -> IdentityRecord {
        let mut rng = UnwrapErr(SysRng);
        let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        IdentityRecord::from_threshold_identity(
            &id,
            "alice",
            [248, 113, 113],
            SystemTime::UNIX_EPOCH
                .checked_add(std::time::Duration::from_secs(100))
                .unwrap_or(SystemTime::UNIX_EPOCH),
        )
        .unwrap()
    }

    #[test]
    fn save_and_load_round_trip_on_disk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = DiskIdentityStore::at(tmp.path().join("primary.json"));
        assert!(store.load().unwrap().is_none());
        let rec = fixture_record();
        store.save(&rec).unwrap();
        assert!(store.identity_present());
        let back = store.load().unwrap().unwrap();
        assert_eq!(back.display_name, rec.display_name);
        assert_eq!(back.public_key(), rec.public_key());
        assert_eq!(back.schema_version, 2);
        assert!(back.threshold_identity.secret_bytes.is_empty());
        assert!(back.secret_ref.is_some());
    }

    #[test]
    fn save_sets_0600_mode_on_unix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = DiskIdentityStore::at(tmp.path().join("primary.json"));
        store.save(&fixture_record()).unwrap();
        #[cfg(unix)]
        {
            let meta = fs::metadata(store.path()).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "primary.json must be 0600, got {mode:o}");
        }
    }

    #[cfg(unix)]
    #[test]
    #[cfg(unix)]
    fn load_refuses_an_identity_path_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("elsewhere.json");
        let bytes = fixture_record().to_json_bytes().unwrap();
        fs::write(&target, bytes).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let path = tmp.path().join("primary.json");
        symlink(&target, &path).unwrap();

        let store = DiskIdentityStore::at(path);
        assert!(store.load().is_err());
    }

    #[test]
    fn load_on_missing_file_returns_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = DiskIdentityStore::at(tmp.path().join("does-not-exist.json"));
        assert!(store.load().unwrap().is_none());
        assert!(!store.identity_present());
    }

    #[test]
    fn host_identity_root_uses_the_xdg_data_contract() {
        assert_eq!(
            host_identity_root(Some("/home/alice/.data".into()), Some("/ignored".into())).unwrap(),
            PathBuf::from("/home/alice/.data/tilo")
        );
        assert_eq!(
            host_identity_root(None, Some("/home/alice".into())).unwrap(),
            PathBuf::from("/home/alice/.local/share/tilo")
        );
    }

    #[test]
    fn host_identity_root_never_depends_on_the_working_directory() {
        assert!(host_identity_root(Some("relative".into()), Some("/home/alice".into())).is_err());
        assert!(host_identity_root(None, Some("relative".into())).is_err());
        assert!(host_identity_root(None, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    #[cfg(unix)]
    fn host_identity_tree_is_private_and_refuses_a_symlink_root() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("data/tilo");
        prepare_private_host_identity_tree(&root).unwrap();
        for directory in [&root, &root.join("user-identity"), &root.join("device")] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }

        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        let linked = tmp.path().join("linked");
        symlink(&target, &linked).unwrap();
        assert!(prepare_private_host_identity_tree(&linked).is_err());
    }

    #[test]
    fn host_files_paths_match_the_user_service() {
        let environment = host_files_environment(
            Some("/home/alice".into()),
            None,
            Some("/run/user/1000".into()),
        )
        .unwrap();
        assert_eq!(
            environment[0].1,
            PathBuf::from("/home/alice/.local/state/tilo/files")
        );
        assert_eq!(environment[1].1, environment[0].1.join("mount"));
        assert_eq!(
            environment[2].1,
            PathBuf::from("/run/user/1000/tilo/files-workspace")
        );
        assert_eq!(environment[3].1, environment[2].1.join("fs-blobs.sock"));
        assert_eq!(
            environment[4],
            (
                "TILO_FS_STORAGE_DIR",
                PathBuf::from("/run/user/1000/tilo/files-storage")
            )
        );
        let custom =
            host_files_environment(None, Some("/state".into()), Some("/runtime".into())).unwrap();
        assert_eq!(custom[0].1, PathBuf::from("/state/tilo/files"));
        assert!(
            host_files_environment(None, Some("relative".into()), Some("/runtime".into())).is_err()
        );
        assert!(host_files_environment(Some("/home/alice".into()), None, None).is_err());
    }

    #[test]
    fn first_run_flow_triggers_only_on_missing_identity() {
        let store = MemoryIdentityStore::new();
        assert!(needs_first_run_flow(&store));
        store.save(&fixture_record()).unwrap();
        assert!(!needs_first_run_flow(&store));
    }

    #[test]
    fn only_the_secret_service_unavailable_error_enters_recovery() {
        assert!(is_secret_service_unavailable(
            &runtime_identity::secret_service_unavailable_error()
        ));
        assert!(!is_secret_service_unavailable(&io::Error::new(
            io::ErrorKind::InvalidData,
            "identity JSON malformed",
        )));
        assert!(!is_secret_service_unavailable(&io::Error::new(
            io::ErrorKind::PermissionDenied,
            "identity directory is not private",
        )));
    }

    #[cfg(unix)]
    #[test]
    #[cfg(unix)]
    fn invalid_existing_identity_keeps_trusted_setup_in_front_of_the_desktop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("primary.json");
        fs::write(&path, b"not an identity").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let store = DiskIdentityStore::at(path);
        assert!(
            needs_first_run_flow(&store),
            "an invalid identity must not let an identity-less desktop start"
        );
    }
}
