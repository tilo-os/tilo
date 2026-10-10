//! Environment images: `env-runtime` and `tool` packages as read-only trees.
//!
//! An environment's `/usr` and `/etc` are a stack of images. The supervisor
//! names each by its [`PackageId`] and turns it into a directory with
//! [`materialized_tree`] (or [`local_materialized_tree`] for overlay lowers):
//!
//! 1. The system stores the OS or the host package shipped (read-only, so
//!    the first environment opens offline) are looked in first.
//! 2. Then the user's store.
//! 3. Otherwise the image is fetched from the [`Store`]'s sources in turn
//!    (an [`ImageShelf`], the registry), checked to be the package asked
//!    for, admitted by the [`ImageTrust`], and materialised into the user's
//!    store.
//!
//! Every use admits the image again, wherever it was found: trust is a
//! property of the image's publisher *now*, so an image a device signed
//! stops opening the moment the user removes that device.
//!
//! A materialised image is a plain directory tree with read-only files at
//! `pkgs/<package-id>/`, which bwrap stacks as an overlay lower directory
//! without any mount privilege. Images have no registry row: nothing
//! launches them, and two versions of one image id coexist for as long as
//! environments use them.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use tilo_app_package::trust::{RevocationList, TrustedAttestor, TrustedAttestors};
use tilo_app_package::{
    BlobFetcher, BlobHash, BlobSink, Kind, PackageFetcher, PackageId, PackageMeta, Verification,
};
use tilo_user_identity::{CertifiedFleet, DeviceStanding};

use crate::StoreError;
use crate::catalog::SystemStore;
use crate::layout::{COLLECTION_FILE, Layout, MANIFEST_FILE, SIGNED_ROOT_FILE};
use crate::store::{Admission, PackageStore};

/// File, beside the system store's registry, naming the publishers whose
/// images this installation trusts, one hex Ed25519 key per line. The OS
/// image and the host package write the publisher of the runtime they ship.
pub const SYSTEM_IMAGE_PUBLISHERS_FILE: &str = "image-publishers";

/// File, in a system image's metadata directory, naming the host files that
/// carry bytes the image's tree leaves out: one `relative-path<TAB>/host/path`
/// line each (a third field, the SHA-256 the image build checked, is ignored
/// here). The OS image ships its env-runtime this way, because most of that
/// userland is byte-identical to the host's own `/usr`, which a later OCI
/// layer could otherwise only carry a second time (TIL-735). Nothing is
/// trusted for being named here: every byte is checked against the image's
/// signed hash as it is imported, exactly as a byte in the tree is.
pub const HOST_SOURCES_FILE: &str = "host-sources";

