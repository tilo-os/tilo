//! Sealing an upper into a tool tree.
//!
//! The upper holds everything the transaction wrote: the files the packages
//! add, and the package manager's own churn — the rpm database and dnf
//! history (copied up from the runtime and rewritten), caches, logs, the
//! mount point of the build's `resolv.conf`. A tool keeps the former only.
//!
//! The sealed tree is plain and deterministic: regular files and symlinks,
//! every file mode 0644 or 0755 (no setuid — a guest mounts tools
//! `nosuid` anyway, and ownership doesn't survive packing), every timestamp
//! [`EPOCH`], no empty directories, no overlay metadata. A file deleted by
//! the transaction (an overlay whiteout) can't be expressed by a lower layer
//! and is reported instead; so is a file that shadows one of the runtime's.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// The timestamp every sealed entry carries.
pub const EPOCH: i64 = 0;

/// Paths (relative to the tree root, `/`-separated) the seal drops along
/// with everything below them. Package-manager state and per-build churn.
pub const EXCLUDED: &[&str] = &[
    // The rpm database and dnf's history: rewritten by every transaction,
    // and the runtime's own copy is the one the guest reads.
    "usr/lib/sysimage/rpm",
    "usr/lib/sysimage/libdnf5",
    "usr/lib/sysimage/dnf",
    "usr/share/rpm",
    "var",
    // Written by the build itself, not by a package.
    "etc/resolv.conf",
    "etc/mtab",
    "etc/hostname",
    "etc/machine-id",
    "etc/ld.so.cache",
    // Shadow databases: unreadable in a guest and per-system.
    "etc/shadow",
    "etc/gshadow",
    "etc/shadow-",
    "etc/gshadow-",
    "etc/passwd-",
    "etc/group-",
    "etc/subuid-",
    "etc/subgid-",
    // Regenerated indexes and caches that differ from build to build.
    "usr/share/info/dir",
    "usr/lib/fontconfig/cache",
    // Debuginfo links: point into packages a tool never has.
    "usr/lib/.build-id",
];

/// File-name suffixes dropped wherever they appear.
const EXCLUDED_SUFFIXES: &[&str] = &[".rpmnew", ".rpmsave", ".rpmorig"];

/// What a seal kept and what it couldn't.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SealReport {
    pub files: u64,
    pub symlinks: u64,
    /// Bytes of regular-file content kept.
    pub bytes: u64,
    /// Excluded subtrees and files that were present.
    pub dropped: Vec<String>,
    /// Runtime files the transaction deleted; a tool can't delete.
    pub whiteouts: Vec<String>,
    /// Kept files that replace one of the runtime's at the same path.
    pub shadows: Vec<String>,
    /// Sockets, fifos and device nodes, which a tool can't carry.
    pub skipped: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("sealing {path}: {source}")]
pub struct SealError {
    pub path: PathBuf,
    #[source]
    pub source: std::io::Error,
}

fn err(path: &Path) -> impl FnOnce(std::io::Error) -> SealError + '_ {
    move |source| SealError {
        path: path.to_owned(),
        source,
    }
}

/// Whether the seal drops `rel` (a `/`-separated path under the tree root).
pub fn is_excluded(rel: &str) -> bool {
    EXCLUDED.iter().any(|ex| {
        rel == *ex
            || rel
                .strip_prefix(ex)
                .is_some_and(|rest| rest.starts_with('/'))
    }) || EXCLUDED_SUFFIXES.iter().any(|s| rel.ends_with(s))
}

/// Move what the packages added from `upper` into `out` (created; must not
/// exist), comparing against `lower` (the runtime tree) to report shadows.
/// `upper` is consumed: kept entries are renamed out of it, so both must be
/// on one filesystem (a copy is the fallback across filesystems).
///
/// # Errors
///
/// The first I/O failure, naming the path.
pub fn seal(upper: &Path, lower: &Path, out: &Path) -> Result<SealReport, SealError> {
    std::fs::create_dir(out).map_err(err(out))?;
    let mut report = SealReport::default();
    walk(upper, lower, out, "", &mut report)?;
    prune_empty(out)?;
    stamp(out)?;
    report.dropped.sort();
    report.whiteouts.sort();
    report.shadows.sort();
    report.skipped.sort();
    Ok(report)
}

