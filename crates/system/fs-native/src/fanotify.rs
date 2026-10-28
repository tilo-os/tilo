//! A small, safe fanotify binding: groups, marks, event parsing and
//! permission responses.
//!
//! `libc` covers the classic API; the pre-content (HSM) additions from Linux
//! 6.14 are defined here from `<linux/fanotify.h>`.

use std::ffi::OsString;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::time::Duration;

use crate::sys::{self, FileHandle};

// Event mask bits.
pub const FAN_ACCESS: u64 = 0x0000_0001;
pub const FAN_MODIFY: u64 = 0x0000_0002;
pub const FAN_ATTRIB: u64 = 0x0000_0004;
pub const FAN_CLOSE_WRITE: u64 = 0x0000_0008;
pub const FAN_OPEN: u64 = 0x0000_0020;
pub const FAN_MOVED_FROM: u64 = 0x0000_0040;
pub const FAN_MOVED_TO: u64 = 0x0000_0080;
pub const FAN_CREATE: u64 = 0x0000_0100;
pub const FAN_DELETE: u64 = 0x0000_0200;
pub const FAN_DELETE_SELF: u64 = 0x0000_0400;
pub const FAN_MOVE_SELF: u64 = 0x0000_0800;
pub const FAN_Q_OVERFLOW: u64 = 0x0000_4000;
pub const FAN_OPEN_PERM: u64 = 0x0001_0000;
pub const FAN_ACCESS_PERM: u64 = 0x0002_0000;
/// Pre-content access hook (Linux 6.14): raised before a read, write or
/// mapping touches a byte range of a marked regular file.
pub const FAN_PRE_ACCESS: u64 = 0x0010_0000;
pub const FAN_RENAME: u64 = 0x1000_0000;
pub const FAN_ONDIR: u64 = 0x4000_0000;

// fanotify_init flags.
pub const FAN_CLOEXEC: u32 = 0x0000_0001;
pub const FAN_NONBLOCK: u32 = 0x0000_0002;
pub const FAN_CLASS_NOTIF: u32 = 0x0000_0000;
pub const FAN_CLASS_CONTENT: u32 = 0x0000_0004;
pub const FAN_CLASS_PRE_CONTENT: u32 = 0x0000_0008;
pub const FAN_UNLIMITED_QUEUE: u32 = 0x0000_0010;
pub const FAN_UNLIMITED_MARKS: u32 = 0x0000_0020;
/// Report the accessing thread's id instead of its process id.
pub const FAN_REPORT_TID: u32 = 0x0000_0100;
pub const FAN_REPORT_FID: u32 = 0x0000_0200;
pub const FAN_REPORT_DIR_FID: u32 = 0x0000_0400;
pub const FAN_REPORT_NAME: u32 = 0x0000_0800;
pub const FAN_REPORT_TARGET_FID: u32 = 0x0000_1000;
pub const FAN_REPORT_DFID_NAME: u32 = FAN_REPORT_DIR_FID | FAN_REPORT_NAME;
pub const FAN_REPORT_DFID_NAME_TARGET: u32 =
    FAN_REPORT_DFID_NAME | FAN_REPORT_FID | FAN_REPORT_TARGET_FID;

// fanotify_mark flags.
pub const FAN_MARK_ADD: u32 = 0x0000_0001;
pub const FAN_MARK_REMOVE: u32 = 0x0000_0002;
pub const FAN_MARK_DONT_FOLLOW: u32 = 0x0000_0004;
pub const FAN_MARK_IGNORED_SURV_MODIFY: u32 = 0x0000_0040;
pub const FAN_MARK_FLUSH: u32 = 0x0000_0080;
pub const FAN_MARK_EVICTABLE: u32 = 0x0000_0200;
pub const FAN_MARK_IGNORE: u32 = 0x0000_0400;
pub const FAN_MARK_IGNORE_SURV: u32 = FAN_MARK_IGNORE | FAN_MARK_IGNORED_SURV_MODIFY;
pub const FAN_MARK_INODE: u32 = 0x0000_0000;
pub const FAN_MARK_MOUNT: u32 = 0x0000_0010;
pub const FAN_MARK_FILESYSTEM: u32 = 0x0000_0100;

