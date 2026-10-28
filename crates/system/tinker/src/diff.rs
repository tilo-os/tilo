//! Reading a layer's change set straight out of the overlay upper directory.
//!
//! overlayfs records everything a layer did in its upper directory, fully self-describing:
//!
//! * a character device with major 0 / minor 0 is a *whiteout* (deleted, base intact
//!   underneath);
//! * a directory with the `overlay.opaque` xattr replaced the base directory wholesale;
//! * any other entry that also exists in the base was modified (overlayfs copies up on
//!   first write);
//! * anything else is new.
//!
//! No bookkeeping at layer-write time is needed, so the change set is derived
//! directly from the layer and base directories.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fmt;
use std::path::{Path, PathBuf};

/// What a layer did to one path, relative to the base it was stacked on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    /// The path did not exist in the base.
    Added,
    Modified,
    /// The layer deleted a path that exists in the base. Recorded as an overlayfs whiteout;
    /// deleting the whiteout restores the base copy.
    Deleted,
    /// The layer replaced a base directory instead of merging into it, so base entries under
    /// this path are hidden even though they still exist.
    Replaced,
}

impl ChangeKind {
    /// Single-character marker, in the shape people already read in `git status` and `rpm -V` output.
    pub fn marker(self) -> char {
        match self {
            ChangeKind::Added => '+',
            ChangeKind::Modified => '~',
            ChangeKind::Deleted => '-',
            ChangeKind::Replaced => '!',
        }
    }
}

/// One changed path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Change {
    /// Path as it appears inside the layer, absolute and base-relative (`/usr/bin/cowsay`, not
    /// the upper-directory path on the host).
    pub path: PathBuf,
    pub kind: ChangeKind,
    /// Size in bytes for regular files; `None` for whiteouts and directories.
    pub size: Option<u64>,
}

/// A layer's complete change set, plus the interpretation a person actually wants: which
/// packages arrived, and what was changed by hand.
#[derive(Debug, Clone, Default)]
pub struct Diff {
    pub changes: Vec<Change>,
    /// Packages whose install receipt appears in the layer, by package manager name. Derived
    /// from the manager's own on-disk records, not guessed from filenames.
    pub packages: BTreeMap<String, Vec<String>>,
    /// Every path claimed by one of those packages' file manifests.
    owned_by_packages: std::collections::BTreeSet<PathBuf>,
}

impl Diff {
    /// Changes that are not accounted for by an installed package.
    ///
    /// The distinction that makes a diff worth reading: installing nine small packages writes
    /// about 1900 upper-directory entries, and reporting that number teaches people to ignore
    /// the diff. Almost all of those files are explained by a package manifest; what's left is
    /// what the person did with their own hands — the part nobody else can reconstruct. So the
    /// summary owed is "nine packages, and these four other things", not "1848 changes".
    pub fn substantive(&self) -> impl Iterator<Item = &Change> {
        self.changes.iter().filter(|c| {
            if is_pkg_metadata(&c.path) {
                return false;
            }
            // A deletion or directory replacement is never something a package did on the way in, so
            // package ownership does not excuse it — also the most surprising changes a layer can hold.
            if matches!(c.kind, ChangeKind::Deleted | ChangeKind::Replaced) {
                return true;
            }
            !self.owned_by_packages.contains(&c.path)
        })
    }

    /// Changes hidden from [`Diff::substantive`]: package manager bookkeeping plus files a
    /// package's own manifest accounts for.
    pub fn explained_count(&self) -> usize {
        // `substantive` filters `changes`, so its count is always <= `changes.len()`;
        // saturating just avoids spelling that invariant as an unchecked subtraction.
        self.changes
            .len()
            .saturating_sub(self.substantive().count())
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn total_bytes(&self) -> u64 {
        self.changes.iter().filter_map(|c| c.size).sum()
    }
}

impl fmt::Display for Diff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (mgr, pkgs) in &self.packages {
            writeln!(f, "packages ({mgr}): {}", pkgs.join(" "))?;
        }
        for c in self.substantive() {
            writeln!(f, "  {} {}", c.kind.marker(), c.path.display())?;
        }
        let hidden = self.explained_count();
        if hidden > 0 {
            writeln!(
                f,
                "  … and {hidden} more that those packages account for (--all to show)"
            )?;
        }
        Ok(())
    }
}

