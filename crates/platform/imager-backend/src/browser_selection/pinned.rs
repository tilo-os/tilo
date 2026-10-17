//! Read-only source pins retain and revalidate every ancestor edge.
use crate::nofollow::{
    metadata_nofollow, open_directory_nofollow, reject_windows_placeholder, same_directory_identity,
};
use cap_primitives::fs::Metadata;
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) struct PinnedDirectory {
    pub(crate) file: File,
    identity: Metadata,
    parent: Option<(Arc<Self>, OsString)>,
}
impl PinnedDirectory {
    pub(crate) fn open_absolute(path: &Path) -> io::Result<Arc<Self>> {
        if !path.is_absolute() {
            return Err(invalid("folder grant must be a native absolute path"));
        }
        let mut anchor = PathBuf::new();
        let mut names = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(_) | Component::RootDir if names.is_empty() => {
                    anchor.push(component);
                }
                Component::Normal(name) => names.push(name),
                _ => return Err(invalid("folder grant contains a non-normal component")),
            }
        }
        let file =
            cap_primitives::fs::open_ambient_dir(&anchor, cap_primitives::ambient_authority())?;
        let identity = Metadata::from_file(&file)?;
        let mut directory = Arc::new(Self {
            file,
            identity,
            parent: None,
        });
        for name in names {
            let metadata = metadata_nofollow(&directory.file, Path::new(name))?;
            directory = directory.open_child(name, &metadata)?;
        }
        Ok(directory)
    }

    pub(crate) fn open_child(
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
        }))
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        let mut current = self;
        while let Some((parent, name)) = &current.parent {
            let metadata = metadata_nofollow(&parent.file, Path::new(name))?;
            if metadata.is_symlink()
                || !metadata.is_dir()
                || !same_directory_identity(&metadata, &current.identity)
            {
                return Err(invalid(
                    "directory identity changed; select the folder again",
                ));
            }
            reject_windows_placeholder(&metadata)?;
            current = parent;
        }
        Ok(())
    }
}
