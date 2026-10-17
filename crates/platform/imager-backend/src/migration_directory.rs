//! Pinned directory capabilities shared by inventory and verified copy consumers.
use crate::{
    destination_security,
    nofollow::{
        metadata_nofollow, open_directory_nofollow, reject_windows_placeholder,
        same_directory_identity,
    },
};
use cap_fs_ext::OpenOptionsFollowExt;
use cap_primitives::fs::{FollowSymlinks, Metadata, OpenOptions};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};
fn changed(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
/// Each edge retains both the parent capability and the observed child identity.
/// Sharing ancestors bounds descriptors to the depth of the active traversal.
pub struct PinnedDirectory {
    pub file: File,
    pub identity: Metadata,
    parent: Option<(Arc<Self>, OsString)>,
    created: bool,
}

impl PinnedDirectory {
    /// Admit a detached directory only after its caller has validated custody.
    /// Destination namespace protections are independently checked here.
    /// # Errors
    /// Refuses unreadable metadata or an unprotected destination namespace.
    pub fn from_admitted_directory(file: File) -> io::Result<Arc<Self>> {
        let pin = Arc::new(Self {
            identity: Metadata::from_file(&file)?,
            file,
            parent: None,
            created: false,
        });
        pin.validate_destination()?;
        Ok(pin)
    }
    /// Open an exact absolute grant without following links or recalling cloud data.
    ///
    /// # Errors
    /// Rejects non-normal paths, unsupported entries and native open failures.
    pub fn open_absolute(path: &Path) -> io::Result<Arc<Self>> {
        if !path.is_absolute() {
            return Err(changed("folder grant must be a native absolute path"));
        }
        let mut anchor = PathBuf::new();
        let mut names = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(_) | Component::RootDir if names.is_empty() => {
                    anchor.push(component);
                }
                Component::Normal(name) => names.push(name),
                _ => return Err(changed("folder grant contains a non-normal component")),
            }
        }
        let file =
            cap_primitives::fs::open_ambient_dir(&anchor, cap_primitives::ambient_authority())?;
        let identity = Metadata::from_file(&file)?;
        let mut directory = Arc::new(Self {
            file,
            identity,
            parent: None,
            created: false,
        });
        for name in names {
            let metadata = metadata_nofollow(&directory.file, Path::new(name))?;
            directory = directory.open_child(name, &metadata)?;
        }
        Ok(directory)
    }

    /// # Errors
    /// Rejects unsupported metadata, changed identity and native open failures.
    pub fn open_child(
        self: &Arc<Self>,
        name: &OsStr,
        expected: &Metadata,
    ) -> io::Result<Arc<Self>> {
        reject_windows_placeholder(expected)?;
        let file = open_directory_nofollow(&self.file, Path::new(name), expected)?;
        Ok(Arc::new(Self {
            identity: Metadata::from_file(&file)?,
            file,
            parent: Some((Arc::clone(self), name.to_os_string())),
            created: false,
        }))
    }

    /// # Errors
    /// Refuses changed, unreadable, linked or cloud-unavailable grant ancestors.
    pub fn validate(&self) -> io::Result<()> {
        let mut current = self;
        while let Some((parent, name)) = &current.parent {
            let metadata = metadata_nofollow(&parent.file, Path::new(name))?;
            if metadata.is_symlink()
                || !metadata.is_dir()
                || !same_directory_identity(&metadata, &current.identity)
            {
                return Err(changed(
                    "directory identity changed; select the folder again",
                ));
            }
            reject_windows_placeholder(&metadata)?;
            current = parent;
        }
        Ok(())
    }

    pub fn contains_identity(&self, identity: &Metadata) -> bool {
        let mut current = self;
        loop {
            if same_directory_identity(&current.identity, identity) {
                return true;
            }
            match &current.parent {
                Some((parent, _)) => current = parent,
                None => return false,
            }
        }
    }

    /// # Errors
    /// Refuses changed identity or insufficient namespace protection at any ancestor.
    pub fn validate_destination(&self) -> io::Result<()> {
        self.validate()?;
        let mut current = self;
        let mut child = None;
        loop {
            destination_security::check_directory(&current.file, child.as_ref(), current.created)?;
            child = Some(Metadata::from_file(&current.file)?);
            match &current.parent {
                Some((parent, _)) => current = parent,
                None => break,
            }
        }
        Ok(())
    }

    /// # Errors
    /// Refuses unprotected namespaces, existing names and native create/open failures.
    pub fn create_child(self: &Arc<Self>, name: &OsStr) -> io::Result<Arc<Self>> {
        // Establish namespace protection BEFORE mkdir. Observation after an
        // unprotected mkdir could adopt another account's replacement.
        self.validate_destination()?;
        destination_security::create_directory(&self.file, name)?;
        let expected = metadata_nofollow(&self.file, Path::new(name))?;
        let file = open_directory_nofollow(&self.file, Path::new(name), &expected)?;
        destination_security::check_private_file(&file)?;
        let child = Arc::new(Self {
            identity: Metadata::from_file(&file)?,
            file,
            parent: Some((Arc::clone(self), name.to_os_string())),
            created: true,
        });
        child.validate_destination()?;
        Ok(child)
    }
}

pub fn same_snapshot(before: &Metadata, after: &Metadata) -> bool {
    if !same_directory_identity(before, after)
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || before.permissions() != after.permissions()
        || before.is_file() != after.is_file()
        || before.is_dir() != after.is_dir()
        || after.is_symlink()
    {
        return false;
    }
    #[cfg(unix)]
    {
        use cap_primitives::fs::MetadataExt;
        if before.ctime() != after.ctime() || before.ctime_nsec() != after.ctime_nsec() {
            return false;
        }
    }
    #[cfg(windows)]
    {
        use cap_primitives::fs::MetadataExt;
        if before.file_attributes() != after.file_attributes() {
            return false;
        }
    }
    true
}

/// Preserve timestamps only after reopening the operation-owned destination.
///
/// # Errors
/// Refuses changed identity, unsupported timestamp fidelity or native I/O failures.
pub fn preserve_directory_times_values(
    directory: &PinnedDirectory,
    accessed: std::time::SystemTime,
    modified: std::time::SystemTime,
) -> io::Result<()> {
    use cap_fs_ext::OpenOptionsMaybeDirExt;
    directory.validate_destination()?;
    let (parent, name) = directory
        .parent
        .as_ref()
        .ok_or_else(|| changed("destination has no pinned parent"))?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .maybe_dir(true)
        .follow(FollowSymlinks::No);
    #[cfg(windows)]
    {
        use cap_primitives::fs::OpenOptionsExt;
        options.access_mode(0x80 | 0x100); // FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES
    }
    let writable = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
    let observed = Metadata::from_file(&writable)?;
    if !observed.is_dir() || !same_directory_identity(&directory.identity, &observed) {
        return Err(changed("destination changed before timestamp preservation"));
    }
    preserve_time_values(&writable, accessed, modified)
}

/// # Errors
/// Refuses failed or inexact timestamp preservation.
pub fn preserve_time_values(
    file: &File,
    accessed: std::time::SystemTime,
    modified: std::time::SystemTime,
) -> io::Result<()> {
    file.set_times(
        fs::FileTimes::new()
            .set_accessed(accessed)
            .set_modified(modified),
    )?;
    let actual = file.metadata()?;
    if actual.modified()? != modified || actual.accessed()? != accessed {
        return Err(changed(
            "destination cannot preserve source timestamps exactly",
        ));
    }
    Ok(())
}
