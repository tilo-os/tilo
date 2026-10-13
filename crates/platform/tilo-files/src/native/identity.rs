//! A file's durable identity: a 16-byte id stored on the file itself, in an
//! extended attribute, so it follows the file through a rename or a move
//! while a copy, or another file with the same bytes, is a different file.
//!
//! On Linux the attribute holds the id alone, which is also how a
//! collaborative filesystem can serve an id of its own there. macOS carries
//! extended attributes along on every copy — Finder's, `cp`'s and
//! `std::fs::copy`'s, which clones the file on APFS — so there the id is
//! followed by the inode it was minted for, and an id found on any other
//! inode is one a copy inherited: the copy is given its own. A rename or a
//! move within a volume keeps the inode; a move to another volume is a copy.
//!
//! Elsewhere files carry no id and every call reports none.

use std::io;
use std::path::Path;

/// A file's id: 16 bytes, never all zero.
pub type FileId = [u8; 16];

/// The id stored on `path` under `attribute`, if it carries a well-formed
/// one. Follows symlinks, as opening the file does.
#[must_use]
pub fn read(path: &Path, attribute: &str) -> Option<FileId> {
    platform::read(path, attribute)
}

/// The id stored on `path` under `attribute`, storing `fresh` first when the
/// path names a regular file that carries none of its own.
///
/// The write is create-only: when another process stores an id between the
/// read and the write, its id is the one returned, so both agree. `Ok(None)`
/// when the path is not a regular file, or `fresh` is all zero.
///
/// # Errors
///
/// The failure to store an id: a filesystem without extended attributes, a
/// read-only file, or a target that has no file ids at all
/// ([`io::ErrorKind::Unsupported`]).
pub fn read_or_mint(path: &Path, attribute: &str, fresh: FileId) -> io::Result<Option<FileId>> {
    platform::read_or_mint(path, attribute, fresh)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod platform {
    use super::FileId;
    use std::ffi::CString;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// An all-zero id is no id.
    fn usable(id: FileId) -> Option<FileId> {
        (id != [0; 16]).then_some(id)
    }

    fn c_strings(path: &Path, attribute: &str) -> io::Result<(CString, CString)> {
        let invalid = |_| io::Error::from(io::ErrorKind::InvalidInput);
        Ok((
            CString::new(path.as_os_str().as_bytes()).map_err(invalid)?,
            CString::new(attribute).map_err(invalid)?,
        ))
    }

    /// The attribute's value, when it is exactly `N` bytes long.
    fn get<const N: usize>(path: &CString, name: &CString) -> Option<[u8; N]> {
        let mut buf = [0_u8; N];
        // SAFETY: both strings are NUL-terminated and `buf` is writable for
        // its full length, which is what we pass.
        #[cfg(target_os = "linux")]
        let n = unsafe {
            libc::getxattr(
                path.as_ptr(),
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        // SAFETY: as above; position 0 and no options read the whole value.
        #[cfg(target_os = "macos")]
        let n = unsafe {
            libc::getxattr(
                path.as_ptr(),
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                0,
                0,
            )
        };
        (usize::try_from(n).ok() == Some(N)).then_some(buf)
    }

    /// Store `value` unless the attribute exists. `Ok(false)` when it does.
    fn create(path: &CString, name: &CString, value: &[u8]) -> io::Result<bool> {
        // SAFETY: NUL-terminated strings and a readable value of the length
        // passed.
        #[cfg(target_os = "linux")]
        let rc = unsafe {
            libc::setxattr(
                path.as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                libc::XATTR_CREATE,
            )
        };
        // SAFETY: as above, at position 0.
        #[cfg(target_os = "macos")]
        let rc = unsafe {
            libc::setxattr(
                path.as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                libc::XATTR_CREATE,
            )
        };
        if rc == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    #[cfg(target_os = "linux")]
    pub fn read(path: &Path, attribute: &str) -> Option<FileId> {
        let (path, name) = c_strings(path, attribute).ok()?;
        get::<16>(&path, &name).and_then(usable)
    }

    #[cfg(target_os = "linux")]
    pub fn read_or_mint(path: &Path, attribute: &str, fresh: FileId) -> io::Result<Option<FileId>> {
        if let Some(id) = read(path, attribute) {
            return Ok(Some(id));
        }
        if usable(fresh).is_none() || !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
            return Ok(None);
        }
        let (c_path, name) = c_strings(path, attribute)?;
        create(&c_path, &name, &fresh)?;
        // Read back rather than trusting the value written: the stored
        // attribute is the identity, whoever wrote it.
        Ok(read(path, attribute))
    }

    /// The id's 16 bytes, then its inode's 8, little-endian.
    #[cfg(target_os = "macos")]
    const STORED: usize = 24;

    #[cfg(target_os = "macos")]
    fn read_for(path: &CString, name: &CString, ino: u64) -> Option<FileId> {
        let stored = get::<STORED>(path, name)?;
        let (id, owner) = stored.split_at(16);
        if owner != ino.to_le_bytes() {
            return None;
        }
        usable(id.try_into().ok()?)
    }

    #[cfg(target_os = "macos")]
    pub fn read(path: &Path, attribute: &str) -> Option<FileId> {
        use std::os::unix::fs::MetadataExt;
        let ino = std::fs::metadata(path).ok()?.ino();
        let (path, name) = c_strings(path, attribute).ok()?;
        read_for(&path, &name, ino)
    }

    #[cfg(target_os = "macos")]
    pub fn read_or_mint(path: &Path, attribute: &str, fresh: FileId) -> io::Result<Option<FileId>> {
        use std::os::unix::fs::MetadataExt;
        let Ok(metadata) = std::fs::metadata(path) else {
            return Ok(None);
        };
        let ino = metadata.ino();
        let (c_path, name) = c_strings(path, attribute)?;
        if let Some(id) = read_for(&c_path, &name, ino) {
            return Ok(Some(id));
        }
        if usable(fresh).is_none() || !metadata.is_file() {
            return Ok(None);
        }
        // An id bound to another inode came with a copy: drop it so the
        // write below can stay create-only. There being none is fine.
        // SAFETY: NUL-terminated strings.
        unsafe {
            libc::removexattr(c_path.as_ptr(), name.as_ptr(), 0);
        }
        let mut value = [0_u8; STORED];
        let (id, owner) = value.split_at_mut(16);
        id.copy_from_slice(&fresh);
        owner.copy_from_slice(&ino.to_le_bytes());
        create(&c_path, &name, &value)?;
        Ok(read_for(&c_path, &name, ino))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::FileId;
    use std::io;
    use std::path::Path;

    pub fn read(_path: &Path, _attribute: &str) -> Option<FileId> {
        None
    }

    pub fn read_or_mint(
        _path: &Path,
        _attribute: &str,
        _fresh: FileId,
    ) -> io::Result<Option<FileId>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    const ATTRIBUTE: &str = "user.tilo.test-id";

    #[test]
    fn an_id_follows_a_rename_but_not_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("a.pdf");
        std::fs::write(&first, b"same bytes").unwrap();
        let id = read_or_mint(&first, ATTRIBUTE, [1; 16]).unwrap();
        assert_eq!(id, Some([1; 16]));
        assert_eq!(
            read_or_mint(&first, ATTRIBUTE, [2; 16]).unwrap(),
            id,
            "an id once stored is kept"
        );

        let renamed = dir.path().join("b.pdf");
        std::fs::rename(&first, &renamed).unwrap();
        assert_eq!(read(&renamed, ATTRIBUTE), id, "a rename keeps the id");

        // On macOS the copy carries the attribute along; it is still a
        // different file.
        let copy = dir.path().join("b copy.pdf");
        std::fs::copy(&renamed, &copy).unwrap();
        assert_eq!(read(&copy, ATTRIBUTE), None, "a copy has no id of its own");
        assert_eq!(
            read_or_mint(&copy, ATTRIBUTE, [3; 16]).unwrap(),
            Some([3; 16])
        );
        assert_eq!(read(&renamed, ATTRIBUTE), id, "the original keeps its id");

        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(read_or_mint(&folder, ATTRIBUTE, [4; 16]).unwrap(), None);
        let missing = dir.path().join("missing.pdf");
        assert_eq!(read_or_mint(&missing, ATTRIBUTE, [5; 16]).unwrap(), None);
        assert!(!missing.exists());
    }
}
