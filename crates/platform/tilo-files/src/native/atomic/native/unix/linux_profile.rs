//! Which Linux filesystems hold a directory or private file under the POSIX
//! discretionary access model the privacy checks reason about: mode bits, and
//! POSIX access ACLs whose mask the group mode bits reflect.
//!
//! The answer comes from the opened object's own `fstatfs`, never from a
//! parent's. FUSE may skip DAC checks entirely, so it is never admitted.
//! ZFS enforces mode bits with `acltype=off` and POSIX access ACLs with
//! `acltype=posixacl`; a dataset whose ACLs are NFS version 4 ACLs is decided by those
//! ACLs instead of the mode bits, so ZFS is admitted only once the object shows
//! no NFS version 4 ACL.
use std::{fs::File, io};

const EXT4: u32 = 0xef53;
const BTRFS: u32 = 0x9123_683e;
const XFS: u32 = 0x5846_5342;
const TMPFS: u32 = 0x0102_1994;
const OVERLAYFS: u32 = 0x794c_7630;
/// ZFS's `ZFS_SUPER_MAGIC`, as `stat -f -c %t` prints it on a dataset.
const ZFS: u32 = 0x2fc1_2fc1;

/// What the check is admitting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Use {
    /// A held directory's permission profile. overlayfs is admitted here: its
    /// callers also require trusted host/backing names.
    Directory,
    /// A private object whose privacy is read from its mode bits alone.
    Private,
}

/// How an object's access is decided, where the filesystem lets it vary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum AclModel {
    /// Mode bits, with POSIX access ACLs guarded by the mask if any.
    Posix,
    /// NFS version 4 ACLs, which decide access whatever the mode bits say.
    Nfs4,
}

/// Whether `file` is on a filesystem `use_` may rely on.
pub(super) fn admits(file: &File, use_: Use) -> io::Result<bool> {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "statfs magic numbers are 32-bit; the width and sign of `f_type` vary by target"
    )]
    let magic = rustix::fs::fstatfs(file)?.f_type as u32;
    decide(magic, use_, || acl_model(file))
}

/// The decision for a filesystem `magic`; `acl` is asked only for a
/// filesystem whose ACL model varies per dataset.
fn decide(magic: u32, use_: Use, acl: impl FnOnce() -> io::Result<AclModel>) -> io::Result<bool> {
    Ok(match magic {
        EXT4 | BTRFS | XFS | TMPFS => true,
        OVERLAYFS => use_ == Use::Directory,
        ZFS => acl()? == AclModel::Posix,
        _ => false,
    })
}

/// The ACL model of `file`'s dataset. An NFS version 4 ACL dataset gives every object
/// an ACL under one of these names (the second is ZFS's XDR encoding); a
/// POSIX or ACL-less dataset knows neither name.
fn acl_model(file: &File) -> io::Result<AclModel> {
    use rustix::io::Errno;
    for name in [c"system.nfs4_acl", c"system.nfs4_acl_xdr"] {
        // An empty buffer asks for the size only.
        match rustix::fs::fgetxattr(file, name, &mut [] as &mut [u8]) {
            Ok(_) | Err(Errno::RANGE) => return Ok(AclModel::Nfs4),
            Err(Errno::NODATA | Errno::NOTSUP) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(AclModel::Posix)
}