/// Paths that exist only because a package manager runs a database there — present in the
/// diff, but not what anyone means by "what changed".
fn is_pkg_metadata(path: &Path) -> bool {
    const PREFIXES: &[&str] = &[
        "/var/lib/dpkg",
        "/var/lib/apt",
        "/var/cache/apt",
        "/var/cache/debconf",
        "/var/lib/rpm",
        "/var/cache/dnf",
        "/var/cache/libdnf5",
        "/var/lib/dnf",
        "/var/log/apt",
        "/var/log/dpkg.log",
    ];
    let s = path.to_string_lossy();
    PREFIXES
        .iter()
        .any(|p| s == *p || s.starts_with(&format!("{p}/")))
}

/// Walk `upper` and classify every entry against `base`. `base` is the lower directory the
/// layer was stacked on, read only to answer "did this path already exist" — a missing base
/// yields an all-`Added` diff rather than an error.
///
/// Filesystem failures for individual entries are represented as unknown-size
/// additions or skipped; failures opening `upper` are returned.
///
/// # Errors
///
/// Returns an I/O error when the upper layer cannot be traversed.
pub fn compute(upper: &Path, base: Option<&Path>) -> std::io::Result<Diff> {
    let mut changes = Vec::new();

    for entry in walkdir::WalkDir::new(upper)
        .min_depth(1)
        .follow_links(false)
    {
        let entry = match entry {
            Ok(e) => e,
            // A namespace-mapped uid can write entries the plain host user cannot stat. Skipping them
            // silently would under-report the diff, so they surface as unknown-size additions instead.
            Err(e) => {
                if let Some(rel) = e.path().and_then(|p| p.strip_prefix(upper).ok()) {
                    changes.push(Change {
                        path: Path::new("/").join(rel),
                        kind: ChangeKind::Added,
                        size: None,
                    });
                }
                continue;
            }
        };

        // Every entry WalkDir yields is nested under `upper`; treat an unexpected miss as
        // unclassifiable rather than panicking on it.
        let Ok(rel) = entry.path().strip_prefix(upper) else {
            continue;
        };
        let logical = Path::new("/").join(rel);
        let Ok(meta) = entry.metadata() else {
            continue;
        };

        if is_whiteout(&meta) {
            changes.push(Change {
                path: logical,
                kind: ChangeKind::Deleted,
                size: None,
            });
            continue;
        }

        if meta.is_dir() {
            // Plain directories in the upper dir are usually just the parent path of a real change and
            // carry no information. An opaque directory only means something when the base has that
            // directory too — base entries under it would then be hidden despite still existing. Package
            // managers create brand-new directories opaque routinely, and reporting each as a
            // "replacement" would bury the handful that really are one.
            if is_opaque(entry.path()) && base.is_some_and(|b| b.join(rel).is_dir()) {
                changes.push(Change {
                    path: logical,
                    kind: ChangeKind::Replaced,
                    size: None,
                });
            }
            continue;
        }

        let existed = base.is_some_and(|b| b.join(rel).symlink_metadata().is_ok());
        changes.push(Change {
            path: logical,
            kind: if existed {
                ChangeKind::Modified
            } else {
                ChangeKind::Added
            },
            size: Some(meta.len()),
        });
    }

    changes.sort();
    let (packages, owned_by_packages) = read_package_receipts(upper);
    Ok(Diff {
        changes,
        packages,
        owned_by_packages,
    })
}

/// overlayfs marks a deleted path with a 0:0 character device.
fn is_whiteout(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    meta.file_type().is_char_device() && meta.rdev() == 0
}

/// An `overlay.opaque` xattr means "do not merge the base directory under me". Lives in the
/// `trusted.` namespace for privileged mounts and `user.` for unprivileged `userxattr`
/// mounts a work layer uses, so both are checked.
fn is_opaque(path: &Path) -> bool {
    ["user.overlay.opaque", "trusted.overlay.opaque"]
        .iter()
        .any(|attr| getxattr(path, attr).as_deref() == Some(b"y"))
}

fn getxattr(path: &Path, name: &str) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    let cpath = CString::new(path.as_os_str().as_bytes()).ok()?;
    let cname = CString::new(name).ok()?;
    let mut buf = [0u8; 64];
    // SAFETY: both strings are NUL-terminated and outlive the call; the buffer
    // length passed is the real length of `buf`.
    let n = unsafe {
        libc::lgetxattr(
            cpath.as_ptr(),
            cname.as_ptr(),
            buf.as_mut_ptr().cast::<libc::c_void>(),
            buf.len(),
        )
    };
    // A negative return is `lgetxattr`'s error signal (absent attribute, too-small buffer,
    // ...); `try_from` folds that check into the same conversion that avoids an unchecked
    // sign-losing cast.
    let n = usize::try_from(n).ok()?;
    buf.get(..n).map(<[u8]>::to_vec)
}

