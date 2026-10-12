//! Tilo's pinned font catalog.
//!
//! The catalog is one directory of unmodified font files plus a
//! `manifest.toml` that pins every file by size and SHA-256. Source metadata lives in
//! `crates/sdk/ui-core/assets/fonts`; builds prepare the pinned font bytes in
//! a verified artifact cache. The OS image installs its catalog once,
//! at [`INSTALLED_ROOT`], where fontconfig and every Tilo process that sets
//! text find the same bytes. This crate owns the manifest's schema and the
//! checks that tie an installed directory to the build that reads it: the
//! installed manifest must be byte-identical to the one compiled in here, and
//! every file a reader uses must match its pinned size and hash — read and
//! hashed, unless the file is on read-only storage this user cannot change
//! ([`Trust`]), where the image's own integrity already pins its bytes.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

/// The manifest this build was made against: a copy of the checkout
/// catalog's `manifest.toml` inside this crate, so a published crate carries
/// it. `the_checkout_holds_the_pinned_catalog` keeps the two identical.
pub const MANIFEST: &str = include_str!("../manifest.toml");

/// Where the OS image installs the catalog's product fonts. It sits inside
/// the system font directory, so fontconfig and fontdb discover it without
/// extra configuration.
pub const INSTALLED_ROOT: &str = "/usr/share/fonts/tilo";

/// The catalog in this source checkout.
#[must_use]
pub fn checkout_root() -> PathBuf {
    PathBuf::from(env!("TILO_CHECKOUT_FONT_ROOT"))
}

/// The installed catalog when the system has one, otherwise the checkout's
/// (a development or test run outside the image). `None` when neither
/// exists. Only the location is resolved here; readers verify what they use.
#[must_use]
pub fn locate() -> Option<PathBuf> {
    [PathBuf::from(INSTALLED_ROOT), checkout_root()]
        .into_iter()
        .find(|root| root.join("manifest.toml").is_file())
}

#[derive(Debug, thiserror::Error)]
#[error("Tilo font catalog error at {path}: {problem}")]
pub struct Error {
    pub path: PathBuf,
    pub problem: String,
}

impl Error {
    #[allow(
        clippy::needless_pass_by_value,
        reason = "The error constructor accepts owned diagnostics and string literals and stores their formatted message."
    )]
    pub fn at(path: impl Into<PathBuf>, problem: impl ToString) -> Self {
        Self {
            path: path.into(),
            problem: problem.to_string(),
        }
    }
}

/// Where a font ships.
///
/// | Scope | Tilo OS image | SDK standalone catalog | test fixtures |
/// |---|---|---|---|
/// | `Product` | yes | yes | yes |
/// | `Os` | yes | no | yes |
/// | `Fixture` | no | no | yes |
///
/// The standalone catalog is the one the SDK ships next to a third-party
/// app's Windows, macOS, Android and web builds. It carries only fonts whose
/// licence lets any app bundle them (OFL, GUST); a font under another
/// licence, such as New Computer Modern's GPL-licensed Regular cut, is
/// `Os`. A catalog directory without its `Os` files is a standalone
/// catalog, and the toolkit sets text in the faces it has.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Product,
    Os,
    Fixture,
}

impl Scope {
    /// Whether the SDK's standalone catalog for third-party app builds
    /// carries fonts of this scope.
    #[must_use]
    pub const fn standalone(self) -> bool {
        matches!(self, Self::Product)
    }

    /// Whether the Tilo OS image installs fonts of this scope.
    #[must_use]
    pub const fn os(self) -> bool {
        matches!(self, Self::Product | Self::Os)
    }
}

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub font: Vec<Asset>,
}

/// One pinned font file.
#[derive(Debug, Deserialize)]
pub struct Asset {
    pub scope: Scope,
    /// Whether a browser host must deliver this file before first paint.
    #[serde(default)]
    pub browser_startup: bool,
    /// Path relative to the catalog root.
    pub file: String,
    pub bytes: u64,
    /// Lowercase hex SHA-256 of the whole file.
    pub sha256: String,
    pub reserved_aliases: Vec<String>,
    pub faces: Vec<Face>,
}

/// One face of a pinned file (a collection has several).
#[derive(Debug, Deserialize)]
pub struct Face {
    pub index: u32,
    /// The family the file itself names.
    pub family: String,
    pub post_script_name: String,
    pub style: String,
    /// The families Tilo sets this face under, in place of the file's own,
    /// when that name cannot tell the face apart from its siblings: every
    /// New Computer Modern file says `NewComputerModern` at weight 400,
    /// whether it is the Book, Regular or 8 pt cut. Empty keeps the file's
    /// own family. The file's bytes are never changed.
    #[serde(default)]
    pub tilo_families: Vec<String>,
}

