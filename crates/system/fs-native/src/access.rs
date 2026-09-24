//! Telling a read from a write when a permission event arrives.
//!
//! Pre-content events (`FAN_PRE_ACCESS`) do not say whether the access
//! reads or writes, and `FAN_OPEN_PERM` does not say how the file is being
//! opened. The accessing thread, however, is blocked inside the system call
//! that raised the event until the listener answers, so its
//! `/proc/<tid>/syscall` names that call and its arguments exactly: `read`
//! or `pwrite64`, the descriptor it goes through, `openat`'s flags, `mmap`'s
//! protection. With `FAN_REPORT_TID` the event carries that thread's id.
//!
//! This decides per access, not per descriptor: a process that opened a
//! file read-write and only reads it is a reader. When the call cannot be
//! read (the thread is not blocked in a call, or `/proc` is not readable
//! with the daemon's privileges) the answer is [`Access::Unknown`] and the
//! caller falls back to looking at the process's open descriptors.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

/// What an access does to the file it touches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Unknown,
}

/// The system call a blocked thread is in, from `/proc/<tid>/syscall`:
/// its number and its six arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Syscall {
    pub nr: libc::c_long,
    pub args: [u64; 6],
}

/// Parses `/proc/<tid>/syscall`: `nr arg0 … arg5 sp pc`, arguments in hex.
/// `running` or a negative number (not in a call) yields `None`.
pub fn parse_syscall(text: &str) -> Option<Syscall> {
    let mut fields = text.split_whitespace();
    let nr: libc::c_long = fields.next()?.parse().ok()?;
    if nr < 0 {
        return None;
    }
    let mut args = [0u64; 6];
    for a in &mut args {
        let f = fields.next()?;
        *a = u64::from_str_radix(f.trim_start_matches("0x"), 16).ok()?;
    }
    Some(Syscall { nr, args })
}

fn proc_task(tid: i32) -> PathBuf {
    PathBuf::from("/proc").join(tid.to_string())
}

/// Whether descriptor `fd` of thread `tid` refers to the inode `key`.
fn fd_is(tid: i32, fd: u64, key: (u64, u64)) -> bool {
    let Ok(fd) = i32::try_from(fd) else {
        return false;
    };
    fs::metadata(proc_task(tid).join("fd").join(fd.to_string()))
        .is_ok_and(|m| (m.dev(), m.ino()) == key)
}

/// Reads the `u64` at `addr` in thread `tid`'s memory (`openat2`'s flags).
fn read_u64(tid: i32, addr: u64) -> Option<u64> {
    use std::os::unix::fs::FileExt as _;
    let mem = fs::File::open(proc_task(tid).join("mem")).ok()?;
    let mut buf = [0u8; 8];
    mem.read_exact_at(&mut buf, addr).ok()?;
    Some(u64::from_ne_bytes(buf))
}

fn open_flags_write(flags: u64) -> bool {
    let flags = i32::try_from(flags & 0xffff_ffff).unwrap_or(0);
    flags & libc::O_ACCMODE != libc::O_RDONLY || flags & libc::O_TRUNC != 0
}

/// Classifies one call against the gated file. `refers` says whether a
/// descriptor of the calling thread is that file; `openat2_flags` reads
/// the flags of an `openat2` call's `struct open_how`.
pub fn classify_call(
    call: &Syscall,
    refers: &dyn Fn(u64) -> bool,
    openat2_flags: &dyn Fn(u64) -> Option<u64>,
) -> Access {
    let [a0, a1, a2, a3, ..] = call.args;
    let nr = call.nr;
    let is = |n: libc::c_long| nr == n;
    if is(libc::SYS_read)
        || is(libc::SYS_pread64)
        || is(libc::SYS_readv)
        || is(libc::SYS_preadv)
        || is(libc::SYS_preadv2)
        || is(libc::SYS_execve)
        || is(libc::SYS_execveat)
    {
        return Access::Read;
    }
    if is(libc::SYS_write)
        || is(libc::SYS_pwrite64)
        || is(libc::SYS_writev)
        || is(libc::SYS_pwritev)
        || is(libc::SYS_pwritev2)
        || is(libc::SYS_fallocate)
        || is(libc::SYS_ftruncate)
        || is(libc::SYS_truncate)
    {
        return Access::Write;
    }
    if is(libc::SYS_mmap) {
        let prot = i32::try_from(a2 & 0xffff_ffff).unwrap_or(0);
        let flags = i32::try_from(a3 & 0xffff_ffff).unwrap_or(0);
        let shared_write = prot & libc::PROT_WRITE != 0 && flags & libc::MAP_SHARED != 0;
        return if shared_write {
            Access::Write
        } else {
            Access::Read
        };
    }
    // Two descriptors: the file is written when it is the destination.
    if is(libc::SYS_sendfile) {
        return if refers(a0) {
            Access::Write
        } else {
            Access::Read
        };
    }
    if is(libc::SYS_splice) || is(libc::SYS_copy_file_range) {
        return if refers(a2) {
            Access::Write
        } else {
            Access::Read
        };
    }
    if is(libc::SYS_ioctl) {
        // FICLONE/FICLONERANGE go through the destination's descriptor.
        return if refers(a0) {
            Access::Write
        } else {
            Access::Read
        };
    }
    if is(libc::SYS_openat) {
        return if open_flags_write(a2) {
            Access::Write
        } else {
            Access::Read
        };
    }
    #[cfg(target_arch = "x86_64")]
    if is(libc::SYS_open) {
        return if open_flags_write(a1) {
            Access::Write
        } else {
            Access::Read
        };
    }
    #[cfg(target_arch = "x86_64")]
    if is(libc::SYS_creat) {
        return Access::Write;
    }
    if is(libc::SYS_openat2) {
        return match openat2_flags(a2) {
            Some(flags) if open_flags_write(flags) => Access::Write,
            Some(_) => Access::Read,
            None => Access::Unknown,
        };
    }
    let _ = a1;
    Access::Unknown
}

