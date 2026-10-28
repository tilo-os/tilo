//! XFS project quotas: a hard limit on the bytes beneath one directory.
//!
//! The directory gets a project id with `PROJINHERIT`, so everything created
//! in it later carries the id too; files already there are tagged by a walk.
//! The project gets a hard block limit, and a write past it fails with
//! `EDQUOT`. Setting either needs `CAP_SYS_ADMIN` on the host, which is why
//! callers reach this through a [`crate::job::cgroup::CgroupProvider`].
//!
//! Only filesystems that enforce project quotas (XFS mounted with
//! `prjquota`) can: everything else answers [`QuotaOutcome::Unsupported`],
//! and the caller runs the job without a disk bound.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::path::Path;

use crate::job::cgroup::QuotaOutcome;

/// `struct fsxattr`.
#[repr(C)]
#[derive(Default)]
struct FsXattr {
    xflags: u32,
    extsize: u32,
    nextents: u32,
    projid: u32,
    cowextsize: u32,
    pad: [u8; 8],
}

#[repr(C)]
#[derive(Default)]
struct FsDiskQuota {
    version: i8,
    flags: i8,
    fieldmask: u16,
    id: u32,
    blk_hardlimit: u64,
    blk_softlimit: u64,
    ino_hardlimit: u64,
    ino_softlimit: u64,
    bcount: u64,
    icount: u64,
    itimer: i32,
    btimer: i32,
    iwarns: u16,
    bwarns: u16,
    itimer_hi: i8,
    btimer_hi: i8,
    rtbtimer_hi: i8,
    padding2: i8,
    rtb_hardlimit: u64,
    rtb_softlimit: u64,
    rtbcount: u64,
    rtbtimer: i32,
    rtbwarns: u16,
    padding3: i16,
    padding4: [u8; 8],
}

const FS_IOC_FSGETXATTR: libc::c_ulong = 0x801c_581f;
const FS_IOC_FSSETXATTR: libc::c_ulong = 0x401c_5820;
const FS_XFLAG_PROJINHERIT: u32 = 0x200;

const PRJQUOTA: u32 = 2;
/// `XQM_CMD(n)`: `('X' << 8) + n`.
const fn xqm(n: u32) -> u32 {
    (0x58 << 8) | n
}
const Q_XSETQLIM: u32 = xqm(4);
const Q_XGETQSTAT: u32 = xqm(5);
/// `QCMD(cmd, PRJQUOTA)`.
const fn qcmd(cmd: u32) -> u32 {
    (cmd << 8) | PRJQUOTA
}
const FS_DQUOT_VERSION: i8 = 1;
const FS_PROJ_QUOTA: i8 = 2;
const FS_DQ_BSOFT: u16 = 1 << 2;
const FS_DQ_BHARD: u16 = 1 << 3;
/// `fs_quota_stat.qs_flags`: project quota enforcement on.
const FS_QUOTA_PDQ_ENFD: u16 = 1 << 5;
/// Quota block units: 512 bytes.
const BASIC_BLOCK: u64 = 512;

fn quotactl(dir: BorrowedFd<'_>, cmd: u32, id: u32, addr: *mut u8) -> io::Result<()> {
    // SAFETY: quotactl_fd on a live descriptor; `addr` points at a buffer
    // of the structure `cmd` reads or writes.
    let rc = unsafe { libc::syscall(libc::SYS_quotactl_fd, dir.as_raw_fd(), qcmd(cmd), id, addr) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Whether the filesystem holding `dir` enforces project quotas, or why
/// not.
///
/// # Errors
/// Returns a human-readable reason when quotas are off or the filesystem
/// has none — not a failure, but the answer the caller degrades on.
pub fn enforced(dir: BorrowedFd<'_>) -> Result<(), String> {
    // `struct fs_quota_stat` is 80 bytes; its flags are a u16 at offset 2.
    let mut stat = [0_u64; 16];
    match quotactl(dir, Q_XGETQSTAT, 0, stat.as_mut_ptr().cast()) {
        Ok(()) => {
            let bytes = stat[0].to_ne_bytes();
            let flags = u16::from_ne_bytes([bytes[2], bytes[3]]);
            if flags & FS_QUOTA_PDQ_ENFD == 0 {
                Err(
                    "project quotas are not enforced on this filesystem (mount it with prjquota)"
                        .into(),
                )
            } else {
                Ok(())
            }
        }
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::ENOSYS | libc::ENOTTY | libc::EINVAL)
            ) =>
        {
            Err(format!("the filesystem has no XFS quotas ({e})"))
        }
        Err(e) if e.raw_os_error() == Some(libc::ESRCH) => {
            Err("quotas are off on this filesystem (mounted noquota)".into())
        }
        Err(e) => Err(format!("asking for the quota state: {e}")),
    }
}

