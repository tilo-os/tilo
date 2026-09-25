use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use cap_primitives::fs::{DirBuilderExt, DirOptions, Metadata};

use super::refusal;

#[cfg(target_os = "macos")]
#[path = "macos_acl.rs"]
mod macos_acl;

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no input pointers or preconditions.
    unsafe { libc::geteuid() }
}

fn trusted_owner(uid: u32, current: u32) -> bool {
    uid == current || uid == 0
}

pub(crate) fn check_directory(
    file: &File,
    child: Option<&Metadata>,
    private: bool,
) -> io::Result<()> {
    check_filesystem(file)?;
    let observed = file.metadata()?;
    let current = effective_uid();
    if !observed.is_dir() || !trusted_owner(observed.uid(), current) {
        return Err(refusal("destination ancestry has an untrusted owner"));
    }
    if private {
        check_private_file(file)?;
    } else if observed.mode() & 0o022 != 0 {
        if observed.mode() & 0o1000 == 0 {
            return Err(refusal(
                "destination ancestor is writable by another account without sticky protection",
            ));
        }
        // Sticky directories protect only the entry owner (and directory owner
        // / root). Checking the parent bit alone does not protect a foreign
        // child. None means the future child will be created by this account.
        if let Some(child) = child {
            use cap_primitives::fs::MetadataExt;
            if !trusted_owner(child.uid(), current) {
                return Err(refusal(
                    "sticky destination ancestor does not protect the selected child",
                ));
            }
        }
    }
    #[cfg(target_os = "macos")]
    macos_acl::check(file, current, private)?;
    Ok(())
}

pub(crate) fn check_private_file(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    let current = effective_uid();
    if metadata.uid() != current || metadata.mode() & 0o077 != 0 {
        return Err(refusal(
            "created destination is not private to the current account",
        ));
    }
    #[cfg(target_os = "macos")]
    macos_acl::check(file, current, true)?;
    Ok(())
}

pub(crate) fn create_directory(parent: &File, name: &OsStr) -> io::Result<()> {
    let mut options = DirOptions::new();
    options.mode(0o700);
    cap_primitives::fs::create_dir(parent, Path::new(name), &options)
}

fn check_filesystem(file: &File) -> io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let mut status = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: valid borrowed fd and correctly sized writable output.
        if unsafe { libc::fstatfs(file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstatfs succeeded and initialized the entire output.
        let status = unsafe { status.assume_init() };
        #[cfg(target_os = "linux")]
        {
            // These local filesystems enforce Unix ownership and POSIX ACL
            // masks. Inherited default ACLs are restricted by 0700/0600 at
            // every creation. Do not infer this for FUSE, CIFS, NFS or FAT.
            if !matches!(
                u64::try_from(status.f_type)
                    .map_err(|_| refusal("unknown destination filesystem type"))?,
                0xef53 | 0x5846_5342 | 0x9123_683e | 0x0102_1994 | 0x794c_7630
            ) {
                return Err(refusal(
                    "destination filesystem permission semantics are unsupported",
                ));
            }
        }
        #[cfg(target_os = "macos")]
        {
            // SAFETY: Darwin statfs contains a nul-terminated f_fstypename.
            let name = unsafe { std::ffi::CStr::from_ptr(status.f_fstypename.as_ptr()) };
            if !matches!(name.to_bytes(), b"apfs" | b"hfs")
                || status.f_flags & libc::MNT_LOCAL as u32 == 0
                || status.f_flags & libc::MNT_IGNORE_OWNERSHIP as u32 != 0
            {
                return Err(refusal(
                    "destination needs local APFS/HFS with ownership enforcement",
                ));
            }
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = file;
        Err(refusal(
            "destination permission verification is unavailable on this platform",
        ))
    }
}
