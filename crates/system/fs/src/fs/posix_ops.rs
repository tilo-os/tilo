//! The FUSE mount's side of a record's POSIX attributes: creating a
//! symbolic link, reading a link's target back, and publishing the
//! permission bits a `chmod` (or a create's mode) asks for.
//!
//! A link is a file record whose content is its target, byte for byte, so
//! its target is versioned, signed, synced and hydrated like any other
//! content; only the fact that the record is a link is carried beside it
//! (`namespace::posix`). Permission bits replicate the same way. Ownership
//! never does: every materialised path belongs to the local person.

use super::{EINVAL, EIO, ENOENT, FileType, QuickFS, now_ms};
use crate::namespace::PosixAttrs;

impl QuickFS {
    /// Creates `name` in `parent` as a symbolic link to `target`.
    ///
    /// A link is a file record whose content is its target, verbatim, so the
    /// target is versioned, signed and synced like any other content, and
    /// only the fact that the record is a link is carried beside it.
    pub(super) fn create_symlink(
        &self,
        parent: u64,
        name: &[u8],
        target: &[u8],
    ) -> Result<u64, i32> {
        if target.is_empty() || target.contains(&0) {
            return Err(EINVAL);
        }
        self.check_new_name(parent, name)?;
        let (ino, delta) = self.create_empty_file_stamped(parent, name, now_ms())?;
        self.publish(delta);
        // A link's own permission bits mean nothing on Linux.
        let attrs = PosixAttrs::of_local(0, false, true);
        self.set_posix(ino, attrs, false)?;
        self.commit_content_at(ino, target, now_ms())
            .map_err(|error| error.raw_os_error().unwrap_or(EIO))?;
        self.record_version(ino, fs_protocol::FileProvenance::Create, None);
        Ok(ino)
    }

    /// Publishes the permission bits the kernel asks for, keeping only what
    /// replicates. Ownership is never replicated: every materialised path
    /// belongs to the local person, so asking for that owner is already true
    /// and asking for another is refused.
    pub(super) fn set_mode(
        &self,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
    ) -> Result<(), i32> {
        let (kind, current) = {
            let ns = self.ns();
            let attr = ns.getattr(ino).ok_or(ENOENT)?;
            let dir = attr.kind == FileType::Directory;
            let current = ns
                .record_id_for(ino)
                .and_then(|record| ns.posix_attrs(record))
                .unwrap_or_else(|| PosixAttrs::default_for(dir));
            (attr.kind, current)
        };
        let local = (
            rustix::process::getuid().as_raw(),
            rustix::process::getgid().as_raw(),
        );
        if uid.is_some_and(|uid| uid != local.0) || gid.is_some_and(|gid| gid != local.1) {
            return Err(libc::EPERM);
        }
        let Some(mode) = mode else {
            return Ok(());
        };
        let dir = kind == FileType::Directory;
        let attrs = PosixAttrs::of_local(mode, dir, current.symlink);
        self.set_posix(ino, attrs, dir)
    }
}