// Responses.
pub const FAN_ALLOW: u32 = 0x01;
pub const FAN_DENY: u32 = 0x02;

/// `FAN_DENY_ERRNO(err)`: deny a permission event with a specific errno
/// (Linux 6.14, pre-content groups only).
pub fn deny_errno(errno: i32) -> u32 {
    let err = u32::try_from(errno).unwrap_or(0) & 0xff;
    FAN_DENY | err.checked_shl(24).unwrap_or(0)
}

// Info record types.
const INFO_FID: u8 = 1;
const INFO_DFID_NAME: u8 = 2;
const INFO_DFID: u8 = 3;
const INFO_RANGE: u8 = 6;
const INFO_OLD_DFID_NAME: u8 = 10;
const INFO_NEW_DFID_NAME: u8 = 12;

const METADATA_VERSION: u8 = 3;
const FAN_NOFD: i32 = -1;

/// Which object a FID record describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FidKind {
    /// The object itself (`FAN_REPORT_FID` / `FAN_REPORT_TARGET_FID`).
    Object,
    /// Its parent directory, optionally with the entry name.
    Parent,
    RenameFrom,
    /// The target directory entry of a `FAN_RENAME`.
    RenameTo,
}

#[derive(Debug)]
pub enum Info {
    Fid {
        kind: FidKind,
        handle: FileHandle,
        name: Option<OsString>,
    },
    /// The byte range of a pre-content event.
    Range {
        offset: u64,
        count: u64,
    },
    Other(u8),
}

/// One fanotify event. For fd-reporting groups `fd` is an open descriptor
/// of the object; permission events must be answered with
/// [`Group::respond`] before it is dropped.
#[derive(Debug)]
pub struct Event {
    pub mask: u64,
    pub pid: i32,
    pub fd: Option<OwnedFd>,
    pub infos: Vec<Info>,
}

impl Event {
    pub fn is_overflow(&self) -> bool {
        self.mask & FAN_Q_OVERFLOW != 0
    }

    pub fn range(&self) -> Option<(u64, u64)> {
        self.infos.iter().find_map(|i| match i {
            Info::Range { offset, count } => Some((*offset, *count)),
            _ => None,
        })
    }

    pub fn fid(&self, kind: FidKind) -> Option<(&FileHandle, Option<&OsString>)> {
        self.infos.iter().find_map(|i| match i {
            Info::Fid {
                kind: k,
                handle,
                name,
            } if *k == kind => Some((handle, name.as_ref())),
            _ => None,
        })
    }
}

#[derive(Debug)]
pub struct Group {
    fd: OwnedFd,
}

impl AsFd for Group {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl Group {
    /// `fanotify_init(2)`. `FAN_CLOEXEC` is always added.
    pub fn new(flags: u32, event_f_flags: u32) -> io::Result<Self> {
        // SAFETY: plain syscall; the result is checked.
        let fd = sys::cvt(unsafe { libc::fanotify_init(flags | FAN_CLOEXEC, event_f_flags) })?;
        // SAFETY: a fresh descriptor that nothing else owns.
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }

    /// `fanotify_mark(2)` on `path`, resolved relative to `dir` (or the
    /// current directory). With `path == None` the mark is placed on `dir`
    /// itself.
    pub fn mark(
        &self,
        flags: u32,
        mask: u64,
        dir: Option<BorrowedFd<'_>>,
        path: Option<&Path>,
    ) -> io::Result<()> {
        let dirfd = dir.map_or(libc::AT_FDCWD, |d| d.as_raw_fd());
        let c = path.map(sys::cstring).transpose()?;
        let ptr = c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        // SAFETY: `ptr` is null or a NUL-terminated string that outlives the call.
        sys::cvt(unsafe { libc::fanotify_mark(self.fd.as_raw_fd(), flags, mask, dirfd, ptr) })?;
        Ok(())
    }

