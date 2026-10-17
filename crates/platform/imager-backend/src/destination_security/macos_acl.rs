//! Darwin extended ACLs can grant access even when mode bits are 0700.
//! ABI constants/signatures follow the macOS SDK sys/acl.h and membership.h.
use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::ptr;

use super::refusal;

const KNOWN_PERMISSIONS: u64 = 0x0000_3ffe | (1 << 20);

type AclObject = *mut c_void;
unsafe extern "C" {
    fn acl_get_fd_np(fd: i32, kind: u32) -> AclObject;
    fn acl_free(object: AclObject) -> i32;
    fn acl_get_entry(acl: AclObject, selector: i32, entry: *mut AclObject) -> i32;
    fn acl_get_tag_type(entry: AclObject, tag: *mut u32) -> i32;
    fn acl_get_permset_mask_np(entry: AclObject, mask: *mut u64) -> i32;
    fn acl_get_flagset_np(entry: AclObject, flags: *mut AclObject) -> i32;
    fn acl_get_flag_np(flags: AclObject, flag: u32) -> i32;
    fn acl_get_qualifier(entry: AclObject) -> AclObject;
    fn mbr_uid_to_uuid(uid: u32, uuid: *mut u8) -> i32;
}

struct Allocation(AclObject);
impl Drop for Allocation {
    fn drop(&mut self) {
        // SAFETY: every instance owns an allocation returned by the ACL API.
        unsafe {
            acl_free(self.0);
        }
    }
}

pub(super) fn check(file: &File, current: u32, private: bool) -> io::Result<()> {
    let mut user = [0_u8; 16];
    let mut root = [0_u8; 16];
    // SAFETY: both UUID outputs are 16-byte writable arrays.
    if unsafe { mbr_uid_to_uuid(current, user.as_mut_ptr()) } != 0
        || unsafe { mbr_uid_to_uuid(0, root.as_mut_ptr()) } != 0
    {
        return Err(refusal("cannot identify destination ACL principals"));
    }
    // SAFETY: valid borrowed fd; ACL_TYPE_EXTENDED from Darwin's sys/acl.h.
    let raw = unsafe { acl_get_fd_np(file.as_raw_fd(), 0x100) };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        // On a live APFS/HFS descriptor Darwin returns ENOENT when no extended
        // ACL is attached (verified with native ACL/no-ACL fixtures).
        return if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(())
        } else {
            Err(error)
        };
    }
    let acl = Allocation(raw);
    let mut selector = 0; // ACL_FIRST_ENTRY; ACL_NEXT_ENTRY is -1 on Darwin.
    loop {
        let mut entry = ptr::null_mut();
        // SAFETY: ACL allocation stays live; entry is an output into that ACL.
        let result = unsafe { acl_get_entry(acl.0, selector, &raw mut entry) };
        if result == -1 {
            let error = io::Error::last_os_error();
            // Darwin returns -1/EINVAL at the end, unlike POSIX ACL APIs.
            if error.raw_os_error() == Some(libc::EINVAL) {
                break;
            }
            return Err(error);
        }
        selector = -1;
        if entry.is_null() {
            return Err(refusal("invalid destination ACL entry"));
        }
        // SAFETY: entry belongs to the still-live native ACL allocation.
        unsafe {
            check_entry(entry, &user, &root, private)?;
        }
    }
    Ok(())
}

unsafe fn check_entry(
    entry: AclObject,
    user: &[u8; 16],
    root: &[u8; 16],
    private: bool,
) -> io::Result<()> {
    let mut tag = 0;
    let mut mask = 0;
    let mut flags = ptr::null_mut();
    // SAFETY: entry is a native ACL entry; all outputs are correctly sized.
    if unsafe { acl_get_tag_type(entry, &raw mut tag) } != 0
        || unsafe { acl_get_permset_mask_np(entry, &raw mut mask) } != 0
        || unsafe { acl_get_flagset_np(entry, &raw mut flags) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    match tag {
        2 => return Ok(()), // Denies cannot grant mutation or disclosure rights.
        1 => {}
        _ => return Err(refusal("unsupported destination ACL entry type")),
    }
    // SAFETY: entry is valid; the returned UUID allocation requires acl_free.
    let qualifier = unsafe { acl_get_qualifier(entry) };
    if qualifier.is_null() {
        return Err(io::Error::last_os_error());
    }
    let qualifier = Allocation(qualifier);
    // SAFETY: an extended ACL qualifier is a UUID, exactly 16 bytes.
    let principal = unsafe { ptr::read_unaligned(qualifier.0.cast::<[u8; 16]>()) };
    if principal == *user || principal == *root {
        return Ok(());
    }
    // SAFETY: flags belongs to entry; these are Darwin's inheritance flags.
    let file_inherit = unsafe { acl_get_flag_np(flags, 1 << 5) };
    let directory_inherit = unsafe { acl_get_flag_np(flags, 1 << 6) };
    if file_inherit < 0 || directory_inherit < 0 {
        return Err(io::Error::last_os_error());
    }
    let mutation =
        (1 << 2) | (1 << 4) | (1 << 5) | (1 << 6) | (1 << 8) | (1 << 10) | (1 << 12) | (1 << 13);
    if mask & !KNOWN_PERMISSIONS != 0 {
        return Err(refusal("unsupported destination ACL permission bits"));
    }
    // Include inherit-only mutation grants: a newly created 0700 directory
    // would inherit them. Refuse inheritable read grants too, to keep output
    // private from its first creation without editing an existing parent's ACL.
    if mask != 0 && (private || mask & mutation != 0 || file_inherit != 0 || directory_inherit != 0)
    {
        return Err(refusal(
            "destination ACL grants another principal access to the private namespace",
        ));
    }
    Ok(())
}
