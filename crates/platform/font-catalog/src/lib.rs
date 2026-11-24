//! Tilo's pinned font catalog.
//!
//! The catalog is one directory of unmodified font files plus a
//! `manifest.toml` that pins every file by size and SHA-256. Source metadata lives in
//! `crates/ui/ui-core/assets/fonts`; a build fetches the pinned font bytes into
//! a verified artifact cache and prepares this crate's catalog from it in its
//! profile directory's `font-catalogs`. The OS image installs its catalog once,
//! at [`INSTALLED_ROOT`], where fontconfig and every Tilo process that sets
//! text find the same bytes. This crate owns the manifest's schema and the
//! checks that tie an installed directory to the build that reads it: the
//! installed manifest must be byte-identical to the one compiled in here, and
//! every file a reader uses must have its pinned size. The manifest also
//! states each face's family, style and weight, so a reader lists the faces
//! without opening a file; the SHA-256 pins are checked where a catalog is
//! prepared and where a browser delivers bytes.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

/// Preparing the catalog from what this crate carries, fetching its pinned fonts.
#[cfg(feature = "prepare")]
pub mod prepare;
#[cfg(test)]
mod tests;

/// The web page's cuts of Tilo Sans, which the host serving a page serves beside it.
#[cfg(feature = "web")]
pub mod web {
    /// Each `Web` file of the manifest: its name and its bytes, verified against their pins
    /// where the build prepared them.
    pub static CUTS: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/web.rs"));
}

/// The manifest this build was made against: a copy of the checkout
/// catalog's `manifest.toml` inside this crate, so a published crate carries
/// it. `the_checkout_holds_the_pinned_catalog` keeps the two identical.
pub const MANIFEST: &str = include_str!("../manifest.toml");

/// Where the OS image installs the catalog's product fonts. It sits inside
/// the system font directory, so fontconfig and fontdb discover it without
/// extra configuration.
pub const INSTALLED_ROOT: &str = "/usr/share/fonts/tilo";

/// The catalog this build prepared from the source checkout, in the profile
/// directory's `font-catalogs`: it is part of the build's output, so wherever
/// the build is reused the catalog is too. A catalog is published there once,
/// under the name of the manifest it was prepared from, and never rewritten,
/// so a build — or another job sharing the build directory — that prepares it
/// again cannot take away the tree a running test or program is reading. The
/// build names it from its profile directory, which holds the running program
/// (or its `deps`/`examples`), so it is found above the program; the build
/// directory's path is not compiled in.
///
/// A build directory can carry a build's units without the catalog its build
/// script published (a store that keeps compiled units and moves them to
/// another job), so a run of an existing build looks next at the catalogs
/// prepared in the artifact cache ([`prepared_catalogs`]), which is where the
/// test environment (`tools/test-env-setup.sh`) and [`find_root`]'s readers
/// look: the same pins, prepared once from the image's seed or the release
/// store. What neither has stays the compiled-in name, so a caller that needs
/// the bytes fails on them rather than reading another build's catalog.
#[must_use]
pub fn checkout_root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let named = Path::new(env!("TILO_CHECKOUT_FONT_ROOT"));
        if named.is_absolute() {
            return named.to_path_buf();
        }
        let beside_program = std::env::current_exe()
            .ok()
            .into_iter()
            .flat_map(|program| {
                program
                    .ancestors()
                    .skip(1)
                    .map(|directory| directory.join(named))
                    .collect::<Vec<_>>()
            });
        let prepared_here = std::env::var_os("TILO_FONT_ASSET_CACHE")
            .into_iter()
            .flat_map(|cache| prepared_catalogs(Path::new(&cache)));
        find_root(beside_program.chain(prepared_here)).unwrap_or_else(|| named.to_path_buf())
    })
    .clone()
}

/// `TILO_FONT_CATALOG`: the catalog to read instead of the compiled-in
/// checkout path, for a binary built somewhere else and run here. Without it
/// such a run silently sets every frame in the host's faces, which looks
/// almost right and is wrong in a way that only shows up in the review of a
/// screenshot. The toolkit reads it into `ProcessPaths::font_catalog`, which is
/// where a reader takes it from.
pub const OVERRIDE_ENV: &str = "TILO_FONT_CATALOG";

/// The installed catalog when the system has one, otherwise the checkout's
/// (a development or test run outside the image). `None` when neither
/// exists. Only the location is resolved here; readers verify what they use.
#[must_use]
pub fn locate() -> Option<PathBuf> {
    [PathBuf::from(INSTALLED_ROOT), checkout_root()]
        .into_iter()
        .find(|root| root.join("manifest.toml").is_file())
}

/// The first root holding a catalog *this build* pins, checked without reading
/// a font: a root whose manifest is not the one compiled in is passed over, so
/// an older image under a newer checkout is not mistaken for the product's.
#[must_use]
pub fn find_root(roots: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    roots.into_iter().find(|root| {
        std::fs::read_to_string(root.join("manifest.toml"))
            .is_ok_and(|manifest| manifest == MANIFEST)
    })
}

