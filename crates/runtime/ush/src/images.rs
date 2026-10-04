//! Turning image ids into read-only trees.
//!
//! An image is a tapp package of kind `env-runtime` or `tool`, materialised
//! by the package store as a plain read-only directory tree whose `usr/` and
//! `etc/` become the guest's. The supervisor needs no privilege to use one:
//! `bwrap` stacks the trees as overlay lowers inside the guest's own user
//! namespace.

use std::path::PathBuf;

use tilo_environment::{PackageId, ToolBuildError, ToolBuilder};

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

    /// Make the image `id`, built here, available to the user's other
    /// devices, so they fetch it rather than build it again.
    ///
    /// # Errors
    /// Returns why it could not be shared.
    fn share(&self, _id: &PackageId) -> Result<(), String> {
        Ok(())
    }
}

/// OCI references (`host/repo[:tag]`, comma-separated) to fetch images no
/// local store holds from. A relay sets it for its members' supervisors; a
/// device ships its images and leaves it unset.
pub const IMAGE_SOURCES_ENV: &str = "TILO_USH_IMAGE_SOURCES";

/// Where images fetched from [`IMAGE_SOURCES_ENV`] are cached.
pub const IMAGE_CACHE_ENV: &str = "TILO_USH_IMAGE_CACHE";

/// A publisher (hex Ed25519 key) whose images are trusted besides this
/// device's own: on a relay, the relay's key, which signs the tools its
/// worker builds for a member.
pub const TRUSTED_PUBLISHER_ENV: &str = "TILO_USH_TRUSTED_PUBLISHER";

/// The shelf images are fetched from and shared on, when not the one in
/// the user's synced files: on a relay, the member's own shelf, where the
/// worker leaves the tools it signed for them.
pub const IMAGE_SHELF_ENV: &str = "TILO_USH_IMAGE_SHELF";

/// The device's package store: system stores first, then the user's, with
/// the store's own trust deciding which images are admitted, and the shelf
/// the user's devices share their builds on.
#[derive(Debug, Clone)]
pub struct StoreImages {
    store: package_store::Store,
    shelf: Option<package_store::ImageShelf>,
}

/// Where in the user's synced files their devices shelve the images they
/// build.
pub const SHELF_DIR: &str = ".tilo/images";

impl StoreImages {
    pub fn new(store: package_store::Store) -> Self {
        Self { store, shelf: None }
    }

    /// Fetch images from `shared` too, and share the ones built here on it.
    #[must_use]
    pub fn with_shelf(mut self, shared: package_store::ImageShelf) -> Self {
        self.store = self.store.with_shelf(shared.clone());
        self.shelf = Some(shared);
        self
    }

    /// The device's stores, as the package store finds them, also trusting
    /// images this device signed (the tools its builder made) and the ones
    /// the user's other devices signed while they are the user's
    /// ([`crate::fleet`]). Images built elsewhere are fetched from the shelf
    /// in the user's synced files, when this session has them mounted.
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
        let mut store = store.with_trust(trust);
        if let Some(fetcher) = oci_fetcher_from_env()? {
            store = store.with_fetcher(fetcher);
        }
        let images = Self::new(store);
        let shelf = std::env::var_os(IMAGE_SHELF_ENV)
            .map(PathBuf::from)
            .filter(|shelf| shelf.is_absolute())
            .or_else(|| xdg_paths::workspace_root().map(|synced| synced.join(SHELF_DIR)));
        Ok(match shelf {
            Some(shelf) => images.with_shelf(package_store::ImageShelf::at(shelf)),
            None => images,
        })
    }
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

