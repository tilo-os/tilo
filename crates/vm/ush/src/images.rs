//! Turning image ids into read-only trees.
//!
//! An image is a tapp package of kind `env-runtime` or `tool`, materialised by the
//! package store as a plain read-only tree whose `usr/` and `etc/` become the guest's.
//! The supervisor needs no privilege for it: `bwrap` stacks the trees as overlay lowers
//! inside the guest's own user namespace.

use std::path::PathBuf;

use tilo_environment::{PackageId, ToolBuildError, ToolBuilder};

pub trait Images: Send + Sync {
    /// The read-only root of an image, fetching and materialising it first
    /// when it is not on this device.
    fn tree(&self, id: &PackageId) -> Result<PathBuf, String>;

    fn is_local(&self, _id: &PackageId) -> bool {
        true
    }

    /// Whether the tool image `tool` was built for `runtime`. A tool is
    /// never stacked on a runtime it was not built for.
    fn check_tool(&self, tool: &PackageId, runtime: &PackageId) -> Result<(), String>;

    fn current_runtime(&self) -> Result<(String, PackageId), String>;

    /// Make the image `id`, built here, available to the user's other
    /// devices, so they fetch it rather than build it again.
    fn share(&self, _id: &PackageId) -> Result<(), String> {
        Ok(())
    }
}

/// OCI references (`host/repo[:tag]`, comma-separated) to fetch images no
/// local store holds from instead of the repositories CI publishes to. A
/// server sets it for its members' supervisors; a device leaves it unset.
pub const IMAGE_SOURCES_ENV: &str = "TILO_USH_IMAGE_SOURCES";

pub const IMAGE_CACHE_ENV: &str = "TILO_USH_IMAGE_CACHE";

pub const TRUSTED_PUBLISHER_ENV: &str = "TILO_USH_TRUSTED_PUBLISHER";

/// The shelf images are fetched from and shared on, when not the one in
/// the user's synced files: on a server, the member's own shelf, where the
/// worker leaves the tools it signed for them.
pub const IMAGE_SHELF_ENV: &str = "TILO_USH_IMAGE_SHELF";

#[derive(Debug, Clone)]
pub struct StoreImages {
    store: package_store::Store,
    shelf: Option<package_store::ImageShelf>,
}

pub const SHELF_DIR: &str = ".tilo/images";

impl StoreImages {
    pub fn new(store: package_store::Store) -> Self {
        Self { store, shelf: None }
    }

    #[must_use]
    pub fn with_shelf(mut self, shared: package_store::ImageShelf) -> Self {
        self.store = self.store.with_shelf(shared.clone());
        self.shelf = Some(shared);
        self
    }

    /// The device's stores, as the package store finds them, also trusting
    /// images this device signed (the tools its builder made) and the ones
    /// the user's other devices signed while they are the user's
    /// ([`crate::fleet`]). An image no store holds comes from the shelf in
    /// the user's synced files ([`synced_files`]), then from the OCI registry
    /// ([`registry_images`]); only a tool neither has is built.
    ///
    /// # Errors
    /// Returns why the stores cannot be opened.
    pub fn from_env() -> Result<Self, String> {
        let store = package_store::Store::from_env().map_err(|e| e.to_string())?;
        let mut trust = store
            .trust()
            .clone()
            .with_fleet(crate::fleet::UserFleet::from_env());
        if let Some(key) = device_publisher() {
            trust = trust.with_publisher(key);
        }
        if let Some(key) = trusted_publisher_from_env()? {
            trust = trust.with_publisher(key);
        }
        let mut images = Self::new(store.with_trust(trust));
        let shelf = std::env::var_os(IMAGE_SHELF_ENV)
            .map(PathBuf::from)
            .filter(|shelf| shelf.is_absolute())
            .or_else(|| synced_files().map(|synced| synced.join(SHELF_DIR)));
        if let Some(shelf) = shelf {
            images = images.with_shelf(package_store::ImageShelf::at(shelf));
        }
        if let Some(registry) = registry_images()? {
            images.store = images.store.with_fetcher(registry);
        }
        Ok(images)
    }
}

/// The user's synced files as the supervisor reaches them: the workspace the
/// session names, else the filesystem daemon's mount at
/// [`tilo_paths::WORKSPACE_MOUNT`], where the Files app reads them too. The
/// supervisor is the `tilo-ush` user service, started with no workspace in
/// its environment and in the host's mount namespace, so the mount is what a
/// normal session finds.
pub fn synced_files() -> Option<PathBuf> {
    tilo_paths::workspace_root().or_else(|| {
        let mount = PathBuf::from(tilo_paths::WORKSPACE_MOUNT);
        mount.is_dir().then_some(mount)
    })
}