    /// Marks the object behind an open descriptor.
    pub fn mark_fd(&self, flags: u32, mask: u64, fd: BorrowedFd<'_>) -> io::Result<()> {
        self.mark(flags, mask, Some(fd), None)
    }

    /// Waits up to `timeout` for events (`None` blocks) and returns what is
    /// queued. An empty vector means the timeout elapsed.
    pub fn read(&self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Vec<Event>> {
        let ms = timeout.map_or(-1, |t| {
            libc::c_int::try_from(t.as_millis()).unwrap_or(i32::MAX)
        });
        let mut pfd = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // SAFETY: one valid pollfd.
            let ret = unsafe { libc::poll(&raw mut pfd, 1, ms) };
            if ret < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if ret == 0 {
                return Ok(Vec::new());
            }
            break;
        }
        // SAFETY: `buf` is writable for its length.
        let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        let Ok(n) = usize::try_from(n) else {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock || err.kind() == io::ErrorKind::Interrupted {
                return Ok(Vec::new());
            }
            return Err(err);
        };
        parse_events(buf.get(..n).unwrap_or_default())
    }

    /// Answers a permission event.
    pub fn respond(&self, event_fd: RawFd, response: u32) -> io::Result<()> {
        let resp = libc::fanotify_response {
            fd: event_fd,
            response,
        };
        let size = std::mem::size_of::<libc::fanotify_response>();
        // SAFETY: `resp` is a valid `fanotify_response` of `size` bytes.
        let n = unsafe {
            libc::write(
                self.fd.as_raw_fd(),
                (&raw const resp).cast::<libc::c_void>(),
                size,
            )
        };
        if usize::try_from(n).ok() == Some(size) {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(
        b.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn i32_at(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_ne_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(
        b.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn malformed() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "malformed fanotify event")
}

/// Parses a buffer returned by `read(2)` on a fanotify group.
///
/// Layout of `struct fanotify_event_metadata`: `event_len` u32, `vers` u8,
/// reserved u8, `metadata_len` u16, `mask` u64, `fd` i32, `pid` i32.
pub(crate) fn parse_events(mut buf: &[u8]) -> io::Result<Vec<Event>> {
    let mut events = Vec::new();
    while !buf.is_empty() {
        let event_len = u32_at(buf, 0).ok_or_else(malformed)? as usize;
        let vers = *buf.get(4).ok_or_else(malformed)?;
        let meta_len = usize::from(u16_at(buf, 6).ok_or_else(malformed)?);
        let mask = u64_at(buf, 8).ok_or_else(malformed)?;
        let fd = i32_at(buf, 16).ok_or_else(malformed)?;
        let pid = i32_at(buf, 20).ok_or_else(malformed)?;
        if vers != METADATA_VERSION || event_len < meta_len {
            return Err(malformed());
        }
        let body = buf.get(meta_len..event_len).ok_or_else(malformed)?;
        let owned = (fd >= 0 && fd != FAN_NOFD).then(|| {
            // SAFETY: the kernel installed this descriptor for us; it is ours.
            unsafe { OwnedFd::from_raw_fd(fd) }
        });
        events.push(Event {
            mask,
            pid,
            fd: owned,
            infos: parse_infos(body)?,
        });
        buf = buf.get(event_len..).ok_or_else(malformed)?;
    }
    Ok(events)
}

fn parse_infos(mut body: &[u8]) -> io::Result<Vec<Info>> {
    let mut infos = Vec::new();
    while body.len() >= 4 {
        let info_type = *body.first().ok_or_else(malformed)?;
        let len = usize::from(u16_at(body, 2).ok_or_else(malformed)?);
        if len < 4 {
            return Err(malformed());
        }
        let rec = body.get(..len).ok_or_else(malformed)?;
        let info = match info_type {
            INFO_FID | INFO_DFID | INFO_DFID_NAME | INFO_OLD_DFID_NAME | INFO_NEW_DFID_NAME => {
                // header(4) + fsid(8) + struct file_handle [+ name\0]
                let handle_raw = rec.get(12..).ok_or_else(malformed)?;
                let handle = FileHandle::parse(handle_raw).ok_or_else(malformed)?;
                let handle_end = 8usize
                    .checked_add(handle.bytes.len())
                    .ok_or_else(malformed)?;
                let has_name = matches!(
                    info_type,
                    INFO_DFID_NAME | INFO_OLD_DFID_NAME | INFO_NEW_DFID_NAME
                );
                let name = if has_name {
                    let tail = handle_raw.get(handle_end..).unwrap_or_default();
                    let end = tail.iter().position(|b| *b == 0).unwrap_or(tail.len());
                    let n = tail.get(..end).unwrap_or_default();
                    (!n.is_empty() && n != b".").then(|| sys::os_string(n))
                } else {
                    None
                };
                let kind = match info_type {
                    INFO_FID => FidKind::Object,
                    INFO_OLD_DFID_NAME => FidKind::RenameFrom,
                    INFO_NEW_DFID_NAME => FidKind::RenameTo,
                    _ => FidKind::Parent,
                };
                Info::Fid { kind, handle, name }
            }
            INFO_RANGE => Info::Range {
                offset: u64_at(rec, 8).ok_or_else(malformed)?,
                count: u64_at(rec, 16).ok_or_else(malformed)?,
            },
            other => Info::Other(other),
        };
        infos.push(info);
        body = body.get(len..).ok_or_else(malformed)?;
    }
    Ok(infos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(event_len: u32, mask: u64, fd: i32, infos: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&event_len.to_ne_bytes());
        v.push(METADATA_VERSION);
        v.push(0);
        v.extend_from_slice(&24u16.to_ne_bytes());
        v.extend_from_slice(&mask.to_ne_bytes());
        v.extend_from_slice(&fd.to_ne_bytes());
        v.extend_from_slice(&42i32.to_ne_bytes());
        v.extend_from_slice(infos);
        v
    }

    #[test]
    fn fanotify_records_parse_strictly_and_deny_errno_is_encoded() {
        {
            let mut fid = Vec::new();
            fid.push(INFO_DFID_NAME);
            fid.push(0);
            let handle = [1u8, 2, 3, 4, 5, 6, 7, 8];
            let name = b"hello\0\0\0";
            let len = 4 + 8 + 8 + handle.len() + name.len();
            fid.extend_from_slice(&u16::try_from(len).unwrap().to_ne_bytes());
            fid.extend_from_slice(&[0u8; 8]);
            fid.extend_from_slice(&u32::try_from(handle.len()).unwrap().to_ne_bytes());
            fid.extend_from_slice(&7i32.to_ne_bytes());
            fid.extend_from_slice(&handle);
            fid.extend_from_slice(name);
            let mut range = vec![INFO_RANGE, 0];
            range.extend_from_slice(&24u16.to_ne_bytes());
            range.extend_from_slice(&0u32.to_ne_bytes());
            range.extend_from_slice(&4096u64.to_ne_bytes());
            range.extend_from_slice(&512u64.to_ne_bytes());
            let mut infos = fid;
            infos.extend_from_slice(&range);
            let total = u32::try_from(24 + infos.len()).unwrap();
            let buf = record(total, FAN_CREATE, FAN_NOFD, &infos);
            let events = parse_events(&buf).unwrap();
            assert_eq!(events.len(), 1);
            let ev = &events[0];
            assert_eq!(ev.mask, FAN_CREATE);
            assert_eq!(ev.pid, 42);
            assert!(ev.fd.is_none());
            let (h, n) = ev.fid(FidKind::Parent).unwrap();
            assert_eq!(h.bytes, handle);
            assert_eq!(h.handle_type, 7);
            assert_eq!(n.unwrap(), "hello");
            assert_eq!(ev.range(), Some((4096, 512)));
        }
        {
            let buf = record(64, FAN_CREATE, FAN_NOFD, &[]);
            assert!(parse_events(&buf).is_err());
        }
        {
            assert_eq!(deny_errno(libc::EIO), FAN_DENY | (5 << 24));
        }
    }
}