/// What the access by blocked thread `tid` does to the file `key`.
pub fn classify(tid: i32, key: (u64, u64)) -> Access {
    let Ok(text) = fs::read_to_string(proc_task(tid).join("syscall")) else {
        return Access::Unknown;
    };
    let Some(call) = parse_syscall(&text) else {
        return Access::Unknown;
    };
    classify_call(&call, &|fd| fd_is(tid, fd, key), &|addr| {
        read_u64(tid, addr)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(nr: libc::c_long, args: [u64; 6]) -> Syscall {
        Syscall { nr, args }
    }

    fn of(c: &Syscall) -> Access {
        classify_call(c, &|fd| fd == 3, &|_| None)
    }

    #[test]
    fn the_blocked_call_decides_read_or_write() {
        let text = format!(
            "{} 0x3 0x7ffd1000 0x1000 0x0 0x0 0x0 0x7ffd0ff8 0x7f00deadbeef",
            libc::SYS_pread64
        );
        let parsed = parse_syscall(&text).unwrap();
        assert_eq!(parsed.args[2], 0x1000);
        assert_eq!(of(&parsed), Access::Read);
        assert_eq!(parse_syscall("running"), None);
        assert_eq!(parse_syscall("-1 0x0 0x0"), None);

        assert_eq!(
            of(&call(libc::SYS_pwrite64, [3, 0, 1, 0, 0, 0])),
            Access::Write
        );
        let map = |prot: i32, flags: i32| {
            call(
                libc::SYS_mmap,
                [
                    0,
                    4096,
                    u64::from(prot.cast_unsigned()),
                    u64::from(flags.cast_unsigned()),
                    3,
                    0,
                ],
            )
        };
        assert_eq!(of(&map(libc::PROT_READ, libc::MAP_SHARED)), Access::Read);
        assert_eq!(
            of(&map(libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE)),
            Access::Read,
            "a private writable mapping never writes the file"
        );
        assert_eq!(
            of(&map(libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED)),
            Access::Write
        );
        // The gated file (descriptor 3) as source or destination.
        assert_eq!(
            of(&call(libc::SYS_copy_file_range, [3, 0, 4, 0, 9, 0])),
            Access::Read
        );
        assert_eq!(
            of(&call(libc::SYS_copy_file_range, [4, 0, 3, 0, 9, 0])),
            Access::Write
        );
        assert_eq!(
            of(&call(libc::SYS_sendfile, [3, 4, 0, 9, 0, 0])),
            Access::Write
        );
        let rdwr = libc::O_RDWR as u64;
        assert_eq!(
            of(&call(libc::SYS_openat, [0, 0, rdwr, 0, 0, 0])),
            Access::Write
        );
        assert_eq!(
            of(&call(libc::SYS_openat, [0, 0, 0, 0, 0, 0])),
            Access::Read
        );
        let trunc = (libc::O_RDONLY | libc::O_TRUNC) as u64;
        assert_eq!(
            of(&call(libc::SYS_openat, [0, 0, trunc, 0, 0, 0])),
            Access::Write
        );
        assert_eq!(
            of(&call(libc::SYS_openat2, [0, 0, 0x1000, 24, 0, 0])),
            Access::Unknown
        );
        assert_eq!(of(&call(libc::SYS_getpid, [0; 6])), Access::Unknown);
    }
}
