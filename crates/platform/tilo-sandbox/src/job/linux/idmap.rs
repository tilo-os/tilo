//! Idmapped mounts for a job with host ids of its own ([`IdBlock`]): the
//! worker clones each host path it binds into the job as a detached mount on
//! which the files' owner shows as the job's user, and hands the job the
//! mount to attach (`move_mount`).
//!
//! Idmapping a mount takes `CAP_SYS_ADMIN` in the user namespace of the
//! filesystem, the host's, so the worker does it before the job starts,
//! not the job itself.

use std::collections::HashMap;
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use super::sys;
use crate::job::spec::{IdBlock, IdMap};

/// The ids an idmapped mount translates: the files' owner on the host, the
/// job's own id in its namespace, and the job's block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Translation {
    owner_uid: u32,
    owner_gid: u32,
    inside_uid: u32,
    inside_gid: u32,
    block: IdBlock,
}

impl Translation {
    fn of(ids: &IdMap, block: IdBlock) -> Option<Self> {
        // The job's own ids must be in its block (checked when planning).
        (ids.inside_uid < block.count && ids.inside_gid < block.count).then_some(Self {
            owner_uid: ids.outside_uid,
            owner_gid: ids.outside_gid,
            inside_uid: ids.inside_uid,
            inside_gid: ids.inside_gid,
            block,
        })
    }
}

/// The map of an idmapped mount, as `uid_map` takes it: every id on disk
/// below `count` shows as the job's id of the same number (host id
/// `first + k`), so files and ACL entries of any of the job's users keep
/// their owner across jobs; but the files' owner on the host shows as the
/// job's own id (`inside`), and the id `inside` on disk takes the owner's
/// place, so the map stays one to one. An owner at or past `count` takes
/// `inside`'s place, which then has no mapping.
fn map_lines(owner: u32, inside: u32, first: u32, count: u32) -> String {
    use std::fmt::Write as _;
    // (disk id, host id, length), in order of disk id.
    let mut ranges: Vec<(u32, u32, u32)> = Vec::new();
    let identity = |from: u32, to: u32, ranges: &mut Vec<(u32, u32, u32)>| {
        if from < to {
            ranges.push((from, first.saturating_add(from), to.saturating_sub(from)));
        }
    };
    if owner == inside {
        identity(0, count, &mut ranges);
    } else {
        // The ids on disk that leave the identity: `owner` (if in range)
        // and `inside`.
        let mut special = vec![inside];
        if owner < count {
            special.push(owner);
        }
        special.sort_unstable();
        let mut at = 0;
        for k in special {
            identity(at, k, &mut ranges);
            if k == inside {
                if owner < count {
                    ranges.push((inside, first.saturating_add(owner), 1));
                }
            } else {
                ranges.push((owner, first.saturating_add(inside), 1));
            }
            at = k.saturating_add(1);
        }
        identity(at, count, &mut ranges);
        if owner >= count {
            ranges.push((owner, first.saturating_add(inside), 1));
        }
    }
    let mut lines = String::new();
    for (disk, host, len) in ranges {
        let _ = writeln!(lines, "{disk} {host} {len}");
    }
    lines
}

/// User namespaces that carry each translation, kept for the process's
/// life: a worker reuses its blocks, so there are a few per block at most.
static NAMESPACES: Mutex<Option<HashMap<Translation, OwnedFd>>> = Mutex::new(None);

/// A user namespace whose map translates the files' owner into the job's
/// host ids, as `MOUNT_ATTR_IDMAP` takes it: an id on disk is read as an id
/// inside the namespace and shown as the host id it maps to.
///
/// # Errors
/// When `ids` has no block, the block overflows, or the namespace can't be
/// made (the caller lacks `CAP_SETUID`/`CAP_SETGID`).
pub fn namespace(ids: &IdMap) -> io::Result<OwnedFd> {
    let block = ids
        .block
        .ok_or_else(|| io::Error::other("the job has no block of ids"))?;
    let t = Translation::of(ids, block)
        .ok_or_else(|| io::Error::other("the job's ids overflow its block"))?;
    let mut guard = NAMESPACES.lock().unwrap_or_else(PoisonError::into_inner);
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(fd) = cache.get(&t) {
        return fd.try_clone();
    }
    let fd = make_namespace(t)?;
    let copy = fd.try_clone()?;
    cache.insert(t, fd);
    drop(guard);
    Ok(copy)
}

