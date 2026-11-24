//! Preparing the catalog on a machine with no Tilo checkout and no Tilo OS: what
//! `tools/font-catalog.py root` does in a checkout, from what this crate carries.
//!
//! The crate carries the manifest, the artifact list that pins every font file to an immutable
//! public URL ([`ARTIFACTS`]) and the notices a catalog ships beside its fonts. Preparing a catalog
//! fetches the pinned fonts it lacks into the font artifact cache (`<cache>/<sha256>/<file>`),
//! checks every byte against its pin, and publishes the catalog in the cache's `catalogs`
//! directory under the same name the tool gives it, so a catalog either of them prepared is the
//! other's too, and [`crate::prepared_catalogs`] finds it where every reader already looks.
//!
//! A cached file that does not match its pin fails preparation; it is never replaced, because a
//! cache that holds other bytes under a pin's name is something a person has to look at. A
//! catalog is published by renaming a verified directory into place, so a reader finds it whole
//! or not at all.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::{Asset, Error, MANIFEST, Scope, check_manifest, manifest, sha256_file};

/// The artifact list: every font file of the manifest, by size, SHA-256 and the immutable URL its
/// bytes are fetched from. A copy of the checkout's `artifacts.json`; the crate's tests keep the
/// two identical.
pub const ARTIFACTS: &str = include_str!("../catalog/artifacts.json");

/// Set to `1`, a catalog is prepared only from fonts already in the cache: nothing is fetched.
pub const OFFLINE_ENV: &str = "TILO_FONT_ASSETS_OFFLINE";

/// Where every pinned font is fetched from, and the only origin a fetch may answer from.
const ORIGIN: &str = "https://code.tilo.so/";

/// The pinned fonts' URLs: each names its SHA-256 as its package version and in its file name.
const URL_PREFIX: &str = "https://code.tilo.so/api/packages/tilo/generic/source-evidence/";

/// How many fonts are fetched at once.
const FETCHES: usize = 6;

/// How long one request for a font may take, its body included, before the download resumes
/// from what it holds with another.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Each notice a catalog carries: its path in the catalog, the SHA-256 the manifest pins it to
/// (the provenance document and the upstream note are pinned by being this crate's copies), and
/// its bytes.
type Notice = (&'static str, Option<&'static str>, &'static [u8]);

/// The notices, as the build read them from this crate's `catalog/` directory.
static NOTICES: &[Notice] = include!(concat!(env!("OUT_DIR"), "/notices.rs"));

/// The notices that name a catalog, with its manifest: what `tools/font-catalog.py` hashes into
/// a prepared catalog's name.
const IDENTITY: [&str; 2] = ["PROVENANCE.md", "LICENSES/UPSTREAM.md"];

/// Which fonts of the manifest a catalog carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    /// The product's catalog, which any app may bundle (the SDK's standalone catalog):
    /// [`Scope::standalone`].
    Product,
    /// What the Tilo OS image installs: [`Scope::os`].
    Os,
    /// Every font text is set in, for tests: [`Scope::fixtures`].
    Fixtures,
    /// The web page's cuts of Tilo Sans.
    Web,
}

impl Selection {
    /// The name `tools/font-catalog.py` gives this selection (its `--scope`), which ends the
    /// prepared catalog's name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Product => "product",
            Self::Os => "os",
            Self::Fixtures => "fixtures",
            Self::Web => "web",
        }
    }

    /// Whether this selection carries fonts of `scope`.
    #[must_use]
    pub const fn carries(self, scope: Scope) -> bool {
        match self {
            Self::Product => scope.standalone(),
            Self::Os => scope.os(),
            Self::Fixtures => scope.fixtures(),
            Self::Web => matches!(scope, Scope::Web),
        }
    }

    /// The fonts of the compiled-in manifest this selection carries.
    pub fn fonts(self) -> Result<Vec<&'static Asset>, Error> {
        Ok(manifest()?
            .font
            .iter()
            .filter(|asset| self.carries(asset.scope))
            .collect())
    }

    /// The bytes of this selection's fonts.
    pub fn bytes(self) -> Result<u64, Error> {
        Ok(self.fonts()?.iter().map(|asset| asset.bytes).sum())
    }

    /// The name of the catalog of this selection prepared from this build's manifest and notices:
    /// the SHA-256 of each of them, named, and the selection's name, as the tool names it.
    #[must_use]
    pub fn catalog_name(self) -> String {
        let mut identity = Sha256::new();
        identity.update(b"manifest.toml");
        identity.update(MANIFEST.as_bytes());
        for name in IDENTITY {
            if let Some((_, _, bytes)) = NOTICES.iter().find(|(file, _, _)| *file == name) {
                identity.update(name.as_bytes());
                identity.update(bytes);
            }
        }
        format!("{}-{}", hex::encode(identity.finalize()), self.name())
    }
}

