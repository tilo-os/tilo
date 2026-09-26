//! A layer: the writable half of the guest's root filesystem.
//!
//! A layer is a plain directory owned by the user, mounted as the
//! `upperdir` of an `overlayfs` whose `lowerdir` is the host's, so
//! everything the guest writes to a system path lands here and the host's
//! own filesystem is never touched.
//!
//! Unprivileged overlayfs has two limits that shape this module. The guest's
//! user namespace maps only the user, and
//! host `uid 0` is not in that map, so a capability check against a
//! root-owned inode fails regardless of what the process holds.
//!
//! - **Directories.** Creating `/usr/bin/htop` needs a copy-up of `/usr/bin`,
//!   which is a write into a root-owned directory. So [`Layer::create`]
//!   mirrors the base's entire directory tree into the layer up front, owned
//!   by the user (~22 000 empty directories on Fedora 44, under a second) —
//!   after that, writes anywhere the base has a directory need no copy-up.
//!
//! - **Files.** `rpm` and `dnf` rewrite root-owned database/history files
//!   that can't be copied up, so [`Layer::create`] seeds the layer with the
//!   user's own copies ([`STATE_SEED_DIRS`]). This also makes a layer a
//!   *delta*: `pkg install htop` downloads htop, not glibc.
//!
//! A layer can add to or shadow the base, but cannot alter a file that came
//! from the image.

use std::path::{Path, PathBuf};

/// Host subtrees a layer shadows; everything a package installs lands under
/// one of these. `/var` is deliberately absent: the store lives under
/// `$HOME`, which is inside `/var` on a bootc system, and overlayfs refuses
/// an `upperdir` nested in its own `lowerdir` — `/var/lib`, `/var/cache` and
/// `/var/log` are plain binds instead.
pub const BASE_SUBTREES: &[&str] = &["usr", "etc"];

/// Directories whose *files* are copied into a new layer: package-manager
/// state that must be rewritten in place. Keep this small — it is not a
/// place to make the base mutable by the back door.
pub const STATE_SEED_DIRS: &[&str] = &[
    // The rpm database. `%{_dbpath}` on a Fedora bootc image.
    "usr/share/rpm",
    "usr/lib/sysimage/rpm",
    // dnf5's transaction history.
    "usr/lib/sysimage/libdnf5",
];

/// Which store slot a layer occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    /// `layers/persistent` — survives the session.
    Persistent,
    /// `layers/ephemeral/<id>` — destroyed when the session ends.
    Ephemeral,
}

impl LayerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LayerKind::Persistent => "persistent",
            LayerKind::Ephemeral => "ephemeral",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LayerError {
    #[error("layer {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("refusing to destroy {0}: it is not a layer (no .ush-layer marker)")]
    NotALayer(PathBuf),
    #[error(
        "{path} is on a filesystem that cannot hold a layer ({fs}). overlayfs \
         refuses it as an upper directory, so the guest's root cannot be built \
         there. Point {env} at a directory on ordinary disk."
    )]
    UnusableFilesystem {
        path: PathBuf,
        fs: &'static str,
        env: &'static str,
    },
}

/// `f_type` values overlayfs will not take as an upper. `tmpfs` is the one
/// that bites in practice — a store under `/tmp` mounts and then fails at
/// the overlay with only `Invalid argument` to go on; naming it here turns
/// a dead end into a sentence.
const UNUSABLE_FS: &[(i64, &str)] = &[
    (0x0102_1994, "tmpfs"),
    (0x8584_58f6, "ramfs"),
    (0x6969, "nfs"),
];

