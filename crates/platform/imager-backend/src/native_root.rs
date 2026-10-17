//! Fixed native service directory admission. Directory identity is not a source
//! grant: source scope still comes exclusively from the signed shim's descriptors.
use std::{
    ffi::CString,
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt, fs::OpenOptionsExt},
    },
    path::{Component, Path},
};

struct Edge {
    parent: File,
    name: CString,
    identity: (u64, u64),
}

/// No public path/descriptor constructor: only the fixed service role can admit
/// this root, and the actor retains every ancestor until native work has stopped.
pub(crate) struct NativeRoot {
    file: File,
    edges: Vec<Edge>,
}

fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

impl NativeRoot {
    pub(crate) fn retained_service() -> io::Result<Self> {
        let uid = rustix::process::getuid().as_raw();
        if uid == 0 || rustix::process::geteuid().as_raw() != uid {
            return Err(refused());
        }
        let root = Self::open(Path::new(launcher::appdir::SANDBOX_APP_DIR))?;
        let facts = root.file.metadata()?;
        if facts.uid() != uid || facts.mode() & 0o077 != 0 {
            return Err(refused());
        }
        root.validate()?;
        Ok(root)
    }

    fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(refused());
        }
        let mut file = File::options()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")?;
        let mut edges = Vec::new();
        for component in path.components() {
            let name = match component {
                Component::RootDir => continue,
                Component::Normal(name) => CString::new(name.as_bytes()).map_err(|_| refused())?,
                _ => return Err(refused()),
            };
            // SAFETY: held parent and NUL-terminated single normal component.
            let raw = unsafe {
                libc::openat(
                    file.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful openat returns a new uniquely owned descriptor.
            let child = unsafe { File::from_raw_fd(raw) };
            let facts = child.metadata()?;
            edges.push(Edge {
                parent: file,
                name,
                identity: (facts.dev(), facts.ino()),
            });
            file = child;
        }
        let root = Self { file, edges };
        root.validate()?;
        Ok(root)
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        for edge in &self.edges {
            // SAFETY: initialized output buffer, held parent, single component.
            let mut facts: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe {
                libc::fstatat(
                    edge.parent.as_raw_fd(),
                    edge.name.as_ptr(),
                    &raw mut facts,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            if facts.st_mode & libc::S_IFMT != libc::S_IFDIR
                || (facts.st_dev, facts.st_ino) != edge.identity
            {
                return Err(refused());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replaced_ancestor_does_not_reauthorize_the_held_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("selected");
        std::fs::create_dir(&selected).unwrap();
        let root = NativeRoot::open(&selected).unwrap();
        let held = root.file().metadata().unwrap();
        std::fs::rename(&selected, temporary.path().join("old")).unwrap();
        std::fs::create_dir(&selected).unwrap();
        assert!(root.validate().is_err());
        assert_eq!(root.file().metadata().unwrap().ino(), held.ino());
    }
    #[test]
    fn symlinked_roots_are_not_native_authority() {
        let temporary = tempfile::tempdir().unwrap();
        let link = temporary.path().join("link");
        std::os::unix::fs::symlink(temporary.path(), &link).unwrap();
        assert!(NativeRoot::open(&link).is_err());
    }
}
