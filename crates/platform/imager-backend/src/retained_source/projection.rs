//! Source descriptor narrowing, not person or source-consent admission.
//!
//! The installed actor must still revalidate its canonical source and person
//! proof before and after using this handle. No pathname, digest or descriptor
//! accepted here can replace that admission.
use std::{ffi::OsStr, fs::File, io};

#[cfg(target_os = "linux")]
#[path = "projection/native.rs"]
mod native;

/// An exact held source object and its context-bound descriptive identity.
/// It cannot be deserialized or constructed from a raw descriptor number.
pub struct SourceProjection {
    #[cfg(target_os = "linux")]
    native: native::SourceProjection,
    #[cfg(not(target_os = "linux"))]
    _private: (),
}
impl SourceProjection {
    /// Narrow an already admitted Home to one existing, singly-linked file.
    /// # Errors
    /// Returns an error for non-root callers, invalid names, failed opens or
    /// metadata reads, nonregular or multiply linked files, or unsupported hosts.
    pub fn selected(directory: &File, name: &OsStr, context: &str) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            native::SourceProjection::selected(directory, name, context)
                .map(|native| Self { native })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (directory, name, context);
            Err(super::Unsupported.into())
        }
    }

    /// Pin the actual admitted Home directory, never a selected file's parent.
    /// # Errors
    /// Returns an error for non-root callers, descriptor duplication or metadata
    /// failures, a non-directory object, or an unsupported host.
    pub fn home(directory: &File, context: &str) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            native::SourceProjection::home(directory, context).map(|native| Self { native })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (directory, context);
            Err(super::Unsupported.into())
        }
    }

    /// Duplicate this held object; this does not grant additional source rights.
    /// # Errors
    /// Returns an error if descriptor duplication fails or the host is unsupported.
    pub fn duplicate_file(&self) -> io::Result<File> {
        #[cfg(target_os = "linux")]
        {
            self.native.duplicate_file()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(super::Unsupported.into())
        }
    }

    /// Descriptive identity only; callers must retain live source custody.
    pub fn context_sha256(&self) -> [u8; 32] {
        #[cfg(target_os = "linux")]
        {
            self.native.context_sha256()
        }
        #[cfg(not(target_os = "linux"))]
        {
            unreachable!("unsupported hosts cannot construct a source projection")
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod native_tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    #[test]
    fn public_selected_factory_never_accepts_path_link_or_directory() {
        let tree = tempfile::tempdir().unwrap();
        let directory = File::open(tree.path()).unwrap();
        File::create(tree.path().join("selected")).unwrap();
        symlink("selected", tree.path().join("alias")).unwrap();
        std::fs::create_dir(tree.path().join("directory")).unwrap();
        for name in ["", ".", "..", "../selected", "alias", "directory", "nul\0"] {
            assert!(SourceProjection::selected(&directory, OsStr::new(name), "context").is_err());
        }
        std::fs::hard_link(tree.path().join("selected"), tree.path().join("hardlink")).unwrap();
        assert!(SourceProjection::selected(&directory, OsStr::new("selected"), "context").is_err());
        let file = File::open(tree.path().join("selected")).unwrap();
        assert!(SourceProjection::home(&file, "context").is_err());
    }

    #[test]
    fn public_factory_retains_exact_object_or_refuses_unprivileged_caller() {
        let tree = tempfile::tempdir().unwrap();
        let directory = File::open(tree.path()).unwrap();
        File::create(tree.path().join("selected")).unwrap();
        let selected = SourceProjection::selected(&directory, OsStr::new("selected"), "context");
        let home = SourceProjection::home(&directory, "context");
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            for result in [selected, home] {
                assert_eq!(
                    result.err().unwrap().kind(),
                    io::ErrorKind::PermissionDenied
                );
            }
            return;
        }
        let selected = selected.unwrap();
        let home = home.unwrap();
        let expected = selected.duplicate_file().unwrap().metadata().unwrap();
        std::fs::rename(tree.path().join("selected"), tree.path().join("retained")).unwrap();
        File::create(tree.path().join("selected")).unwrap();
        let held = selected.duplicate_file().unwrap().metadata().unwrap();
        let replacement =
            SourceProjection::selected(&directory, OsStr::new("selected"), "context").unwrap();
        let other_context = SourceProjection::home(&directory, "other-context").unwrap();
        assert_eq!((held.dev(), held.ino()), (expected.dev(), expected.ino()));
        assert_ne!(selected.context_sha256(), replacement.context_sha256());
        assert_ne!(selected.context_sha256(), home.context_sha256());
        assert_ne!(home.context_sha256(), other_context.context_sha256());
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn unsupported_host_refuses_source_projection() {
        let file = tempfile::tempfile().unwrap();
        for result in [
            SourceProjection::home(&file, "context"),
            SourceProjection::selected(&file, OsStr::new("selected"), "context"),
        ] {
            let error = result.err().expect("no native source projection");
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert!(error.get_ref().unwrap().is::<super::super::Unsupported>());
        }
    }
}