/// Can a layer live here? Checked before anything is written, so a wrong
/// filesystem fails with an explanation, not after mirroring 22 000 dirs.
///
/// # Errors
///
/// Returns [`LayerError::UnusableFilesystem`] if `path` (or its nearest
/// existing ancestor) sits on a filesystem overlayfs refuses as an upper
/// directory.
pub fn upper_filesystem_supported(path: &Path) -> Result<(), LayerError> {
    // Walk up to the nearest existing ancestor — `statfs` needs a path that exists.
    let mut probe = path;
    while !probe.exists() {
        match probe.parent() {
            Some(parent) => probe = parent,
            None => return Ok(()),
        }
    }
    let Ok(c_path) = std::ffi::CString::new(probe.as_os_str().as_encoded_bytes()) else {
        return Ok(());
    };
    // SAFETY: `buf` is fully written by a successful `statfs`; `c_path` is a
    // valid NUL-terminated path that outlives the call.
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &raw mut buf) } != 0 {
        // Unable to tell is not the same as unusable. Let the mount decide.
        return Ok(());
    }
    let ty = buf.f_type as i64;
    match UNUSABLE_FS.iter().find(|(magic, _)| *magic == ty) {
        Some((_, fs)) => Err(LayerError::UnusableFilesystem {
            path: path.to_path_buf(),
            fs,
            env: crate::store::STORE_ENV,
        }),
        None => Ok(()),
    }
}

fn io(path: &Path, source: std::io::Error) -> LayerError {
    LayerError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The marker file that says a directory is a layer this runtime made.
/// [`Layer::burn`] refuses to `rm -rf` without it, so a typo in a store path
/// can't delete a home directory.
const MARKER: &str = ".ush-layer";

/// One layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    path: PathBuf,
    kind: LayerKind,
}

