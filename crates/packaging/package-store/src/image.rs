//! Environment images: `env-runtime` and `tool` packages as read-only trees.
//!
//! An environment's `/usr` and `/etc` are a stack of images. The supervisor
//! names each by its [`PackageId`] and turns it into a directory with
//! [`materialized_tree`], the only way an image ref becomes a path:
//!
//! 1. The system stores the OS or the host package shipped (read-only, so
//!    the first environment opens offline) are looked in first.
//! 2. Then the user's store.
//! 3. Otherwise the image is fetched from the [`Store`]'s registry, checked
//!    to be the package asked for, admitted by the [`ImageTrust`], and
//!    materialised into the user's store.
//!
//! A materialised image is a plain directory tree with read-only files at
//! `pkgs/<package-id>/`, which bwrap stacks as an overlay lower directory
//! without any mount privilege. Images have no registry row: nothing
//! launches them, and two versions of one image id coexist for as long as
//! environments use them.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tilo_app_package::trust::{RevocationList, TrustedAttestor, TrustedAttestors};
use tilo_app_package::{Kind, PackageFetcher, PackageId, PackageMeta, Verification};

use crate::StoreError;
use crate::catalog::SystemStore;
use crate::layout::{COLLECTION_FILE, Layout, MANIFEST_FILE, SIGNED_ROOT_FILE};
use crate::store::{Admission, PackageStore};

/// File, beside the system store's registry, naming the publishers whose
/// images this installation trusts, one hex Ed25519 key per line. The OS
/// image and the host package write the publisher of the runtime they ship.
pub const SYSTEM_IMAGE_PUBLISHERS_FILE: &str = "image-publishers";

/// Why an image could not be turned into a tree.
#[derive(Debug, thiserror::Error)]
pub enum MaterializeError {
    #[error("image {0} is not in any store and no registry offers it")]
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
/// caller adds: the supervisor adds the user's own device keys, so tool
/// images the user's devices built and signed are accepted on all of them.
///
/// Images found in a read-only system store are trusted by being there;
/// their signature is still checked and a revoked publisher still refused.
#[derive(Clone, Debug, Default)]
pub struct ImageTrust {
    publishers: Vec<[u8; 32]>,
    attestors: Vec<TrustedAttestor>,
    revoked: RevocationList,
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

    /// Replace the trusted attestors.
    #[must_use]
    pub fn with_attestors(mut self, attestors: &TrustedAttestors) -> Self {
        self.attestors = attestors.as_slice().to_vec();
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
            Verification::UntrustedPublisher { .. } if placed => Ok(String::new()),
            Verification::UntrustedPublisher { .. } => Err(MaterializeError::Untrusted {
                id,
                publisher: publisher(),
            }),
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
/// the trust that admits an image, and the registry missing ones come from.
#[derive(Clone)]
pub struct Store {
    user: PackageStore,
    system: Vec<Layout>,
    trust: ImageTrust,
    fetcher: Option<Arc<dyn PackageFetcher + Send + Sync>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("user", &self.user)
            .field("system", &self.system)
            .field("trust", &self.trust)
            .field("fetcher", &self.fetcher.is_some())
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
            fetcher: None,
        }
    }

    /// The device's stores as the environment names them: the user's store,
    /// the system store and its payload root, and [`ImageTrust::from_env`].
    /// Add a registry with [`Store::with_fetcher`].
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

    /// Fetch images no store holds from `fetcher` — normally the registry,
    /// `app_registry::HttpBlobs`.
    #[must_use]
    pub fn with_fetcher(mut self, fetcher: impl PackageFetcher + Send + Sync + 'static) -> Self {
        self.fetcher = Some(Arc::new(fetcher));
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

    /// The admitted metadata of image `id` and its tree, fetching and
    /// materialising it into the user's store when no store holds it.
    fn resolve(&self, id: PackageId) -> Result<(PackageMeta, PathBuf), MaterializeError> {
        if let Some((meta, layout, placed)) = self.local(id)? {
            self.trust.admit(&meta, placed)?;
            return Ok((meta, layout.payload(id)));
        }
        let fetcher = self
            .fetcher
            .as_deref()
            .ok_or(MaterializeError::NotFound(id))?;
        let fetch = |detail: String| MaterializeError::Fetch { id, detail };
        let bytes = fetcher
            .fetch_package(&id)
            .map_err(|error| fetch(error.to_string()))?
            .ok_or(MaterializeError::NotFound(id))?;
        let refusal = Mutex::new(None);
        let admitted = self.user.install_image(
            bytes.as_slice(),
            |meta| {
                let verdict = if meta.id == id {
                    self.trust.admit(meta, false)
                } else {
                    Err(fetch(format!("the registry served package {}", meta.id)))
                };
                match verdict {
                    Ok(attested_by) => Ok(Admission {
                        pinned: false,
                        attested_by,
                        source: "registry".into(),
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
            Some(fetcher),
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

/// The read-only root of image `id`: fetched (from the [`Store`]'s registry
/// when no store holds it), verified, and materialised.
///
/// The returned directory and everything in it must be treated as
/// read-only; it is shared by every environment that stacks the image.
///
/// # Errors
///
/// Returns [`MaterializeError::NotFound`] when no store holds the image and
/// no registry offers it, [`MaterializeError::NotAnImage`] for a package of
/// another kind, a trust error for an image whose publisher is not trusted
/// or is revoked, or a fetch or store error.
pub fn materialized_tree(store: &Store, id: &PackageId) -> Result<PathBuf, MaterializeError> {
    store.resolve(*id).map(|(_, tree)| tree)
}

/// The `env-runtime` the tool image `tool` was built for, materialising
/// the tool's metadata as [`materialized_tree`] does.
///
/// # Errors
///
/// Returns [`MaterializeError::NotATool`] when `tool` is not a tool image,
/// or what [`materialized_tree`] returns.
pub fn tool_runtime(store: &Store, tool: &PackageId) -> Result<PackageId, MaterializeError> {
    let (meta, _) = store.resolve(*tool)?;
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
