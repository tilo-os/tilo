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
    tilo_process::effective_uid()
}

fn trusted_owner(uid: u32, current: u32) -> bool {
    uid == current || uid == 0
}

/// Type, owner and permission bits of one directory on the destination path.
#[derive(Clone, Copy, Debug)]
struct Observed {
    is_dir: bool,
    uid: u32,
    mode: u32,
}

impl Observed {
    fn of(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            is_dir: metadata.is_dir(),
            uid: metadata.uid(),
            mode: metadata.mode(),
        })
    }
}

/// # Errors
/// Refuses unsupported filesystem semantics, non-directory or untrusted ancestry,
/// unsafe writable ancestors, and non-private ownership or permissions when requested.
/// Propagates filesystem metadata and, on macOS, ACL inspection errors.
pub fn check_directory(file: &File, child: Option<&Metadata>, private: bool) -> io::Result<()> {
    #[cfg(any(test, feature = "test-support"))]
    if test_environment::is_above_fixtures(file)? {
        return Ok(());
    }
    check_filesystem(file)?;
    let child_uid = child.map(|child| {
        use cap_primitives::fs::MetadataExt;
        child.uid()
    });
    let current = effective_uid();
    check_ownership(Observed::of(file)?, child_uid, private, current)?;
    #[cfg(target_os = "macos")]
    macos_acl::check(file, current, private)?;
    Ok(())
}

/// The ownership decision for one ancestor as seen by account `current`.
/// `child_uid` owns the next directory down the path; `None` means that
/// directory is about to be created by `current`.
fn check_ownership(
    observed: Observed,
    child_uid: Option<u32>,
    private: bool,
    current: u32,
) -> io::Result<()> {
    if !observed.is_dir || !trusted_owner(observed.uid, current) {
        return Err(refusal("destination ancestry has an untrusted owner"));
    }
    if private {
        check_private_ownership(observed, current)?;
    } else if observed.mode & 0o022 != 0 {
        if observed.mode & 0o1000 == 0 {
            return Err(refusal(
                "destination ancestor is writable by another account without sticky protection",
            ));
        }
        // Sticky directories protect only the entry owner (and directory owner
        // / root). Checking the parent bit alone does not protect a foreign
        // child. None means the future child will be created by this account.
        if let Some(child_uid) = child_uid
            && !trusted_owner(child_uid, current)
        {
            return Err(refusal(
                "sticky destination ancestor does not protect the selected child",
            ));
        }
    }
    Ok(())
}

/// # Errors
/// Refuses an object not owned exclusively by the current account, including
/// group or other access; propagates metadata and macOS ACL inspection errors.
pub fn check_private_file(file: &File) -> io::Result<()> {
    let current = effective_uid();
    check_private_ownership(Observed::of(file)?, current)?;
    #[cfg(target_os = "macos")]
    macos_acl::check(file, current, true)?;
    Ok(())
}

/// Test fixtures live in fresh directories below the platform temporary
/// directory. That directory and its ancestors belong to the machine running
/// the tests, not to any fixture: a rootless development container shows the
/// host's `/tmp` under the overflow uid and mounts its own root with FUSE,
/// both of which the checks rightly refuse, so tests there would only ever
/// observe that refusal. Those machine directories are recognised by identity
/// and skipped; every directory a fixture creates is still checked in full,
/// also in the helper processes some tests spawn. The ownership decision
/// itself is covered with injected uids by the unit tests below.
#[cfg(any(test, feature = "test-support"))]
mod test_environment {
    use std::collections::HashSet;
    use std::fs::File;
    use std::io;
    use std::os::unix::fs::MetadataExt;
    use std::sync::OnceLock;