/// Why an image could not be turned into a tree.
#[derive(Debug, thiserror::Error)]
pub enum MaterializeError {
    #[error("image {0} is not in any store and no source offers it")]
    NotFound(PackageId),
    #[error("package {id} is a {kind} package, not an environment image")]
    NotAnImage { id: PackageId, kind: Kind },
    #[error("image {id} is signed by ed25519:{publisher}, which is not a trusted image publisher")]
    Untrusted { id: PackageId, publisher: String },
    #[error("image {id} is signed by revoked publisher ed25519:{publisher}: {reason}")]
    Revoked {
        id: PackageId,
        publisher: String,
        reason: String,
    },
    #[error(
        "image {id} is signed by ed25519:{publisher}; whether that is one of your devices \
         could not be checked: {detail}"
    )]
    FleetUnavailable {
        id: PackageId,
        publisher: String,
        detail: String,
    },
    #[error("image {id} has an invalid manifest: {detail}")]
    Invalid { id: PackageId, detail: String },
    #[error("image {id} has an invalid signature: {detail}")]
    BadSignature { id: PackageId, detail: String },
    #[error("fetching image {id}: {detail}")]
    Fetch { id: PackageId, detail: String },
    #[error("{0} is not a tool image")]
    NotATool(PackageId),
    #[error("tool {tool} was built for env-runtime {built_for}, not {runtime}")]
    ToolRuntimeMismatch {
        tool: PackageId,
        built_for: PackageId,
        runtime: PackageId,
    },
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Whose images are accepted.
///
/// The same trust model as application packages: the signature must
/// verify, a revoked publisher is refused, and a trusted attestor's
/// signature vouches for a package. Images are stricter than applications
/// in one way — an image from an unknown publisher is refused outright
/// rather than installed with a warning, because every environment that
/// stacks it runs its binaries. Trusted publishers are the ones the
/// installation shipped ([`SYSTEM_IMAGE_PUBLISHERS_FILE`]) plus whatever the
/// caller adds.
///
/// # The user's devices
///
/// A store that serves one person's environments also trusts that person's
/// devices ([`ImageTrust::with_fleet`]): an image signed by a device key the
/// user's identity certified is admitted, so a tool one of their devices or
/// their relay built is fetched on the others instead of rebuilt. The rule,
/// asked of the [`FleetSource`] at every admission rather than remembered:
///
/// - the publisher holds a certificate from the user's identity that is
///   valid now (signed by the identity root, issued, not expired);
/// - the identity never revoked the publisher's key. A revocation refuses
///   every image the key signed, including ones signed before it and ones
///   already materialised on this device, because a lost key can back-date
///   what it signs. The user's remaining devices rebuild what is refused;
/// - a relay counts as one of the user's devices exactly when it holds such
///   a certificate (a home peer is enrolled with one). A relay without one is
///   anyone: its images need a publisher trusted outright, the CI catalog
///   key.
///
/// Only the fleet's own identity speaks for it, so another person's devices
/// are never trusted this way, and the fleet is the user's own: it belongs
/// on the trust of the store serving their environments, not a shared one.
/// A key trusted outright or vouched for by an attestor does not consult the
/// fleet.
///
/// Images found in a read-only system store are trusted by being there;
/// their signature is still checked and a revoked publisher (or a device the
/// fleet revoked) still refused.
#[derive(Clone, Debug, Default)]
pub struct ImageTrust {
    publishers: Vec<[u8; 32]>,
    attestors: Vec<TrustedAttestor>,
    revoked: RevocationList,
    fleet: Option<Arc<dyn FleetSource>>,
}

/// Where the devices of the user a store serves are read from.
pub trait FleetSource: Send + Sync + std::fmt::Debug {
    /// The user's fleet as it stands now. Asked at every admission that
    /// needs it, so a device removed since is refused at once.
    ///
    /// # Errors
    ///
    /// Returns why the fleet cannot be read. An image only the fleet could
    /// vouch for is then refused.
    fn fleet(&self) -> Result<CertifiedFleet, String>;
}

impl FleetSource for CertifiedFleet {
    fn fleet(&self) -> Result<CertifiedFleet, String> {
        Ok(self.clone())
    }
}

impl ImageTrust {
    /// Trust nothing beyond the system stores' contents.
    pub fn new() -> Self {
        Self::default()
    }

    /// The installation's trust configuration: the user's trusted attestors
    /// and revocation list (the files `tilo-install trust` and
    /// `tilo-install revoke` keep) and the publishers the system store
    /// names.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Refused`] when a configuration file exists but
    /// does not parse; a broken trust file is never read as "trust nothing
    /// revoked".
    pub fn from_env(system: Option<&SystemStore>) -> Result<Self, StoreError> {
        let refused = |error: tilo_app_package::TrustLoadError| {
            StoreError::Refused(format!("image trust configuration: {error}"))
        };
        let attestors = match tilo_app_package::default_trusted_attestors_path() {
            Some(path) => TrustedAttestors::load(&path).map_err(refused)?,
            None => TrustedAttestors::empty(),
        };
        let revoked = match tilo_app_package::default_revoked_keys_path() {
            Some(path) => RevocationList::load(&path).map_err(refused)?,
            None => RevocationList::empty(),
        };
        let mut trust = Self {
            publishers: Vec::new(),
            attestors: attestors.as_slice().to_vec(),
            revoked,
            fleet: None,
        };
        if let Some(system) = system {
            trust.publishers =
                read_publishers(&system.data_dir.join(SYSTEM_IMAGE_PUBLISHERS_FILE))?;
        }
        Ok(trust)
    }

