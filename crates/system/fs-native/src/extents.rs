//! Physical extent maps (`FIEMAP`), their comparison between two snapshots,
//! and reflink clones.
//!
//! Two snapshots of one copy-on-write file share every extent that was not
//! written in between. A byte range that maps to the same physical bytes in
//! both is unchanged and never needs to be read, let alone re-hashed. That
//! turns "what changed in this 10 GiB image" into a metadata query.

use std::fs::File;
use std::io;
use std::ops::Range;
use std::os::fd::AsFd;

use crate::sys;

/// `FS_IOC_FIEMAP`: `_IOWR('f', 11, struct fiemap)`.
const FS_IOC_FIEMAP: libc::Ioctl = 0xC020_660B;
/// `FICLONE`: `_IOW(0x94, 9, int)`.
const FICLONE: libc::Ioctl = 0x4004_9409;
/// `FICLONERANGE`: `_IOW(0x94, 13, struct file_clone_range)`.
const FICLONERANGE: libc::Ioctl = 0x4020_940d;

const FIEMAP_FLAG_SYNC: u32 = 0x0001;

pub const EXTENT_LAST: u32 = 0x0001;
pub const EXTENT_UNKNOWN: u32 = 0x0002;
pub const EXTENT_DELALLOC: u32 = 0x0004;
pub const EXTENT_ENCODED: u32 = 0x0008;
pub const EXTENT_NOT_ALIGNED: u32 = 0x0100;
pub const EXTENT_DATA_INLINE: u32 = 0x0200;
pub const EXTENT_DATA_TAIL: u32 = 0x0400;
pub const EXTENT_UNWRITTEN: u32 = 0x0800;
pub const EXTENT_SHARED: u32 = 0x2000;

/// Flags under which a physical address says nothing reliable about the
/// bytes, so the range must be treated as changed.
const OPAQUE: u32 =
    EXTENT_UNKNOWN | EXTENT_DELALLOC | EXTENT_DATA_INLINE | EXTENT_DATA_TAIL | EXTENT_NOT_ALIGNED;

const EXTENTS_PER_CALL: usize = 256;

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(
    clippy::struct_field_names,
    reason = "field names follow <linux/fiemap.h>"
)]
struct FiemapExtent {
    fe_logical: u64,
    fe_physical: u64,
    fe_length: u64,
    fe_reserved64: [u64; 2],
    fe_flags: u32,
    fe_reserved: [u32; 3],
}

#[repr(C)]
#[allow(
    clippy::struct_field_names,
    reason = "field names follow <linux/fiemap.h>"
)]
struct Fiemap {
    fm_start: u64,
    fm_length: u64,
    fm_flags: u32,
    fm_mapped_extents: u32,
    fm_extent_count: u32,
    fm_reserved: u32,
    fm_extents: [FiemapExtent; EXTENTS_PER_CALL],
}

#[repr(C)]
struct FileCloneRange {
    src_fd: i64,
    src_offset: u64,
    src_length: u64,
    dest_offset: u64,
}

/// One mapped extent of a file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Extent {
    pub logical: u64,
    pub physical: u64,
    pub length: u64,
    pub flags: u32,
}

impl Extent {
    fn end(&self) -> u64 {
        self.logical.saturating_add(self.length)
    }
}

/// Returns the extent map of `file`, flushing delayed allocation first.
pub fn fiemap(file: &File) -> io::Result<Vec<Extent>> {
    let mut out = Vec::new();
    let mut start: u64 = 0;
    loop {
        // SAFETY: an all-zero `Fiemap` is a valid request header.
        let mut req: Box<Fiemap> = Box::new(unsafe { std::mem::zeroed() });
        req.fm_start = start;
        req.fm_length = u64::MAX.saturating_sub(start);
        req.fm_flags = FIEMAP_FLAG_SYNC;
        req.fm_extent_count = u32::try_from(EXTENTS_PER_CALL).unwrap_or(0);
        // SAFETY: FS_IOC_FIEMAP takes a `struct fiemap` followed by
        // `fm_extent_count` extents, which is exactly `Fiemap`'s layout.
        unsafe { sys::ioctl_ptr(file.as_fd(), FS_IOC_FIEMAP, &raw mut *req) }?;
        let n = usize::try_from(req.fm_mapped_extents).unwrap_or(0);
        let mut last = false;
        for e in req.fm_extents.iter().take(n) {
            out.push(Extent {
                logical: e.fe_logical,
                physical: e.fe_physical,
                length: e.fe_length,
                flags: e.fe_flags,
            });
            last |= e.fe_flags & EXTENT_LAST != 0;
            start = e.fe_logical.saturating_add(e.fe_length);
        }
        if last || n == 0 || n < EXTENTS_PER_CALL {
            return Ok(out);
        }
    }
}

/// What the bytes at one position are made of.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backing {
    Hole,
    /// Physical address of this position and the extent flags.
    Mapped {
        physical: u64,
        flags: u32,
        extent: Extent,
    },
}

fn backing_at(extents: &[Extent], pos: u64) -> (Backing, u64) {
    // Returns the backing at `pos` and the position where it next can change.
    let idx = extents.partition_point(|e| e.end() <= pos);
    match extents.get(idx) {
        Some(e) if e.logical <= pos => (
            Backing::Mapped {
                physical: e.physical.saturating_add(pos.saturating_sub(e.logical)),
                flags: e.flags,
                extent: *e,
            },
            e.end(),
        ),
        Some(e) => (Backing::Hole, e.logical),
        None => (Backing::Hole, u64::MAX),
    }
}

