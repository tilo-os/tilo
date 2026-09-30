//! Tilo's pinned font catalog.
//!
//! The catalog is one directory of unmodified font files plus a
//! `manifest.toml` that pins every file by size and SHA-256. The checked-in
//! copy is `crates/sdk/ui-core/assets/fonts`; the OS image installs it once,
//! at [`INSTALLED_ROOT`], where fontconfig and every Tilo process that sets
//! text find the same bytes. This crate owns the manifest's schema and the
//! checks that tie an installed directory to the build that reads it: the
//! installed manifest must be byte-identical to the one compiled in here, and
//! every file a reader uses must match its pinned size and hash.

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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui-core/assets/fonts")
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

/// Whether a font ships in the product or only in controlled test fixtures.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Product,
    Fixture,
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
    pub family: String,
    pub post_script_name: String,
    pub style: String,
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
        let path = self.path_in(root)?;
        let metadata = std::fs::metadata(&path).map_err(|e| Error::at(&path, e))?;
        if !metadata.is_file() || metadata.len() != self.bytes {
            return Err(Error::at(
                &path,
                format!("expected regular file of {} bytes", self.bytes),
            ));
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