    /// Also trust images `publisher` signed.
    #[must_use]
    pub fn with_publisher(mut self, publisher: [u8; 32]) -> Self {
        if !self.publishers.contains(&publisher) {
            self.publishers.push(publisher);
        }
        self
    }

    /// Also trust images signed by the devices of the user `fleet` reads,
    /// as [`ImageTrust`] describes.
    #[must_use]
    pub fn with_fleet(mut self, fleet: impl FleetSource + 'static) -> Self {
        self.fleet = Some(Arc::new(fleet));
        self
    }

    /// Replace the revocation list.
    #[must_use]
    pub fn with_revoked(mut self, revoked: RevocationList) -> Self {
        self.revoked = revoked;
        self
    }

    /// The publishers trusted outright.
    pub fn publishers(&self) -> &[[u8; 32]] {
        &self.publishers
    }

    /// Admit `meta` into a writable store: an image whose manifest is sound
    /// and whose signature verifies, from a publisher that is trusted or
    /// attested and not revoked. Returns the attestor id, empty when a
    /// trusted publisher signed it.
    ///
    /// # Errors
    ///
    /// Returns the reason the image is refused.
    pub fn check(&self, meta: &PackageMeta) -> Result<String, MaterializeError> {
        self.admit(meta, false)
    }

    /// Admit `meta`: an image whose signature verifies, from a publisher not
    /// revoked, and — unless `placed` says it came from a system store —
    /// trusted or attested.
    fn admit(&self, meta: &PackageMeta, placed: bool) -> Result<String, MaterializeError> {
        let id = meta.id;
        let kind = meta.manifest.package.kind;
        if !kind.is_image() {
            return Err(MaterializeError::NotAnImage { id, kind });
        }
        // A signature says who wrote the manifest, not that it is sound: a
        // tool that names no runtime could be stacked on anything.
        tilo_app_package::validate_manifest(&meta.manifest).map_err(|error| {
            MaterializeError::Invalid {
                id,
                detail: error.to_string(),
            }
        })?;
        let publisher = || hex::encode(meta.publisher());
        match tilo_app_package::verify_with_trust(
            meta,
            &self.publishers,
            &self.attestors,
            &self.revoked,
        ) {
            Verification::Trusted { .. } => Ok(String::new()),
            Verification::AttestedBy { attestor, .. } => Ok(attestor),
            Verification::UntrustedPublisher { identity, .. } => {
                match self.fleet_standing(&identity) {
                    Some(Ok(DeviceStanding::Certified)) => Ok(String::new()),
                    Some(Ok(DeviceStanding::Revoked(reason))) => Err(MaterializeError::Revoked {
                        id,
                        publisher: publisher(),
                        reason: format!("the device was removed from your devices ({reason:?})"),
                    }),
                    _ if placed => Ok(String::new()),
                    Some(Err(detail)) => Err(MaterializeError::FleetUnavailable {
                        id,
                        publisher: publisher(),
                        detail,
                    }),
                    Some(Ok(DeviceStanding::Uncertified)) | None => {
                        Err(MaterializeError::Untrusted {
                            id,
                            publisher: publisher(),
                        })
                    }
                }
            }
            Verification::Revoked { reason, .. } => Err(MaterializeError::Revoked {
                id,
                publisher: publisher(),
                reason,
            }),
            other => Err(MaterializeError::BadSignature {
                id,
                detail: format!("{other:?}"),
            }),
        }
    }

    /// Where `publisher` stands with the user's fleet now; `None` without a
    /// fleet.
    fn fleet_standing(&self, publisher: &[u8; 32]) -> Option<Result<DeviceStanding, String>> {
        let source = self.fleet.as_deref()?;
        let now_ns = tilo_clock::unix_now_ns();
        Some(
            source
                .fleet()
                .map(|fleet| fleet.standing(publisher, now_ns)),
        )
    }
}