/// The publisher [`TRUSTED_PUBLISHER_ENV`] names.
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

    fn share(&self, id: &PackageId) -> Result<(), String> {
        // A catalog image the builder named rather than built is fetched by
        // every device from where this one gets it.
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use package_store::{Admission, CertifiedFleet, ImageShelf, ImageTrust, PackageStore, Store};
    use tilo_app_package::manifest::Arch;
    use tilo_app_package::{ImageSpec, Kind, Packed, SigningIdentity, seal_image};
    use tilo_environment::{DEFAULT_RECORD, Environment};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, UserIdentity};

    use super::*;
    use crate::supervisor::{Session, SessionOptions};

    fn seal(
        dir: &Path,
        kind: Kind,
        runtime: Option<PackageId>,
        signer: &SigningIdentity,
    ) -> Packed {
        let file = if kind == Kind::Tool {
            "usr/bin/pandoc"
        } else {
            "usr/bin/bash"
        };
        std::fs::create_dir_all(dir.join("usr/bin")).unwrap();
        std::fs::create_dir_all(dir.join("etc")).unwrap();
        std::fs::write(dir.join(file), file).unwrap();
        let spec = ImageSpec {
            kind,
            id: format!("so.tilo.{kind}"),
            version: "44.1.0".into(),
            name: None,
            arch: if cfg!(target_arch = "aarch64") {
                Arch::AArch64
            } else {
                Arch::X86_64
            },
            runtime,
            packages: vec![format!(
                "pandoc-0:3.1-1.fc44.{}",
                tilo_environment::host_arch()
            )],
        };
        seal_image(dir, &spec, signer).unwrap()
    }

    fn full(packed: &Packed) -> Vec<u8> {
        tilo_app_package::tapp::to_bytes(&packed.meta, Some(packed)).unwrap()
    }

    /// Counts builds; a fetched image must not need one.
    struct CountedBuilds(AtomicUsize);
    impl ToolBuilder for CountedBuilds {
        fn build(
            &self,
            _runtime: &PackageId,
            _packages: &[String],
            _progress: &mut dyn std::io::Write,
        ) -> Result<PackageId, ToolBuildError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Err(ToolBuildError::Failed("no builder here".into()))
        }
    }

    /// A tool the user's laptop built and shelved opens on their relay as
    /// it is, while the laptop is theirs; once it is removed, the relay
    /// refuses it and builds its own.
    #[test]
    fn a_tool_built_on_another_of_your_devices_is_fetched_not_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let ci = SigningIdentity::generate();
        let laptop = SigningIdentity::generate();
        let (root, user) = UserIdentity::generate("Ada", 1);
        let mut cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: laptop.public_key_bytes(),
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        let mut fleet = CertifiedFleet::new(user.public_key);
        fleet.certify(cert).unwrap();

        // The OS ships the runtime; the laptop built pandoc on it.
        let runtime = seal(&dir.path().join("runtime"), Kind::EnvRuntime, None, &ci);
        let tool = seal(
            &dir.path().join("tool"),
            Kind::Tool,
            Some(runtime.meta.id),
            &laptop,
        );
        let system = dir.path().join("system");
        PackageStore::at(&system)
            .install_image(
                full(&runtime).as_slice(),
                |_| Ok(Admission::default()),
                None,
            )
            .unwrap();
        let laptop_store = PackageStore::at(dir.path().join("laptop"));
        laptop_store
            .install_image(full(&tool).as_slice(), |_| Ok(Admission::default()), None)
            .unwrap();
        let shelf = ImageShelf::at(dir.path().join("synced").join(SHELF_DIR));
        let laptop_images = StoreImages::new(Store::new(laptop_store)).with_shelf(shelf.clone());
        laptop_images.share(&tool.meta.id).unwrap();

        // The relay: its own store, the user's fleet, the same synced files.
        let relay_store = |fleet: CertifiedFleet| {
            let store = Store::new(PackageStore::at(dir.path().join("relay")))
                .with_system(package_store::Layout::new(&system))
                .with_trust(ImageTrust::new().with_fleet(fleet));
            Arc::new(StoreImages::new(store).with_shelf(shelf.clone()))
        };
        let builds = Arc::new(CountedBuilds(AtomicUsize::new(0)));
        let ush = crate::store::Store::at(dir.path().join("ush"));
        ush.ensure().unwrap();
        let mut session = Session::new(
            ush,
            SessionOptions::default(),
            relay_store(fleet.clone()),
            builds.clone(),
        );
        session.resolve().unwrap();
        crate::records::Records::at(session.store.records_root())
            .apply(
                DEFAULT_RECORD,
                Environment::ADD_TOOL.with((
                    vec!["pandoc".into()],
                    tilo_environment::host_arch().into(),
                    tool.meta.id.to_string(),
                )),
            )
            .unwrap();

        let resolved = session.resolve().unwrap();
        assert_eq!(builds.0.load(Ordering::Relaxed), 0, "fetched, not rebuilt");
        let (label, tree) = &resolved.images[0];
        assert!(label.contains("pandoc"), "{label}");
        assert!(tree.starts_with(dir.path().join("relay")));
        assert!(tree.join("usr/bin/pandoc").is_file());

        // The laptop is removed from the user's devices.
        let mut removal = tilo_user_identity::RevocationEntry {
            user_public_key: user.public_key,
            revoked_device_public_key: laptop.public_key_bytes(),
            revoked_at_ns: 2,
            reason: tilo_user_identity::RevocationReason::Stolen,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut removal, &root).unwrap();
        fleet.revoke(removal).unwrap();
        session.images = relay_store(fleet);
        let refused = session.resolve().unwrap_err().to_string();
        assert!(refused.contains("removed from your devices"), "{refused}");
        assert_eq!(builds.0.load(Ordering::Relaxed), 1, "built here instead");
    }
}