/// One pinned font file of [`ARTIFACTS`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    pub file: String,
    pub bytes: u64,
    pub sha256: String,
    pub url: String,
}

/// The artifact list, checked against the compiled-in manifest: one row for every font it pins,
/// with that font's size and SHA-256, and the URL that names that SHA-256.
pub fn artifacts() -> Result<Vec<Artifact>, Error> {
    let invalid = |problem: String| Error::at("artifacts.json", problem);
    let inventory: serde_json::Value =
        serde_json::from_str(ARTIFACTS).map_err(|error| invalid(error.to_string()))?;
    let object = inventory
        .as_object()
        .filter(|object| object.len() == 2 && object.get("schema") == Some(&1.into()))
        .ok_or_else(|| invalid("unsupported font artifact inventory".into()))?;
    let rows = object
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid("no assets".into()))?;
    let fonts = &manifest()?.font;
    let mut artifacts: Vec<Artifact> = Vec::with_capacity(rows.len());
    for row in rows {
        let row = row
            .as_object()
            .filter(|row| row.len() == 4)
            .ok_or_else(|| invalid("invalid font artifact row".into()))?;
        let text = |key: &str| {
            row.get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| invalid(format!("a font artifact has no `{key}`")))
        };
        let artifact = Artifact {
            file: text("file")?,
            bytes: row
                .get("bytes")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| invalid("a font artifact has no `bytes`".into()))?,
            sha256: text("sha256")?,
            url: text("url")?,
        };
        let pin = fonts
            .iter()
            .find(|asset| asset.file == artifact.file)
            .ok_or_else(|| invalid(format!("{} is no font of the manifest", artifact.file)))?;
        if artifacts.iter().any(|seen| seen.file == artifact.file) {
            return Err(invalid(format!("{} is listed twice", artifact.file)));
        }
        if artifact.bytes != pin.bytes || artifact.sha256 != pin.sha256 {
            return Err(invalid(format!(
                "{} differs from the manifest's pin",
                artifact.file
            )));
        }
        let suffix = Path::new(&artifact.file)
            .extension()
            .map(|extension| format!(".{}", extension.to_string_lossy()))
            .unwrap_or_default();
        let sha = &pin.sha256;
        if artifact.url != format!("{URL_PREFIX}{sha}/font-{sha}{suffix}") {
            return Err(invalid(format!(
                "{}'s URL is not its immutable pin",
                artifact.file
            )));
        }
        artifacts.push(artifact);
    }
    if artifacts.len() != fonts.len() {
        return Err(invalid("the inventory does not list every font".into()));
    }
    Ok(artifacts)
}

/// Whether `TILO_FONT_ASSETS_OFFLINE=1` forbids fetching.
#[must_use]
pub fn offline() -> bool {
    std::env::var_os(OFFLINE_ENV).is_some_and(|value| value == "1")
}

/// What preparing a catalog is about to fetch, said before it starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fetching {
    pub files: usize,
    pub bytes: u64,
}

/// The catalog of `selection` prepared in the font artifact cache `cache`
/// (`<cache>/catalogs/<name>`): the one there, verified, or one made from the cache's fonts and
/// the notices this crate carries, fetching the fonts the cache lacks (`fetching` is told what,
/// once, before the first request) unless `offline`.
///
/// Every font is checked against its pin, in the cache and in the catalog; a cached font that
/// differs fails preparation and stays as it is. Processes sharing the cache take turns: one
/// prepares, and the others find its catalog.
pub fn prepared(
    cache: &Path,
    selection: Selection,
    offline: bool,
    fetching: impl FnOnce(Fetching),
) -> Result<PathBuf, Error> {
    let catalogs = cache.join("catalogs");
    let dest = catalogs.join(selection.catalog_name());
    if let Some(found) = published(&dest, selection)? {
        return Ok(found);
    }
    std::fs::create_dir_all(&catalogs).map_err(|error| Error::at(&catalogs, error))?;
    let _turn = Turn::take(cache);
    if let Some(found) = published(&dest, selection)? {
        return Ok(found);
    }
    let fonts = selection.fonts()?;
    let artifacts = artifacts()?;
    let mut missing = Vec::new();
    for asset in &fonts {
        let path = cached(cache, asset);
        if path.exists() || path.is_symlink() {
            check_cached(&path, asset)?;
        } else {
            let artifact = artifacts
                .iter()
                .find(|artifact| artifact.file == asset.file)
                .ok_or_else(|| Error::at("artifacts.json", format!("{} has no URL", asset.file)))?;
            missing.push((*asset, artifact));
        }
    }
    if !missing.is_empty() {
        let bytes = missing.iter().map(|(asset, _)| asset.bytes).sum();
        if offline {
            return Err(Error::at(
                cache,
                format!(
                    "Tilo's fonts are not on this computer yet ({} of them), and \
                     {OFFLINE_ENV}=1 keeps them from being fetched from {ORIGIN}: run the \
                     command once without it, with network access, or set {}",
                    missing.len(),
                    crate::OVERRIDE_ENV,
                ),
            ));
        }
        fetching(Fetching {
            files: missing.len(),
            bytes,
        });
        fetch(cache, &missing)?;
    }
    let staging = tempfile::Builder::new()
        .prefix(".tilo-fonts-")
        .tempdir_in(&catalogs)
        .map_err(|error| Error::at(&catalogs, error))?;
    let root = staging.path().join("catalog");
    for asset in &fonts {
        let target = asset.path_in(&root)?;
        create_parent(&target)?;
        std::fs::copy(cached(cache, asset), &target).map_err(|error| Error::at(&target, error))?;
    }
    write_notices(&root)?;
    verify(&root, selection)?;
    match std::fs::rename(&root, &dest) {
        Ok(()) => Ok(dest),
        // Another process published it first, without taking its turn: its catalog is the one.
        Err(_) if dest.is_dir() => published(&dest, selection)?
            .ok_or_else(|| Error::at(&dest, "the catalog published here is incomplete")),
        Err(error) => Err(Error::at(&dest, error)),
    }
}