/// The compiled-in manifest, parsed once.
///
/// # Errors
///
/// When the compiled-in manifest does not parse or has an unsupported
/// schema — a packaging mistake in this build.
pub fn manifest() -> Result<&'static Manifest, Error> {
    static PARSED: OnceLock<Result<Manifest, String>> = OnceLock::new();
    PARSED
        .get_or_init(|| {
            let manifest: Manifest = toml::from_str(MANIFEST).map_err(|e| e.to_string())?;
            if manifest.schema == 1 {
                Ok(manifest)
            } else {
                Err("unsupported catalog schema".to_string())
            }
        })
        .as_ref()
        .map_err(|problem| Error::at("manifest.toml", problem))
}

/// SHA-256 of the compiled-in manifest, which identifies the catalog.
#[must_use]
pub fn manifest_sha256() -> String {
    hex::encode(Sha256::digest(MANIFEST.as_bytes()))
}

/// Check that `root` holds the catalog this build was made against: its
/// `manifest.toml` must be byte-identical to [`MANIFEST`].
///
/// # Errors
///
/// When the manifest is missing, unreadable or different.
pub fn check_manifest(root: &Path) -> Result<(), Error> {
    let path = root.join("manifest.toml");
    let shipped = std::fs::read(&path).map_err(|e| Error::at(&path, e))?;
    if shipped != MANIFEST.as_bytes() {
        return Err(Error::at(path, "manifest differs from this Tilo build"));
    }
    Ok(())
}

/// How much of a catalog file a reader must check before using it.
///
/// Hashing the whole catalog (36 files, 54.9 MB, 32.7 MB of it one CJK
/// collection) costs every process 22 ms on a CPU with SHA instructions and
/// about 140 ms without. That proof is redundant when the bytes cannot have
/// changed since the image was built and checked: the Tilo image is a bootc
/// deployment whose `/usr` is a read-only composefs mount, every object of
/// which the kernel checks against its fs-verity digest as it is read, and
/// `tools/font-catalog.py` checked each file against this same manifest when
/// the image was assembled. The launcher binds a runtime's catalog into an
/// app's sandbox read-only, and a system runtime lives in that same `/usr`.
///
/// [`Trust::of_root`] grants [`Trust::Immutable`] only from what the
/// filesystem reports about the open directory, never from a path name, and
/// [`Asset::verify_with`] checks the same property again for each file before
/// it skips the read, so a file that is not provably immutable is hashed
/// regardless. That property is [`file_is_immutable`]: root-owned, writable
/// by nobody else, on a read-only mount. Every byte used then comes from a
/// file only root could have written, listed in a directory only root could
/// have changed, under a manifest byte-identical to this build's. No
/// ancestor walk is needed, and none would work: inside an app's sandbox
/// every directory above a bind is the sandbox's own tmpfs, which belongs to
/// the user. Renaming a root-owned directory over another only substitutes
/// one root-made catalog for another, and only one with this build's exact
/// manifest is accepted at all.
///
/// A user-installed catalog, a per-user runtime's (the user owns those
/// files, so a read-only bind only hides writes inside the sandbox), a
/// checkout and any directory on writable storage are hashed in full, as is
/// everything on hosts without `fstatvfs`. A process running as root hashes
/// too: inside a user namespace a "root" can be an ordinary user whose own
/// files appear root-owned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// The root and its manifest, and (per [`Asset::verify_with`]) each
    /// file, are [immutable](file_is_immutable): size and manifest identity
    /// are checked, the bytes are not read.
    Immutable,
    /// Every file is read and its SHA-256 compared with the pin.
    Verify,
}

impl Trust {
    /// The trust a catalog at `root` earns from where it is stored:
    /// [`Trust::Immutable`] when the directory and its `manifest.toml` are
    /// [immutable](file_is_immutable), [`Trust::Verify`] otherwise.
    #[must_use]
    pub fn of_root(root: &Path) -> Self {
        let immutable = root.canonicalize().is_ok_and(|root| {
            file_is_immutable(&root) && file_is_immutable(&root.join("manifest.toml"))
        });
        if immutable {
            Self::Immutable
        } else {
            Self::Verify
        }
    }
}

/// Whether `path` names a file or directory only root can change: it is not
/// a symlink, it is owned by root (as this user namespace sees it), neither
/// group nor others may write it, and it lives on a read-only mount. Always
/// `false` in a process running as root and on hosts without `fstatvfs`
/// (see [`Trust`]). Opening the path reads none of its content.
#[must_use]
pub fn file_is_immutable(path: &Path) -> bool {
    immutable::file(path)
}

#[cfg(unix)]
mod immutable {
    use std::fs::{File, OpenOptions};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::Path;

    /// The kernel's default `overflowuid`: how a user namespace that maps
    /// only the user (an app's bubblewrap sandbox) shows root's files.
    const OVERFLOW_UID: u32 = 65_534;

