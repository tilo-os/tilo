//! Current privacy on local Linux POSIX ACL filesystems, without userspace ACL
//! enumeration. `ACL_MASK` is reflected in group mode bits and gates named users
//! and groups. Kernel/capability and hostile same-principal bypasses are outside
//! this discretionary privacy contract. tmpfs is not persistence evidence.
use std::{
    fs::{File, Metadata},
    io,
    os::unix::fs::MetadataExt,
};

pub(super) fn validate_private(file: &File, metadata: &Metadata) -> io::Result<()> {
    // Inspect this actual opened object, never infer its profile from a parent.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "statfs magic numbers are 32-bit; the width and sign of `f_type` vary by target"
    )]
    let profile = rustix::fs::fstatfs(file)?.f_type as u32;
    if !matches!(profile, 0xef53 | 0x9123_683e | 0x5846_5342 | 0x0102_1994) {
        return Err(io::ErrorKind::Unsupported.into());
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}

/// Only the shared exclusive-mkdir custody invokes this fresh-only operation.
pub(super) fn normalize_fresh_empty(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_dir() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    validate_private(file, &metadata)?;
    empty(file)?;
    // POSIX chmod updates the effective ACL mask; inherited named/default ACLs
    // remain intact. No parent or adopted object is normalized here.
    rustix::fs::fchmod(file, rustix::fs::Mode::from_raw_mode(0o700))?;
    validate_private(file, &file.metadata()?)?;
    empty(file)
}

fn empty(file: &File) -> io::Result<()> {
    use rustix::fs::{Mode, OFlags, RawDir};
    use std::mem::MaybeUninit;
    // An independent open description is essential: dup would share the old
    // directory cursor, letting a retry mistake a previous EOF for emptiness.
    let scan = File::from(rustix::fs::openat(
        file,
        c".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?);
    let original = file.metadata()?;
    let scanned = scan.metadata()?;
    if !original.is_dir()
        || !scanned.is_dir()
        || original.dev() != scanned.dev()
        || original.ino() != scanned.ino()
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    validate_private(&scan, &scanned)?;
    let mut buffer = [MaybeUninit::<u8>::uninit(); 2048];
    let mut entries = RawDir::new(scan, &mut buffer);
    let (mut dot, mut parent) = (false, false);
    for _ in 0..3 {
        let entry = match entries.next() {
            None => return Ok(()),
            Some(entry) => entry?,
        };
        match entry.file_name().to_bytes() {
            b"." if !dot => dot = true,
            b".." if !parent => parent = true,
            b"." | b".." => return Err(io::ErrorKind::InvalidData.into()),
            _ => return Err(io::ErrorKind::DirectoryNotEmpty.into()),
        }
    }
    Err(io::ErrorKind::InvalidData.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_linux_actual_opened_file_cannot_borrow_another_filesystem_profile() {
        // A current process-owned read-only regular procfs descriptor has private
        // mode but must never be admitted as an XFS/POSIX storage object.
        // Metadata only: never read the process auxiliary-vector contents.
        let proc_file = File::open("/proc/self/auxv").unwrap();
        let metadata = proc_file.metadata().unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
        assert_eq!(metadata.mode() & 0o077, 0);
        assert_eq!(
            validate_private(&proc_file, &metadata).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
}