/// Read a publishers file: one hex key per line, `#` comments. A missing
/// file names none.
fn read_publishers(path: &std::path::Path) -> Result<Vec<[u8; 32]>, StoreError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut keys = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let mut key = [0u8; 32];
        hex::decode_to_slice(line, &mut key).map_err(|_| {
            StoreError::Refused(format!(
                "{}: {line:?} is not a 64-character hex Ed25519 key",
                path.display()
            ))
        })?;
        keys.push(key);
    }
    Ok(keys)
}

/// The image stores one device reads and writes: the user's store, where
/// fetched images land, the read-only system stores searched before it,
/// the trust that admits an image, and the sources missing ones come from.
#[derive(Clone)]
pub struct Store {
    user: PackageStore,
    system: Vec<Layout>,
    trust: ImageTrust,
    sources: Vec<Source>,
}

/// Somewhere missing images are fetched from, and what to call it.
#[derive(Clone)]
struct Source {
    name: String,
    fetcher: Arc<dyn PackageFetcher + Send + Sync>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sources: Vec<&str> = self.sources.iter().map(|s| s.name.as_str()).collect();
        f.debug_struct("Store")
            .field("user", &self.user)
            .field("system", &self.system)
            .field("trust", &self.trust)
            .field("sources", &sources)
            .finish()
    }
}

impl Store {
    /// Images in `user` only, trusting nothing, fetching nothing.
    pub fn new(user: PackageStore) -> Self {
        Self {
            user,
            system: Vec::new(),
            trust: ImageTrust::new(),
            sources: Vec::new(),
        }
    }

    /// The device's stores as the environment names them: the user's store,
    /// the system store and its payload root, and [`ImageTrust::from_env`].
    /// Add sources with [`Store::with_shelf`] and [`Store::with_fetcher`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NoHome`] when no user store can be located, or
    /// a trust configuration error.
    pub fn from_env() -> Result<Self, StoreError> {
        let system = SystemStore::from_env();
        let trust = ImageTrust::from_env(system.as_ref())?;
        let mut store = Self::new(PackageStore::from_env()?).with_trust(trust);
        if let Some(system) = system {
            store = store
                .with_system(Layout::new(system.data_dir))
                .with_system(Layout::new(system.payload_dir));
        }
        Ok(store)
    }

    /// Also search the read-only store at `layout`, before the user's.
    #[must_use]
    pub fn with_system(mut self, layout: Layout) -> Self {
        if !self.system.contains(&layout) {
            self.system.push(layout);
        }
        self
    }

    #[must_use]
    pub fn with_trust(mut self, trust: ImageTrust) -> Self {
        self.trust = trust;
        self
    }

    /// Also fetch images no store holds from `fetcher` — normally the
    /// registry, `app_registry::HttpBlobs` — after the sources added before.
    #[must_use]
    pub fn with_fetcher(self, fetcher: impl PackageFetcher + Send + Sync + 'static) -> Self {
        self.with_source("registry".into(), Arc::new(fetcher))
    }

    /// Also fetch images no store holds from `shared`, after the sources
    /// added before.
    #[must_use]
    pub fn with_shelf(self, shared: ImageShelf) -> Self {
        let name = format!("shelf {}", shared.dir.display());
        self.with_source(name, Arc::new(shared))
    }

    fn with_source(mut self, name: String, fetcher: Arc<dyn PackageFetcher + Send + Sync>) -> Self {
        self.sources.push(Source { name, fetcher });
        self
    }

    pub fn user(&self) -> &PackageStore {
        &self.user
    }

    pub fn trust(&self) -> &ImageTrust {
        &self.trust
    }

    /// Every image held on this device, system stores first, each once.
    ///
    /// # Errors
    ///
    /// Returns an error when a store cannot be read.
    pub fn images(&self) -> Result<Vec<PackageMeta>, StoreError> {
        let mut out: Vec<PackageMeta> = Vec::new();
        for layout in self.system.iter().chain([self.user.layout()]) {
            for meta in list_images(layout)? {
                if !out.iter().any(|seen| seen.id == meta.id) {
                    out.push(meta);
                }
            }
        }
        Ok(out)
    }

