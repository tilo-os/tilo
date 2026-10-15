//! No regular scratch inode may have an additional name outside scratch.
//! This checks scratch only: valid source-side hardlinks remain supported.
use super::{AsRawFd, File, MetadataExt, identity, io, refuse};
use std::{os::fd::FromRawFd, os::unix::ffi::OsStrExt};
const ENTRIES_MAX: usize = 65536;
const DEPTH_MAX: usize = 64;
#[derive(PartialEq, Eq)]
struct Stamp {
    identity: (u64, u64),
    links: u64,
    length: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}
fn fact(file: &File) -> io::Result<Stamp> {
    let m = file.metadata()?;
    Ok(Stamp {
        identity: (m.dev(), m.ino()),
        links: m.nlink(),
        length: m.len(),
        mode: m.mode(),
        uid: m.uid(),
        gid: m.gid(),
        modified: (m.mtime(), m.mtime_nsec()),
        changed: (m.ctime(), m.ctime_nsec()),
    })
}
pub(super) fn check(directory: &File) -> io::Result<()> {
    let mount = identity(directory)?.2;
    let uid = directory.metadata()?.uid();
    let mut count = 0;
    walk(directory, mount, uid, 0, &mut count)
}
fn walk(directory: &File, mount: u64, uid: u32, depth: usize, count: &mut usize) -> io::Result<()> {
    if depth > DEPTH_MAX || identity(directory)?.2 != mount {
        return Err(refuse("Inventory scratch subtree exceeds scope"));
    }
    let before = fact(directory)?;
    if before.uid != uid || before.mode & 0o7777 != 0o700 {
        return Err(refuse("Inventory scratch directory is not caller-private"));
    }
    let entries = std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?;
    for name in entries {
        *count = count
            .checked_add(1)
            .ok_or_else(|| refuse("Inventory scratch count overflow"))?;
        if *count > ENTRIES_MAX {
            return Err(refuse("Inventory scratch membership exceeds bound"));
        }
        let name = name?.file_name();
        let name = std::ffi::CString::new(name.as_bytes()).map_err(io::Error::other)?;
        let raw = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let entry = unsafe { File::from_raw_fd(raw) };
        let metadata = entry.metadata()?;
        if metadata.uid() != uid || metadata.mode() & 0o7077 != 0 {
            return Err(refuse("Inventory scratch entry is not caller-private"));
        }
        if identity(&entry)?.2 != mount {
            return Err(refuse("Inventory scratch contains a mounted source"));
        }
        if metadata.is_dir() {
            walk(
                &entry,
                mount,
                uid,
                depth
                    .checked_add(1)
                    .ok_or_else(|| refuse("Inventory scratch depth overflow"))?,
                count,
            )?;
        } else if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(refuse(
                "Inventory scratch contains a linked or unsupported inode",
            ));
        }
        // Reopen the exact child again; directory facts bracket the complete
        // enumeration and reject link/unlink/replacement during this check.
        let raw = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let named = unsafe { File::from_raw_fd(raw) };
        if fact(&entry)? != fact(&named)? || identity(&entry)? != identity(&named)? {
            return Err(refuse("Inventory scratch entry changed during review"));
        }
    }
    if fact(directory)? != before || identity(directory)?.2 != mount {
        return Err(refuse("Inventory scratch directory changed during review"));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, os::unix::fs::PermissionsExt};
    #[test]
    fn actual_scratch_hardlink_refuses_but_source_only_hardlinks_and_disjoint_siblings_remain_valid()
     {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let scratch = root.path().join("scratch");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&scratch).unwrap();
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700)).unwrap();
        let original = source.join("document");
        std::fs::write(&original, b"source bytes").unwrap();
        std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&original, source.join("source-alias")).unwrap();
        let held = File::open(&scratch).unwrap();
        check(&held).unwrap();
        std::fs::write(scratch.join("inventory"), b"private inventory bytes").unwrap();
        std::fs::set_permissions(
            scratch.join("inventory"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        check(&held).unwrap();
        std::fs::hard_link(&original, scratch.join("outside-alias")).unwrap();
        assert_eq!(
            check(&held).unwrap_err().to_string(),
            "Inventory scratch contains a linked or unsupported inode"
        );
        std::fs::remove_file(scratch.join("outside-alias")).unwrap();
        check(&held).unwrap();
        std::os::unix::fs::symlink(&original, scratch.join("outside-symlink")).unwrap();
        assert!(check(&held).is_err());
        let mut bytes = Vec::new();
        File::open(original)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"source bytes");
    }
}
