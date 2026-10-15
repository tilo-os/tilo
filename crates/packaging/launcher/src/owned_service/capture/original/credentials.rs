//! Credential transition after confinement, never in the receiving parent.
use super::{Facts, refused, root};
use std::io;
#[repr(C)]
struct Header {
    version: u32,
    pid: i32,
}
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct Data {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}
fn clear_capabilities() -> io::Result<()> {
    let header = Header {
        version: 0x2008_0522,
        pid: 0,
    };
    let mut caps = [Data {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    if unsafe { libc::syscall(libc::SYS_capset, &raw const header, caps.as_ptr()) } < 0
        || unsafe { libc::syscall(libc::SYS_capget, &raw const header, caps.as_mut_ptr()) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    if caps
        .iter()
        .any(|c| c.effective != 0 || c.permitted != 0 || c.inheritable != 0)
    {
        return Err(refused("Original-source worker retained capabilities"));
    }
    Ok(())
}
pub(super) fn drop_to(facts: &Facts) -> io::Result<()> {
    drop_authenticated(facts.uid, facts.gid, &facts.groups)
}

pub(in crate::owned_service) fn drop_authenticated(
    uid: u32,
    gid: u32,
    original_groups: &[u32],
) -> io::Result<()> {
    root()?;
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } < 0
        || unsafe { libc::prctl(libc::PR_SET_KEEPCAPS, 0, 0, 0, 0) } < 0
        || unsafe { libc::setgroups(original_groups.len(), original_groups.as_ptr()) } < 0
        || unsafe { libc::setresgid(gid, gid, gid) } < 0
        || unsafe { libc::setresuid(uid, uid, uid) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    clear_capabilities()?;
    super::super::check_credentials(uid)?;
    let mut real = 0;
    let mut effective = 0;
    let mut saved = 0;
    if unsafe { libc::getresgid(&raw mut real, &raw mut effective, &raw mut saved) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    let mut groups = vec![0; usize::try_from(count).map_err(io::Error::other)?];
    if unsafe { libc::getgroups(count, groups.as_mut_ptr()) } != count {
        return Err(io::Error::last_os_error());
    }
    groups.sort_unstable();
    groups.dedup();
    if (real, effective, saved) != (gid, gid, gid) || groups != original_groups {
        return Err(refused("Original-source credentials differ after drop"));
    }
    Ok(())
}

fn capabilities() -> io::Result<[Data; 2]> {
    let header = Header {
        version: 0x2008_0522,
        pid: 0,
    };
    let mut data = [Data {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    // SAFETY: capget reads the fixed ABI header and fills both valid data entries.
    if unsafe { libc::syscall(libc::SYS_capget, &raw const header, data.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(data)
}
fn groups() -> io::Result<Vec<u32>> {
    // SAFETY: zero count queries the length without accessing the null pointer.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if usize::try_from(count).map_or(true, |length| length > super::GROUPS_LIMIT) {
        return Err(refused("Filesystem group shape differs"));
    }
    let mut value = vec![0; usize::try_from(count).map_err(io::Error::other)?];
    // SAFETY: the queried count equals the allocated gid_t output buffer length.
    if unsafe { libc::getgroups(count, value.as_mut_ptr()) } != count {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}
fn filesystem_ids() -> (u32, u32) {
    // SAFETY: the invalid all-ones ID cannot be installed; these calls report
    // the CURRENT IDs. setfsuid/setfsgid return previous IDs, not success codes.
    unsafe {
        (
            libc::setfsuid(u32::MAX).cast_unsigned(),
            libc::setfsgid(u32::MAX).cast_unsigned(),
        )
    }
}
fn apply_filesystem_ids(uid: u32, gid: u32, groups: &[u32]) -> io::Result<()> {
    // SAFETY: groups is a bounded valid gid_t input buffer; both setters take IDs.
    if unsafe { libc::setgroups(groups.len(), groups.as_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe {
        libc::setfsgid(gid);
        libc::setfsuid(uid);
    }
    if filesystem_ids() != (uid, gid) {
        return Err(refused("Filesystem credential transition refused"));
    }
    let mut actual = self::groups()?;
    let mut expected = groups.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    if actual != expected {
        return Err(refused("Filesystem groups differ"));
    }
    Ok(())
}

/// Nonserializable synchronous proof, minted only after exact caller FS-DAC
/// transition. It cannot be constructed by the worker or retained past closure.
pub(super) struct ProjectionFilesystemAccess {
    uid: u32,
    gid: u32,
    groups: Vec<u32>,
    capabilities: [Data; 2],
    root_gid: u32,
    root_groups: Vec<u32>,
}
impl ProjectionFilesystemAccess {
    /// Only dereference already-held descriptor paths for the two mount calls.
    /// /proc/self/fd is owned by the effective-root task, so caller fsuid cannot
    /// traverse it. Placement and ancestry lookup remain in the caller scope.
    pub(super) fn with_root_mount<T>(
        &self,
        mount: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        self.validate()?;
        let result = (|| {
            apply_filesystem_ids(0, self.root_gid, &self.root_groups)?;
            root()?;
            if capabilities()? != self.capabilities {
                return Err(refused("Mount filesystem capabilities changed"));
            }
            let result = mount();
            root()?;
            if filesystem_ids() != (0, self.root_gid) || capabilities()? != self.capabilities {
                return Err(refused("Mount filesystem custody changed"));
            }
            let mut actual = groups()?;
            let mut expected = self.root_groups.clone();
            actual.sort_unstable();
            expected.sort_unstable();
            if actual != expected {
                return Err(refused("Mount filesystem groups changed"));
            }
            result
        })();
        // Even a partial root transition or a failed mount must restore caller
        // DAC before any further original-Home placement traversal.
        apply_filesystem_ids(self.uid, self.gid, &self.groups)?;
        self.validate()?;
        result
    }
    pub(super) fn validate(&self) -> io::Result<()> {
        // SAFETY: credential getters have no pointer arguments.
        if rustix::process::getuid().as_raw() != 0
            || rustix::process::geteuid().as_raw() != 0
            || filesystem_ids() != (self.uid, self.gid)
            || capabilities()? != self.capabilities
        {
            return Err(refused("Scoped projection credential custody changed"));
        }
        let mut current = groups()?;
        let mut expected = self.groups.clone();
        current.sort_unstable();
        expected.sort_unstable();
        if current != expected {
            return Err(refused("Scoped projection groups changed"));
        }
        Ok(())
    }
}

/// The signed shim is already inside the exact confined readonly namespace.
/// `O_PATH` placement/ancestry traversal uses the original caller's DAC rights,
/// while held-FD mount syscalls briefly restore the prior root filesystem IDs.
/// Only the existing transient mount capabilities remain; no source contents
/// or ambient paths are read.
/// Root filesystem ownership is restored before the immutable claim is created.
pub(super) fn with_source_filesystem_access<T>(
    facts: &Facts,
    project: impl FnOnce(&ProjectionFilesystemAccess) -> io::Result<T>,
) -> io::Result<T> {
    root()?;
    facts.validate()?;
    let before = filesystem_ids();
    if before.0 != 0 {
        return Err(refused("Original shim filesystem owner differs"));
    }
    let before_groups = groups()?;
    let before_caps = capabilities()?;
    let result = (|| {
        apply_filesystem_ids(facts.uid, facts.gid, &facts.groups)?;
        // Changing fsuid can clear filesystem capabilities. The admitted shim
        // has no DAC override; SETUID/SETGID/SYS_ADMIN must remain exactly held.
        if capabilities()? != before_caps {
            return Err(refused("Projection capabilities changed"));
        }
        let access = ProjectionFilesystemAccess {
            uid: facts.uid,
            gid: facts.gid,
            groups: facts.groups.clone(),
            capabilities: before_caps,
            root_gid: before.1,
            root_groups: before_groups.clone(),
        };
        access.validate()?;
        let result = project(&access);
        access.validate()?;
        result
    })();
    // Always restore, including partial setter or projection failures. Refuse
    // completion if restoration cannot be independently observed.
    let restored = apply_filesystem_ids(before.0, before.1, &before_groups);
    restored?;
    root()?;
    if capabilities()? != before_caps {
        return Err(refused("Restored shim capabilities differ"));
    }
    result
}

#[cfg(all(test, target_os = "linux", feature = "managed-tree-test"))]
mod native_tests;
