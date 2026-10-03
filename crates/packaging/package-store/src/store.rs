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

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tilo_clock::unix_now;

use tilo_app_package::{
    BlobFetcher, BlobHash, BlobSink, HashingReader, PackageId, PackageMeta, Tapp, TappError,
};

use crate::StoreError;
use crate::content::{self, Content};
use crate::integrity;
use crate::layout::{COLLECTION_FILE, Layout, MANIFEST_FILE, SIGNED_ROOT_FILE};
use crate::registry::{InstalledPackage, Registry};

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

/// Unique name for a staging path under `tmp/`.
fn staging_name(id: PackageId, what: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    format!("{}.{what}.{}.{nanos}", id.to_hex(), std::process::id())
}

/// Replace `live` with the fully written `staged` directory.
fn swap_in(staged: &Path, live: &Path, retired: &Path) -> std::io::Result<()> {
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
    if had_live {
        remove_tree(retired)?;
    }
    Ok(())
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
    /// budget, and I/O or content-store errors.
    pub fn install<F>(
        &self,
        tapp: impl Read + Send,
        admit: F,
        fetcher: Option<&dyn BlobFetcher>,
    ) -> Result<Installed, StoreError>
    where
        F: FnOnce(&PackageMeta) -> Result<Admission, String> + Send,
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
        let (meta, admission, blobs_already_stored, blobs_fetched) =
            self.ingest(tapp, admit, fetcher, false)?;
        let blobs = meta.payload_hashes().len();
        let (package, replaced) = self.commit(&meta, admission)?;
        self.collect_garbage_locked()?;
        Ok(Installed {
            package,
            replaced,
            blobs,
            blobs_already_stored,
            blobs_fetched,
        })
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
        })
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
            swap_in(
                &tree,
                &layout.payload(meta.id),
                &tmp.join(staging_name(meta.id, "retired-tree")),
            )?;
            swap_in(
                &record,
                &layout.meta_dir(meta.id),
                &tmp.join(staging_name(meta.id, "retired-pkg")),
            )?;
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
            system: false,
        }
    }

    /// Remove the files of a row that is no longer in the registry.
    pub(crate) fn retire_tree(&self, row: &InstalledPackage) -> Result<(), StoreError> {
        let layout = &self.layout;
        if let Some(id) = layout.materialised_id(&row.payload_root) {
            remove_tree(&layout.payload(id))?;
            remove_tree(&layout.meta_dir(id))?;
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
        let row = registry
            .find(app_id)
            .cloned()
            .ok_or_else(|| StoreError::NotInstalled(app_id.to_string()))?;
        if row.pinned && !force {
            return Err(StoreError::Pinned(app_id.to_string()));
        }
        registry.remove(app_id);
        registry.save(&self.layout.registry())?;
        self.retire_tree(&row)?;
        let evicted_blobs = self.collect_garbage_locked()?;
        Ok(Removed {
            package: Some(row),
            evicted_blobs,
        })
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
    fn referenced_blobs(&self) -> Result<BTreeSet<BlobHash>, StoreError> {
        let mut live = BTreeSet::new();
        for row in self.registry()?.entries() {
            if self.layout.materialised_id(&row.payload_root).is_none() {
                // Not backed by the content store (a tree kept from before
                // it existed); it references nothing here.
                continue;
            }
            let record = integrity::read_record(
                &integrity::record_dir(row),
                &row.id,
                &row.package_id_hex,
            )
            .map_err(|error| StoreError::CorruptPackage(row.id.clone(), error.to_string()))?;
            live.extend(record.collection.entries.values().copied());
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
            let dead: Vec<BlobHash> = content
                .hashes()?
                .into_iter()
                .filter(|hash| !live.contains(hash))
                .collect();
            content.evict(&dead)?;
            Ok(dead.len())
        })
    }

    /// Distinct payload bytes each installed package references, and the
    /// total with shared blobs counted once.
    ///
    /// # Errors
    ///
    /// Returns an error if the registry cannot be read.
    pub fn disk_usage(&self) -> Result<DiskUsage, StoreError> {
        let mut usage = DiskUsage::default();
        let mut counted = BTreeSet::new();
        for row in self.registry()?.entries() {
            let Ok(record) =
                integrity::read_record(&integrity::record_dir(row), &row.id, &row.package_id_hex)
            else {
                let size = crate::tree_size(&row.payload_root);
                usage.add(row.pinned, size, size);
                continue;
            };
            let mut own = 0u64;
            let mut fresh = 0u64;
            for (rel, hash) in &record.collection.entries {
                let len = std::fs::metadata(row.payload_root.join(rel)).map_or(0, |m| m.len());
                own = own.saturating_add(len);
                if counted.insert(*hash) {
                    fresh = fresh.saturating_add(len);
                }
            }
            usage.add(row.pinned, own, fresh);
        }
        Ok(usage)
    }

    /// Evict unpinned packages, least recently used first, until the
    /// store's distinct payload bytes are at most `max_bytes`. `0` disables
    /// the sweep. Returns the app ids removed.
    ///
    /// # Errors
    ///
    /// Returns an error if usage cannot be measured or a removal fails.
    pub fn sweep(&self, max_bytes: u64) -> Result<Vec<String>, StoreError> {
        if max_bytes == 0 {
            return Ok(Vec::new());
        }
        let mut candidates: Vec<InstalledPackage> = self
            .registry()?
            .into_entries()
            .into_iter()
            .filter(|row| !row.pinned)
            .collect();
        candidates.sort_by_key(|row| row.last_used);
        let mut removed = Vec::new();
        for row in candidates {
            if self.disk_usage()?.total_bytes <= max_bytes {
                break;
            }
            match self.remove(&row.id, false) {
                Ok(_) => removed.push(row.id),
                // Pinned or removed concurrently since the snapshot.
                Err(StoreError::Pinned(_) | StoreError::NotInstalled(_)) => {}
                Err(other) => return Err(other),
            }
        }
        Ok(removed)
    }
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