/// Read package names, and the files they claim, out of the package managers' own install
/// records inside the layer.
///
/// dpkg writes one `<pkg>.list` per installed package under `/var/lib/dpkg/info`, its
/// complete file manifest. Since a layer contains only files it wrote itself, every receipt
/// found is by construction a package *this layer* added — no base comparison needed. These
/// manifests are what lets the diff separate "a package put this here" from "a person put
/// this here".
///
/// rpm keeps a database rather than per-package files, so its package list is left to the
/// in-layer `rpm` binary and is not derived here.
fn read_package_receipts(
    upper: &Path,
) -> (
    BTreeMap<String, Vec<String>>,
    std::collections::BTreeSet<PathBuf>,
) {
    let mut out = BTreeMap::new();
    let mut owned = std::collections::BTreeSet::new();
    let dpkg_info = upper.join("var/lib/dpkg/info");
    let Ok(rd) = std::fs::read_dir(&dpkg_info) else {
        return (out, owned);
    };

    let mut pkgs: Vec<String> = Vec::new();
    for entry in rd.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".list") else {
            continue;
        };
        // `pkg:arch.list` for multi-arch; the architecture is noise in a summary.
        pkgs.push(stem.split(':').next().unwrap_or(stem).to_string());
        if let Ok(manifest) = std::fs::read_to_string(entry.path()) {
            for line in manifest.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    owned.insert(PathBuf::from(line));
                }
            }
        }
    }
    pkgs.sort();
    pkgs.dedup();
    if !pkgs.is_empty() {
        out.insert("apt".to_string(), pkgs);
    }
    (out, owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn diffs_tell_added_modified_and_deleted_paths_apart_by_base() {
        {
            let d = scratch();
            let base = d.path().join("base");
            let upper = d.path().join("upper");
            fs::create_dir_all(base.join("usr/bin")).unwrap();
            fs::create_dir_all(upper.join("usr/bin")).unwrap();
            fs::write(base.join("usr/bin/existing"), b"old").unwrap();
            fs::write(upper.join("usr/bin/existing"), b"new").unwrap();
            fs::write(upper.join("usr/bin/fresh"), b"hi").unwrap();

            let diff = compute(&upper, Some(&base)).unwrap();
            let by_path: BTreeMap<_, _> = diff
                .changes
                .iter()
                .map(|c| (c.path.clone(), c.kind))
                .collect();
            assert_eq!(
                by_path[Path::new("/usr/bin/existing")],
                ChangeKind::Modified
            );
            assert_eq!(by_path[Path::new("/usr/bin/fresh")], ChangeKind::Added);
            // Plain parent directories are structure, not change.
            assert!(!by_path.contains_key(Path::new("/usr")));
            assert!(!by_path.contains_key(Path::new("/usr/bin")));
        }
        {
            let d = scratch();
            let upper = d.path().join("upper");
            fs::create_dir_all(upper.join("etc")).unwrap();
            fs::write(upper.join("etc/hosts"), b"x").unwrap();

            let diff = compute(&upper, None).unwrap();
            assert_eq!(diff.changes.len(), 1);
            assert_eq!(diff.changes[0].kind, ChangeKind::Added);
        }
        // Phase `a_brand_new_directory_is_not_reported_as_a_replacement`.
        // `user.overlay.opaque` can only be set on a real overlay mount, so these two tests cover
        // the base-existence half directly; the xattr half is exercised by the live layer path.
        {
            let d = scratch();
            let base = d.path().join("base");
            let upper = d.path().join("upper");
            fs::create_dir_all(&base).unwrap();
            // dpkg creates directories like this constantly; none of them replace anything, since the
            // base has no such path.
            fs::create_dir_all(upper.join("usr/share/cowsay/cows")).unwrap();
            fs::write(upper.join("usr/share/cowsay/cows/default.cow"), b"cow").unwrap();

            let diff = compute(&upper, Some(&base)).unwrap();
            assert!(
                !diff.changes.iter().any(|c| c.kind == ChangeKind::Replaced),
                "{:?}",
                diff.changes
            );
        }
        {
            // `/var/lib/dpkg-custom` is not dpkg's database and must stay visible.
            assert!(is_pkg_metadata(Path::new("/var/lib/dpkg/status")));
            assert!(is_pkg_metadata(Path::new("/var/lib/dpkg")));
            assert!(!is_pkg_metadata(Path::new("/var/lib/dpkg-custom/thing")));
            assert!(!is_pkg_metadata(Path::new("/usr/bin/dpkg")));
        }
    }

    #[test]
    fn package_bookkeeping_and_claimed_files_are_attributed_to_packages() {
        {
            let d = scratch();
            let upper = d.path().join("upper");
            let info = upper.join("var/lib/dpkg/info");
            fs::create_dir_all(&info).unwrap();
            fs::write(info.join("cowsay.list"), b"/usr/games/cowsay\n").unwrap();
            fs::write(info.join("cowsay.md5sums"), b"").unwrap();
            fs::write(info.join("libfoo1:amd64.list"), b"").unwrap();

            let diff = compute(&upper, None).unwrap();
            assert_eq!(diff.packages["apt"], vec!["cowsay", "libfoo1"]);
        }
        {
            let d = scratch();
            let upper = d.path().join("upper");
            fs::create_dir_all(upper.join("var/lib/dpkg/info")).unwrap();
            fs::create_dir_all(upper.join("usr/games")).unwrap();
            // An empty manifest claims nothing, so the binary stays unexplained.
            fs::write(upper.join("var/lib/dpkg/info/cowsay.list"), b"").unwrap();
            fs::write(upper.join("var/lib/dpkg/status"), b"").unwrap();
            fs::write(upper.join("usr/games/cowsay"), b"#!/bin/sh\n").unwrap();

            let diff = compute(&upper, None).unwrap();
            let substantive: Vec<_> = diff.substantive().map(|c| c.path.clone()).collect();
            assert_eq!(substantive, vec![PathBuf::from("/usr/games/cowsay")]);
            assert_eq!(diff.explained_count(), 2);
        }
        {
            // The point of the whole exercise: nine packages' worth of files must not drown the one
            // config a person edited by hand.
            let d = scratch();
            let upper = d.path().join("upper");
            let info = upper.join("var/lib/dpkg/info");
            fs::create_dir_all(&info).unwrap();
            fs::create_dir_all(upper.join("usr/games")).unwrap();
            fs::create_dir_all(upper.join("usr/share/cowsay/cows")).unwrap();
            fs::create_dir_all(upper.join("etc")).unwrap();

            fs::write(
                info.join("cowsay.list"),
                b"/usr/games/cowsay\n/usr/share/cowsay/cows/default.cow\n",
            )
            .unwrap();
            fs::write(upper.join("usr/games/cowsay"), b"bin").unwrap();
            fs::write(upper.join("usr/share/cowsay/cows/default.cow"), b"cow").unwrap();
            // Not claimed by any package: this is the user's own work.
            fs::write(upper.join("etc/my-tweak.conf"), b"tuned = yes").unwrap();

            let diff = compute(&upper, None).unwrap();
            let substantive: Vec<_> = diff.substantive().map(|c| c.path.clone()).collect();
            assert_eq!(substantive, vec![PathBuf::from("/etc/my-tweak.conf")]);
            assert_eq!(diff.packages["apt"], vec!["cowsay"]);
            // Nothing is lost — the full change set is still there to be printed.
            assert_eq!(diff.changes.len(), 4);
        }
        {
            // Deleting a file a package installed is the user's doing and the single most surprising
            // thing a layer can hide; ownership must not excuse it. (Constructed directly — a real
            // whiteout needs a mount.)
            let mut owned = std::collections::BTreeSet::new();
            owned.insert(PathBuf::from("/usr/games/cowsay"));
            owned.insert(PathBuf::from("/usr/share/cowsay/cows/default.cow"));
            let diff = Diff {
                changes: vec![
                    Change {
                        path: PathBuf::from("/usr/games/cowsay"),
                        kind: ChangeKind::Deleted,
                        size: None,
                    },
                    Change {
                        path: PathBuf::from("/usr/share/cowsay/cows/default.cow"),
                        kind: ChangeKind::Added,
                        size: Some(3),
                    },
                ],
                packages: BTreeMap::new(),
                owned_by_packages: owned,
            };
            let shown: Vec<_> = diff.substantive().map(|c| c.path.clone()).collect();
            assert_eq!(shown, vec![PathBuf::from("/usr/games/cowsay")]);
        }
    }
}