    /// The image `id`'s metadata and the store holding it, without
    /// fetching.
    fn local(&self, id: PackageId) -> Result<Option<(PackageMeta, &Layout, bool)>, StoreError> {
        for layout in &self.system {
            if let Some(meta) = read_image(layout, id)? {
                return Ok(Some((meta, layout, true)));
            }
        }
        Ok(read_image(self.user.layout(), id)?.map(|meta| (meta, self.user.layout(), false)))
    }

    /// The admitted metadata of image `id`, its tree and, for a system image
    /// shipped sparse, the host files holding the rest of its bytes; fetching
    /// and materialising it into the user's store when no store holds it.
    fn resolve(&self, id: PackageId) -> Result<Resolved, MaterializeError> {
        if let Some((meta, layout, placed)) = self.local(id)? {
            self.trust.admit(&meta, placed)?;
            let host = if self.system.contains(layout) {
                read_host_sources(layout, &meta)?
            } else {
                None
            };
            return Ok(Resolved {
                tree: layout.payload(id),
                meta,
                host,
            });
        }
        // The first source that has the package serves it; one that cannot
        // be asked does not hide the ones after it.
        let mut unreachable = None;
        for source in &self.sources {
            match source.fetcher.fetch_package(&id) {
                Ok(Some(bytes)) => {
                    let (meta, tree) = self.install(id, source, &bytes)?;
                    return Ok(Resolved {
                        meta,
                        tree,
                        host: None,
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    unreachable.get_or_insert_with(|| MaterializeError::Fetch {
                        id,
                        detail: format!("{}: {error}", source.name),
                    });
                }
            }
        }
        Err(unreachable.unwrap_or(MaterializeError::NotFound(id)))
    }

    /// Admit the package `bytes` that `source` served for `id`, and
    /// materialise it into the user's store.
    fn install(
        &self,
        id: PackageId,
        source: &Source,
        bytes: &[u8],
    ) -> Result<(PackageMeta, PathBuf), MaterializeError> {
        let fetch = |detail: String| MaterializeError::Fetch { id, detail };
        let refusal = Mutex::new(None);
        let admitted = self.user.install_image(
            bytes,
            |meta| {
                let verdict = if meta.id == id {
                    self.trust.admit(meta, false)
                } else {
                    Err(fetch(format!("{} served package {}", source.name, meta.id)))
                };
                match verdict {
                    Ok(attested_by) => Ok(Admission {
                        pinned: false,
                        attested_by,
                        source: source.name.clone(),
                        held: false,
                    }),
                    Err(error) => {
                        let reason = error.to_string();
                        *refusal
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                        Err(reason)
                    }
                }
            },
            Some(&*source.fetcher),
        );
        let meta = match admitted {
            Ok(meta) => meta,
            Err(error) => {
                let refused = refusal
                    .into_inner()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                return Err(refused.unwrap_or(MaterializeError::Store(error)));
            }
        };
        Ok((meta, self.user.layout().payload(id)))
    }
}

/// What [`Store::resolve`] found for an image.
struct Resolved {
    meta: PackageMeta,
    tree: PathBuf,
    /// Set for a system image whose tree leaves bytes to host files
    /// ([`HOST_SOURCES_FILE`]); such a tree is never handed out as is.
    host: Option<HostSources>,
}

/// The host files a sparse system image's tree points at, by the tree path
/// they stand in for.
#[derive(Debug, Default)]
struct HostSources(BTreeMap<String, PathBuf>);

/// The [`HOST_SOURCES_FILE`] of system image `meta` in `layout`, if it has
/// one. Every line must name one of the image's signed files, once, and an
/// absolute host path without `..`.
fn read_host_sources(
    layout: &Layout,
    meta: &PackageMeta,
) -> Result<Option<HostSources>, StoreError> {
    let path = layout.meta_dir(meta.id).join(HOST_SOURCES_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let corrupt = |number: usize, detail: &str| {
        StoreError::CorruptPackage(
            meta.id.to_string(),
            format!("{HOST_SOURCES_FILE} line {number}: {detail}"),
        )
    };
    let mut sources = BTreeMap::new();
    for (number, line) in (1usize..).zip(text.lines()) {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let (Some(relative), Some(host)) = (fields.next(), fields.next()) else {
            return Err(corrupt(number, "expected a tree path and a host path"));
        };
        if !meta.collection.entries.contains_key(relative) {
            return Err(corrupt(number, "names no signed file of the image"));
        }
        let host = Path::new(host);
        let plain = host
            .components()
            .skip(1)
            .all(|component| matches!(component, Component::Normal(_)));
        if !host.is_absolute() || !plain || host.components().count() < 2 {
            return Err(corrupt(
                number,
                "the host path is not a plain absolute path",
            ));
        }
        if sources
            .insert(relative.to_owned(), host.to_path_buf())
            .is_some()
        {
            return Err(corrupt(number, "names a tree path twice"));
        }
    }
    Ok(Some(HostSources(sources)))
}

/// Largest package a shelf hands over. A shelved image is whole, payload
/// included, and is read into memory to be installed.
const MAX_SHELVED_IMAGE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// A directory of whole images, `<dir>/<hex id>.tapp`, where the user's
/// devices leave the images they built for the others to fetch.
///
/// Nothing on a shelf is trusted: a [`Store`] checks that a package it
/// serves is the one asked for and admits it by its [`ImageTrust`], so a
/// shelf may sit anywhere the user's devices can all read, such as their
/// synced files.
#[derive(Clone, Debug)]
pub struct ImageShelf {
    dir: PathBuf,
}

impl ImageShelf {
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// Where image `id` sits on the shelf.
    pub fn path(&self, id: &PackageId) -> PathBuf {
        self.dir.join(format!("{}.tapp", id.to_hex()))
    }

    /// Put image `id`, which `store` holds, on the shelf as a whole
    /// package. An image already there is left as it is.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInstalled`] when `store` does not hold the
    /// image, or an I/O or content-store error.
    pub fn put(&self, store: &PackageStore, id: PackageId) -> Result<PathBuf, StoreError> {
        let path = self.path(&id);
        if path.is_file() {
            return Ok(path);
        }
        std::fs::create_dir_all(&self.dir)?;
        let staged = tempfile::Builder::new()
            .prefix(".partial-")
            .tempfile_in(&self.dir)?;
        let written = store.export_image(id, std::io::BufWriter::new(staged.as_file()))?;
        written
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?
            .sync_all()?;
        staged.persist(&path).map_err(|error| error.error)?;
        Ok(path)
    }
}

impl PackageFetcher for ImageShelf {
    fn fetch_package(&self, id: &PackageId) -> std::io::Result<Option<Vec<u8>>> {
        let path = self.path(id);
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take(MAX_SHELVED_IMAGE_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SHELVED_IMAGE_BYTES {
            return Err(std::io::Error::other(format!(
                "{} is larger than an image may be",
                path.display()
            )));
        }
        Ok(Some(bytes))
    }
}

impl BlobFetcher for ImageShelf {
    fn fetch(&self, hashes: &[BlobHash], _sink: &mut dyn BlobSink) -> std::io::Result<()> {
        Err(std::io::Error::other(format!(
            "a shelved image carries its whole payload, yet {} blob(s) are missing from it",
            hashes.len()
        )))
    }
}

/// The read-only root of image `id`: fetched (from the [`Store`]'s sources
/// when no store holds it), verified, and materialised.
///
/// The returned directory and everything in it must be treated as
/// read-only; it is shared by every environment that stacks the image.
///
/// # Errors
///
/// Returns [`MaterializeError::NotFound`] when no store holds the image and
/// no source offers it, [`MaterializeError::NotAnImage`] for a package of
/// another kind, a trust error for an image whose publisher is not trusted
/// or is revoked, or a fetch or store error.
pub fn materialized_tree(store: &Store, id: &PackageId) -> Result<PathBuf, MaterializeError> {
    let resolved = store.resolve(*id)?;
    if resolved.host.is_some() {
        // A sparse system tree is not the image: rebuild it whole, from the
        // tree and the host files, each byte checked against the signed hash.
        return local_materialized_tree(store, id);
    }
    Ok(resolved.tree)
}

/// Resolve an admitted image on the user's filesystem, suitable as an overlay lower.
/// System image trees can themselves sit on bootc/system-extension overlays;
/// another overlay over those exceeds the kernel's stacking limit. Rebuild the
/// same signed tree from verified bytes in the existing content store instead.
/// Every call checks current publisher trust before consulting this cache.
///
/// # Errors
/// Returns the ordinary image resolution errors, or an integrity/store error.
pub fn local_materialized_tree(store: &Store, id: &PackageId) -> Result<PathBuf, MaterializeError> {
    let Resolved {
        meta,
        tree: source,
        host,
    } = store.resolve(*id)?;
    let local = store.user.layout().payload(*id);
    let _lock = store.user.lock()?;
    if read_image(store.user.layout(), *id)?.is_some() {
        verify_local_tree(&meta, &local)?;
        return Ok(local);
    }
    let layout = store.user.layout();
    crate::content::session(&layout.content(), &layout.tmp(), |content| {
        // An image is thousands of files: one database sync for all of them,
        // before the tree is materialised and published, not one per file.
        content.deferred(|| import_image_payload(content, &meta, &source, host.as_ref()))?;
        store.user.materialise(content, &meta)
    })?;
    verify_local_tree(&meta, &local)?;
    Ok(local)
}

/// Ingest every file of the image tree at `source` into `content`, checking
/// each against the hash `meta` signed. A file the tree leaves out is read
/// from the host file `host` names for it.
fn import_image_payload(
    content: &crate::content::Content<'_>,
    meta: &PackageMeta,
    source: &std::path::Path,
    host: Option<&HostSources>,
) -> Result<(), StoreError> {
    for (relative, expected) in &meta.collection.entries {
        let mut path = source.join(relative);
        if let Some(stand_in) = host.and_then(|host| host.0.get(relative.as_str()))
            && matches!(std::fs::symlink_metadata(&path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            path.clone_from(stand_in);
        }
        if !std::fs::symlink_metadata(&path)?.is_file() {
            return Err(StoreError::Payload(format!(
                "{relative}: image payload is not a regular file"
            )));
        }
        let mut file = std::fs::File::open(&path)?;
        let len = file.metadata()?.len();
        // A previous interrupted import may already hold this signed blob.
        // Cache completeness selects reuse; it does not verify the source.
        let actual = if content.len(expected)? == Some(len) {
            BlobHash::of_reader(&mut file)?.0
        } else {
            content.ingest(len, &mut file)?
        };
        if actual != *expected {
            return Err(StoreError::Payload(format!(
                "{relative}: image payload does not match its signed hash"
            )));
        }
    }
    Ok(())
}

fn verify_local_tree(meta: &PackageMeta, root: &std::path::Path) -> Result<(), StoreError> {
    for (relative, expected) in &meta.collection.entries {
        let path = root.join(relative);
        if !std::fs::symlink_metadata(&path)?.is_file() {
            return Err(StoreError::Payload(format!(
                "{relative}: cached image payload is not a regular file"
            )));
        }
        let actual = BlobHash::of_reader(&mut std::fs::File::open(path)?)?.0;
        if actual != *expected {
            return Err(StoreError::Payload(format!(
                "{relative}: cached image payload does not match its signed hash"
            )));
        }
    }
    let tree = tilo_app_package::TreeSpec::from_bytes(&std::fs::read(
        root.join(tilo_app_package::TREE_FILE),
    )?)
    .map_err(|error| StoreError::Payload(error.to_string()))?;
    tree.check(&meta.collection)
        .map_err(|error| StoreError::Payload(error.to_string()))?;
    if tilo_app_package::TreeSpec::scan(root)
        .map_err(|error| StoreError::Payload(error.to_string()))?
        != tree
    {
        return Err(StoreError::Payload(
            "cached image links, executable modes or directories differ from the signed tree"
                .into(),
        ));
    }
    verify_local_entries(root, root, meta)
}

fn verify_local_entries(
    root: &std::path::Path,
    dir: &std::path::Path,
    meta: &PackageMeta,
) -> Result<(), StoreError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            verify_local_entries(root, &entry.path(), meta)?;
        } else if kind.is_file() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|error| StoreError::Payload(error.to_string()))?
                .to_str()
                .ok_or_else(|| StoreError::Payload("cached image has a non-UTF-8 path".into()))?;
            if !meta.collection.entries.contains_key(relative) {
                return Err(StoreError::Payload(format!(
                    "{relative}: unsigned file in cached image"
                )));
            }
        }
    }
    Ok(())
}