fn same_bytes(a: Backing, b: Backing) -> bool {
    match (a, b) {
        (Backing::Hole, Backing::Hole) => true,
        (
            Backing::Mapped {
                physical: pa,
                flags: fa,
                extent: ea,
            },
            Backing::Mapped {
                physical: pb,
                flags: fb,
                extent: eb,
            },
        ) => {
            if (fa | fb) & OPAQUE != 0 {
                return false;
            }
            if (fa ^ fb) & EXTENT_UNWRITTEN != 0 {
                return false;
            }
            if (fa | fb) & EXTENT_ENCODED != 0 {
                // A compressed extent's physical address names the whole
                // encoded extent, not the byte; only an identical mapping
                // proves identical bytes.
                return ea.logical == eb.logical
                    && ea.physical == eb.physical
                    && ea.length == eb.length;
            }
            pa == pb
        }
        _ => false,
    }
}

/// Byte ranges of the new file (`new`, `new_len` bytes) that may hold
/// different bytes from the old one (`old`, `old_len` bytes).
///
/// The result is conservative: anything the extent maps cannot prove equal
/// is reported as changed. Ranges are sorted and merged.
pub fn changed_ranges(
    old: &[Extent],
    old_len: u64,
    new: &[Extent],
    new_len: u64,
) -> Vec<Range<u64>> {
    let mut out: Vec<Range<u64>> = Vec::new();
    let mut push = |r: Range<u64>| {
        if r.start >= r.end {
            return;
        }
        if let Some(last) = out.last_mut()
            && last.end >= r.start
        {
            last.end = last.end.max(r.end);
            return;
        }
        out.push(r);
    };
    let common = old_len.min(new_len);
    let mut pos = 0u64;
    while pos < common {
        let (a, next_a) = backing_at(old, pos);
        let (b, next_b) = backing_at(new, pos);
        let next = next_a.min(next_b).min(common);
        let next = if next <= pos { common } else { next };
        if !same_bytes(a, b) {
            push(pos..next);
        }
        pos = next;
    }
    if new_len > common {
        push(common..new_len);
    }
    out
}

/// Compares the same file in two snapshots.
pub fn diff_files(old: &File, new: &File) -> io::Result<Vec<Range<u64>>> {
    let old_len = old.metadata()?.len();
    let new_len = new.metadata()?.len();
    Ok(changed_ranges(
        &fiemap(old)?,
        old_len,
        &fiemap(new)?,
        new_len,
    ))
}

/// Makes `dst` share all of `src`'s extents (`FICLONE`). Both must be on the
/// same filesystem; nothing is copied.
pub fn reflink(src: &File, dst: &File) -> io::Result<()> {
    let fd = std::os::fd::AsRawFd::as_raw_fd(src);
    sys::ioctl_int(dst.as_fd(), FICLONE, fd)?;
    Ok(())
}

/// Shares `len` bytes of `src` at `src_offset` into `dst` at `dst_offset`
/// (`FICLONERANGE`). Offsets and length must be block aligned, except that
/// the range may end at `src`'s end of file.
pub fn reflink_range(
    src: &File,
    src_offset: u64,
    len: u64,
    dst: &File,
    dst_offset: u64,
) -> io::Result<()> {
    let mut args = FileCloneRange {
        src_fd: i64::from(std::os::fd::AsRawFd::as_raw_fd(src)),
        src_offset,
        src_length: len,
        dest_offset: dst_offset,
    };
    // SAFETY: FICLONERANGE takes a pointer to `struct file_clone_range`.
    unsafe { sys::ioctl_ptr(dst.as_fd(), FICLONERANGE, &raw mut args) }?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(logical: u64, physical: u64, length: u64) -> Extent {
        Extent {
            logical,
            physical,
            length,
            flags: 0,
        }
    }

    #[test]
    fn extent_diffs_report_exactly_the_changed_blocks() {
        // Phase `identical_maps_have_no_changes`.
        {
            let m = [ext(0, 1 << 20, 65536), ext(65536, 9 << 20, 4096)];
            assert!(changed_ranges(&m, 69632, &m, 69632).is_empty());
        }
        // Phase `a_rewritten_block_inside_a_shared_extent_is_the_only_change`.
        {
            let old = [ext(0, 1 << 20, 1 << 20)];
            let new = [
                ext(0, 1 << 20, 8192),
                ext(8192, 7 << 20, 4096),
                ext(12288, (1 << 20) + 12288, (1 << 20) - 12288),
            ];
            assert_eq!(
                changed_ranges(&old, 1 << 20, &new, 1 << 20),
                vec![8192..12288]
            );
        }
        // Phase `growth_and_hole_fills_are_changes`.
        {
            let old = [ext(0, 1 << 20, 4096)];
            let new = [ext(0, 1 << 20, 4096), ext(8192, 5 << 20, 4096)];
            assert_eq!(changed_ranges(&old, 12288, &new, 20000), vec![8192..20000]);
        }
        // Phase `opaque_extents_are_always_changed`.
        {
            let mut inline = ext(0, 0, 100);
            inline.flags = EXTENT_DATA_INLINE | EXTENT_LAST;
            assert_eq!(changed_ranges(&[inline], 100, &[inline], 100), vec![0..100]);
            let mut enc = ext(0, 1 << 20, 131_072);
            enc.flags = EXTENT_ENCODED;
            assert!(changed_ranges(&[enc], 131_072, &[enc], 131_072).is_empty());
            let mut moved = enc;
            moved.logical = 0;
            moved.length = 65536;
            assert_eq!(
                changed_ranges(
                    &[enc],
                    131_072,
                    &[moved, ext(65536, 3 << 20, 65536)],
                    131_072
                ),
                vec![0..131_072]
            );
        }
    }
}