/// Start a process in a new user namespace, map it, keep the namespace,
/// end the process.
fn make_namespace(t: Translation) -> io::Result<OwnedFd> {
    let bits = |f: libc::c_int| u64::from(f.unsigned_abs());
    let args = sys::CloneArgs {
        flags: bits(libc::CLONE_NEWUSER),
        exit_signal: bits(libc::SIGCHLD),
        ..sys::CloneArgs::default()
    };
    // SAFETY: clone3 without CLONE_VM, like fork; the child only waits for
    // its signal (pause is async-signal-safe) and never returns.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_clone3,
            &raw const args,
            std::mem::size_of::<sys::CloneArgs>(),
        )
    };
    if ret == 0 {
        loop {
            // SAFETY: waits for SIGKILL from the parent.
            unsafe { libc::pause() };
        }
    }
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    let pid = libc::pid_t::try_from(ret).map_err(|_| io::Error::other("clone3: bad pid"))?;
    let made = (|| {
        std::fs::write(
            format!("/proc/{pid}/uid_map"),
            map_lines(t.owner_uid, t.inside_uid, t.block.uid, t.block.count),
        )?;
        std::fs::write(
            format!("/proc/{pid}/gid_map"),
            map_lines(t.owner_gid, t.inside_gid, t.block.gid, t.block.count),
        )?;
        let ns = std::fs::File::open(format!("/proc/{pid}/ns/user"))?;
        Ok(OwnedFd::from(ns))
    })();
    // SAFETY: ending and reaping the child started above.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        libc::waitpid(pid, std::ptr::null_mut(), 0);
    }
    made
}

/// A detached copy of the mount tree at `path` (submounts included), with
/// the translation of `userns`, private (nothing the job mounts beneath it
/// reaches the host's tree).
///
/// # Errors
/// When the filesystem can't be idmapped (`EINVAL`), or the kernel is older
/// than 5.12 (`ENOSYS`).
pub fn tree(path: &Path, userns: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::other("a path with a NUL byte"))?;
    // SAFETY: a live C string; open_tree returns a new descriptor or -1.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_open_tree,
            libc::AT_FDCWD,
            c.as_ptr(),
            sys::OPEN_TREE_CLONE | sys::OPEN_TREE_CLOEXEC | sys::AT_RECURSIVE,
        )
    };
    let fd = RawFd::try_from(fd).map_err(|_| io::Error::other("descriptor out of range"))?;
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the kernel just returned this descriptor; nothing else owns it.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let attr = sys::MountAttr {
        attr_set: sys::MOUNT_ATTR_IDMAP,
        propagation: libc::MS_PRIVATE,
        userns_fd: u64::try_from(userns.as_raw_fd()).unwrap_or_default(),
        ..sys::MountAttr::default()
    };
    // SAFETY: a descriptor we own, an empty path, and a live structure of
    // the size passed.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            fd.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH.unsigned_abs() | sys::AT_RECURSIVE,
            &raw const attr,
            std::mem::size_of::<sys::MountAttr>(),
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

#[cfg(test)]
mod tests {
    use super::map_lines;

    #[test]
    fn a_root_owner_and_a_root_job_map_the_block_as_it_is() {
        assert_eq!(map_lines(0, 0, 100_000, 65536), "0 100000 65536\n");
    }

    #[test]
    fn a_root_owner_and_a_nested_job_swap_their_two_ids() {
        assert_eq!(
            map_lines(0, 1000, 100_000, 65536),
            "0 101000 1\n1 100001 999\n1000 100000 1\n1001 101001 64535\n"
        );
    }

    #[test]
    fn an_owner_past_the_block_takes_the_jobs_place() {
        assert_eq!(
            map_lines(70_000, 0, 100_000, 65536),
            "1 100001 65535\n70000 100000 1\n"
        );
    }

    #[test]
    fn an_owner_inside_the_block_swaps_with_a_root_job() {
        assert_eq!(
            map_lines(1000, 0, 100_000, 65536),
            "0 101000 1\n1 100001 999\n1000 100000 1\n1001 101001 64535\n"
        );
    }
}