/// The `env-runtime` the tool image `tool` was built for, materialising
/// the tool's metadata as [`materialized_tree`] does.
///
/// # Errors
///
/// Returns [`MaterializeError::NotATool`] when `tool` is not a tool image,
/// or what [`materialized_tree`] returns.
pub fn tool_runtime(store: &Store, tool: &PackageId) -> Result<PackageId, MaterializeError> {
    let Resolved { meta, .. } = store.resolve(*tool)?;
    match (&meta.manifest.package.kind, &meta.manifest.image) {
        (Kind::Tool, Some(image)) => image.runtime.ok_or(MaterializeError::NotATool(*tool)),
        _ => Err(MaterializeError::NotATool(*tool)),
    }
}

/// Check that the tool image `tool` may be stacked on `runtime`: it was
/// built for exactly that `env-runtime`. A tool is never mounted on any
/// other runtime.
///
/// # Errors
///
/// Returns [`MaterializeError::ToolRuntimeMismatch`] when it was built for
/// another runtime, or what [`tool_runtime`] returns.
pub fn check_tool_runtime(
    store: &Store,
    tool: &PackageId,
    runtime: &PackageId,
) -> Result<(), MaterializeError> {
    let built_for = tool_runtime(store, tool)?;
    if built_for == *runtime {
        Ok(())
    } else {
        Err(MaterializeError::ToolRuntimeMismatch {
            tool: *tool,
            built_for,
            runtime: *runtime,
        })
    }
}