impl Layer {
    pub fn new(path: impl Into<PathBuf>, kind: LayerKind) -> Self {
        Self {
            path: path.into(),
            kind,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn kind(&self) -> LayerKind {
        self.kind
    }

    /// Has this layer been created (skeleton mirrored, state seeded)?
    pub fn exists(&self) -> bool {
        self.path.join(MARKER).is_file()
    }

    /// The layer's shadow of one base subtree — the overlay `upperdir`.
    pub fn upper_for(&self, subtree: &str) -> PathBuf {
        self.path.join(subtree)
    }

    /// Create the layer if it is not there yet. `base` is the root of the
    /// filesystem being shadowed — `/` in production, a fixture in tests.
    /// Idempotent: an existing layer is left alone.
    ///
    /// # Errors
    ///
    /// Returns [`LayerError::Io`] if the layer directory, the mirrored
    /// skeleton, the seeded package-manager state, or the marker file can't
    /// be written.
    pub fn create(&self, base: &Path) -> Result<(), LayerError> {
        // Deliberately no filesystem check here: creating a layer is an ordinary
        // directory operation, useful anywhere (`pkg diff` on a copied store, a
        // fixture). What constrains the filesystem is *mounting* it, checked in
        // [`upper_filesystem_supported`] where the overlay is built.
        if self.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.path).map_err(|e| io(&self.path, e))?;

        for subtree in BASE_SUBTREES {
            let src = base.join(subtree);
            if !src.is_dir() {
                continue;
            }
            mirror_directories(&src, &self.path.join(subtree))?;
        }

        for dir in STATE_SEED_DIRS {
            let src = base.join(dir);
            // `usr/lib/sysimage/rpm` symlinks to `usr/share/rpm` on Fedora; copying
            // both is harmless since the second pass overwrites with identical bytes.
            if !src.is_dir() {
                continue;
            }
            copy_files_shallow(&src, &self.path.join(dir))?;
        }

        // `rpm` still looks here on a non-ostree base, and a dangling symlink
        // in the layer would shadow a working one in the lower.
        let var_lib = self.path.join("var/lib");
        std::fs::create_dir_all(&var_lib).map_err(|e| io(&var_lib, e))?;
        for d in ["var/lib/dnf", "var/cache/libdnf5", "var/log"] {
            let p = self.path.join(d);
            std::fs::create_dir_all(&p).map_err(|e| io(&p, e))?;
        }

        let marker = self.path.join(MARKER);
        std::fs::write(&marker, format!("{}\n", self.kind.as_str())).map_err(|e| io(&marker, e))?;
        Ok(())
    }

    /// Destroy the layer's files (`pkg burn`). Refuses on anything without the
    /// marker — an `rm -rf` on the wrong path is unrecoverable.
    ///
    /// # Errors
    ///
    /// Returns [`LayerError::NotALayer`] if the path lacks the marker file,
    /// or [`LayerError::Io`] if the directory survives the forced removal.
    pub fn burn(&self) -> Result<(), LayerError> {
        if !self.path.exists() {
            return Ok(());
        }
        if !self.exists() {
            return Err(LayerError::NotALayer(self.path.clone()));
        }
        crate::guest::remove_dir_all_forced(&self.path);
        if self.path.exists() {
            return Err(io(
                &self.path,
                std::io::Error::other("the layer directory survived removal"),
            ));
        }
        Ok(())
    }

    /// What this layer changed relative to the base. Empty directories are
    /// skipped — the layer mirrors the base's whole tree by construction, so
    /// reporting those would drown the answer in ~22 000 meaningless lines.
    pub fn diff(&self, base: &Path) -> Vec<LayerChange> {
        let mut out = Vec::new();
        for subtree in BASE_SUBTREES {
            let root = self.path.join(subtree);
            if !root.is_dir() {
                continue;
            }
            for entry in walkdir::WalkDir::new(&root).follow_links(false) {
                let Ok(entry) = entry else { continue };
                let meta = entry.path().symlink_metadata().ok();
                let Some(meta) = meta else { continue };
                if meta.is_dir() {
                    continue;
                }
                let Ok(rel) = entry.path().strip_prefix(&self.path) else {
                    continue;
                };
                let guest_path = Path::new("/").join(rel);
                // overlayfs records a deletion as a 0/0 character device.
                let kind = if is_whiteout(&meta) {
                    ChangeKind::Removed
                } else if base.join(rel).symlink_metadata().is_ok() {
                    ChangeKind::Shadowed
                } else {
                    ChangeKind::Added
                };
                out.push(LayerChange {
                    path: guest_path,
                    kind,
                    bytes: if matches!(kind, ChangeKind::Removed) {
                        0
                    } else {
                        meta.len()
                    },
                });
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// Bytes the layer's own files occupy. Skips the mirrored directories,
    /// which cost metadata rather than data.
    pub fn size_bytes(&self, base: &Path) -> u64 {
        self.diff(base).iter().map(|c| c.bytes).sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerChange {
    /// The path as the guest sees it.
    pub path: PathBuf,
    pub kind: ChangeKind,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Not in the base at all.
    Added,
    /// Present in the base; the layer's copy is what the guest sees.
    Shadowed,
    /// An overlayfs whiteout — deleted, for this guest only.
    Removed,
}

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Added => "added",
            ChangeKind::Shadowed => "shadowed",
            ChangeKind::Removed => "removed",
        }
    }
}

fn is_whiteout(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::MetadataExt;
    meta.file_type().is_char_device() && meta.rdev() == 0
}

/// Recreate `src`'s directory tree under `dst`; directories only, nothing copied.
fn mirror_directories(src: &Path, dst: &Path) -> Result<(), LayerError> {
    std::fs::create_dir_all(dst).map_err(|e| io(dst, e))?;
    for entry in walkdir::WalkDir::new(src)
        .follow_links(false)
        .same_file_system(true)
        .into_iter()
        // An unreadable directory in the base is not a reason to refuse the layer —
        // the guest won't be able to write into it either, matching the base.
        .filter_map(std::result::Result::ok)
    {
        if !entry.file_type().is_dir() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(src) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = dst.join(rel);
        if let Err(e) = std::fs::create_dir_all(&target)
            && e.kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(io(&target, e));
        }
    }
    Ok(())
}

/// Copy the regular files directly inside `src` (not recursive) into `dst`.
fn copy_files_shallow(src: &Path, dst: &Path) -> Result<(), LayerError> {
    std::fs::create_dir_all(dst).map_err(|e| io(dst, e))?;
    let entries = std::fs::read_dir(src).map_err(|e| io(src, e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = path.symlink_metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        let target = dst.join(name);
        std::fs::copy(&path, &target).map_err(|e| io(&target, e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture that looks like a root filesystem, with a file standing in for the rpm database.
    fn fake_base() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for d in [
            "usr/bin",
            "usr/lib64",
            "usr/share/rpm",
            "usr/share/terminfo/x",
            "usr/lib/sysimage/libdnf5",
            "etc/dnf",
            "var/lib",
        ] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("usr/bin/bash"), b"#!elf").unwrap();
        std::fs::write(dir.path().join("usr/share/rpm/rpmdb.sqlite"), b"BASE-RPMDB").unwrap();
        std::fs::write(
            dir.path()
                .join("usr/lib/sysimage/libdnf5/transaction_history.sqlite"),
            b"BASE-HISTORY",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("etc/passwd"),
            b"root:x:0:0::/root:/bin/bash\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn creating_a_layer_mirrors_the_base_shape_and_seeds_state_once() {
        let base = fake_base();
        let store = tempfile::tempdir().unwrap();
        let layer = Layer::new(store.path().join("persistent"), LayerKind::Persistent);
        layer.create(base.path()).unwrap();

        // The whole point of the mirror: a guest can create a file anywhere the
        // base has a directory, without copying up a root-owned one.
        for d in ["usr/bin", "usr/lib64", "usr/share/terminfo/x", "etc/dnf"] {
            assert!(
                layer.path().join(d).is_dir(),
                "{d} was not mirrored into the layer; a write there would need \
                 a copy-up of a root-owned directory and would be refused"
            );
        }
        // Directories only — mirroring files would make the layer a second copy of the OS.
        assert!(
            !layer.path().join("usr/bin/bash").exists(),
            "the mirror copied a file; it must copy shape, not content"
        );

        // The seeded state is what makes the layer a delta, and rpm's database writable at all.
        let db = layer.path().join("usr/share/rpm/rpmdb.sqlite");
        assert!(db.is_file(), "the rpm database was not seeded");
        assert_eq!(std::fs::read(&db).unwrap(), b"BASE-RPMDB");
        let hist = layer
            .path()
            .join("usr/lib/sysimage/libdnf5/transaction_history.sqlite");
        assert!(hist.is_file(), "dnf's transaction history was not seeded");

        // `var` must not become a shadowed base subtree: the store lives inside
        // `$HOME`, itself inside `/var` on a bootc system, and overlayfs refuses
        // an upperdir nested in its lowerdir.
        assert!(
            !BASE_SUBTREES.contains(&"var"),
            "shadowing /var makes the overlay unmountable wherever $HOME is under /var"
        );
        assert_eq!(BASE_SUBTREES, &["usr", "etc"]);

        // Create is idempotent and does not re-seed.
        std::fs::write(&db, b"MODIFIED-BY-A-TRANSACTION").unwrap();
        layer.create(base.path()).unwrap();
        assert_eq!(
            std::fs::read(&db).unwrap(),
            b"MODIFIED-BY-A-TRANSACTION",
            "re-creating an existing layer overwrote its state; a second \
             session would lose every package installed in the first"
        );
    }

    #[test]
    fn diff_and_size_count_only_layer_content() {
        // phase: diff_names_added_and_shadowed_files_and_skips_mirrored_directories
        {
            let base = fake_base();
            let store = tempfile::tempdir().unwrap();
            let layer = Layer::new(store.path().join("persistent"), LayerKind::Persistent);
            layer.create(base.path()).unwrap();

            // Baseline: a fresh layer holds only the state it was seeded with.
            let seeded = layer.diff(base.path());
            assert!(
                seeded.iter().all(|c| c.kind == ChangeKind::Shadowed),
                "a fresh layer must only shadow the state files it seeded: {seeded:?}"
            );
            assert!(
                !seeded.iter().any(|c| c.path.ends_with("bin")),
                "diff reported a mirrored directory: {seeded:?}"
            );

            std::fs::write(layer.path().join("usr/bin/htop"), b"HTOP").unwrap();
            std::fs::write(layer.path().join("usr/bin/bash"), b"REPLACED").unwrap();

            let changes = layer.diff(base.path());
            let added = changes
                .iter()
                .find(|c| c.path == Path::new("/usr/bin/htop"))
                .expect("added file missing from diff");
            assert_eq!(added.kind, ChangeKind::Added);
            assert_eq!(added.bytes, 4);

            let shadowed = changes
                .iter()
                .find(|c| c.path == Path::new("/usr/bin/bash"))
                .expect("shadowed file missing from diff");
            assert_eq!(
                shadowed.kind,
                ChangeKind::Shadowed,
                "a path that exists in the base is shadowed, not added"
            );
        }
        // phase: size_counts_layer_content_only
        {
            let base = fake_base();
            let store = tempfile::tempdir().unwrap();
            let layer = Layer::new(store.path().join("persistent"), LayerKind::Persistent);
            layer.create(base.path()).unwrap();
            let seeded = layer.size_bytes(base.path());
            std::fs::write(layer.path().join("usr/bin/htop"), vec![0u8; 1024]).unwrap();
            assert_eq!(layer.size_bytes(base.path()), seeded + 1024);
        }
    }

    #[test]
    fn the_layer_filesystem_is_checked_before_anything_is_written() {
        // phase: a_layer_on_tmpfs_is_refused_with_an_explanation
        // A store on `/tmp` mounts and then dies at the overlay with `Invalid
        // argument`; naming the filesystem and the fix-it variable turns that into a diagnosable error.
        {
            fn phase_a_layer_on_tmpfs_is_refused_with_an_explanation() {
                // `/dev/shm` is tmpfs on every Linux this runs on.
                let shm = Path::new("/dev/shm");
                if !shm.is_dir() {
                    eprintln!("no /dev/shm on this machine; tmpfs rejection not exercised");
                    return;
                }
                let layer = Layer::new(shm.join("ush-fs-probe"), LayerKind::Persistent);
                match upper_filesystem_supported(layer.path()) {
                    Err(LayerError::UnusableFilesystem { fs, env, .. }) => {
                        assert_eq!(fs, "tmpfs");
                        assert_eq!(env, crate::store::STORE_ENV);
                    }
                    other => panic!("tmpfs must be refused as a layer location: {other:?}"),
                }
                assert!(
                    !layer.path().exists(),
                    "the check must run before anything is written"
                );
            }
            phase_a_layer_on_tmpfs_is_refused_with_an_explanation();
        }
        // phase: an_ordinary_directory_is_accepted
        {
            // The data directory, not `/tmp` (often tmpfs, which is what's under test).
            // It may not exist yet — a container running as root has a bare `/root` —
            // so create it first, or a missing parent looks like a rejected filesystem.
            let base = xdg_paths::data_dir().unwrap_or_else(|| PathBuf::from("."));
            std::fs::create_dir_all(&base).expect("the data directory must be creatable");
            let dir = tempfile::TempDir::with_prefix_in("ush-fs-", &base).expect("tempdir on disk");
            upper_filesystem_supported(&dir.path().join("layers/persistent"))
                .expect("an ordinary data directory must be usable");
        }
    }

    #[test]
    fn burn_destroys_only_a_marked_layer() {
        // phase: burn_destroys_the_layer
        {
            let base = fake_base();
            let store = tempfile::tempdir().unwrap();
            let layer = Layer::new(store.path().join("persistent"), LayerKind::Persistent);
            layer.create(base.path()).unwrap();
            std::fs::write(layer.path().join("usr/bin/htop"), b"HTOP").unwrap();
            assert!(layer.exists());

            layer.burn().unwrap();
            assert!(!layer.exists());
            assert!(!layer.path().exists());
        }
        // phase: burn_refuses_a_directory_that_is_not_a_layer
        // `burn` is `rm -rf` on a path. The marker is the only thing standing
        // between a mistyped store root and someone's home directory.
        {
            let dir = tempfile::tempdir().unwrap();
            let precious = dir.path().join("Documents");
            std::fs::create_dir_all(&precious).unwrap();
            std::fs::write(precious.join("thesis.md"), b"years of work").unwrap();

            let layer = Layer::new(&precious, LayerKind::Persistent);
            let err = layer.burn().unwrap_err();
            assert!(matches!(err, LayerError::NotALayer(_)), "{err:?}");
            assert!(
                precious.join("thesis.md").is_file(),
                "burn deleted a directory it did not create"
            );
        }
    }
}
