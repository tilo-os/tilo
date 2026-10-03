//! Turning image ids into read-only trees.
//!
//! An image is a tapp package of kind `env-runtime` or `tool`, materialised
//! by the package store as a plain read-only directory tree whose `usr/` and
//! `etc/` become the guest's. The supervisor needs no privilege to use one:
//! `bwrap` stacks the trees as overlay lowers inside the guest's own user
//! namespace.

use std::path::PathBuf;

use tilo_environment::PackageId;

/// Where images come from.
pub trait Images: Send + Sync {
    /// The read-only root of an image, fetching and materialising it first
    /// when it is not on this device.
    ///
    /// # Errors
    /// Returns why the image is unavailable.
    fn tree(&self, id: &PackageId) -> Result<PathBuf, String>;

    /// Whether the tool image `tool` was built for `runtime`. A tool is
    /// never stacked on a runtime it was not built for.
    ///
    /// # Errors
    /// Returns why it may not be stacked there.
    fn check_tool(&self, tool: &PackageId, runtime: &PackageId) -> Result<(), String>;

    /// The `env-runtime` a new environment starts on: the newest one this
    /// device holds, the system store's shipped one included.
    ///
    /// # Errors
    /// Returns why there is none.
    fn current_runtime(&self) -> Result<PackageId, String>;
}

/// OCI references (`host/repo[:tag]`, comma-separated) to fetch images no
/// local store holds from. A relay sets it for its members' supervisors; a
/// device ships its images and leaves it unset.
pub const IMAGE_SOURCES_ENV: &str = "TILO_USH_IMAGE_SOURCES";

/// Where images fetched from [`IMAGE_SOURCES_ENV`] are cached.
pub const IMAGE_CACHE_ENV: &str = "TILO_USH_IMAGE_CACHE";

/// The device's package store: system stores first, then the user's, with
/// the store's own trust deciding which images are admitted.
#[derive(Debug, Clone)]
pub struct StoreImages {
    store: package_store::Store,
}

impl StoreImages {
    pub fn new(store: package_store::Store) -> Self {
        Self { store }
    }

    /// The device's stores, as the package store finds them.
    ///
    /// # Errors
    /// Returns why the stores cannot be opened.
    pub fn from_env() -> Result<Self, String> {
        let store = package_store::Store::from_env().map_err(|e| e.to_string())?;
        Ok(Self::new(match oci_fetcher_from_env()? {
            Some(fetcher) => store.with_fetcher(fetcher),
            None => store,
        }))
    }
}

/// The OCI fetcher [`IMAGE_SOURCES_ENV`] and [`IMAGE_CACHE_ENV`] describe.
fn oci_fetcher_from_env() -> Result<Option<tilo_oci_images::OciImages>, String> {
    let Some(sources) = std::env::var_os(IMAGE_SOURCES_ENV) else {
        return Ok(None);
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
        package_store::materialized_tree(&self.store, id).map_err(|e| e.to_string())
    }

    fn check_tool(&self, tool: &PackageId, runtime: &PackageId) -> Result<(), String> {
        package_store::check_tool_runtime(&self.store, tool, runtime).map_err(|e| e.to_string())
    }

    fn current_runtime(&self) -> Result<PackageId, String> {
        let images = self.store.images().map_err(|e| e.to_string())?;
        images
            .iter()
            .filter(|meta| meta.manifest.package.kind == tilo_app_package::Kind::EnvRuntime)
            .max_by(|a, b| {
                let version = |m: &tilo_app_package::PackageMeta| {
                    semver::Version::parse(&m.manifest.package.version).ok()
                };
                version(a).cmp(&version(b))
            })
            .map(|meta| meta.id)
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
    fn current_runtime(&self) -> Result<PackageId, String> {
        Err(self.0.clone())
    }
}

/// Already-materialised trees kept as `<root>/<hex id>/`, with the current
/// runtime's id in `<root>/current-runtime` — fixture images for tests,
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

    fn current_runtime(&self) -> Result<PackageId, String> {
        let raw = std::fs::read_to_string(self.root.join("current-runtime"))
            .map_err(|e| format!("no env-runtime is installed: {e}"))?;
        tilo_environment::parse_image(raw.trim())
    }
}