/// A tool builder whose images the user's other devices can fetch: each
/// one built is shared through [`Images::share`]. A build that cannot be
/// shared still serves this device.
pub struct SharedBuilds {
    builder: std::sync::Arc<dyn ToolBuilder>,
    images: std::sync::Arc<dyn Images>,
}

impl SharedBuilds {
    pub fn new(
        builder: std::sync::Arc<dyn ToolBuilder>,
        images: std::sync::Arc<dyn Images>,
    ) -> Self {
        Self { builder, images }
    }
}

impl ToolBuilder for SharedBuilds {
    fn build(
        &self,
        runtime: &PackageId,
        packages: &[String],
        progress: &mut dyn std::io::Write,
    ) -> Result<PackageId, ToolBuildError> {
        let id = self.builder.build(runtime, packages, progress)?;
        if let Err(reason) = self.images.share(&id) {
            let _ = writeln!(
                progress,
                "ush: {id} is built, but your other devices will build it again: {reason}"
            );
        }
        Ok(id)
    }
}

fn trusted_publisher_from_env() -> Result<Option<[u8; 32]>, String> {
    let Some(hex_key) = std::env::var_os(TRUSTED_PUBLISHER_ENV).filter(|key| !key.is_empty())
    else {
        return Ok(None);
    };
    hex::decode(hex_key.to_string_lossy().as_bytes())
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .map(Some)
        .ok_or_else(|| format!("{TRUSTED_PUBLISHER_ENV} is not a 32-byte hex key"))
}

/// The OCI registry images no local store or shelf holds come from:
/// [`IMAGE_SOURCES_ENV`] (cached in [`IMAGE_CACHE_ENV`]) on a server, else the
/// repositories CI publishes env-runtimes and catalog tools to, cached under
/// `$XDG_CACHE_HOME/tilo/env-images`. `None` on a device without a cache
/// directory. Where an image came from earns it no trust: the CI key the
/// system store names, or one of the user's devices, must have signed it.
pub fn registry_images() -> Result<Option<tilo_oci_images::OciImages>, String> {
    let Some(sources) = std::env::var_os(IMAGE_SOURCES_ENV).filter(|s| !s.is_empty()) else {
        return Ok(tilo_paths::xdg::cache_home()
            .filter(|cache| cache.is_absolute())
            .map(|cache| tilo_oci_images::OciImages::published(cache.join("tilo/env-images"))));
    };
    let sources = sources
        .to_string_lossy()
        .split(',')
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .map(tilo_oci_images::OciSource::parse)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{IMAGE_SOURCES_ENV}: {e}"))?;
    let cache = std::env::var_os(IMAGE_CACHE_ENV)
        .map(PathBuf::from)
        .filter(|cache| cache.is_absolute())
        .ok_or_else(|| format!("{IMAGE_SOURCES_ENV} needs an absolute {IMAGE_CACHE_ENV}"))?;
    Ok((!sources.is_empty()).then(|| tilo_oci_images::OciImages::new(sources, cache)))
}

impl Images for StoreImages {
    fn tree(&self, id: &PackageId) -> Result<PathBuf, String> {
        package_store::local_materialized_tree(&self.store, id).map_err(|e| e.to_string())
    }

    fn is_local(&self, id: &PackageId) -> bool {
        // Whether using the image needs no import: this account holds a copy, or
        // a system store serves the image's tree where it lies (which never
        // fetches).
        self.store
            .user()
            .image(*id)
            .is_ok_and(|meta| meta.is_some())
            || package_store::serves_in_place(&self.store, id).unwrap_or(false)
    }

    fn check_tool(&self, tool: &PackageId, runtime: &PackageId) -> Result<(), String> {
        package_store::check_tool_runtime(&self.store, tool, runtime).map_err(|e| e.to_string())
    }

    fn share(&self, id: &PackageId) -> Result<(), String> {
        if self
            .store
            .user()
            .image(*id)
            .map_err(|e| e.to_string())?
            .is_none()
        {
            return Ok(());
        }
        let Some(target) = &self.shelf else {
            return Err("this session has no synced files to share it through".into());
        };
        target
            .put(self.store.user(), *id)
            .map(drop)
            .map_err(|e| e.to_string())
    }

