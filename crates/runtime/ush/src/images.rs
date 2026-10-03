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

    /// The `env-runtime` a new environment starts on — its release and this
    /// architecture's image: the newest one this device holds, the system
    /// store's shipped one included.
    ///
    /// # Errors
    /// Returns why there is none.
    fn current_runtime(&self) -> Result<(String, PackageId), String>;
}

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

    /// The device's stores, as the package store finds them, also trusting
    /// images this device signed — the tools its builder made.
    ///
    /// # Errors
    /// Returns why the stores cannot be opened.
    pub fn from_env() -> Result<Self, String> {
        let mut store = package_store::Store::from_env().map_err(|e| e.to_string())?;
        if let Some(key) = device_publisher() {
            let trust = store.trust().clone().with_publisher(key);
            store = store.with_trust(trust);
        }
        Ok(Self::new(store))
    }
}

impl Images for StoreImages {
    fn tree(&self, id: &PackageId) -> Result<PathBuf, String> {
        package_store::materialized_tree(&self.store, id).map_err(|e| e.to_string())
    }

    fn check_tool(&self, tool: &PackageId, runtime: &PackageId) -> Result<(), String> {
        package_store::check_tool_runtime(&self.store, tool, runtime).map_err(|e| e.to_string())
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

/// The public key of this device, whose key signs the tool images its
/// builder makes. `None` where the session cannot read it.
fn device_publisher() -> Option<[u8; 32]> {
    let paths = std::env::var_os("TILO_IDENTITY_ROOT")
        .filter(|root| !root.is_empty())
        .map_or_else(
            tilo_user_identity::IdentityPaths::default_system,
            tilo_user_identity::IdentityPaths::under,
        );
    match tilo_user_identity::persist::load_device_key(&paths) {
        Ok(key) => Some(key.verifying_key().to_bytes()),
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

/// Where images come from on this device: the package store, or fixture
/// trees under `TILO_USH_IMAGES` in tests.
pub fn on_this_device() -> std::sync::Arc<dyn Images> {
    if let Some(root) = std::env::var_os("TILO_USH_IMAGES") {
        return std::sync::Arc::new(DirImages::at(PathBuf::from(root)));
    }
    match package_store::Store::from_env() {
        Ok(store) => std::sync::Arc::new(StoreImages::new(
            env_tools::store::trusting_device_tools(store),
        )),
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