/// The image `id` in `layout`, when its signed record and its tree are both
/// in place. A record that exists but does not form package `id` is an
/// error: a store never holds a record under another package's name.
pub(crate) fn read_image(
    layout: &Layout,
    id: PackageId,
) -> Result<Option<PackageMeta>, StoreError> {
    let dir = layout.meta_dir(id);
    let read = |name: &str| std::fs::read(dir.join(name));
    let signed_root = match read(SIGNED_ROOT_FILE) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let meta = PackageMeta::from_parts(
        id,
        &signed_root,
        read(MANIFEST_FILE)?,
        read(COLLECTION_FILE)?,
    )
    .map_err(|error| StoreError::CorruptPackage(id.to_string(), error.to_string()))?;
    if !meta.manifest.package.kind.is_image() || !layout.payload(id).is_dir() {
        return Ok(None);
    }
    Ok(Some(meta))
}

/// Every complete image in `layout`. A record that cannot be read is
/// skipped: listing serves garbage collection and discovery, and
/// [`materialized_tree`] reports a broken record of the image it is asked
/// for.
pub(crate) fn list_images(layout: &Layout) -> Result<Vec<PackageMeta>, StoreError> {
    let entries = match std::fs::read_dir(layout.packages()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut images = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".pkg"))
            .and_then(crate::layout::parse_id_hex)
        else {
            continue;
        };
        if let Ok(Some(meta)) = read_image(layout, id) {
            images.push(meta);
        }
    }
    images.sort_by_key(|meta| meta.id);
    Ok(images)
}