fn get_xattr(fd: libc::c_int) -> io::Result<FsXattr> {
    let mut x = FsXattr::default();
    // SAFETY: the ioctl fills a structure of the size it encodes.
    if unsafe { libc::ioctl(fd, FS_IOC_FSGETXATTR, &raw mut x) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(x)
}

fn tag(fd: libc::c_int, project: u32, inherit: bool) -> io::Result<()> {
    let mut x = get_xattr(fd)?;
    if x.projid == project && (!inherit || x.xflags & FS_XFLAG_PROJINHERIT != 0) {
        return Ok(());
    }
    x.projid = project;
    if inherit {
        x.xflags |= FS_XFLAG_PROJINHERIT;
    }
    // SAFETY: as above, the structure the ioctl reads.
    if unsafe { libc::ioctl(fd, FS_IOC_FSSETXATTR, &raw const x) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Tag `dir` and everything beneath it with `project`. Symlinks, sockets,
/// FIFOs and devices hold no blocks and are left alone.
fn tag_tree(dir: &Path, project: u32) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let open = |p: &Path| {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(p)
    };
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let f = open(&d)?;
        // Already tagged: a walk done before (new files inherit the id).
        if get_xattr(f.as_raw_fd())
            .is_ok_and(|x| x.projid == project && x.xflags & FS_XFLAG_PROJINHERIT != 0)
            && d != dir
        {
            continue;
        }
        tag(f.as_raw_fd(), project, true)?;
        for entry in std::fs::read_dir(&d)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                let f = open(&entry.path())?;
                tag(f.as_raw_fd(), project, false)?;
            }
        }
    }
    Ok(())
}

/// Put `dir` under project `project` with a hard limit of `bytes`.
///
/// # Errors
/// Returns why a filesystem that enforces project quotas refused; one that
/// doesn't answers `Ok(QuotaOutcome::Unsupported)`.
pub fn set_project_quota(
    dir: BorrowedFd<'_>,
    project: u32,
    bytes: u64,
) -> io::Result<QuotaOutcome> {
    if let Err(why) = enforced(dir) {
        return Ok(QuotaOutcome::Unsupported(why));
    }
    // The descriptor may come from another process (the broker's client),
    // so the walk goes through /proc.
    let path = format!("/proc/self/fd/{}", dir.as_raw_fd());
    let real = std::fs::read_link(&path)?;
    tag_tree(&real, project)?;
    let blocks = bytes.div_ceil(BASIC_BLOCK);
    let mut q = FsDiskQuota {
        version: FS_DQUOT_VERSION,
        flags: FS_PROJ_QUOTA,
        fieldmask: FS_DQ_BHARD | FS_DQ_BSOFT,
        id: project,
        blk_hardlimit: blocks,
        blk_softlimit: blocks,
        ..FsDiskQuota::default()
    };
    quotactl(dir, Q_XSETQLIM, project, (&raw mut q).cast())?;
    Ok(QuotaOutcome::Applied)
}

/// Open a directory for [`set_project_quota`].
pub fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    let c = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()))
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: open with a C string; the result is checked.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new descriptor nobody else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