fn walk(
    upper: &Path,
    lower: &Path,
    out: &Path,
    rel: &str,
    report: &mut SealReport,
) -> Result<(), SealError> {
    let dir = if rel.is_empty() {
        upper.to_owned()
    } else {
        upper.join(rel)
    };
    // Copied-up directories keep the runtime's mode (0555 for /usr/lib64);
    // the kernel's work dirs are 0000. Make them readable to walk.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).map_err(err(&dir))?;
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .map_err(err(&dir))?
        .collect::<Result<_, _>>()
        .map_err(err(&dir))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            report
                .skipped
                .push(format!("{rel}/{}", name.to_string_lossy()));
            continue;
        };
        let child_rel = if rel.is_empty() {
            name.to_owned()
        } else {
            format!("{rel}/{name}")
        };
        let src = entry.path();
        if is_excluded(&child_rel) {
            report.dropped.push(child_rel);
            continue;
        }
        let meta = std::fs::symlink_metadata(&src).map_err(err(&src))?;
        let ft = meta.file_type();
        let dst = out.join(&child_rel);
        if is_whiteout(&src, &meta) {
            report.whiteouts.push(child_rel);
        } else if ft.is_dir() {
            std::fs::create_dir_all(&dst).map_err(err(&dst))?;
            walk(upper, lower, out, &child_rel, report)?;
        } else if ft.is_symlink() {
            let target = std::fs::read_link(&src).map_err(err(&src))?;
            std::os::unix::fs::symlink(&target, &dst).map_err(err(&dst))?;
            report.symlinks = report.symlinks.saturating_add(1);
            note_shadow(lower, &child_rel, report);
        } else if ft.is_file() {
            move_file(&src, &dst)?;
            let mode = if meta.mode() & 0o111 == 0 {
                0o644
            } else {
                0o755
            };
            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode))
                .map_err(err(&dst))?;
            report.files = report.files.saturating_add(1);
            report.bytes = report.bytes.saturating_add(meta.len());
            note_shadow(lower, &child_rel, report);
        } else {
            report.skipped.push(child_rel);
        }
    }
    Ok(())
}

fn note_shadow(lower: &Path, rel: &str, report: &mut SealReport) {
    if std::fs::symlink_metadata(lower.join(rel)).is_ok_and(|m| !m.is_dir()) {
        report.shadows.push(rel.to_owned());
    }
}

fn move_file(src: &Path, dst: &Path) -> Result<(), SealError> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            std::fs::copy(src, dst).map(drop).map_err(err(dst))
        }
        Err(e) => Err(err(src)(e)),
    }
}

/// An overlay whiteout: a 0/0 character device, or (unprivileged overlay)
/// an empty file marked with the `overlay.whiteout` xattr.
fn is_whiteout(path: &Path, meta: &std::fs::Metadata) -> bool {
    let ft = meta.file_type();
    if ft.is_char_device() && meta.rdev() == 0 {
        return true;
    }
    ft.is_file()
        && meta.len() == 0
        && (has_xattr(path, "user.overlay.whiteout") || has_xattr(path, "trusted.overlay.whiteout"))
}

fn has_xattr(path: &Path, name: &str) -> bool {
    let (Ok(cpath), Ok(cname)) = (
        CString::new(path.as_os_str().as_bytes()),
        CString::new(name),
    ) else {
        return false;
    };
    // SAFETY: both strings are NUL-terminated and outlive the call; a null
    // buffer with size 0 only asks for the value's length.
    let len = unsafe { libc::lgetxattr(cpath.as_ptr(), cname.as_ptr(), std::ptr::null_mut(), 0) };
    len >= 0
}

/// Remove directories that ended up with nothing in them. Returns whether
/// `dir` itself is now empty.
fn prune_empty(dir: &Path) -> Result<bool, SealError> {
    let mut empty = true;
    for entry in std::fs::read_dir(dir).map_err(err(dir))? {
        let entry = entry.map_err(err(dir))?;
        let path = entry.path();
        if entry.file_type().map_err(err(&path))?.is_dir() && prune_empty(&path)? {
            std::fs::remove_dir(&path).map_err(err(&path))?;
        } else {
            empty = false;
        }
    }
    Ok(empty)
}

/// Set every entry's access and modification time to [`EPOCH`], children
/// before their directory, symlinks themselves rather than their targets.
fn stamp(path: &Path) -> Result<(), SealError> {
    let meta = std::fs::symlink_metadata(path).map_err(err(path))?;
    if meta.is_dir() {
        for entry in std::fs::read_dir(path).map_err(err(path))? {
            stamp(&entry.map_err(err(path))?.path())?;
        }
        if meta.mode() & 0o7777 != 0o755 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .map_err(err(path))?;
        }
    }
    set_times(path).map_err(err(path))
}

fn set_times(path: &Path) -> std::io::Result<()> {
    let cpath = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let times = [
        libc::timespec {
            tv_sec: EPOCH,
            tv_nsec: 0,
        },
        libc::timespec {
            tv_sec: EPOCH,
            tv_nsec: 0,
        },
    ];
    // SAFETY: `cpath` is NUL-terminated and `times` is a two-element array,
    // as utimensat requires; AT_FDCWD with an absolute or relative path is
    // valid.
    let rc = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            cpath.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusions_match_whole_components() {
        assert!(is_excluded("usr/lib/sysimage/rpm"));
        assert!(is_excluded("usr/lib/sysimage/rpm/rpmdb.sqlite"));
        assert!(is_excluded("etc/resolv.conf"));
        assert!(is_excluded("etc/foo.conf.rpmnew"));
        assert!(!is_excluded("usr/lib/sysimage/rpm-ostree"));
        assert!(!is_excluded("etc/resolv.conf.d/x"));
        assert!(!is_excluded("usr/bin/rpm"));
        assert!(!is_excluded("etc/passwd"));
    }
}