/// Put the catalog of `selection` at `dest`, from the catalog at `from` (any catalog of this
/// build that carries the selection's fonts: an OS catalog gives a product one): the fonts
/// linked or copied from there and the notices this crate carries, verified, then renamed into
/// place, replacing what `dest` held. A `dest` that already holds the selection for this build is
/// left as it is.
///
/// What an app's build bundles beside its program as `fonts/`, the first place a Tilo process
/// looks for its catalog, so the program sets text in Tilo's faces on a machine without Tilo.
pub fn bundle(from: &Path, dest: &Path, selection: Selection) -> Result<(), Error> {
    let fonts = selection.fonts()?;
    let current = check_manifest(dest).is_ok()
        && fonts.iter().all(|asset| asset.sized_in(dest).is_ok())
        && NOTICES
            .iter()
            .all(|(file, _, bytes)| std::fs::read(dest.join(file)).is_ok_and(|held| held == *bytes));
    if current {
        return Ok(());
    }
    check_manifest(from)?;
    let parent = dest
        .parent()
        .ok_or_else(|| Error::at(dest, "a bundled catalog needs a parent directory"))?;
    std::fs::create_dir_all(parent).map_err(|error| Error::at(parent, error))?;
    let staging = tempfile::Builder::new()
        .prefix(".tilo-fonts-")
        .tempdir_in(parent)
        .map_err(|error| Error::at(parent, error))?;
    let root = staging.path().join("catalog");
    for asset in &fonts {
        let source = asset.sized_in(from)?;
        let target = asset.path_in(&root)?;
        create_parent(&target)?;
        if std::fs::hard_link(&source, &target).is_err() {
            std::fs::copy(&source, &target).map_err(|error| Error::at(&target, error))?;
        }
    }
    write_notices(&root)?;
    verify(&root, selection)?;
    if dest.exists() || dest.is_symlink() {
        let old = staging.path().join("replaced");
        std::fs::rename(dest, &old).map_err(|error| Error::at(dest, error))?;
    }
    std::fs::rename(&root, dest).map_err(|error| Error::at(dest, error))
}

/// Check that `root` holds the catalog of `selection` this build pins: its manifest and notices
/// byte for byte, and every font of the selection against its size and SHA-256.
pub fn verify(root: &Path, selection: Selection) -> Result<(), Error> {
    check_manifest(root)?;
    for (file, _, bytes) in NOTICES {
        let path = root.join(file);
        let held = std::fs::read(&path).map_err(|error| Error::at(&path, error))?;
        if held != *bytes {
            return Err(Error::at(path, "notice differs from this Tilo build's"));
        }
    }
    for asset in selection.fonts()? {
        asset.verify(root)?;
    }
    Ok(())
}

/// `dest`, verified, when a catalog is published there; `None` when none is.
fn published(dest: &Path, selection: Selection) -> Result<Option<PathBuf>, Error> {
    if dest.is_symlink() {
        return Err(Error::at(dest, "a prepared catalog may not be a symbolic link"));
    }
    if !dest.exists() {
        return Ok(None);
    }
    verify(dest, selection)?;
    Ok(Some(dest.to_path_buf()))
}

