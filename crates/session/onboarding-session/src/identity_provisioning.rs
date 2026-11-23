//! Shell-side host setup for the person's identity: where a host-installed
//! session keeps its identity tree, and the first-run trigger.
//!
//! The record itself and its sharing belong to `tilo_identity::store`; this
//! module only decides when setup has to run in front of the desktop.

use std::io;
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::fs;

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
            PathBuf::from("/usr/libexec/tilo/tilo-server-admin"),
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

/// Environment marker used only within the shell process to carry the result
/// of the pre-compositor Secret Service probe into native compositor startup.
/// It is deliberately one-way: an unavailable existing key can request the
/// recovery surface, but no environment value can bypass identity checks.
pub const IDENTITY_RECOVERY_ENV: &str = "TILO_IDENTITY_RECOVERY";

/// Whether an existing schema-2 identity should be recovered interactively.
///
/// A locked/missing Secret Service is recoverable only through the trusted
/// onboarding surface (unlock the host keyring or restore an encrypted copy).
/// Do not broaden this to every startup error: malformed metadata, permission
/// failures, and broken device certificates must continue to fail closed.
pub fn is_secret_service_unavailable(error: &io::Error) -> bool {
    tilo_identity::store::is_secret_service_unavailable_io(error)
}

/// A free function, not a method on the store, so the boot path can
/// call it without owning the store first.
/// Complete this session's side of a device the person enrolled without
/// their root (a node's session, certified when its node was installed), so
/// it holds that person as its identity like any joined device
/// ([`tilo_identity::provisioning::complete_enrolled_device`]).
///
/// # Errors
///
/// Returns an error if the identity directory cannot be resolved or the
/// enrolled device cannot be completed.
pub fn complete_enrolled_device() -> io::Result<bool> {
    let store = tilo_identity::store::IdentityRegistryStore::from_env()?;
    tilo_identity::provisioning::complete_enrolled_device(
        store.identity_dir(),
        &crate::firstboot::collaboration_identity_paths(),
        tilo_clock::unix_now_ns(),
    )
}

pub fn needs_first_run_flow(identity_path: &Path) -> bool {
    match tilo_identity::store::load_identity_file(identity_path) {
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
    use tilo_identity::store::IdentityRecord;

    fn fixture_record() -> IdentityRecord {
        IdentityRecord::from_root(
            &ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng)),
            "alice",
            [248, 113, 113],
            SystemTime::UNIX_EPOCH
                .checked_add(std::time::Duration::from_secs(100))
                .unwrap_or(SystemTime::UNIX_EPOCH),
        )
    }

    /// Write a record private from creation, as every writer of the identity
    /// directory does.
    fn write_fixture_identity(path: &Path) -> IdentityRecord {
        let record = fixture_record();
        tilo_files::native::atomic::write_with_mode(path, &record.to_json_bytes().unwrap(), 0o600)
            .unwrap();
        record
    }

    tilo_test_env::scenario! {
        fn the_store_reads_what_the_first_run_flow_asks_about;

        phase a_stored_identity_reads_back_with_its_public_key;
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("primary.json");
            assert!(
                tilo_identity::store::load_identity_file(&path)
                    .expect("missing record")
                    .is_none()
            );
            let rec = write_fixture_identity(&path);
            let back = tilo_identity::store::load_identity_file(&path)
                .expect("stored record")
                .expect("a record");
            assert_eq!(back.display_name, rec.display_name);
            assert_eq!(back.public_key(), rec.public_key());
            assert_eq!(back.schema_version, 1);
            #[cfg(unix)]
            {
                let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "primary.json must be 0600, got {mode:o}");
            }
        }

        phase first_run_flow_triggers_only_on_missing_identity;
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("primary.json");
            assert!(needs_first_run_flow(&path));
            write_fixture_identity(&path);
            assert!(!needs_first_run_flow(&path));
        }

        phase only_the_secret_service_unavailable_error_enters_recovery;
        {
            assert!(is_secret_service_unavailable(
                &tilo_identity::store::secret_service_unavailable_error()
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
    }

    tilo_test_env::scenario! {
        #[cfg(unix)]
        #[cfg(unix)]
        fn identity_paths_never_follow_a_link_and_an_invalid_record_is_no_identity;

        phase load_refuses_an_identity_path_symlink;
        {
            use std::os::unix::fs::symlink;

            let tmp = tempfile::tempdir().expect("tempdir");
            let target = tmp.path().join("elsewhere.json");
            fs::write(&target, fixture_record().to_json_bytes().unwrap()).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
            let path = tmp.path().join("primary.json");
            symlink(&target, &path).unwrap();

            assert!(tilo_identity::store::load_identity_file(&path).is_err());
        }

        phase host_identity_tree_is_private_and_refuses_a_symlink_root;
        {
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

        phase invalid_existing_identity_keeps_trusted_setup_in_front_of_the_desktop;
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("primary.json");
            fs::write(&path, b"not an identity").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(
                needs_first_run_flow(&path),
                "an invalid identity must not let an identity-less desktop start"
            );
        }
    }

    tilo_test_env::scenario! {
        fn the_host_identity_root_and_files_paths_follow_the_session_environment;

        phase host_identity_root_uses_the_xdg_data_contract;
        {
            assert_eq!(
                host_identity_root(Some("/home/alice/.data".into()), Some("/ignored".into())).unwrap(),
                PathBuf::from("/home/alice/.data/tilo")
            );
            assert_eq!(
                host_identity_root(None, Some("/home/alice".into())).unwrap(),
                PathBuf::from("/home/alice/.local/share/tilo")
            );
        }

        phase host_identity_root_never_depends_on_the_working_directory;
        {
            assert!(host_identity_root(Some("relative".into()), Some("/home/alice".into())).is_err());
            assert!(host_identity_root(None, Some("relative".into())).is_err());
            assert!(host_identity_root(None, None).is_err());
        }

        phase host_files_paths_match_the_user_service;
        {
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
    }
}
