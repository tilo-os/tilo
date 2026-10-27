//! Installing, removing and collecting the device's packages.
//!
//! An install streams a `.tapp` through the format codec: the metadata comes
//! first, the caller's policy accepts or refuses it before a single payload
//! byte is stored, and each carried blob is ingested into the content store
//! as it arrives. Blobs a thin package does not carry are looked up in the
//! store first — an update shares most of its files with the version it
//! replaces — and only the rest are fetched. The payload tree is then
//! materialised once from the store (reflinked where the filesystem allows)
//! and the registry row is replaced atomically; that replacement is the
//! commit point. The tree of a replaced version is removed afterwards and
//! every blob no remaining package references is evicted.
//!
//! A tree a running app has mounted ([`crate::mounts`]), or that a launch
//! holds ([`crate::lease`]) because it chose the tree and has not mounted it
//! yet, is never removed: retiring it leaves it in place, and
//! [`PackageStore::collect_orphans`] removes it once nothing holds it. The
//! size sweep never evicts a running app. A tree leaves `pkgs/` in one
//! rename into `tmp/` before any of its files is deleted, so a removal
//! stopped halfway never leaves a partial tree where a launch looks.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use tilo_clock::unix_now;

use tilo_app_package::{
    BlobFetcher, BlobHash, BlobSink, HashingReader, PackageId, PackageMeta, Tapp, TappError,
};

use crate::StoreError;
use crate::content::{self, Content};
use crate::integrity;
use crate::layout::{COLLECTION_FILE, Layout, MANIFEST_FILE, SIGNED_ROOT_FILE};
use crate::lease;
use crate::mounts::InUse;
use crate::registry::{InstalledPackage, Registry};

/// The store's size cap when nothing configures one: the app host's
/// `--max-store-bytes` default, and what `tilo-install gc` applies.
pub const DEFAULT_MAX_STORE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// What [`PackageStore::collect_orphans`] removed and kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Orphans {
    /// Payload trees and records no row or image names, now removed.
    pub removed: Vec<PathBuf>,
    /// Bytes those held.
    pub removed_bytes: u64,
    /// Unnamed trees kept because a running app has them mounted or a
    /// launch holds them.
    pub kept_in_use: Vec<PathBuf>,
    /// Bytes of install staging under `tmp/` that crashed installs left,
    /// removed.
    pub staging_bytes: u64,
}

/// Payload blobs read back from the content store.
struct StoredBlobs<'a, 'b> {
    content: &'a Content<'b>,
}

impl tilo_app_package::BlobSource for StoredBlobs<'_, '_> {
    fn open_blob(
        &self,
        hash: &BlobHash,
    ) -> std::io::Result<Option<tilo_app_package::OpenBlob<'_>>> {
        let opened = self
            .content
            .open(hash)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(opened.map(|(len, file)| tilo_app_package::OpenBlob {
            len,
            reader: Box::new(file),
        }))
    }
}

fn image_install_time(
    image_build: bool,
    epoch: Option<&str>,
    wall_clock: u64,
) -> Result<u64, StoreError> {
    if !image_build {
        return Ok(wall_clock);
    }
    epoch
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(StoreError::InvalidImageBuildEpoch)
}

/// What the caller's policy decided about a package it has seen the
/// metadata of.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Admission {
    /// Keep the package under size pressure.
    pub pinned: bool,
    /// Attestor id when an attestor chain vouched for the package.
    pub attested_by: String,
    /// Where the bytes came from, recorded for provenance.
    pub source: String,
    /// Keep this version in front of a newer one the system image ships;
    /// see [`InstalledPackage::held`].
    pub held: bool,
}

/// The outcome of a development install that preserves explicit version holds.
#[derive(Clone, Debug)]
pub enum InstallSelection {
    Installed(Box<Installed>),
    Preserved(Box<InstalledPackage>),
}

/// The result of an install.
#[derive(Clone, Debug)]
pub struct Installed {
    pub package: InstalledPackage,
    /// The row this install replaced (another version of the same app).
    pub replaced: Option<InstalledPackage>,
    /// Distinct payload blobs the package references.
    pub blobs: usize,
    /// Of those, how many the store already held before this install.
    pub blobs_already_stored: usize,
    /// How many had to be fetched from a [`BlobFetcher`].
    pub blobs_fetched: usize,
}

/// What a removal freed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub package: Option<InstalledPackage>,
    /// Blobs evicted from the content store.
    pub evicted_blobs: usize,
}

/// A handle on one package store root. Cheap; every mutation takes the
/// store's lock for its own duration, so a long-lived handle (the app host)
/// never blocks another process (`tilo-install`) between operations.
#[derive(Clone, Debug)]
pub struct PackageStore {
    layout: Layout,
}

/// Holds the store's exclusive lock until dropped.
pub(crate) struct StoreLock {
    _lock: advisory_lock::FileLock,
}

impl StoreLock {
    fn acquire(layout: &Layout) -> Result<Self, StoreError> {
        std::fs::create_dir_all(layout.root())?;
        let lock =
            advisory_lock::FileLock::open(&layout.lock(), advisory_lock::LockMode::Exclusive)?;
        Ok(Self { _lock: lock })
    }
}

/// The sink the codec streams carried blobs into: the caller's admission
/// runs on the metadata before the first blob, and each referenced blob the
/// store lacks is ingested and checked against its name.
struct IngestSink<'a, 'c, F> {
    content: &'a Content<'c>,
    admit: Option<F>,
    admission: Option<Admission>,
    refused: Option<String>,
    referenced: BTreeSet<BlobHash>,
    already: BTreeSet<BlobHash>,
    ingested: BTreeSet<BlobHash>,
}

impl<F> IngestSink<'_, '_, F>
where
    F: FnOnce(&PackageMeta) -> Result<Admission, String>,
{
    fn admit(&mut self, meta: &PackageMeta) -> std::io::Result<()> {
        if self.admission.is_some() || self.refused.is_some() {
            return Ok(());
        }
        let Some(admit) = self.admit.take() else {
            return Ok(());
        };
        match admit(meta) {
            Ok(admission) => {
                self.referenced = meta.payload_hashes();
                self.admission = Some(admission);
                Ok(())
            }
            Err(reason) => {
                self.refused = Some(reason.clone());
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    reason,
                ))
            }
        }
    }
}

impl<F> BlobSink for IngestSink<'_, '_, F>
where
    F: FnOnce(&PackageMeta) -> Result<Admission, String>,
{
    fn metadata(&mut self, meta: &PackageMeta) -> std::io::Result<()> {
        self.admit(meta)
    }

    fn accept(&mut self, hash: &BlobHash, len: u64, bytes: &mut dyn Read) -> std::io::Result<()> {
        if self.admission.is_none() {
            return Err(std::io::Error::other(
                "payload blob before package admission",
            ));
        }
        if !self.referenced.contains(hash) {
            return Ok(());
        }
        if self.content.len(hash).map_err(std::io::Error::other)? == Some(len) {
            self.already.insert(*hash);
            return Ok(());
        }
        let stored = self
            .content
            .ingest(len, bytes)
            .map_err(std::io::Error::other)?;
        if stored != *hash {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("payload blob {hash} does not match its name"),
            ));
        }
        self.ingested.insert(*hash);
        Ok(())
    }
}

/// A sink for fetched blobs: verify, then ingest.
struct FetchSink<'a, 'c> {
    content: &'a Content<'c>,
    wanted: BTreeSet<BlobHash>,
    received: BTreeSet<BlobHash>,
}