/// Write the manifest and the notices into the catalog being made at `root`, each checked
/// against its pin first.
fn write_notices(root: &Path) -> Result<(), Error> {
    let manifest_path = root.join("manifest.toml");
    std::fs::write(&manifest_path, MANIFEST).map_err(|error| Error::at(&manifest_path, error))?;
    for (file, sha256, bytes) in NOTICES {
        let path = root.join(file);
        if let Some(sha256) = sha256 {
            let actual = hex::encode(Sha256::digest(bytes));
            if actual != *sha256 {
                return Err(Error::at(
                    &path,
                    format!("SHA-256 mismatch: expected {sha256}, found {actual}"),
                ));
            }
        }
        create_parent(&path)?;
        std::fs::write(&path, bytes).map_err(|error| Error::at(&path, error))?;
    }
    Ok(())
}

/// Where the artifact cache holds `asset`: named by its SHA-256, as the tool lays it out.
fn cached(cache: &Path, asset: &Asset) -> PathBuf {
    cache.join(&asset.sha256).join(&asset.file)
}

/// Check a font in the cache: a regular file (never a link to one) of the pinned bytes.
fn check_cached(path: &Path, asset: &Asset) -> Result<(), Error> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| Error::at(path, error))?;
    if !metadata.is_file() || metadata.len() != asset.bytes {
        return Err(Error::at(
            path,
            format!(
                "the cached font is not a regular file of {} bytes; it was left as it is: \
                 remove it to fetch it again",
                asset.bytes
            ),
        ));
    }
    let actual = sha256_file(path).map_err(|error| Error::at(path, error))?;
    asset.check_digest(path, &actual)
}

/// Fetch `missing` into the cache, a few at once: each into a temporary file beside its place,
/// verified, then renamed there.
fn fetch(cache: &Path, missing: &[(&Asset, &Artifact)]) -> Result<(), Error> {
    use tilo_net::http::fetch::{Downloader, Request};
    let origin = tilo_net::Url::parse(ORIGIN).map_err(|error| Error::at(ORIGIN, error))?;
    let downloader = Downloader::new(origin, REQUEST_TIMEOUT, Some("tilo-font-catalog"))
        .map_err(|error| Error::at(ORIGIN, error))?;
    let next = AtomicUsize::new(0);
    let cancel = AtomicBool::new(false);
    let failed: Mutex<Option<Error>> = Mutex::new(None);
    let one = |asset: &Asset, artifact: &Artifact| -> Result<(), Error> {
        let path = cached(cache, asset);
        create_parent(&path)?;
        let directory = path.parent().unwrap_or(cache);
        let mut file = tempfile::NamedTempFile::new_in(directory)
            .map_err(|error| Error::at(directory, error))?;
        let url = tilo_net::Url::parse(&artifact.url)
            .map_err(|error| Error::at(&artifact.url, error))?;
        downloader
            .fetch(
                Request {
                    url,
                    size: asset.bytes,
                    sha256: Some(&asset.sha256),
                    cancel: &cancel,
                    pause: None,
                },
                file.as_file_mut(),
                &mut (),
                |_| {},
            )
            .map_err(|error| {
                Error::at(
                    &path,
                    format!("could not fetch {} from {}: {error}", asset.file, artifact.url),
                )
            })?;
        match file.persist_noclobber(&path) {
            Ok(_) => {}
            // Another process fetched it first: its file must be the pinned one too.
            Err(_) if path.exists() => {}
            Err(error) => return Err(Error::at(&path, error.error)),
        }
        check_cached(&path, asset)
    };
    std::thread::scope(|scope| {
        for _ in 0..FETCHES.min(missing.len()) {
            scope.spawn(|| {
                while !cancel.load(Ordering::Relaxed) {
                    let Some((asset, artifact)) = missing.get(next.fetch_add(1, Ordering::Relaxed))
                    else {
                        break;
                    };
                    if let Err(error) = one(asset, artifact) {
                        cancel.store(true, Ordering::Relaxed);
                        let mut first = failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        first.get_or_insert(error);
                    }
                }
            });
        }
    });
    match failed.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn create_parent(path: &Path) -> Result<(), Error> {
    match path.parent() {
        Some(parent) => std::fs::create_dir_all(parent).map_err(|error| Error::at(parent, error)),
        None => Ok(()),
    }
}

/// The cache's turn: an exclusive lock on its directory, which `tools/font-catalog.py` takes
/// too, held while one process fetches and publishes, and released when it is dropped. A
/// platform or file system without locks prepares without taking turns; a catalog is still
/// published whole.
struct Turn(#[allow(dead_code, reason = "held for its drop, which releases the lock")] Option<advisory_lock::FileLock>);

impl Turn {
    fn take(cache: &Path) -> Self {
        Self(
            File::open(cache)
                .ok()
                .and_then(|directory| advisory_lock::FileLock::exclusive(directory).ok()),
        )
    }
}
