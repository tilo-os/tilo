//! [`ToolStore`] over the device's package store.
//!
//! Runtimes come from [`package_store::materialized_tree`] (system store,
//! user store, then the registry). A built tool is sealed with the device's
//! key and installed into the user's store as an image, where the same
//! `materialized_tree` finds it — provided the reader trusts the device key
//! as an image publisher ([`trust_builder`]).

use std::path::{Path, PathBuf};

use package_store::{Admission, Store};
use tilo_app_package::manifest::Arch;
use tilo_app_package::{PackageId, SigningIdentity};

use crate::builder::{Builder, ToolStore};
use crate::image;
use crate::index::Catalog;
use crate::key::CacheKey;

/// The package store, and the identity tools built here are signed with.
pub struct PackageStoreTools {
    store: Store,
    identity: SigningIdentity,
    arch: Arch,
}

impl PackageStoreTools {
    /// Tools signed by `identity`, kept in `store`'s user store. The store's
    /// trust is extended to `identity`, so the tools it builds materialise.
    ///
    /// # Errors
    ///
    /// On an architecture images aren't built for.
    pub fn new(store: Store, identity: SigningIdentity) -> anyhow::Result<Self> {
        Ok(Self {
            store: trust_builder(store, &identity),
            identity,
            arch: image::host_arch()?,
        })
    }

    pub fn store(&self) -> &Store {
        &self.store
    }
}

/// `store`, also trusting images `identity` signs: what a reader of this
/// device's tools (the `ush` supervisor) needs to stack them.
pub fn trust_builder(store: Store, identity: &SigningIdentity) -> Store {
    let trust = store
        .trust()
        .clone()
        .with_publisher(identity.public_key_bytes());
    store.with_trust(trust)
}

impl ToolStore for PackageStoreTools {
    fn runtime_tree(&self, runtime: &PackageId) -> anyhow::Result<PathBuf> {
        Ok(package_store::materialized_tree(&self.store, runtime)?)
    }

    fn has(&self, tool: &PackageId) -> bool {
        package_store::materialized_tree(&self.store, tool).is_ok()
    }

    fn store_tool(
        &self,
        tree: &Path,
        key: &CacheKey,
        requested: &[String],
    ) -> anyhow::Result<PackageId> {
        let packed = image::seal_tool(tree, key, requested, self.arch, &self.identity)?;
        let tapp = tree.with_extension("tapp");
        image::write_tapp(&tapp, &packed)?;
        let installed = std::fs::File::open(&tapp)
            .map_err(anyhow::Error::from)
            .and_then(|file| {
                Ok(self.store.user().install_image(
                    std::io::BufReader::new(file),
                    |_| {
                        Ok(Admission {
                            pinned: false,
                            attested_by: String::new(),
                            source: "built on this device".into(),
                        })
                    },
                    None,
                )?)
            });
        let _ = std::fs::remove_file(&tapp);
        let meta = installed?;
        anyhow::ensure!(
            meta.id == packed.id(),
            "the store kept {} for the tool sealed as {}",
            meta.id,
            packed.id()
        );
        Ok(meta.id)
    }
}

/// Where a device's builder keeps its index, dnf caches and scratch:
/// `$XDG_CACHE_HOME/tilo/env-tools` (all of it can be rebuilt).
pub fn default_state_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|home| PathBuf::from(home).join(".cache"))
        })
        .map(|cache| cache.join("tilo/env-tools"))
}

/// The catalog a device consults: the document at `TILO_TOOL_CATALOG`, or
/// none.
pub fn default_catalog() -> Option<Catalog> {
    let path = std::env::var_os("TILO_TOOL_CATALOG").filter(|v| !v.is_empty())?;
    match Catalog::load(Path::new(&path)) {
        Ok(catalog) => Some(catalog),
        Err(error) => {
            tracing::warn!(%error, "ignoring the tool catalog");
            None
        }
    }
}

/// This device's tool builder: the package store from the environment, the
/// device key, the default catalog and state directory. What `pkg install`
/// calls on a device, and what a relay's compute worker runs for a member.
///
/// # Errors
///
/// No user store, no device key, or no state directory.
pub fn device_builder() -> anyhow::Result<Builder<PackageStoreTools, Option<Catalog>>> {
    let store = Store::from_env()?;
    let identity = crate::signing::device_identity()?;
    let state = default_state_dir()
        .ok_or_else(|| anyhow::anyhow!("no cache directory for the tool builder"))?;
    Ok(Builder::new(
        PackageStoreTools::new(store, identity)?,
        default_catalog(),
        state,
    ))
}