    pub(super) fn file(path: &Path) -> bool {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        if euid == 0 {
            return false;
        }
        let Ok(file) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
        else {
            return false;
        };
        let Ok(metadata) = file.metadata() else {
            return false;
        };
        matches!(metadata.uid(), 0 | OVERFLOW_UID)
            && metadata.uid() != euid
            && metadata.mode() & 0o022 == 0
            && read_only_mount(&file)
    }

    fn read_only_mount(file: &File) -> bool {
        let mut filesystem = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: the descriptor is live for this call and fstatvfs writes
        // exactly one statvfs into the buffer.
        if unsafe { libc::fstatvfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
            return false;
        }
        // SAFETY: fstatvfs succeeded, so it initialised every field.
        unsafe { filesystem.assume_init() }.f_flag & libc::ST_RDONLY != 0
    }
}

#[cfg(not(unix))]
mod immutable {
    use std::path::Path;

    pub(super) fn file(_path: &Path) -> bool {
        false
    }
}

impl Asset {
    /// The file's path under `root`, refusing a manifest entry that would
    /// leave the catalog directory.
    ///
    /// # Errors
    ///
    /// When the entry is absolute or climbs out of `root`.
    pub fn path_in(&self, root: &Path) -> Result<PathBuf, Error> {
        let relative = Path::new(&self.file);
        if !relative
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(Error::at(
                root,
                format!("invalid manifest path: {}", self.file),
            ));
        }
        Ok(root.join(relative))
    }

    /// Check the file under `root` against its pinned size and SHA-256 and
    /// return its path.
    ///
    /// # Errors
    ///
    /// When the file is missing, not a regular file, or differs.
    pub fn verify(&self, root: &Path) -> Result<PathBuf, Error> {
        self.verify_with(root, Trust::Verify)
    }

    /// Check the file under `root` as `trust` (from [`Trust::of_root`])
    /// allows and return its path. Under [`Trust::Immutable`] a file that is
    /// itself [immutable](file_is_immutable) is checked by type and size and
    /// not read; any other file is hashed as under [`Trust::Verify`].
    ///
    /// # Errors
    ///
    /// When the file is missing, not a regular file, or differs.
    pub fn verify_with(&self, root: &Path, trust: Trust) -> Result<PathBuf, Error> {
        self.verify_probed(root, trust, file_is_immutable)
    }

    /// [`Asset::verify_with`] with its immutability probe passed in, so a
    /// test can show what a file the probe accepts is spared.
    fn verify_probed(
        &self,
        root: &Path,
        trust: Trust,
        immutable: impl Fn(&Path) -> bool,
    ) -> Result<PathBuf, Error> {
        let path = self.path_in(root)?;
        let metadata = std::fs::metadata(&path).map_err(|e| Error::at(&path, e))?;
        if !metadata.is_file() || metadata.len() != self.bytes {
            return Err(Error::at(
                &path,
                format!("expected regular file of {} bytes", self.bytes),
            ));
        }
        if trust == Trust::Immutable && immutable(&path) {
            return Ok(path);
        }
        let actual = sha256_file(&path).map_err(|e| Error::at(&path, e))?;
        self.check_digest(&path, &actual)?;
        Ok(path)
    }

    /// Check bytes already read from this asset's file (at `path`, for the
    /// error) against its pinned size and SHA-256.
    ///
    /// # Errors
    ///
    /// When the bytes differ.
    pub fn verify_bytes(&self, path: &Path, bytes: &[u8]) -> Result<(), Error> {
        if bytes.len() as u64 != self.bytes {
            return Err(Error::at(
                path,
                format!("expected {} bytes, found {}", self.bytes, bytes.len()),
            ));
        }
        self.check_digest(path, &hex::encode(Sha256::digest(bytes)))
    }

    /// Compare a lowercase hex SHA-256 with the pinned one.
    ///
    /// # Errors
    ///
    /// When they differ.
    pub fn check_digest(&self, path: &Path, actual: &str) -> Result<(), Error> {
        if actual == self.sha256 {
            Ok(())
        } else {
            Err(Error::at(
                path,
                format!("SHA-256 mismatch: expected {}, found {actual}", self.sha256),
            ))
        }
    }
}

/// Lowercase hex SHA-256 of a file, streamed through a fixed buffer.
///
/// # Errors
///
/// When the file cannot be read.
#[allow(
    clippy::large_stack_arrays,
    reason = "A single fixed 64 KiB stack buffer bounds memory while streaming each font into SHA-256."
)]
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let n = match file.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if n == 0 {
            break;
        }
        let chunk = buffer.get(..n).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "read exceeded its buffer")
        })?;
        hash.update(chunk);
    }
    Ok(hex::encode(hash.finalize()))
}