    fn above_fixtures() -> io::Result<&'static HashSet<(u64, u64)>> {
        static ABOVE: OnceLock<HashSet<(u64, u64)>> = OnceLock::new();
        if let Some(above) = ABOVE.get() {
            return Ok(above);
        }
        let above = {
            let temp = std::env::temp_dir();
            let temp = temp.canonicalize().unwrap_or(temp);
            temp.ancestors()
                .map(|ancestor| {
                    let metadata = std::fs::metadata(ancestor)?;
                    Ok((metadata.dev(), metadata.ino()))
                })
                .collect::<io::Result<HashSet<_>>>()?
        };
        Ok(ABOVE.get_or_init(|| above))
    }

    pub(super) fn is_above_fixtures(file: &File) -> io::Result<bool> {
        let metadata = file.metadata()?;
        Ok(above_fixtures()?.contains(&(metadata.dev(), metadata.ino())))
    }
}

fn check_private_ownership(observed: Observed, current: u32) -> io::Result<()> {
    if observed.uid != current || observed.mode & 0o077 != 0 {
        return Err(refusal(
            "created destination is not private to the current account",
        ));
    }
    Ok(())
}

/// # Errors
/// Propagates directory creation errors, including an existing name, invalid
/// path, insufficient permissions or unavailable storage.
pub fn create_directory(parent: &File, name: &OsStr) -> io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    const ME: u32 = 1000;
    const OTHER: u32 = 1001;
    // The overflow uid: how a rootless container shows directories owned by
    // the host's root, such as a bind-mounted /tmp.
    const OVERFLOW: u32 = 65_534;

    fn dir(uid: u32, mode: u32) -> Observed {
        Observed {
            is_dir: true,
            uid,
            mode: 0o040_000 | mode,
        }
    }

    fn refused(result: io::Result<()>, message: &str) {
        let error = result.expect_err("expected a refusal");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains(message), "{error}");
    }

    /// Ancestor ownership, type and sticky-bit rules.
    #[test]
    fn ancestor_trust_requires_trusted_owner_directory_and_sticky_protection() {
        // Phase `ancestors_owned_by_self_or_root_are_trusted`.
        {
            check_ownership(dir(ME, 0o755), None, false, ME).unwrap();
            check_ownership(dir(0, 0o755), Some(ME), false, ME).unwrap();
            check_ownership(dir(0, 0o755), Some(0), false, 0).unwrap();
        }

        // Phase `ancestor_owned_by_another_account_is_refused`.
        {
            for owner in [OTHER, OVERFLOW] {
                refused(
                    check_ownership(dir(owner, 0o755), None, false, ME),
                    "untrusted owner",
                );
            }
            // Root does not trust an ordinary account's directory either.
            refused(
                check_ownership(dir(ME, 0o700), None, false, 0),
                "untrusted owner",
            );
        }

        // Phase `non_directory_ancestor_is_refused`.
        {
            let file = Observed {
                is_dir: false,
                uid: ME,
                mode: 0o100_700,
            };
            refused(check_ownership(file, None, false, ME), "untrusted owner");
        }

        // Phase `writable_ancestor_needs_the_sticky_bit`.
        {
            refused(
                check_ownership(dir(0, 0o777), Some(ME), false, ME),
                "without sticky protection",
            );
            refused(
                check_ownership(dir(ME, 0o775), None, false, ME),
                "without sticky protection",
            );
            check_ownership(dir(0, 0o1777), Some(ME), false, ME).unwrap();
            check_ownership(dir(0, 0o1777), None, false, ME).unwrap();
        }

        // Phase `sticky_ancestor_does_not_protect_a_foreign_child`.
        {
            for child in [OTHER, OVERFLOW] {
                refused(
                    check_ownership(dir(0, 0o1777), Some(child), false, ME),
                    "does not protect the selected child",
                );
            }
            check_ownership(dir(0, 0o1777), Some(0), false, ME).unwrap();
        }
    }

    #[test]
    fn private_directory_belongs_to_self_with_no_group_or_other_access() {
        check_ownership(dir(ME, 0o700), None, true, ME).unwrap();
        refused(
            check_ownership(dir(ME, 0o750), None, true, ME),
            "not private",
        );
        // A root-owned directory is trusted as an ancestor, never as private.
        refused(
            check_ownership(dir(0, 0o700), None, true, ME),
            "not private",
        );
        refused(
            check_private_ownership(dir(OTHER, 0o700), ME),
            "not private",
        );
    }
}