impl BlobSink for FetchSink<'_, '_> {
    fn accept(&mut self, hash: &BlobHash, len: u64, bytes: &mut dyn Read) -> std::io::Result<()> {
        if !self.wanted.contains(hash) || self.received.contains(hash) {
            return std::io::copy(bytes, &mut std::io::sink()).map(drop);
        }
        let mut hashing = HashingReader::new(bytes);
        let stored = self
            .content
            .ingest(len, &mut hashing)
            .map_err(std::io::Error::other)?;
        if stored != *hash || hashing.finish().0 != *hash {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("fetched blob {hash} does not match its name"),
            ));
        }
        self.received.insert(*hash);
        Ok(())
    }
}

fn remove_tree(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Remove a directory tree, a file or a link, never following a link.
fn remove_entry(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => remove_tree(path),
        Ok(_) => match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The staging name `materialise` gives a same-id tree it replaced.
const RETIRED_TREE: &str = "retired-tree";

/// Unique name for a staging path under `tmp/`.
fn staging_name(id: PackageId, what: &str) -> String {
    let nanos = tilo_clock::unix_now_ns() % 1_000_000_000;
    format!("{}.{what}.{}.{nanos}", id.to_hex(), std::process::id())
}

/// Replace `live` with the fully written `staged` directory, moving what
/// was there to `retired`. Returns whether anything was; the caller removes
/// it.
fn swap_in(staged: &Path, live: &Path, retired: &Path) -> std::io::Result<bool> {
    let had_live = std::fs::symlink_metadata(live).is_ok();
    if had_live {
        std::fs::rename(live, retired)?;
    }
    if let Err(error) = std::fs::rename(staged, live) {
        if had_live {
            let _ = std::fs::rename(retired, live);
        }
        return Err(error);
    }
    Ok(had_live)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

impl PackageStore {
    /// A store rooted at `root`. Nothing is created until the first write.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            layout: Layout::new(root),
        }
    }

    /// The per-user store the environment names; see
    /// [`crate::layout::default_user_root`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NoHome`] when no store can be located.
    pub fn from_env() -> Result<Self, StoreError> {
        crate::layout::default_user_root()
            .map(Self::at)
            .ok_or(StoreError::NoHome)
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub(crate) fn lock(&self) -> Result<StoreLock, StoreError> {
        StoreLock::acquire(&self.layout)
    }

    /// The registry as it is now.
    ///
    /// # Errors
    ///
    /// Returns an error if the registry exists but cannot be read.
    pub fn registry(&self) -> Result<Registry, StoreError> {
        Ok(Registry::load(&self.layout.registry())?)
    }

    /// Install the `.tapp` streamed from `tapp`.
    ///
    /// `admit` sees the verified-in-structure metadata before any payload
    /// byte is stored and decides trust: return `Err(reason)` to refuse.
    /// Blobs the archive does not carry and the store lacks come from
    /// `fetcher`; without one, a thin package installs only when the store
    /// already holds its whole payload.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Refused`] when `admit` refuses,
    /// [`StoreError::Tapp`] for a malformed archive or a blob that does not
    /// match its name, [`StoreError::MissingBlobs`] when payload remains
    /// unavailable, [`StoreError::Payload`] when the payload exceeds its
    /// budget, and I/O or content-store errors. A precommit content barrier
    /// failure leaves the registry selection unchanged. Garbage collection
    /// errors can occur after the new selection is committed. If final close
    /// fails after otherwise successful work, [`StoreError::CommittedSelection`]
    /// identifies the selected app, version and package root.
    pub fn install<F>(
        &self,
        tapp: impl Read + Send,
        admit: F,
        fetcher: Option<&dyn BlobFetcher>,
    ) -> Result<Installed, StoreError>
    where
        F: FnOnce(&PackageMeta) -> Result<Admission, String> + Send,
    {
        self.install_with_flush(tapp, admit, fetcher, content::flush)
    }

    fn install_with_flush<F, B>(
        &self,
        tapp: impl Read + Send,
        admit: F,
        fetcher: Option<&dyn BlobFetcher>,
        flush: B,
    ) -> Result<Installed, StoreError>
    where
        F: FnOnce(&PackageMeta) -> Result<Admission, String> + Send,
        B: FnOnce(&Content<'_>) -> Result<(), StoreError> + Send,
    {
        self.install_with_hooks(tapp, admit, fetcher, flush, close_install_result)
    }

    fn install_with_hooks<F, B, C>(
        &self,
        tapp: impl Read + Send,
        admit: F,
        fetcher: Option<&dyn BlobFetcher>,
        flush: B,
        after_close: C,
    ) -> Result<Installed, StoreError>
    where
        F: FnOnce(&PackageMeta) -> Result<Admission, String> + Send,
        B: FnOnce(&Content<'_>) -> Result<(), StoreError> + Send,
        C: FnOnce(Option<&Installed>, Result<(), StoreError>) -> Result<(), StoreError> + Send,
    {
        let _lock = self.lock()?;
        let admit = |meta: &PackageMeta| {
            let kind = meta.manifest.package.kind;
            if kind.is_image() {
                return Err(format!(
                    "{} is a {kind} image, which environments stack and nothing launches; \
                     it is not installed as an application",
                    meta.manifest.package.id
                ));
            }
            admit(meta)
        };
        content::session_with_close(
            &self.layout.content(),
            &self.layout.tmp(),
            |content| {
                let (meta, admission, blobs_already_stored, blobs_fetched) =
                    self.ingest_with_content(content, tapp, admit, fetcher, false)?;
                // The registry is the commit point: content writes must finish before
                // the row becomes visible. Final close runs after GC and on every
                // error; an error from work still takes precedence over close.
                flush(content)?;
                let blobs = meta.payload_hashes().len();
                let (package, replaced) = self.commit(&meta, admission)?;
                self.collect_garbage_with_content(content)?;
                Ok(Installed {
                    package,
                    replaced,
                    blobs,
                    blobs_already_stored,
                    blobs_fetched,
                })
            },
            after_close,
        )
    }

    /// Install the environment image streamed from `tapp`: an `env-runtime`
    /// or `tool` package, which gets no registry row — nothing launches it —
    /// and is kept, read-only, at [`Layout::payload`] until removed with
    /// [`Self::remove_image`]. Installing an image that is already present
    /// changes nothing, so a tree a running environment stacks is never
    /// replaced underneath it.
    ///
    /// `admit` decides trust exactly as for [`Self::install`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Refused`] for a package that is not an image or
    /// that `admit` refuses, [`StoreError::Payload`] for a malformed image
    /// tree, and otherwise what [`Self::install`] returns.
    pub fn install_image<F>(
        &self,
        tapp: impl Read + Send,
        admit: F,
        fetcher: Option<&dyn BlobFetcher>,
    ) -> Result<PackageMeta, StoreError>
    where
        F: FnOnce(&PackageMeta) -> Result<Admission, String> + Send,
    {
        let _lock = self.lock()?;
        let admit = |meta: &PackageMeta| {
            let kind = meta.manifest.package.kind;
            if !kind.is_image() {
                return Err(format!(
                    "{} is a {kind} package, not an environment image",
                    meta.manifest.package.id
                ));
            }
            admit(meta)
        };
        let (meta, ..) = self.ingest(tapp, admit, fetcher, true)?;
        Ok(meta)
    }

    /// The image `id`, when this store holds it complete: its signed
    /// record and its materialised tree.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::CorruptPackage`] when a record is present but
    /// does not form the package `id`.
    pub fn image(&self, id: PackageId) -> Result<Option<PackageMeta>, StoreError> {
        crate::image::read_image(&self.layout, id)
    }

    /// Write the image `id` to `out` as a whole `.tapp`, its payload read
    /// back from the content store, and hand `out` back.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInstalled`] when this store does not hold
    /// the image, [`StoreError::Tapp`] when a payload blob is missing or
    /// does not match its hash, or an I/O or content-store error.
    pub fn export_image<W: std::io::Write + Send>(
        &self,
        id: PackageId,
        out: W,
    ) -> Result<W, StoreError> {
        let _lock = self.lock()?;
        let meta = self
            .image(id)?
            .ok_or_else(|| StoreError::NotInstalled(id.to_string()))?;
        content::session(&self.layout.content(), &self.layout.tmp(), |content| {
            let source = StoredBlobs { content };
            tilo_app_package::tapp::write(out, &meta, Some(&source))
                .map_err(|error| StoreError::Tapp(error.to_string()))
        })
    }

    /// Every image this store holds.
    ///
    /// # Errors
    ///
    /// Returns an error when the package directory cannot be read or an
    /// image's record is corrupt.
    pub fn images(&self) -> Result<Vec<PackageMeta>, StoreError> {
        crate::image::list_images(&self.layout)
    }

    /// Remove the image `id` and evict the blobs nothing else references.
    /// Removing an image that is not held does nothing.
    ///
    /// # Errors
    ///
    /// Returns an I/O or content-store error.
    pub fn remove_image(&self, id: PackageId) -> Result<Removed, StoreError> {
        let _lock = self.lock()?;
        if self.image(id)?.is_some() {
            remove_tree(&self.layout.meta_dir(id))?;
            remove_tree(&self.layout.payload(id))?;
        }
        let evicted_blobs = self.collect_garbage_locked()?;
        Ok(Removed {
            package: None,
            evicted_blobs,
        })
    }

    /// Install an explicitly selected development package while preserving a
    /// user's held version. The admission policy still runs first; the held
    /// decision reads the current registry under the normal installation lock.
    /// Existing pins are retained by the same commit path as ordinary installs.
    /// `Preserved` carries the held row observed under the canonical lock.
    ///
    /// # Errors
    /// Returns the same verification, admission and storage errors as [`Self::install`].
    pub fn install_preserving_hold<F>(
        &self,
        tapp: impl Read + Send,
        admit: F,
        fetcher: Option<&dyn BlobFetcher>,
    ) -> Result<InstallSelection, StoreError>
    where
        F: FnOnce(&PackageMeta) -> Result<Admission, String> + Send,
    {
        let mut preserved = None;
        let result = self.install(
            tapp,
            |meta| {
                let admission = admit(meta)?;
                let registry = self.registry().map_err(|error| error.to_string())?;
                if let Some(row) = registry.find(&meta.manifest.package.id)
                    && row.held
                {
                    preserved = Some(row.clone());
                    return Err("the explicitly held package is preserved".into());
                }
                Ok(admission)
            },
            fetcher,
        );
        match (result, preserved) {
            (Err(StoreError::Refused(_)), Some(row)) => {
                Ok(InstallSelection::Preserved(Box::new(row)))
            }
            (result, _) => result.map(|installed| InstallSelection::Installed(Box::new(installed))),
        }
    }

    /// Decode `tapp`, admit it, store its payload and materialise it.
    /// With `keep_present`, a package already materialised in full is left
    /// exactly as it is. Returns the package, its admission, how many blobs
    /// were already stored and how many were fetched. Call with the lock.
    fn ingest<F>(
        &self,
        tapp: impl Read + Send,
        admit: F,
        fetcher: Option<&dyn BlobFetcher>,
        keep_present: bool,
    ) -> Result<(PackageMeta, Admission, usize, usize), StoreError>
    where
        F: FnOnce(&PackageMeta) -> Result<Admission, String> + Send,
    {
        let layout = self.layout.clone();
        content::session(&layout.content(), &layout.tmp(), |content| {
            self.ingest_with_content(content, tapp, admit, fetcher, keep_present)
        })
    }

    /// Ingest the package streamed from `tapp` and the payload blobs it
    /// lacks, and materialise its tree. Its blobs are written as one
    /// deferred-durability group: one database sync when the package is in,
    /// not one per blob, which on a device's flash cost an fsync for each of
    /// an app's hundreds of files. Nothing refers to the blobs until the
    /// registry row is committed, after the caller's flush.
    fn ingest_with_content<F>(
        &self,
        content: &Content<'_>,
        tapp: impl Read + Send,
        admit: F,
        fetcher: Option<&dyn BlobFetcher>,
        keep_present: bool,
    ) -> Result<(PackageMeta, Admission, usize, usize), StoreError>
    where
        F: FnOnce(&PackageMeta) -> Result<Admission, String> + Send,
    {
        content.deferred(|| self.ingest_deferred(content, tapp, admit, fetcher, keep_present))
    }

    fn ingest_deferred<F>(
        &self,
        content: &Content<'_>,
        tapp: impl Read + Send,
        admit: F,
        fetcher: Option<&dyn BlobFetcher>,
        keep_present: bool,
    ) -> Result<(PackageMeta, Admission, usize, usize), StoreError>
    where
        F: FnOnce(&PackageMeta) -> Result<Admission, String> + Send,
    {
        let mut sink = IngestSink {
            content,
            admit: Some(admit),
            admission: None,
            refused: None,
            referenced: BTreeSet::new(),
            already: BTreeSet::new(),
            ingested: BTreeSet::new(),
        };
        let decoded = tilo_app_package::tapp::read(tapp, &mut sink);
        if let Some(reason) = sink.refused.take() {
            return Err(StoreError::Refused(reason));
        }
        let tapp: Tapp = decoded?;
        // A thin archive has no blobs, so admission runs here.
        sink.admit(&tapp.meta)?;
        if let Some(reason) = sink.refused.take() {
            return Err(StoreError::Refused(reason));
        }
        let admission = sink
            .admission
            .take()
            .ok_or_else(|| StoreError::Refused("package was not admitted".into()))?;
        if keep_present && crate::image::read_image(&self.layout, tapp.meta.id)?.is_some() {
            let blobs = tapp.meta.payload_hashes().len();
            return Ok((tapp.meta, admission, blobs, 0));
        }
        let mut already = sink.already;
        let (delivered, missing) =
            Self::fill_missing(content, &tapp.meta, &mut already, &sink.ingested, fetcher)?;
        if !missing.is_empty() {
            return Err(StoreError::MissingBlobs(missing));
        }
        Self::check_budget(content, &tapp.meta)?;
        self.materialise(content, &tapp.meta)?;
        Ok((tapp.meta, admission, already.len(), delivered))
    }

    /// Look up the blobs `meta` references in the store and fetch the rest.
    /// Returns how many were fetched and which are still missing.
    fn fill_missing(
        content: &Content<'_>,
        meta: &PackageMeta,
        already: &mut BTreeSet<BlobHash>,
        ingested: &BTreeSet<BlobHash>,
        fetcher: Option<&dyn BlobFetcher>,
    ) -> Result<(usize, Vec<BlobHash>), StoreError> {
        let mut wanted = BTreeSet::new();
        for hash in meta.payload_hashes() {
            if content.len(&hash)?.is_none() {
                wanted.insert(hash);
            } else if !ingested.contains(&hash) {
                // Present and not brought by this archive: it was stored
                // before this install began.
                already.insert(hash);
            }
        }
        let mut delivered = 0usize;
        if let Some(fetcher) = fetcher
            && !wanted.is_empty()
        {
            let list: Vec<BlobHash> = wanted.iter().copied().collect();
            let mut sink = FetchSink {
                content,
                wanted: wanted.clone(),
                received: BTreeSet::new(),
            };
            fetcher.fetch(&list, &mut sink)?;
            delivered = sink.received.len();
            wanted.retain(|hash| !sink.received.contains(hash));
        }
        Ok((delivered, wanted.into_iter().collect()))
    }

    fn check_budget(content: &Content<'_>, meta: &PackageMeta) -> Result<(), StoreError> {
        let mut failed = None;
        let result = tilo_app_package::validate_materialized_payload(
            &meta.manifest,
            &meta.collection,
            |hash| match content.len(hash) {
                Ok(len) => len,
                Err(error) => {
                    failed = Some(error);
                    None
                }
            },
        );
        if let Some(error) = failed {
            return Err(error);
        }
        result.map_err(|error| StoreError::Payload(error.to_string()))
    }

    /// Write `meta`'s payload tree and metadata into place from the store.
    pub(crate) fn materialise(
        &self,
        content: &Content<'_>,
        meta: &PackageMeta,
    ) -> Result<(), StoreError> {
        let layout = &self.layout;
        std::fs::create_dir_all(layout.packages())?;
        let tmp = layout.tmp();
        let tree = tmp.join(staging_name(meta.id, "tree"));
        let record = tmp.join(staging_name(meta.id, "pkg"));
        let result = (|| -> Result<(), StoreError> {
            std::fs::create_dir_all(&tree)?;
            let executable = meta.executable_path();
            let image = Self::image_tree(content, meta, &tree)?;
            for (rel, hash) in &meta.collection.entries {
                let target = tree.join(rel);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if !(image.is_some() && rel == tilo_app_package::TREE_FILE) {
                    content.export(hash, &target)?;
                }
                // Read-only: a materialised file is a view of stored
                // content, and the integrity check replays it against the
                // signed collection.
                let runs = match &image {
                    Some(spec) => spec.is_executable(rel),
                    None => executable.as_deref() == Some(rel.as_str()),
                };
                set_mode(&target, if runs { 0o555 } else { 0o444 })?;
            }
            if let Some(spec) = &image {
                spec.apply(&tree)?;
            }
            std::fs::create_dir_all(&record)?;
            tilo_files::native::atomic::write(
                &record.join(SIGNED_ROOT_FILE),
                &meta.signed_root_bytes(),
            )?;
            tilo_files::native::atomic::write(&record.join(MANIFEST_FILE), &meta.manifest_bytes)?;
            tilo_files::native::atomic::write(
                &record.join(COLLECTION_FILE),
                &meta.collection_bytes,
            )?;
            let retired_tree = tmp.join(staging_name(meta.id, RETIRED_TREE));
            // A launch holding or running the replaced copy keeps it: the
            // staging sweep removes it once nothing does.
            if swap_in(&tree, &layout.payload(meta.id), &retired_tree)?
                && !lease::busy(layout, meta.id)?
                && !InUse::now().holds(&retired_tree)
            {
                remove_tree(&retired_tree)?;
            }
            let retired_record = tmp.join(staging_name(meta.id, "retired-pkg"));
            if swap_in(&record, &layout.meta_dir(meta.id), &retired_record)? {
                remove_tree(&retired_record)?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = remove_tree(&tree);
            let _ = remove_tree(&record);
        }
        result
    }

    /// For an image, export its tree file into `tree` and return the checked
    /// tree it describes; `None` for every other kind.
    fn image_tree(
        content: &Content<'_>,
        meta: &PackageMeta,
        tree: &Path,
    ) -> Result<Option<tilo_app_package::TreeSpec>, StoreError> {
        if !meta.manifest.package.kind.is_image() {
            return Ok(None);
        }
        let payload = |error: tilo_app_package::ImageError| StoreError::Payload(error.to_string());
        let hash = meta
            .collection
            .entries
            .get(tilo_app_package::TREE_FILE)
            .ok_or_else(|| {
                StoreError::Payload(format!("image has no {}", tilo_app_package::TREE_FILE))
            })?;
        let path = tree.join(tilo_app_package::TREE_FILE);
        content.export(hash, &path)?;
        let spec =
            tilo_app_package::TreeSpec::from_bytes(&std::fs::read(&path)?).map_err(payload)?;
        spec.check(&meta.collection).map_err(payload)?;
        Ok(Some(spec))
    }

    /// Publish the row for `meta` and retire what it replaced.
    fn commit(
        &self,
        meta: &PackageMeta,
        admission: Admission,
    ) -> Result<(InstalledPackage, Option<InstalledPackage>), StoreError> {
        let mut registry = self.registry()?;
        // Only the image-wide store is built with this flag. Ordinary user
        // installs continue to record wall-clock time; two independent image
        // builds must write identical registry rows for the same source.
        let now = image_install_time(
            std::env::var_os("TILO_IMAGE_BUILD").as_deref() == Some(std::ffi::OsStr::new("1")),
            std::env::var("SOURCE_DATE_EPOCH").ok().as_deref(),
            unix_now(),
        )?;
        let previous = registry.find(&meta.manifest.package.id).cloned();
        let pinned = admission.pinned || previous.as_ref().is_some_and(|row| row.pinned);
        let package = self.row_for(
            meta,
            Admission {
                pinned,
                ..admission
            },
            now,
        );
        let replaced = registry.upsert(package.clone());
        registry.save(&self.layout.registry())?;
        if let Some(old) = &replaced
            && old.package_id_hex != package.package_id_hex
        {
            self.retire_tree(old)?;
        }
        Ok((package, replaced))
    }

    /// The registry row of a materialised package.
    pub(crate) fn row_for(
        &self,
        meta: &PackageMeta,
        admission: Admission,
        installed_at: u64,
    ) -> InstalledPackage {
        InstalledPackage {
            id: meta.manifest.package.id.clone(),
            version: meta.manifest.package.version.clone(),
            package_id_hex: meta.id.to_hex(),
            payload_root: self.layout.payload(meta.id),
            manifest_path: self.layout.manifest(meta.id),
            publisher_key_hex: hex::encode(meta.publisher()),
            pinned: admission.pinned,
            installed_at,
            last_used: installed_at,
            attested_by: admission.attested_by,
            cli_commands: meta
                .manifest
                .cli
                .as_ref()
                .map(|cli| cli.commands.clone())
                .unwrap_or_default(),
            runtime: meta.manifest.package.runtime.clone().unwrap_or_default(),
            source: admission.source,
            held: admission.held,
            payload_bytes: self.materialised_bytes(meta),
            system: false,
        }
    }

    /// Bytes of `meta`'s materialised payload tree, measured once so that
    /// listing the package later never walks it.
    fn materialised_bytes(&self, meta: &PackageMeta) -> u64 {
        let root = self.layout.payload(meta.id);
        meta.collection
            .entries
            .keys()
            .map(|rel| std::fs::metadata(root.join(rel)).map_or(0, |m| m.len()))
            .fold(0u64, u64::saturating_add)
    }

    /// Remove the files of a row that is no longer in the registry. A tree
    /// a running app has mounted or a launch holds stays, with its record,
    /// until [`Self::collect_orphans`] finds it free.
    pub(crate) fn retire_tree(&self, row: &InstalledPackage) -> Result<(), StoreError> {
        let layout = &self.layout;
        if let Some(id) = layout.materialised_id(&row.payload_root) {
            let Some(taken) = lease::take(layout, id)? else {
                return Ok(());
            };
            if InUse::now().holds(&layout.payload(id)) {
                return Ok(());
            }
            self.discard(&layout.payload(id), id, "discarded-tree")?;
            self.discard(&layout.meta_dir(id), id, "discarded-pkg")?;
            lease::forget(layout, id, taken)?;
        } else if row.payload_root.starts_with(layout.root().join("payloads")) {
            // A tree from before payloads were content addressed.
            remove_tree(&row.payload_root)?;
            remove_tree(&integrity::legacy_record_dir(layout.root(), &row.id))?;
        }
        Ok(())
    }

    /// Uninstall `app_id`. A pinned package is removed only with `force`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInstalled`], [`StoreError::Pinned`], or an
    /// I/O, registry or content-store error.
    pub fn remove(&self, app_id: &str, force: bool) -> Result<Removed, StoreError> {
        let _lock = self.lock()?;
        let mut registry = self.registry()?;
        let row = Self::remove_row_locked(&mut registry, app_id, force)?;
        registry.save(&self.layout.registry())?;
        self.retire_tree(&row)?;
        let evicted_blobs = self.collect_garbage_locked()?;
        Ok(Removed {
            package: Some(row),
            evicted_blobs,
        })
    }

    /// Drop `app_id`'s row from `registry` (in memory only) and return it.
    /// The caller holds the lock, saves the registry, retires the tree and
    /// collects garbage.
    fn remove_row_locked(
        registry: &mut Registry,
        app_id: &str,
        force: bool,
    ) -> Result<InstalledPackage, StoreError> {
        let row = registry
            .find(app_id)
            .ok_or_else(|| StoreError::NotInstalled(app_id.to_string()))?;
        if row.pinned && !force {
            return Err(StoreError::Pinned(app_id.to_string()));
        }
        registry
            .remove(app_id)
            .ok_or_else(|| StoreError::NotInstalled(app_id.to_string()))
    }

    /// Move `path` (`id`'s tree or record) out of `pkgs/` into `tmp/` in one
    /// rename, then delete it: a deletion stopped halfway leaves nothing a
    /// launch looks up, and the staging sweep finishes it.
    fn discard(&self, path: &Path, id: PackageId, what: &str) -> Result<(), StoreError> {
        if !std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()) {
            return Ok(());
        }
        let tmp = self.layout.tmp();
        std::fs::create_dir_all(&tmp)?;
        let gone = tmp.join(staging_name(id, what));
        std::fs::rename(path, &gone)?;
        remove_tree(&gone)?;
        Ok(())
    }

    /// Remove every payload tree and record under `pkgs/` that no registry
    /// row and no image names: left by a crash between a commit and retiring
    /// the replaced tree, or kept by [`Self::retire_tree`] while a running
    /// app had it mounted or a launch held it. A tree `in_use` holds or a
    /// launch holds stays. Then empty `tmp/` of the staging crashed
    /// installs left.
    ///
    /// Nothing selects such a tree — an install materialises its tree afresh
    /// — and a launch that named one by path takes its lease first, which
    /// this holds while it removes the tree, so the launch finds the tree
    /// whole or gone.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry cannot be read or a removal fails.
    pub fn collect_orphans(&self, in_use: &InUse) -> Result<Orphans, StoreError> {
        let packages = self.layout.packages();
        if !packages.is_dir() && !self.layout.tmp().is_dir() {
            return Ok(Orphans::default());
        }
        let _lock = self.lock()?;
        let mut out = Orphans {
            staging_bytes: self.sweep_staging_locked(in_use)?,
            ..Orphans::default()
        };
        if !packages.is_dir() {
            return Ok(out);
        }
        let named: BTreeSet<PackageId> = self
            .registry()?
            .entries()
            .iter()
            .filter_map(|row| self.layout.materialised_id(&row.payload_root))
            .collect();
        let mut found = BTreeSet::new();
        for entry in std::fs::read_dir(&packages)? {
            let entry = entry?;
            // Only directories directly under `pkgs/`; a link is never followed.
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            if let Some(id) = name
                .to_str()
                .map(|name| name.strip_suffix(".pkg").unwrap_or(name))
                .and_then(crate::layout::parse_id_hex)
            {
                found.insert(id);
            }
        }
        for id in found.difference(&named) {
            let orphan = match crate::image::read_image(&self.layout, *id) {
                Ok(None) | Err(StoreError::CorruptPackage(..)) => true,
                Err(StoreError::Io(error)) => error.kind() == std::io::ErrorKind::NotFound,
                // An image has no row and stays until `remove_image`; a
                // record unreadable for another reason cannot be told apart.
                Ok(Some(_)) | Err(_) => false,
            };
            if !orphan {
                continue;
            }
            let tree = self.layout.payload(*id);
            let Some(taken) = lease::take(&self.layout, *id)? else {
                out.kept_in_use.push(tree);
                continue;
            };
            if in_use.holds(&tree) {
                out.kept_in_use.push(tree);
                continue;
            }
            for (path, what) in [
                (tree, "orphan-tree"),
                (self.layout.meta_dir(*id), "orphan-pkg"),
            ] {
                if !path.is_dir() {
                    continue;
                }
                let bytes = crate::tree_size(&path);
                self.discard(&path, *id, what)?;
                out.removed_bytes = out.removed_bytes.saturating_add(bytes);
                out.removed.push(path);
            }
            lease::forget(&self.layout, *id, taken)?;
        }
        self.forget_idle_leases()?;
        Ok(out)
    }

    /// Drop the lease file of every tree that is gone and that no launch
    /// holds: a launch that found its tree removed leaves one behind.
    fn forget_idle_leases(&self) -> Result<(), StoreError> {
        let leases = self.layout.root().join(crate::layout::LEASES_DIR);
        if !std::fs::symlink_metadata(&leases).is_ok_and(|meta| meta.is_dir()) {
            return Ok(());
        }
        for entry in std::fs::read_dir(&leases)? {
            let entry = entry?;
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(crate::layout::parse_id_hex)
            else {
                continue;
            };
            if self.layout.payload(id).exists() {
                continue;
            }
            if let Some(taken) = lease::take(&self.layout, id)? {
                lease::forget(&self.layout, id, taken)?;
            }
        }
        Ok(())
    }

    /// Remove what crashed installs left in `tmp/`, returning its bytes.
    /// Every install stages there only while it holds the store lock, which
    /// the caller holds, so all of it is abandoned — except a same-id tree
    /// an install replaced while a launch held it or a running app had it
    /// mounted, which stays until neither does.
    fn sweep_staging_locked(&self, in_use: &InUse) -> Result<u64, StoreError> {
        let tmp = self.layout.tmp();
        if !std::fs::symlink_metadata(&tmp).is_ok_and(|meta| meta.is_dir()) {
            return Ok(0);
        }
        let mut bytes = 0u64;
        for entry in std::fs::read_dir(&tmp)? {
            let path = entry?.path();
            let replaced = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.split_once('.'))
                .filter(|(_, rest)| rest.starts_with(RETIRED_TREE))
                .and_then(|(hex, _)| crate::layout::parse_id_hex(hex));
            let taken = match replaced {
                Some(id) => match lease::take(&self.layout, id)? {
                    Some(taken) => Some((id, taken)),
                    None => continue,
                },
                None => None,
            };
            if taken.is_some() && in_use.holds(&path) {
                continue;
            }
            let size = match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_file() => meta.len(),
                _ => crate::tree_size(&path),
            };
            remove_entry(&path)?;
            bytes = bytes.saturating_add(size);
            if let Some((id, taken)) = taken
                && !self.layout.payload(id).exists()
            {
                lease::forget(&self.layout, id, taken)?;
            }
        }
        Ok(bytes)
    }

    /// Set the pin of `app_id`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInstalled`] or a registry error.
    pub fn set_pinned(&self, app_id: &str, pinned: bool) -> Result<(), StoreError> {
        self.update_row(app_id, |row| row.pinned = pinned)
    }

    /// Record a launch of `app_id` for least-recently-used eviction.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInstalled`] or a registry error.
    pub fn mark_used(&self, app_id: &str) -> Result<(), StoreError> {
        let now = unix_now();
        self.update_row(app_id, |row| row.last_used = now)
    }

    /// Apply `change` to the row of `app_id` and persist it.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInstalled`] or a registry error.
    pub fn update_row(
        &self,
        app_id: &str,
        change: impl FnOnce(&mut InstalledPackage),
    ) -> Result<(), StoreError> {
        let _lock = self.lock()?;
        let mut registry = self.registry()?;
        let row = registry
            .find_mut(app_id)
            .ok_or_else(|| StoreError::NotInstalled(app_id.to_string()))?;
        change(row);
        registry.save(&self.layout.registry())?;
        Ok(())
    }

    /// Evict every stored blob no installed package references. Returns the
    /// number of blobs evicted.
    ///
    /// # Errors
    ///
    /// Returns an error — and evicts nothing — when an installed package's
    /// record cannot be read, since its blobs could not be told apart.
    pub fn collect_garbage(&self) -> Result<usize, StoreError> {
        let _lock = self.lock()?;
        self.collect_garbage_locked()
    }

    /// Every payload blob installed packages reference.
    ///
    /// Reads each record's collection without replaying its signature:
    /// which blobs a package names is bookkeeping, not trust, and the
    /// signature was checked when the package was installed.
    fn referenced_blobs(&self) -> Result<BTreeSet<BlobHash>, StoreError> {
        let mut live = BTreeSet::new();
        for row in self.registry()?.entries() {
            if self.layout.materialised_id(&row.payload_root).is_none() {
                // Not backed by the content store (a tree kept from before
                // it existed); it references nothing here.
                continue;
            }
            let collection = integrity::read_collection(&integrity::record_dir(row))
                .map_err(|error| StoreError::CorruptPackage(row.id.clone(), error.to_string()))?;
            live.extend(collection.entries.values().copied());
        }
        for image in self.images()? {
            live.extend(image.collection.entries.values().copied());
        }
        Ok(live)
    }

    pub(crate) fn collect_garbage_locked(&self) -> Result<usize, StoreError> {
        let live = self.referenced_blobs()?;
        if !self.layout.content().exists() {
            return Ok(0);
        }
        content::session(&self.layout.content(), &self.layout.tmp(), |content| {
            Self::collect_garbage_content(content, &live)
        })
    }

    fn collect_garbage_with_content(&self, content: &Content<'_>) -> Result<usize, StoreError> {
        let live = self.referenced_blobs()?;
        Self::collect_garbage_content(content, &live)
    }

    fn collect_garbage_content(
        content: &Content<'_>,
        live: &BTreeSet<BlobHash>,
    ) -> Result<usize, StoreError> {
        let dead: Vec<BlobHash> = content
            .hashes()?
            .into_iter()
            .filter(|hash| !live.contains(hash))
            .collect();
        content.evict(&dead)?;
        Ok(dead.len())
    }

    /// What each row of `registry` occupies, in one pass: each record's
    /// collection is read without replaying its signature, and each
    /// distinct blob's length is read once from a materialised file.
    fn usage_index(registry: &Registry) -> UsageIndex {
        let mut sizes: BTreeMap<BlobHash, u64> = BTreeMap::new();
        let rows = registry
            .entries()
            .iter()
            .map(|row| {
                let mut usage = UsageRow {
                    id: row.id.clone(),
                    pinned: row.pinned,
                    last_used: row.last_used,
                    ..UsageRow::default()
                };
                let Ok(collection) = integrity::read_collection(&integrity::record_dir(row)) else {
                    // No readable record: the tree's bytes cannot be told
                    // apart, so they count as this row's alone.
                    usage.loose_bytes = crate::tree_size(&row.payload_root);
                    return usage;
                };
                for (rel, hash) in &collection.entries {
                    let len = match sizes.get(hash) {
                        Some(len) => *len,
                        None => match std::fs::metadata(row.payload_root.join(rel)) {
                            Ok(meta) => *sizes.entry(*hash).or_insert(meta.len()),
                            Err(_) => 0,
                        },
                    };
                    usage.blobs.push((*hash, len));
                }
                usage
            })
            .collect();
        UsageIndex::new(rows)
    }

    /// Distinct payload bytes each installed package references, and the
    /// total with shared blobs counted once. Bookkeeping only: no record's
    /// signature is replayed here ([`integrity::verify`] does that).
    ///
    /// # Errors
    ///
    /// Returns an error if the registry cannot be read.
    pub fn disk_usage(&self) -> Result<DiskUsage, StoreError> {
        Ok(Self::usage_index(&self.registry()?).disk_usage())
    }

    /// Evict unpinned packages, least recently used first, until the
    /// store's distinct payload bytes are at most `max_bytes`. `0` disables
    /// the sweep. A package a running app has mounted is never evicted.
    /// Returns the app ids removed.
    ///
    /// The whole sweep runs under the store's lock: usage is measured once,
    /// the evictions are planned from it, the registry is rewritten once and
    /// garbage is collected once.
    ///
    /// # Errors
    ///
    /// Returns an error if usage cannot be measured or a removal fails.
    pub fn sweep(&self, max_bytes: u64) -> Result<Vec<String>, StoreError> {
        if max_bytes == 0 {
            return Ok(Vec::new());
        }
        let _lock = self.lock()?;
        let mut registry = self.registry()?;
        // Read only when a row would be evicted: a store under its cap
        // never needs the mount table.
        let in_use = InUse::now();
        let running = |id: &str| {
            registry
                .find(id)
                .is_some_and(|row| in_use.holds(&row.payload_root))
        };
        let plan = plan_eviction(&Self::usage_index(&registry), max_bytes, running);
        if plan.is_empty() {
            return Ok(plan);
        }
        let mut retired = Vec::with_capacity(plan.len());
        for id in &plan {
            retired.push(Self::remove_row_locked(&mut registry, id, false)?);
        }
        registry.save(&self.layout.registry())?;
        for row in &retired {
            self.retire_tree(row)?;
        }
        self.collect_garbage_locked()?;
        Ok(plan)
    }
}

/// One package's share of the store, for accounting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct UsageRow {
    id: String,
    pinned: bool,
    last_used: u64,
    /// Each payload file's blob and length, in collection order.
    blobs: Vec<(BlobHash, u64)>,
    /// Bytes that belong to no known blob: the tree of a row whose record
    /// cannot be read. Counted as this row's alone.
    loose_bytes: u64,
}

/// Every row's share, with how many rows reference each blob and the
/// length each blob is counted at (its first occurrence).
#[derive(Clone, Debug, Default)]
struct UsageIndex {
    rows: Vec<UsageRow>,
    refcount: BTreeMap<BlobHash, u32>,
    sizes: BTreeMap<BlobHash, u64>,
}

impl UsageIndex {
    fn new(rows: Vec<UsageRow>) -> Self {
        let mut refcount: BTreeMap<BlobHash, u32> = BTreeMap::new();
        let mut sizes = BTreeMap::new();
        for row in &rows {
            let mut seen = BTreeSet::new();
            for (hash, len) in &row.blobs {
                sizes.entry(*hash).or_insert(*len);
                if seen.insert(*hash) {
                    let count = refcount.entry(*hash).or_default();
                    *count = count.saturating_add(1);
                }
            }
        }
        Self {
            rows,
            refcount,
            sizes,
        }
    }

    fn disk_usage(&self) -> DiskUsage {
        let mut usage = DiskUsage::default();
        let mut counted = BTreeSet::new();
        for row in &self.rows {
            let mut own = row.loose_bytes;
            let mut fresh = row.loose_bytes;
            for (hash, len) in &row.blobs {
                own = own.saturating_add(*len);
                if counted.insert(*hash) {
                    fresh = fresh.saturating_add(*len);
                }
            }
            usage.add(row.pinned, own, fresh);
        }
        usage
    }
}

/// The unpinned rows to evict, least recently used first, until the
/// distinct bytes left are at most `max_bytes`. Evicting a row frees its
/// loose bytes and every blob no remaining row references; a blob another
/// row still shares frees nothing. `0` plans nothing.
fn plan_eviction(
    index: &UsageIndex,
    max_bytes: u64,
    running: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut plan = Vec::new();
    if max_bytes == 0 {
        return plan;
    }
    let mut total = index.disk_usage().total_bytes;
    let mut refcount = index.refcount.clone();
    let mut candidates: Vec<&UsageRow> = index.rows.iter().filter(|row| !row.pinned).collect();
    candidates.sort_by_key(|row| row.last_used);
    for row in candidates {
        if total <= max_bytes {
            break;
        }
        // Asked only of a row that would go, in least-recently-used order.
        if running(&row.id) {
            continue;
        }
        total = total.saturating_sub(row.loose_bytes);
        let distinct: BTreeSet<BlobHash> = row.blobs.iter().map(|(hash, _)| *hash).collect();
        for hash in distinct {
            let Some(count) = refcount.get_mut(&hash) else {
                continue;
            };
            *count = count.saturating_sub(1);
            if *count == 0 {
                total = total.saturating_sub(index.sizes.get(&hash).copied().unwrap_or(0));
            }
        }
        plan.push(row.id.clone());
    }
    plan
}

/// On-disk accounting, with blobs shared between packages counted once in
/// the total and attributed to the first package that references them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskUsage {
    pub total_bytes: u64,
    pub pinned_bytes: u64,
    pub tried_bytes: u64,
}

impl DiskUsage {
    fn add(&mut self, pinned: bool, own: u64, fresh: u64) {
        self.total_bytes = self.total_bytes.saturating_add(fresh);
        if pinned {
            self.pinned_bytes = self.pinned_bytes.saturating_add(own);
        } else {
            self.tried_bytes = self.tried_bytes.saturating_add(own);
        }
    }
}

impl From<TappError> for StoreError {
    fn from(error: TappError) -> Self {
        Self::Tapp(error.to_string())
    }
}

#[cfg(test)]
mod eviction_plan_tests {
    use super::{BlobHash, UsageIndex, UsageRow, plan_eviction};
    use std::collections::BTreeSet;

    fn blob(seed: u8) -> BlobHash {
        BlobHash::of(&[seed])
    }

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn plan_eviction_never_picks_a_running_app() {
        let index = UsageIndex::new(vec![
            row("so.tilo.running", 1, false, &[(1, 1_000)]),
            row("so.tilo.idle", 5, false, &[(2, 1_000)]),
        ]);
        let running = BTreeSet::from(["so.tilo.running".to_string()]);
        assert_eq!(
            plan_eviction(&index, 1, |id| running.contains(id)),
            vec!["so.tilo.idle"],
            "the least recently used app is running, so it stays over the cap"
        );
    }

    fn row(id: &str, last_used: u64, pinned: bool, blobs: &[(u8, u64)]) -> UsageRow {
        UsageRow {
            id: id.into(),
            pinned,
            last_used,
            blobs: blobs
                .iter()
                .map(|(seed, len)| (blob(*seed), *len))
                .collect(),
            loose_bytes: 0,
        }
    }

    #[test]
    fn plan_eviction_counts_shared_blobs_once() {
        // a and b each own 100 bytes and share a 300-byte library.
        let index = UsageIndex::new(vec![
            row("so.tilo.a", 1, false, &[(1, 100), (9, 300)]),
            row("so.tilo.b", 2, false, &[(2, 100), (9, 300)]),
        ]);
        assert_eq!(
            index.disk_usage().total_bytes,
            500,
            "the library counts once"
        );
        assert_eq!(index.disk_usage().tried_bytes, 800, "each row's own bytes");
        // Evicting a frees only its own 100 bytes: b still holds the library.
        assert_eq!(plan_eviction(&index, 450, none), vec!["so.tilo.a"]);
        assert_eq!(plan_eviction(&index, 400, none), vec!["so.tilo.a"]);
        // 400 left after a is still over 399, and b then frees 400.
        assert_eq!(
            plan_eviction(&index, 399, none),
            vec!["so.tilo.a", "so.tilo.b"]
        );
        assert!(
            plan_eviction(&index, 500, none).is_empty(),
            "already under the cap"
        );
        assert!(
            plan_eviction(&index, 500, |id| panic!("{id} was asked about")).is_empty(),
            "under the cap no row is asked whether it runs"
        );
        assert!(
            plan_eviction(&index, 0, none).is_empty(),
            "0 disables the sweep"
        );
    }

    #[test]
    fn plan_eviction_never_picks_pinned() {
        let index = UsageIndex::new(vec![
            row("so.tilo.kept", 0, true, &[(1, 1_000)]),
            row("so.tilo.tried", 5, false, &[(2, 1_000)]),
        ]);
        assert_eq!(
            plan_eviction(&index, 1, none),
            vec!["so.tilo.tried"],
            "the oldest row is pinned, and the cap is never met, yet it stays"
        );
    }

    #[test]
    fn plan_eviction_is_lru() {
        let mut loose = row("so.tilo.legacy", 20, false, &[]);
        loose.loose_bytes = 50;
        let index = UsageIndex::new(vec![
            row("so.tilo.newest", 30, false, &[(1, 100)]),
            row("so.tilo.oldest", 10, false, &[(2, 100)]),
            loose,
        ]);
        assert_eq!(index.disk_usage().total_bytes, 250);
        assert_eq!(plan_eviction(&index, 150, none), vec!["so.tilo.oldest"]);
        assert_eq!(
            plan_eviction(&index, 100, none),
            vec!["so.tilo.oldest", "so.tilo.legacy"],
            "a row without a record frees its whole tree"
        );
        assert_eq!(
            plan_eviction(&index, 1, none),
            vec!["so.tilo.oldest", "so.tilo.legacy", "so.tilo.newest"]
        );
    }
}

#[cfg(test)]
mod image_build_tests {
    use super::image_install_time;

    #[test]
    fn image_install_timestamp_is_fixed_without_changing_user_installs() {
        assert_eq!(
            image_install_time(true, Some("1790502039"), 17).unwrap(),
            1_790_502_039
        );
        assert_eq!(
            image_install_time(true, Some("1790502039"), 99).unwrap(),
            1_790_502_039
        );
        assert_eq!(
            image_install_time(false, Some("1790502039"), 17).unwrap(),
            17
        );
        assert!(image_install_time(true, None, 17).is_err());
        assert!(image_install_time(true, Some("invalid"), 17).is_err());
    }
}

fn close_install_result(
    installed: Option<&Installed>,
    closed: Result<(), StoreError>,
) -> Result<(), StoreError> {
    closed.map_err(|source| match installed {
        Some(installed) => StoreError::CommittedSelection {
            id: installed.package.id.clone(),
            version: installed.package.version.clone(),
            root: installed.package.package_id_hex.clone(),
            source: Box::new(source),
        },
        None => source,
    })
}

#[cfg(test)]
mod install_session_tests {
    use super::{Admission, PackageStore, StoreError, close_install_result};
    use crate::content;
    use tilo_app_package::{Manifest, SigningIdentity, pack};

    fn archive(version: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let identity = SigningIdentity::generate();
        let payload = tempfile::tempdir()?;
        std::fs::create_dir(payload.path().join("bin"))?;
        std::fs::write(payload.path().join("bin/app"), vec![42; 300_000])?;
        let manifest = Manifest::from_toml_str(&format!(
            r#"
[package]
id = "so.tilo.flush-test"
version = "{version}"
kind = "app"
[publisher]
identity = "ed25519:{}"
signature = ""
[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[entrypoint]
bin = "bin/app"
type = "gui"
"#,
            hex::encode(identity.public_key_bytes())
        ))?;
        let packed = pack(payload.path(), &manifest, &identity)?;
        Ok(tilo_app_package::tapp::to_bytes(
            &packed.meta,
            Some(&packed),
        )?)
    }

    #[test]
    fn precommit_flush_failure_preserves_selection_and_reopenable_payload() {
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let root = tempfile::tempdir()?;
            let store = PackageStore::at(root.path());
            let first = archive("1.0.0")?;
            let selected = store.install(
                first.as_slice(),
                |_| {
                    Ok(Admission {
                        pinned: true,
                        ..Admission::default()
                    })
                },
                None,
            )?;
            let before = std::fs::read(store.layout().registry())?;
            let replacement = archive("2.0.0")?;
            let mut barrier_reached = false;
            let result = store.install_with_flush(
                replacement.as_slice(),
                |_| Ok(Admission::default()),
                None,
                |content| {
                    barrier_reached = true;
                    assert_eq!(std::fs::read(store.layout().registry())?, before);
                    content::flush(content)?;
                    Err(StoreError::Content(
                        "injected precommit flush failure".into(),
                    ))
                },
            );
            assert!(barrier_reached);
            assert!(
                matches!(result, Err(StoreError::Content(reason)) if reason == "injected precommit flush failure")
            );
            assert_eq!(std::fs::read(store.layout().registry())?, before);
            drop(store);
            let reopened = PackageStore::at(root.path());
            let registry = reopened.registry()?;
            let row = registry
                .find("so.tilo.flush-test")
                .ok_or("selection disappeared")?;
            assert_eq!(row.package_id(), selected.package.package_id());
            assert_eq!(row.version, "1.0.0");
            assert!(row.pinned);
            assert!(crate::integrity::verify(row, &row.payload_root)?.is_intact());
            let installed =
                reopened.install(replacement.as_slice(), |_| Ok(Admission::default()), None)?;
            assert_eq!(installed.package.version, "2.0.0");
            assert!(installed.package.pinned);
            assert!(
                crate::integrity::verify(&installed.package, &installed.package.payload_root)?
                    .is_intact()
            );
            // A fresh session must see the durable content, not only its previous
            // runtime's cache. An identical reinstall uses all existing blobs.
            let repeated = PackageStore::at(root.path()).install(
                replacement.as_slice(),
                |_| Ok(Admission::default()),
                None,
            )?;
            assert_eq!(repeated.blobs_already_stored, repeated.blobs);
            assert!(
                crate::integrity::verify(&repeated.package, &repeated.package.payload_root)?
                    .is_intact()
            );
            Ok(())
        })();
        assert!(result.is_ok(), "{result:?}");
    }

    /// An app of many files: its blobs are written with one database sync,
    /// not one each.
    #[test]
    fn installing_a_package_syncs_its_blobs_once() {
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let identity = SigningIdentity::generate();
            let payload = tempfile::tempdir()?;
            std::fs::create_dir(payload.path().join("bin"))?;
            std::fs::write(payload.path().join("bin/app"), vec![7; 4096])?;
            std::fs::create_dir(payload.path().join("share"))?;
            for index in 0..40_u32 {
                std::fs::write(
                    payload.path().join(format!("share/{index}.txt")),
                    format!("file {index}").repeat(64),
                )?;
            }
            let manifest = Manifest::from_toml_str(&format!(
                r#"
[package]
id = "so.tilo.sync-test"
version = "1.0.0"
kind = "app"
[publisher]
identity = "ed25519:{}"
signature = ""
[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[entrypoint]
bin = "bin/app"
type = "gui"
"#,
                hex::encode(identity.public_key_bytes())
            ))?;
            let packed = pack(payload.path(), &manifest, &identity)?;
            let archive = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed))?;
            let root = tempfile::tempdir()?;
            let store = PackageStore::at(root.path());
            let mut syncs = None;
            let installed = store.install_with_flush(
                archive.as_slice(),
                |_| Ok(Admission::default()),
                None,
                |content| {
                    syncs = Some(content.write_syncs());
                    content::flush(content)
                },
            )?;
            assert_eq!(installed.blobs, 41);
            assert_eq!(installed.blobs_already_stored, 0);
            assert_eq!(syncs, Some(1), "blob writes before the commit");
            Ok(())
        })();
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn postcommit_gc_failure_keeps_committed_selection() {
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let root = tempfile::tempdir()?;
            let store = PackageStore::at(root.path());
            let first = archive("1.0.0")?;
            let selected = store.install(first.as_slice(), |_| Ok(Admission::default()), None)?;
            std::fs::remove_file(
                store
                    .layout()
                    .meta_dir(selected.package.package_id().ok_or("missing id")?)
                    .join(crate::layout::COLLECTION_FILE),
            )?;
            let replacement = archive("2.0.0")?;
            // Keep an additional registry row referencing the corrupt old record,
            // so GC must reject it even after the replacement retires that tree.
            let mut registry = store.registry()?;
            let mut corrupt = selected.package;
            corrupt.id = "so.tilo.corrupt-record".into();
            registry.upsert(corrupt);
            registry.save(&store.layout().registry())?;
            let result = store.install_with_flush(
                replacement.as_slice(),
                |_| Ok(Admission::default()),
                None,
                content::flush,
            );
            assert!(
                matches!(result, Err(StoreError::CorruptPackage(id, _)) if id == "so.tilo.corrupt-record")
            );
            let reopened = PackageStore::at(root.path());
            let registry = reopened.registry()?;
            let row = registry
                .find("so.tilo.flush-test")
                .ok_or("committed selection disappeared")?;
            assert_eq!(row.version, "2.0.0");
            assert!(crate::integrity::verify(row, &row.payload_root)?.is_intact());
            Ok(())
        })();
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn final_close_failure_reports_the_committed_selection() {
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let root = tempfile::tempdir()?;
            let store = PackageStore::at(root.path());
            let tapp = archive("1.0.0")?;
            let mut actual_close_completed = false;
            let result = store.install_with_hooks(
                tapp.as_slice(),
                |_| Ok(Admission::default()),
                None,
                content::flush,
                |installed, closed| {
                    assert!(
                        closed.is_ok(),
                        "actual store.close must run before fault injection"
                    );
                    actual_close_completed = true;
                    close_install_result(
                        installed,
                        Err(StoreError::Content("injected close RPC failure".into())),
                    )
                },
            );
            assert!(actual_close_completed);
            drop(store);
            let reopened = PackageStore::at(root.path());
            let registry = reopened.registry()?;
            let row = registry
                .find("so.tilo.flush-test")
                .ok_or("committed selection disappeared")?;
            assert!(crate::integrity::verify(row, &row.payload_root)?.is_intact());
            let Err(error) = result else {
                panic!("a final close error must be returned");
            };
            let message = error.to_string();
            assert!(message.contains(&row.id));
            assert!(message.contains(&row.version));
            assert!(message.contains(&row.package_id_hex));
            assert!(message.contains("selected"));
            assert!(message.contains("injected close RPC failure"));
            match error {
                StoreError::CommittedSelection {
                    id,
                    version,
                    root,
                    source,
                } => {
                    assert_eq!(id, row.id);
                    assert_eq!(version, row.version);
                    assert_eq!(root, row.package_id_hex);
                    assert!(
                        matches!(*source, StoreError::Content(reason) if reason == "injected close RPC failure")
                    );
                }
                other => panic!("missing committed selection context: {other}"),
            }
            Ok(())
        })();
        assert!(result.is_ok(), "{result:?}");
    }
}
