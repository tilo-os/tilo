//! Backend-owned preparation workspace. A pathname is never a source grant.
use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionIntent {
    PrepareMedia,
    AmendOfflineMedia,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct SessionProjection {
    pub intent: SessionIntent,
}

/// Custody is created by the backend, never adopted from a frontend pathname.
/// This workspace confers no authority over original files or native services.
pub struct BackendSession {
    #[cfg(unix)]
    directory: tempfile::TempDir,
    canonical: PathBuf,
    #[cfg(unix)]
    pin: std::fs::File,
    #[cfg(unix)]
    admitted_uid: u32,
    intent: SessionIntent,
}
impl BackendSession {
    /// # Errors
    /// Always returns an unsupported-platform error.
    #[cfg(not(unix))]
    pub fn create(_intent: SessionIntent) -> io::Result<Self> {
        Err(crate::journey_media::Unsupported.into())
    }

    /// Create a private workspace owned by the effective user.
    #[cfg(unix)]
    pub fn create(intent: SessionIntent) -> io::Result<Self> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let directory = tempfile::Builder::new()
            .prefix(match intent {
                SessionIntent::PrepareMedia => "tilo-prepared-media-",
                SessionIntent::AmendOfflineMedia => "tilo-pending-offline-",
            })
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let pin = std::fs::File::open(directory.path())?;
        let metadata = pin.metadata()?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o777 != 0o700
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let canonical = directory.path().canonicalize()?;
        let owner = Self {
            directory,
            canonical,
            pin,
            admitted_uid: metadata.uid(),
            intent,
        };
        owner.validate()?;
        Ok(owner)
    }

    pub fn projection(&self) -> SessionProjection {
        SessionProjection {
            intent: self.intent,
        }
    }

    /// # Errors
    /// Always returns an unsupported-platform error.
    #[cfg(not(unix))]
    pub fn validate(&self) -> io::Result<()> {
        Err(crate::journey_media::Unsupported.into())
    }

    /// Recheck workspace identity, ownership, and private permissions.
    #[cfg(unix)]
    pub fn validate(&self) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let held = self.pin.metadata()?;
        let named = std::fs::symlink_metadata(&self.canonical)?;
        let original = std::fs::symlink_metadata(self.directory.path())?;
        if !named.is_dir()
            || original.dev() != held.dev()
            || original.ino() != held.ino()
            || named.dev() != held.dev()
            || named.ino() != held.ino()
            || held.uid() != self.admitted_uid
            || named.uid() != self.admitted_uid
            || rustix::process::geteuid().as_raw() != self.admitted_uid
            || held.mode() & 0o777 != 0o700
            || named.mode() & 0o777 != 0o700
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }

    /// Compatibility view for preparation workers still consuming native paths.
    /// This does not grant custody of any path selected by a file chooser.
    pub fn path(&self) -> &Path {
        &self.canonical
    }
}

pub fn preparation_namespace() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|path| !path.is_empty())
                .map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(
        resolve_state_root(&state)
            .ok()?
            .join("tilo/installer/preparations"),
    )
}

/// Resolve only the selected state-root alias; descendants retain their own
/// descriptor-relative nofollow and private-directory admission checks.
pub fn resolve_state_root(path: &Path) -> io::Result<PathBuf> {
    use std::path::Component;
    if !path.is_absolute()
        || path.components().any(|component| {
            !matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Installer state root must be an absolute directory",
        ));
    }
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let mut resolved = ancestor.canonicalize()?;
                if !std::fs::metadata(&resolved)?.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        "Installer state root is not a directory",
                    ));
                }
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(ancestor.file_name().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "Missing state root")
                })?);
                ancestor = ancestor.parent().ok_or(error)?;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn backend_owner_rejects_permission_widening() {
        use std::os::unix::fs::PermissionsExt;
        let owner = BackendSession::create(SessionIntent::PrepareMedia).unwrap();
        std::fs::set_permissions(owner.path(), std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(owner.validate().is_err());
        std::fs::set_permissions(owner.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        owner.validate().unwrap();
    }

    #[test]
    fn backend_owner_detects_namespace_replacement() {
        let owner = BackendSession::create(SessionIntent::PrepareMedia).unwrap();
        owner.validate().unwrap();
        let displaced = owner.path().with_extension("displaced");
        std::fs::rename(owner.path(), &displaced).unwrap();
        std::fs::create_dir(owner.path()).unwrap();
        assert!(owner.validate().is_err());
        std::fs::remove_dir(owner.path()).unwrap();
        std::fs::rename(displaced, owner.path()).unwrap();
        owner.validate().unwrap();
    }
}
