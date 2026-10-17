use sha2::{Digest, Sha256};
use std::{
    ffi::{CString, OsStr},
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
};

fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn root() -> io::Result<()> {
    if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
        return Err(refused());
    }
    Ok(())
}

pub(super) struct SourceProjection {
    file: File,
    context: [u8; 32],
}
impl SourceProjection {
    pub(super) fn selected(directory: &File, name: &OsStr, context: &str) -> io::Result<Self> {
        root()?;
        let file = selected(directory, name)?;
        Self::pin(file, false, context)
    }
    pub(super) fn home(directory: &File, context: &str) -> io::Result<Self> {
        root()?;
        Self::pin(directory.try_clone()?, true, context)
    }
    fn pin(file: File, home: bool, context: &str) -> io::Result<Self> {
        let facts = file.metadata()?;
        if facts.is_dir() != home || (!home && !facts.is_file()) {
            return Err(refused());
        }
        let mut hash = Sha256::new();
        hash.update(if home {
            b"tilo-retained-home-source-v1\0".as_slice()
        } else {
            b"tilo-retained-selected-source-v1\0".as_slice()
        });
        hash.update(context.as_bytes());
        hash.update(facts.dev().to_le_bytes());
        hash.update(facts.ino().to_le_bytes());
        Ok(Self {
            file,
            context: hash.finalize().into(),
        })
    }
    pub(super) fn duplicate_file(&self) -> io::Result<File> {
        self.file.try_clone()
    }
    pub(super) fn context_sha256(&self) -> [u8; 32] {
        self.context
    }
}

// No source path is opened: this only narrows the actor's held directory.
fn selected(directory: &File, name: &OsStr) -> io::Result<File> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 255
        || bytes == b"."
        || bytes == b".."
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        return Err(refused());
    }
    let name = CString::new(bytes).map_err(|_| refused())?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let facts = file.metadata()?;
    if !facts.is_file() || facts.nlink() != 1 {
        return Err(refused());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn selected_factory_refuses_paths_links_and_nonregular_objects() {
        let tree = tempfile::tempdir().unwrap();
        let directory = File::open(tree.path()).unwrap();
        File::create(tree.path().join("selected")).unwrap();
        symlink("selected", tree.path().join("symlink")).unwrap();
        std::fs::create_dir(tree.path().join("directory")).unwrap();
        let fifo = CString::new(tree.path().join("fifo").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        selected(&directory, OsStr::new("selected")).unwrap();
        for name in [
            "",
            ".",
            "..",
            "../selected",
            "selected/child",
            "symlink",
            "directory",
            "fifo",
            "nul\0",
        ] {
            assert!(selected(&directory, OsStr::new(name)).is_err(), "{name:?}");
        }
        std::fs::hard_link(tree.path().join("selected"), tree.path().join("alias")).unwrap();
        assert!(selected(&directory, OsStr::new("selected")).is_err());
    }

    #[test]
    fn held_projection_digest_preserves_domains_context_and_object_identity() {
        let tree = tempfile::tempdir().unwrap();
        let directory = File::open(tree.path()).unwrap();
        File::create(tree.path().join("selected")).unwrap();
        let file = selected(&directory, OsStr::new("selected")).unwrap();
        let projection = SourceProjection::pin(file, false, "context").unwrap();
        let held = projection.duplicate_file().unwrap().metadata().unwrap();
        let mut expected = Sha256::new();
        expected.update(b"tilo-retained-selected-source-v1\0");
        expected.update(b"context");
        expected.update(held.dev().to_le_bytes());
        expected.update(held.ino().to_le_bytes());
        assert_eq!(
            projection.context_sha256(),
            <[u8; 32]>::from(expected.finalize())
        );
        let other =
            SourceProjection::pin(projection.duplicate_file().unwrap(), false, "other").unwrap();
        assert_ne!(projection.context_sha256(), other.context_sha256());
        assert!(SourceProjection::pin(directory, false, "context").is_err());
        assert!(
            SourceProjection::pin(projection.duplicate_file().unwrap(), true, "context").is_err()
        );
    }

    #[test]
    fn ordinary_process_cannot_construct_source_projection() {
        let file = tempfile::tempfile().unwrap();
        // The production root check runs before even inspecting the source.
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            let error = SourceProjection::home(&file, "context").err().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            let error = SourceProjection::selected(&file, OsStr::new("selected"), "context")
                .err()
                .unwrap();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        } else {
            assert!(SourceProjection::home(&file, "context").is_err());
        }
    }
}