    fn current_runtime(&self) -> Result<(String, PackageId), String> {
        let images = self.store.images().map_err(|e| e.to_string())?;
        let for_this_arch = |meta: &&tilo_app_package::PackageMeta| {
            let isa = &meta.manifest.package.isa;
            if cfg!(target_arch = "aarch64") {
                isa.aarch64.is_some()
            } else {
                isa.x86_64.is_some()
            }
        };
        images
            .iter()
            .filter(|meta| meta.manifest.package.kind == tilo_app_package::Kind::EnvRuntime)
            .filter(for_this_arch)
            .max_by(|a, b| {
                let version = |m: &tilo_app_package::PackageMeta| {
                    semver::Version::parse(&m.manifest.package.version).ok()
                };
                version(a).cmp(&version(b))
            })
            .map(|meta| (meta.manifest.package.version.clone(), meta.id))
            .ok_or_else(|| {
                "no env-runtime image is installed; Tilo OS and the tilo-session-apps \
                 package ship one in the system app store"
                    .to_owned()
            })
    }
}

/// A device whose package store could not be opened: every question
/// answers with why.
#[derive(Debug, Clone)]
pub struct NoImages(pub String);

impl Images for NoImages {
    fn tree(&self, _id: &PackageId) -> Result<PathBuf, String> {
        Err(self.0.clone())
    }
    fn check_tool(&self, _tool: &PackageId, _runtime: &PackageId) -> Result<(), String> {
        Err(self.0.clone())
    }
    fn current_runtime(&self) -> Result<(String, PackageId), String> {
        Err(self.0.clone())
    }
}

/// Where this device's identity is kept: the root every program of this account resolves for it
/// ([`tilo_device_identity::persist::collaboration_identity_root`]), whose certificate names the
/// device's key.
fn identity_paths() -> tilo_device_identity::IdentityPaths {
    tilo_device_identity::IdentityPaths::under(
        tilo_device_identity::persist::collaboration_identity_root(),
    )
}

/// The device actions of this computer's daemon: what keeps Terminal's documents and carries a
/// hosted workspace's shells here.
pub(crate) fn device_actions() -> Option<tilo_device_identity::login::daemon::DeviceActions> {
    tilo_device_identity::login::daemon::DeviceActions::here(identity_paths())
        .inspect_err(|error| tracing::debug!(%error, "ush: this computer's daemon is not reached"))
        .ok()
}

/// This device's public key, as the certificate its person's root signed names it: the device
/// key itself stays with the device's daemon.
fn device_publisher() -> Option<[u8; 32]> {
    match tilo_device_identity::persist::read_device_cert(&identity_paths().device_cert) {
        Ok(cert) => Some(cert.device_public_key),
        Err(error) => {
            tracing::debug!(%error, "no device key; tools built here are not trusted by key");
            None
        }
    }
}

/// Already-materialised trees kept as `<root>/<hex id>/`, with the current
/// runtime's id and release on the two lines of `<root>/current-runtime` —
/// fixture images for tests,
/// selected with `TILO_USH_IMAGES`. Every tool is taken to be built for the
/// runtime it is listed with.
#[derive(Debug, Clone)]
pub struct DirImages {
    root: PathBuf,
}

impl DirImages {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

pub fn on_this_device() -> std::sync::Arc<dyn Images> {
    if let Some(root) = std::env::var_os("TILO_USH_IMAGES") {
        return std::sync::Arc::new(DirImages::at(PathBuf::from(root)));
    }
    match StoreImages::from_env() {
        Ok(images) => std::sync::Arc::new(images),
        Err(reason) => std::sync::Arc::new(NoImages(format!(
            "the package store cannot be opened: {reason}"
        ))),
    }
}

impl Images for DirImages {
    fn tree(&self, id: &PackageId) -> Result<PathBuf, String> {
        let tree = self.root.join(id.to_hex());
        if tree.is_dir() {
            Ok(tree)
        } else {
            Err(format!("image {id} is not on this device"))
        }
    }

    fn check_tool(&self, _tool: &PackageId, _runtime: &PackageId) -> Result<(), String> {
        Ok(())
    }

    fn current_runtime(&self) -> Result<(String, PackageId), String> {
        let raw = std::fs::read_to_string(self.root.join("current-runtime"))
            .map_err(|e| format!("no env-runtime is installed: {e}"))?;
        let mut lines = raw.lines();
        let id = tilo_environment::parse_image(lines.next().unwrap_or_default().trim())?;
        let release = lines.next().unwrap_or("fixture").trim().to_owned();
        Ok((release, id))
    }
}