/// The catalogs prepared into a font artifact cache
/// (`tools/font-catalog.py`), newest-named first is irrelevant: they are named
/// by what they were prepared from, so any of them is a candidate and
/// [`find_root`] says which ones this build can read.
#[must_use]
pub fn prepared_catalogs(cache: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(cache.join("catalogs")) else {
        return Vec::new();
    };
    let mut roots: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    roots.sort();
    roots
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
/// | `Web` | no | no | no |
///
/// `Web` files are WOFF2 cuts of the UI face that the host serving a
/// workspace's web page serves beside it (the `web` feature embeds them as
/// [`web::CUTS`]); no catalog installs them and no native text is set in them.
///
/// The standalone catalog is the one the SDK ships next to a third-party
/// app's Windows, macOS, Android and web builds. It carries only fonts whose
/// licence lets any app bundle them (OFL, GUST); a font under another
/// licence, such as New Computer Modern's GPL-licensed Regular cut, is
/// `Os`. A catalog directory without its `Os` files is a standalone
/// catalog, and the toolkit sets text in the faces it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Product,
    Os,
    Fixture,
    Web,
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

    /// Whether the test fixture catalog carries fonts of this scope: every
    /// font text is set in, which is all but the web page's cuts.
    #[must_use]
    pub const fn fixtures(self) -> bool {
        !matches!(self, Self::Web)
    }
}

#[derive(Debug)]
pub struct Manifest {
    pub schema: u32,
    pub font: Vec<Asset>,
}

/// One pinned font file.
#[derive(Debug)]
pub struct Asset {
    pub scope: Scope,
    /// Whether a browser host must deliver this file before first paint.
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
#[derive(Debug)]
pub struct Face {
    pub index: u32,
    /// The family the file itself names.
    pub family: String,
    pub post_script_name: String,
    pub style: String,
    /// The weight class the file gives the face (its `OS/2` table), so a
    /// reader can list the face without opening the file: 400 unless the
    /// manifest says otherwise.
    pub weight: u16,
    /// The families Tilo sets this face under, in place of the file's own,
    /// when that name cannot tell the face apart from its siblings: every
    /// New Computer Modern file says `NewComputerModern` at weight 400,
    /// whether it is the Book, Regular or 8 pt cut. Empty keeps the file's
    /// own family. The file's bytes are never changed.
    pub tilo_families: Vec<String>,
}

/// One face as the build writes it: index, family, PostScript name, style,
/// weight and Tilo's families.
type FaceRow = (
    u32,
    &'static str,
    &'static str,
    &'static str,
    u16,
    &'static [&'static str],
);
/// One font as the build writes it: scope, startup, file, bytes, SHA-256,
/// reserved aliases and faces.
type FontRow = (
    Scope,
    bool,
    &'static str,
    u64,
    &'static str,
    &'static [&'static str],
    &'static [FaceRow],
);

/// `manifest.toml`'s fonts, as the build read them.
#[allow(
    clippy::unreadable_literal,
    reason = "the sizes are written by the build as manifest.toml states them"
)]
const FONTS: &[FontRow] = include!(concat!(env!("OUT_DIR"), "/manifest.rs"));

/// The compiled-in manifest, built once.
pub fn manifest() -> Result<&'static Manifest, Error> {
    static BUILT: OnceLock<Manifest> = OnceLock::new();
    let strings = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
    Ok(BUILT.get_or_init(|| Manifest {
        schema: 1,
        font: FONTS
            .iter()
            .map(
                |&(scope, browser_startup, file, bytes, sha256, aliases, faces)| Asset {
                    scope,
                    browser_startup,
                    file: file.to_owned(),
                    bytes,
                    sha256: sha256.to_owned(),
                    reserved_aliases: strings(aliases),
                    faces: faces
                        .iter()
                        .map(
                            |&(index, family, post_script_name, style, weight, families)| Face {
                                index,
                                family: family.to_owned(),
                                post_script_name: post_script_name.to_owned(),
                                style: style.to_owned(),
                                weight,
                                tilo_families: strings(families),
                            },
                        )
                        .collect(),
                },
            )
            .collect(),
    }))
}

/// SHA-256 of the compiled-in manifest, which identifies the catalog.
#[must_use]
pub fn manifest_sha256() -> String {
    hex::encode(Sha256::digest(MANIFEST.as_bytes()))
}

/// Check that `root` holds the catalog this build was made against: its
/// `manifest.toml` must be byte-identical to [`MANIFEST`].
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
    /// return its path: what preparing a catalog proves of every file. A
    /// process that sets text checks only the size ([`Asset::sized_in`]).
    pub fn verify(&self, root: &Path) -> Result<PathBuf, Error> {
        let path = self.sized_in(root)?;
        let actual = sha256_file(&path).map_err(|e| Error::at(&path, e))?;
        self.check_digest(&path, &actual)?;
        Ok(path)
    }

    /// The file's path under `root` when it is a regular file of the pinned
    /// size, read from its metadata alone.
    pub fn sized_in(&self, root: &Path) -> Result<PathBuf, Error> {
        let path = self.path_in(root)?;
        let metadata = std::fs::metadata(&path).map_err(|e| Error::at(&path, e))?;
        if !metadata.is_file() || metadata.len() != self.bytes {
            return Err(Error::at(
                &path,
                format!("expected regular file of {} bytes", self.bytes),
            ));
        }
        Ok(path)
    }

    /// Check bytes already read from this asset's file (at `path`, for the
    /// error) against its pinned size and SHA-256.
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
